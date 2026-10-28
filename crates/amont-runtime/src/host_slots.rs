//! Host-wide slots for heavy checks (ADR-0009, `hooks.host-concurrency`).
//!
//! Several worktrees, sessions and agents on one machine each run their own
//! clippy, test suite and type checker, and each of those already uses every
//! core. Run together they thrash the same cores and the same cargo lock,
//! and the kill clocks counted that thrash as the check's own time. A check
//! that compiles or executes the product is therefore Heavy, and takes one
//! of `amont.hostSlots` slots — `flock` on a file under one fixed per-user
//! directory — before its first tool runs. While it waits, its clocks have
//! not started, because its tool has not.
//!
//! The slot is taken LAZILY, at the first tool spawn of a heavy check, not
//! when the check starts: a clippy with no Rust staged returns at once and
//! must not wait behind somebody else's suite to do nothing.
//!
//! Fairness is polling, not first come first served.
//! holds-until: more than a handful of gates contend on one host, when a
//! queue file with tickets is needed.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// The environment variable a held slot exports to every tool it runs, and
/// that a nested amont reads to skip queueing: amont's own
/// `pre-push-cargo-test` runs fixtures that run amont, and those must not
/// wait for the slot their own parent holds.
pub const HELD_ENV: &str = "AMONT_HOST_SLOT";

/// A diagnostic, and the seam the tests use: the slot directory, instead of
/// the fixed per-user one.
pub const DIR_ENV: &str = "AMONT_SLOT_DIR";

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
    /// `amont.hostSlots 0`.
    Disabled,
    /// A slot is already held by the amont that runs this one.
    Nested,
    /// The slot directory is not one this user alone owns.
    DirUnsafe(String),
    /// No `flock` here: Windows, the stated divergence.
    Unsupported,
    /// No slot freed within `amont.timeout`.
    TimedOut(Duration),
    /// A slot file could not be opened or locked for a reason other than
    /// "another holder has it".
    Io(String),
}

/// What [`acquire`] got.
pub enum Acquired {
    Slot(Slot),
    Unqueued(Reason),
}

/// How many slots THIS process holds, across its threads. `CURRENT` is
/// thread-local, so a pre-commit check cannot see that a sibling thread
/// holds a slot; this can, and a check never defers while it is non-zero
/// (ADR-0013: a commit never defers behind its own checks).
static HELD: AtomicUsize = AtomicUsize::new(0);

/// Set once a check of this process deferred: the host was measured busy,
/// and a later Heavy check of the same run defers without waiting again, so
/// a push waits at most one `slotWait`.
static BUSY_SEEN: AtomicBool = AtomicBool::new(false);

/// A held slot: released when dropped, and by the kernel if amont dies.
pub struct Slot {
    file: Option<std::fs::File>,
}

impl Slot {
    fn new(file: std::fs::File) -> Slot {
        HELD.fetch_add(1, Ordering::SeqCst);
        Slot { file: Some(file) }
    }
}

impl Drop for Slot {
    /// The lock first, then the count: the count never reads zero while
    /// this process still holds a flock, so a sibling never defers behind it.
    fn drop(&mut self) {
        drop(self.file.take());
        HELD.fetch_sub(1, Ordering::SeqCst);
    }
}

/// `amont.hostSlots`: a host key, default a quarter of the cores and at
/// least one, `0` disables.
pub fn configured(settings: &crate::config::Settings) -> u64 {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get() as i64);
    crate::config::host_integer_or(settings, "amont.hostSlots", (cores / 4).max(1), 0..=64) as u64
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

/// The slot directory: `AMONT_SLOT_DIR`, else `$XDG_RUNTIME_DIR/amont-slots`
/// on Linux, else `/tmp/amont-slots-<uid>`. A FIXED path, never
/// `std::env::temp_dir()`, which follows a `$TMPDIR` that macOS and agent
/// sandboxes set per session — every session would get a queue of its own.
pub fn dir() -> PathBuf {
    if let Some(d) = std::env::var_os(DIR_ENV).filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    #[cfg(target_os = "linux")]
    if let Some(d) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(d).join("amont-slots");
    }
    PathBuf::from(format!("/tmp/amont-slots-{}", platform::uid()))
}

/// Make `dir` if needed, then refuse it unless it is a real directory this
/// user owns with mode 0700: on a shared `/tmp`, another user could create
/// it first and hold every slot.
pub fn safe_dir(dir: &std::path::Path) -> Result<(), String> {
    platform::make_private(dir)?;
    platform::check_private(dir)
}

/// Take one of `n` slots, waiting at most `budget`.
pub fn acquire(n: u64, budget: Duration) -> Acquired {
    if n == 0 {
        return Acquired::Unqueued(Reason::Disabled);
    }
    if !platform::SUPPORTED {
        return Acquired::Unqueued(Reason::Unsupported);
    }
    let dir = dir();
    if let Err(why) = safe_dir(&dir) {
        return Acquired::Unqueued(Reason::DirUnsafe(why));
    }
    let started = Instant::now();
    loop {
        for i in 0..n {
            let path = dir.join(format!("slot-{i}"));
            match platform::try_lock(&path) {
                Ok(Some(file)) => return Acquired::Slot(Slot::new(file)),
                Ok(None) => {}
                // Not "busy": an error waiting would only repeat, and a
                // full queue must not be claimed when nobody was seen.
                Err(e) => {
                    return Acquired::Unqueued(Reason::Io(format!("{}: {e}", path.display())))
                }
            }
        }
        if started.elapsed() >= budget {
            return Acquired::Unqueued(Reason::TimedOut(started.elapsed()));
        }
        std::thread::sleep(Duration::from_millis(200));
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
    held: Option<Slot>,
    queued: Duration,
    /// Set when the busy policy deferred this check (ADR-0013): how long it
    /// waited. Read by the dispatcher through [`CheckGuard::deferred`], and it
    /// overrides whatever the check returned, so a check whose own error
    /// handling turns the refused spawn into `Unavailable` or `Failed` still
    /// ends Deferred.
    deferred: Option<Duration>,
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
    let (pending, deferred) = CURRENT.with(|c| {
        let c = c.borrow();
        (c.heavy && !c.settled, c.deferred.is_some())
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
        let n = if nested() { 0 } else { configured(settings) };
        if n == 0 {
            CURRENT.with(|c| c.borrow_mut().settled = true);
            return Ok(false);
        }
        let policy = busy_policy(settings);
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
        let sink = crate::live::current_sink();
        if let Some((stage, idx)) = &sink {
            stage.queue(*idx, Some((Instant::now(), n)));
        }
        let started = Instant::now();
        let mut got = acquire(n, budget);
        if let (BusyPolicy::Defer { .. }, Acquired::Unqueued(Reason::TimedOut(_))) = (policy, &got)
        {
            // A sibling check of this process holds a slot: the host is busy
            // with this commit's own work, which is not a reason to defer.
            // Wait as `wait` does, up to the ceiling.
            got = if HELD.load(Ordering::SeqCst) > 0 {
                acquire(n, ceiling.saturating_sub(started.elapsed()))
            } else {
                // One more pass: a sibling may have released between the
                // last poll and the count's read.
                acquire(n, Duration::ZERO)
            };
        }
        if let Some((stage, idx)) = &sink {
            stage.queue(*idx, None);
        }
        let waited = started.elapsed();
        CURRENT.with(|c| c.borrow_mut().queued += waited);
        // Piped output never shows the region, and a heartbeat only comes
        // after a minute: say once where the time went.
        if matches!(got, Acquired::Slot(_)) && waited >= Duration::from_secs(1) {
            crate::hooks::common::say(&format!(
                "  (waited {} for a host slot, {} {n})",
                crate::hooks::common::human_secs(waited.as_secs()),
                crate::ui::highlight("amont.hostSlots")
            ));
        }
        let defer = matches!(policy, BusyPolicy::Defer { .. })
            && matches!(got, Acquired::Unqueued(Reason::TimedOut(_)));
        let held = match got {
            Acquired::Slot(slot) => Some(slot),
            // The deferral line is the dispatcher's, not this note.
            Acquired::Unqueued(_) if defer => None,
            Acquired::Unqueued(reason) => {
                note(&reason, n);
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

/// One line for the reasons a person can act on; nothing for the ones
/// that are the normal case (off, nested, the platform).
fn note(reason: &Reason, n: u64) {
    match reason {
        Reason::DirUnsafe(why) => crate::hooks::common::warn(&format!(
            "host slots are off for this check: {why}. Remove it, or set {} to a directory \
             only you own",
            crate::ui::highlight(DIR_ENV)
        )),
        Reason::TimedOut(waited) => crate::hooks::common::warn(&format!(
            "no host slot freed in {} ({} heavy checks were running on this machine, \
             {} {n}); running it anyway",
            crate::hooks::common::human_secs(waited.as_secs()),
            n,
            crate::ui::highlight("amont.hostSlots")
        )),
        Reason::Io(why) => {
            crate::hooks::common::warn(&format!("host slots are off for this check: {why}"))
        }
        Reason::Disabled | Reason::Nested | Reason::Unsupported => {}
    }
}

#[cfg(unix)]
mod platform {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    use std::os::unix::io::AsRawFd;
    use std::path::Path;

    pub const SUPPORTED: bool = true;

    extern "C" {
        #[link_name = "flock"]
        fn libc_flock(fd: i32, op: i32) -> i32;
        #[link_name = "getuid"]
        fn libc_getuid() -> u32;
    }
    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;

    pub fn uid() -> u32 {
        // SAFETY: getuid takes nothing and cannot fail.
        unsafe { libc_getuid() }
    }

    pub fn make_private(dir: &Path) -> Result<(), String> {
        if dir.symlink_metadata().is_ok() {
            return Ok(());
        }
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))
    }

    pub fn check_private(dir: &Path) -> Result<(), String> {
        let meta = dir
            .symlink_metadata()
            .map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        if meta.file_type().is_symlink() {
            return Err(format!("{} is a symbolic link", dir.display()));
        }
        if !meta.is_dir() {
            return Err(format!("{} is not a directory", dir.display()));
        }
        if meta.uid() != uid() {
            return Err(format!("{} belongs to another user", dir.display()));
        }
        let mode = meta.permissions().mode() & 0o777;
        if mode != 0o700 {
            return Err(format!("{} has mode {mode:o}, not 700", dir.display()));
        }
        Ok(())
    }

    /// `Ok(None)` only when another holder has the lock (EWOULDBLOCK);
    /// every other failure is an error the caller reports.
    pub fn try_lock(path: &Path) -> std::io::Result<Option<std::fs::File>> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;
        // SAFETY: a valid fd we own; flock touches no memory.
        if unsafe { libc_flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0 {
            return Ok(Some(file));
        }
        let e = std::io::Error::last_os_error();
        if e.kind() == std::io::ErrorKind::WouldBlock {
            Ok(None)
        } else {
            Err(e)
        }
    }
}

#[cfg(not(unix))]
mod platform {
    use std::path::Path;

    pub const SUPPORTED: bool = false;

    pub fn uid() -> u32 {
        0
    }
    pub fn make_private(_dir: &Path) -> Result<(), String> {
        Ok(())
    }
    pub fn check_private(_dir: &Path) -> Result<(), String> {
        Ok(())
    }
    pub fn try_lock(_path: &Path) -> std::io::Result<Option<std::fs::File>> {
        Ok(None)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("amont-slots-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// A fresh directory is made 0700 and accepted; a 0755 one, a symlink
    /// and a file are refused.
    #[test]
    fn only_a_private_directory_is_accepted() {
        use std::os::unix::fs::PermissionsExt;
        let d = scratch("private");
        assert_eq!(safe_dir(&d), Ok(()));
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(safe_dir(&d).unwrap_err().contains("mode 755"));
        let link = scratch("link");
        std::os::unix::fs::symlink(&d, &link).unwrap();
        assert!(safe_dir(&link).unwrap_err().contains("symbolic link"));
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Two slots: two holders get one each, a third finds none within its
    /// budget, and a dropped slot is free again.
    #[test]
    fn slots_are_exclusive_and_released_on_drop() {
        let d = scratch("exclusive");
        safe_dir(&d).unwrap();
        let a = platform::try_lock(&d.join("slot-0"))
            .expect("no error")
            .expect("first");
        assert!(platform::try_lock(&d.join("slot-0"))
            .expect("busy is not an error")
            .is_none());
        drop(a);
        // Released on the last close. A thread of another test that forks
        // in the instant before its exec holds a copy of every descriptor
        // until then (O_CLOEXEC closes it at exec, not at fork), so allow
        // that instant rather than assert a release that is a race away.
        let freed = (0..100).any(|_| {
            matches!(platform::try_lock(&d.join("slot-0")), Ok(Some(_))) || {
                std::thread::sleep(Duration::from_millis(20));
                false
            }
        });
        assert!(freed, "a dropped slot was not released within 2 s");
        let _ = std::fs::remove_dir_all(&d);
    }

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

    /// The count rises with a slot and falls only after its lock is gone.
    #[test]
    fn a_dropped_slot_releases_its_lock_before_its_count() {
        let d = scratch("held");
        safe_dir(&d).unwrap();
        let before = HELD.load(Ordering::SeqCst);
        let slot = Slot::new(platform::try_lock(&d.join("slot-0")).unwrap().unwrap());
        assert!(HELD.load(Ordering::SeqCst) > before);
        drop(slot);
        let _ = std::fs::remove_dir_all(&d);
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
    }

    /// An unopenable slot file is an error, not a full queue.
    #[test]
    fn an_unopenable_slot_is_an_error_not_busy() {
        let d = scratch("missing").join("absent-subdir").join("slot-0");
        assert!(platform::try_lock(&d).is_err());
    }
}
