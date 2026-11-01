//! Host slots for heavy checks (ADR-0014, `hooks.host-concurrency`).
//!
//! Several worktrees, sessions and agents on one machine each run their own
//! clippy, test suite and type checker, and each of those already uses every
//! core. A check that compiles or executes the product is therefore Heavy,
//! and takes a host slot before its first tool runs. The protocol (the slot
//! directory, the lock, the queue, admission, the holder record) is the
//! `amont-slots` crate, the one implementation relais links too; this module
//! is the hook's half: which checks are Heavy, the busy policy, deferral, and
//! the queue row.
//!
//! The slot is taken LAZILY, at the first tool spawn of a heavy check, not
//! when the check starts: a clippy with no Rust staged returns at once and
//! must not wait behind somebody else's suite to do nothing.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub use amont_slots::{DIR_ENV, HELD_ENV};

/// The checks that compile or execute the product. Pinned by a registry
/// test: every name here is a built-in.
pub const HEAVY: &[&str] = &[
    "pre-commit-clippy",
    "pre-commit-go-vet",
    "pre-commit-pyright",
    "pre-push-run-tests-js",
    "pre-push-cargo-test",
    "pre-push-go-test",
    "pre-push-pytest",
];

/// Whether a check is Heavy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weight {
    Light,
    Heavy,
}

pub fn weight_of(name: &str) -> Weight {
    if HEAVY.contains(&name) {
        Weight::Heavy
    } else {
        Weight::Light
    }
}

/// Why a heavy check ran without a slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// The slot directory is not one this user alone owns.
    DirUnsafe(String),
    /// No `flock` here: Windows, the stated divergence.
    Unsupported,
    /// No slot freed within the budget.
    TimedOut(Duration),
    /// A slot file could not be opened or locked for a reason other than
    /// "another holder has it".
    Io(String),
}

/// Set once a check of this process deferred: the host was measured busy,
/// and a later Heavy check of the same run defers without waiting again, so
/// a push waits at most one `slotWait`.
static BUSY_SEEN: AtomicBool = AtomicBool::new(false);

/// `amont.hostSlots`, a host key: unset sizes the slots from the machine
/// (`Sizing::Auto`), N fixes them, `0` turns them off (`None`).
pub fn sizing(settings: &crate::config::Settings) -> Option<amont_slots::Sizing> {
    match crate::config::host_integer_or(settings, "amont.hostSlots", -1, 0..=64) {
        0 => None,
        n if n < 0 => Some(amont_slots::Sizing::Auto),
        n => Some(amont_slots::Sizing::Fixed(n as u32)),
    }
}

/// The environment override of `amont.onBusy`, for one run:
/// `AMONT_ON_BUSY=wait git push` runs a gate the file would defer.
pub const ON_BUSY_ENV: &str = "AMONT_ON_BUSY";

/// What a Heavy check does when no slot frees in time (ADR-0013,
/// `hooks.busy-policy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusyPolicy {
    /// Wait up to `amont.timeout`, then run unqueued with a note: ADR-0009's
    /// behaviour, and the default.
    Wait,
    /// Wait up to `slot_wait`, then defer the check to CI.
    Defer { slot_wait: Duration },
}

/// The busy policy in force: `AMONT_ON_BUSY`, then `amont.onBusy` (git
/// config, or `set onBusy` in a trusted `amont.conf`), then `wait`.
pub fn busy_policy(settings: &crate::config::Settings) -> BusyPolicy {
    resolve_busy(std::env::var(ON_BUSY_ENV).ok().as_deref(), settings)
}

/// [`busy_policy`] with the environment value handed in: the testable seam,
/// because setting a variable in a threaded test binary races its siblings.
fn resolve_busy(from_env: Option<&str>, settings: &crate::config::Settings) -> BusyPolicy {
    let from_env = from_env.filter(|v| !v.is_empty());
    let mode = match from_env.map(str::to_ascii_lowercase).as_deref() {
        Some("wait") => "wait",
        Some("defer") => "defer",
        Some(other) => {
            crate::config::complain(
                ON_BUSY_ENV,
                &format!("{other:?} is not one of wait, defer"),
                "amont.onBusy",
            );
            crate::config::enumerated_or(settings, "amont.onBusy", &["wait", "defer"], "wait")
        }
        None => crate::config::enumerated_or(settings, "amont.onBusy", &["wait", "defer"], "wait"),
    };
    if mode == "defer" {
        let secs = crate::config::integer_or(settings, "amont.slotWait", 60, 1..=3600) as u64;
        BusyPolicy::Defer {
            slot_wait: Duration::from_secs(secs),
        }
    } else {
        BusyPolicy::Wait
    }
}

/// What the check running on THIS thread is, as far as slots go.
#[derive(Default)]
struct Current {
    /// The check's name, for the messages its tools' runners print.
    name: String,
    heavy: bool,
    /// The slot question has been answered for this check (held, or
    /// unqueued for a reason).
    settled: bool,
    held: Option<amont_slots::Lease>,
    queued: Duration,
    /// Set when the busy policy deferred this check (ADR-0013): how long it
    /// waited. Read by the dispatcher through [`CheckGuard::deferred`], and it
    /// overrides whatever the check returned, so a check whose own error
    /// handling turns the refused spawn into `Unavailable` or `Failed` still
    /// ends Deferred.
    deferred: Option<Duration>,
    /// How many jobs this heavy check's tools may run
    /// (`amont_slots::parallelism`), held or unqueued; `None` when slots are
    /// off, unsupported, or nested under a holder whose bounds it inherits.
    jobs: Option<u32>,
    /// The check's severity cannot block the commit or push (`warn`): a
    /// finding it might make never stops anything, so it never waits for a
    /// slot. Set by [`never_waits`].
    cannot_block: bool,
}

thread_local! {
    static CURRENT: RefCell<Current> = RefCell::new(Current::default());
}

/// Installed by the dispatcher around one check's `run`; clears this
/// thread's slot state, and releases a held slot, when dropped.
pub struct CheckGuard;

impl CheckGuard {
    /// How long this check waited for a slot so far.
    pub fn queued(&self) -> Duration {
        CURRENT.with(|c| c.borrow().queued)
    }

    /// Whether the busy policy deferred this check, and after how long a
    /// wait. Read before the guard drops.
    pub fn deferred(&self) -> Option<Duration> {
        CURRENT.with(|c| c.borrow().deferred)
    }
}

/// Whether the check running on this thread was deferred by the busy
/// policy: what it says after that is about its refused spawn.
pub fn deferred_here() -> bool {
    CURRENT.with(|c| c.borrow().deferred.is_some())
}

/// The error a refused spawn returns. Only a way to stop the spawn: what the
/// check makes of it does not matter, because [`CheckGuard::deferred`] wins.
fn refusal() -> std::io::Error {
    std::io::Error::other("deferred to CI: no host slot freed (amont.onBusy defer)")
}

impl Drop for CheckGuard {
    fn drop(&mut self) {
        CURRENT.with(|c| *c.borrow_mut() = Current::default());
    }
}

/// The dispatcher is about to run `name`, of `weight`, on this thread. The
/// weight comes from the check (`Check::weight`), not the name: a declared
/// check is Heavy by its line, which no name says.
pub fn enter_check(name: &str, weight: Weight) -> CheckGuard {
    CURRENT.with(|c| {
        *c.borrow_mut() = Current {
            name: name.to_string(),
            heavy: weight == Weight::Heavy,
            ..Current::default()
        }
    });
    CheckGuard
}

/// The check running on this thread cannot block (its severity is `warn`):
/// on a busy host it takes a free slot or defers to CI at once, under either
/// busy policy, instead of holding the commit, or the push and its open
/// connection to the remote, while it queues for a verdict that stops
/// nothing.
pub fn never_waits() {
    CURRENT.with(|c| c.borrow_mut().cannot_block = true);
}

/// The name of the check running on this thread, without its stage
/// prefix (`clippy`), when the dispatcher installed one.
pub fn current_check() -> Option<String> {
    CURRENT.with(|c| {
        let c = c.borrow();
        (!c.name.is_empty()).then(|| crate::short_name(&c.name).to_string())
    })
}

/// Whether this process was started by an amont that holds a slot.
fn nested() -> bool {
    std::env::var_os(HELD_ENV).is_some_and(|v| !v.is_empty())
}

/// Called before a tool is spawned. For the first tool of a heavy check,
/// waits for a slot, showing the wait in the check's row. Returns whether
/// this thread holds a slot, so the caller exports [`HELD_ENV`]; an `Err`
/// when the busy policy deferred the check, so the caller does not spawn.
pub fn before_spawn(settings: &crate::config::Settings) -> std::io::Result<bool> {
    let (pending, deferred, name, cannot_block) = CURRENT.with(|c| {
        let c = c.borrow();
        (
            c.heavy && !c.settled,
            c.deferred.is_some(),
            c.name.clone(),
            c.cannot_block,
        )
    });
    // A check that caught the first refusal and tries another tool is
    // refused at once, not after a second wait.
    if deferred {
        return Err(refusal());
    }
    if pending {
        // Nested first: a nested amont must not pay the host-key scan for a
        // slot count it is about to ignore. Neither answer queues, so neither
        // can defer.
        let sizing = if nested() { None } else { sizing(settings) };
        let Some(sizing) = sizing else {
            CURRENT.with(|c| c.borrow_mut().settled = true);
            return Ok(false);
        };
        if !amont_slots::SUPPORTED {
            CURRENT.with(|c| c.borrow_mut().settled = true);
            return Ok(false);
        }
        let policy = if cannot_block {
            BusyPolicy::Defer {
                slot_wait: Duration::ZERO,
            }
        } else {
            busy_policy(settings)
        };
        let ceiling = Duration::from_secs(match crate::hooks::common::check_timeout(settings) {
            0 => 3600,
            s => s,
        });
        let budget = match policy {
            BusyPolicy::Wait => ceiling,
            // The host was already measured busy by this run: defer now.
            BusyPolicy::Defer { .. } if BUSY_SEEN.load(Ordering::SeqCst) => Duration::ZERO,
            BusyPolicy::Defer { slot_wait } => slot_wait,
        };
        let cap = u64::from(sizing.cap());
        // Hook checks take one slot; the width is set whether a slot is
        // granted or the check runs unqueued, which happens at peak load.
        let jobs = amont_slots::parallelism(1, amont_slots::cores(), sizing.cap());
        let sink = crate::live::current_sink();
        if let Some((stage, idx)) = &sink {
            stage.queue(*idx, Some((Instant::now(), cap)));
        }
        let started = Instant::now();
        let holder = amont_slots::Holder::here("amont", &name);
        let mut got = wait_for_slot(sizing, holder.clone(), budget);
        if let (BusyPolicy::Defer { .. }, Err(Reason::TimedOut(_))) = (policy, &got) {
            // A sibling check of this process holds a slot: the host is busy
            // with this commit's own work, which is not a reason to defer.
            // Wait as `wait` does, up to the ceiling; else one more pass, as a
            // sibling may have released between the last poll and now.
            let rest = if amont_slots::held_in_process() > 0 && !cannot_block {
                ceiling.saturating_sub(started.elapsed())
            } else {
                Duration::ZERO
            };
            got = wait_for_slot(sizing, holder, rest);
        }
        if let Some((stage, idx)) = &sink {
            stage.queue(*idx, None);
        }
        let waited = started.elapsed();
        CURRENT.with(|c| c.borrow_mut().queued += waited);
        // Piped output never shows the region, and a heartbeat only comes
        // after a minute: say once where the time went.
        if got.is_ok() && waited >= Duration::from_secs(1) {
            crate::hooks::common::say(&format!(
                "  (waited {} for a host slot, {} {cap})",
                crate::hooks::common::human_secs(waited.as_secs()),
                crate::ui::highlight("amont.hostSlots")
            ));
        }
        let defer =
            matches!(policy, BusyPolicy::Defer { .. }) && matches!(got, Err(Reason::TimedOut(_)));
        let outcome = match (&got, defer) {
            (Ok(_), _) => "admitted",
            (Err(_), true) => "deferred",
            (Err(_), false) => "unqueued",
        };
        amont_slots::record_wait_jobs(
            &amont_slots::dir(),
            "amont",
            1,
            Some(jobs),
            waited.as_millis(),
            outcome,
        );
        let held = match got {
            Ok(lease) => Some(lease),
            // The deferral line is the dispatcher's, not this note.
            Err(_) if defer => None,
            Err(reason) => {
                note(&reason, cap);
                None
            }
        };
        if defer {
            BUSY_SEEN.store(true, Ordering::SeqCst);
        }
        CURRENT.with(|c| {
            let mut c = c.borrow_mut();
            c.held = held;
            c.settled = true;
            c.jobs = Some(jobs);
            if defer {
                c.deferred = Some(waited);
            }
        });
        if defer {
            return Err(refusal());
        }
    }
    Ok(CURRENT.with(|c| c.borrow().held.is_some()))
}

/// Bound `cmd` to the width this thread's heavy check was given (see
/// `Current::jobs`); a light check, or slots off, sets nothing. Called by
/// every runner after [`before_spawn`].
pub fn bound(cmd: &mut std::process::Command) {
    if let Some(jobs) = CURRENT.with(|c| c.borrow().jobs) {
        amont_slots::bound_width(cmd, jobs);
    }
}

/// Wait up to `budget` for one slot, saying once whom the check waits
/// behind.
fn wait_for_slot(
    sizing: amont_slots::Sizing,
    holder: amont_slots::Holder,
    budget: Duration,
) -> Result<amont_slots::Lease, Reason> {
    let started = Instant::now();
    let mut waiter = match amont_slots::Waiter::new(sizing, 1, holder) {
        Ok(w) => w,
        Err(e) if e.kind() == std::io::ErrorKind::Other => {
            return Err(Reason::DirUnsafe(e.to_string()))
        }
        Err(e) => return Err(Reason::Io(e.to_string())),
    };
    let mut said = false;
    loop {
        match waiter.poll() {
            Ok(amont_slots::Poll::Granted(lease)) => return Ok(lease),
            Ok(amont_slots::Poll::Waiting(queue)) => {
                if !said && (!queue.holders.is_empty() || queue.unknown > 0) {
                    said = true;
                    crate::hooks::common::say(&format!("  {}", behind(&queue)));
                }
            }
            Err(e) => return Err(Reason::Io(e.to_string())),
        }
        if started.elapsed() >= budget {
            return Err(Reason::TimedOut(started.elapsed()));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// `queued behind relais: cargo test (amont-wt-x, 4m) · 2 waiting`.
pub fn behind(queue: &amont_slots::Queue) -> String {
    let now = amont_slots::now_ms();
    let mut who: Vec<String> = queue.holders.iter().map(|h| h.describe(now)).collect();
    if queue.unknown > 0 {
        who.push(format!("{} unknown (old client)", queue.unknown));
    }
    format!(
        "queued behind {} · {} waiting",
        who.join(", "),
        queue.waiting.max(1)
    )
}

/// One line for the reasons a person can act on; nothing for the normal
/// case (the platform).
fn note(reason: &Reason, cap: u64) {
    match reason {
        Reason::DirUnsafe(why) => crate::hooks::common::warn(&format!(
            "host slots are off for this check: {why}. Remove it, or set {} to a directory \
             only you own",
            crate::ui::highlight(DIR_ENV)
        )),
        Reason::TimedOut(waited) => crate::hooks::common::warn(&format!(
            "no host slot freed in {} ({} {cap}); running it anyway",
            crate::hooks::common::human_secs(waited.as_secs()),
            crate::ui::highlight("amont.hostSlots")
        )),
        Reason::Io(why) => {
            crate::hooks::common::warn(&format!("host slots are off for this check: {why}"))
        }
        Reason::Unsupported => {}
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn settings_from(conf: &str) -> crate::config::Settings {
        let lines = crate::manifest::parse_lines(conf);
        let (policy, _) = crate::policy::Policy::from_lines(&lines);
        crate::config::Settings::new(policy)
    }

    /// `AMONT_ON_BUSY`, then `amont.conf`, then `wait`; a bad environment
    /// value is ignored for the file's.
    #[test]
    fn the_busy_policy_is_env_then_file_then_wait() {
        let none = settings_from("");
        let defer = settings_from("set onBusy defer\nset slotWait 5\n");
        assert_eq!(resolve_busy(None, &none), BusyPolicy::Wait);
        assert_eq!(
            resolve_busy(None, &defer),
            BusyPolicy::Defer {
                slot_wait: Duration::from_secs(5)
            }
        );
        assert_eq!(resolve_busy(Some("wait"), &defer), BusyPolicy::Wait);
        assert_eq!(
            resolve_busy(Some("DEFER"), &none),
            BusyPolicy::Defer {
                slot_wait: Duration::from_secs(60)
            }
        );
        assert_eq!(
            resolve_busy(Some("later"), &defer),
            BusyPolicy::Defer {
                slot_wait: Duration::from_secs(5)
            }
        );
        assert_eq!(resolve_busy(Some(""), &none), BusyPolicy::Wait);
    }

    #[test]
    fn heavy_is_the_list() {
        assert_eq!(weight_of("pre-commit-clippy"), Weight::Heavy);
        assert_eq!(weight_of("pre-commit-cargo-fmt"), Weight::Light);
    }

    /// Every heavy name is a built-in: a renamed check must not silently
    /// stop queueing.
    #[test]
    fn every_heavy_name_is_a_builtin() {
        for name in HEAVY {
            assert!(
                crate::registry::CHECKS.iter().any(|c| c.name == *name),
                "{name} is not a built-in check"
            );
        }
    }

    /// `.cargo/config.toml` reaches the test binaries: without it the
    /// integration fixtures would queue on the real host slots.
    #[test]
    fn cargo_pins_the_host_knobs_for_tests() {
        assert_eq!(std::env::var(HELD_ENV).as_deref(), Ok("held"));
        assert_eq!(std::env::var("AMONT_IDLE_LOAD_SCALE").as_deref(), Ok("1"));
    }

    /// A light check never asks. A heavy one run by an amont that holds a
    /// slot (the test process has `AMONT_HOST_SLOT=held` from
    /// `.cargo/config.toml`) is nested: it settles without a slot.
    #[test]
    fn a_light_check_never_takes_a_slot() {
        let settings = crate::config::Settings::default();
        let _g = enter_check("pre-commit-cargo-fmt", weight_of("pre-commit-cargo-fmt"));
        assert!(!before_spawn(&settings).unwrap());
    }

    #[test]
    fn a_nested_heavy_check_settles_without_a_slot() {
        let settings = crate::config::Settings::default();
        let _g = enter_check("pre-commit-clippy", weight_of("pre-commit-clippy"));
        assert_eq!(current_check().as_deref(), Some("clippy"));
        assert!(!before_spawn(&settings).unwrap());
        assert!(CURRENT.with(|c| c.borrow().settled));
        // Nested: the parent's bounds are inherited, so none is set here.
        let mut cmd = std::process::Command::new("true");
        bound(&mut cmd);
        assert_eq!(cmd.get_envs().count(), 0);
    }

    #[test]
    fn a_light_check_sets_no_width() {
        let settings = crate::config::Settings::default();
        let _g = enter_check("pre-commit-cargo-fmt", weight_of("pre-commit-cargo-fmt"));
        before_spawn(&settings).unwrap();
        let mut cmd = std::process::Command::new("true");
        bound(&mut cmd);
        assert_eq!(cmd.get_envs().count(), 0);
    }
}
