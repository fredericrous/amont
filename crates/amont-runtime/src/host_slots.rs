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
}

/// What [`acquire`] got.
pub enum Acquired {
    Slot(Slot),
    Unqueued(Reason),
}

/// A held slot: released when dropped, and by the kernel if amont dies.
pub struct Slot {
    _file: std::fs::File,
}

/// `amont.hostSlots`: a host key, default a quarter of the cores and at
/// least one, `0` disables.
pub fn configured(settings: &crate::config::Settings) -> u64 {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get() as i64);
    crate::config::host_integer_or(settings, "amont.hostSlots", (cores / 4).max(1), 0..=64) as u64
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
            if let Some(file) = platform::try_lock(&dir.join(format!("slot-{i}"))) {
                return Acquired::Slot(Slot { _file: file });
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
    heavy: bool,
    /// The slot question has been answered for this check (held, or
    /// unqueued for a reason).
    settled: bool,
    held: Option<Slot>,
    queued: Duration,
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
}

impl Drop for CheckGuard {
    fn drop(&mut self) {
        CURRENT.with(|c| *c.borrow_mut() = Current::default());
    }
}

/// The dispatcher is about to run `name` on this thread.
pub fn enter_check(name: &str) -> CheckGuard {
    CURRENT.with(|c| {
        *c.borrow_mut() = Current {
            heavy: weight_of(name) == Weight::Heavy,
            ..Current::default()
        }
    });
    CheckGuard
}

/// Whether this process was started by an amont that holds a slot.
fn nested() -> bool {
    std::env::var_os(HELD_ENV).is_some_and(|v| !v.is_empty())
}

/// Called before a tool is spawned. For the first tool of a heavy check,
/// waits for a slot, showing the wait in the check's row. Returns whether
/// this thread holds a slot, so the caller exports [`HELD_ENV`].
pub fn before_spawn(settings: &crate::config::Settings) -> bool {
    let pending = CURRENT.with(|c| {
        let c = c.borrow();
        c.heavy && !c.settled
    });
    if pending {
        let n = configured(settings);
        let outcome = if nested() {
            Acquired::Unqueued(Reason::Nested)
        } else if n == 0 {
            Acquired::Unqueued(Reason::Disabled)
        } else {
            let budget = match crate::hooks::common::check_timeout(settings) {
                0 => 3600,
                s => s,
            };
            let sink = crate::live::current_sink();
            if let Some((stage, idx)) = &sink {
                stage.queue(*idx, Some((Instant::now(), n)));
            }
            let started = Instant::now();
            let got = acquire(n, Duration::from_secs(budget));
            if let Some((stage, idx)) = &sink {
                stage.queue(*idx, None);
            }
            CURRENT.with(|c| c.borrow_mut().queued += started.elapsed());
            got
        };
        let held = match outcome {
            Acquired::Slot(slot) => Some(slot),
            Acquired::Unqueued(reason) => {
                note(&reason, n);
                None
            }
        };
        CURRENT.with(|c| {
            let mut c = c.borrow_mut();
            c.held = held;
            c.settled = true;
        });
    }
    CURRENT.with(|c| c.borrow().held.is_some())
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

    pub fn try_lock(path: &Path) -> Option<std::fs::File> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .ok()?;
        // SAFETY: a valid fd we own; flock touches no memory.
        (unsafe { libc_flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0).then_some(file)
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
    pub fn try_lock(_path: &Path) -> Option<std::fs::File> {
        None
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
        let a = platform::try_lock(&d.join("slot-0")).expect("first");
        assert!(platform::try_lock(&d.join("slot-0")).is_none());
        drop(a);
        // Released on the last close. A thread of another test that forks
        // in the instant before its exec holds a copy of every descriptor
        // until then (O_CLOEXEC closes it at exec, not at fork), so allow
        // that instant rather than assert a release that is a race away.
        let freed = (0..100).any(|_| {
            platform::try_lock(&d.join("slot-0")).is_some() || {
                std::thread::sleep(Duration::from_millis(20));
                false
            }
        });
        assert!(freed, "a dropped slot was not released within 2 s");
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

    /// A light check never asks; a heavy one settles once per check.
    #[test]
    fn a_light_check_never_takes_a_slot() {
        let settings = crate::config::Settings::default();
        let _g = enter_check("pre-commit-cargo-fmt");
        assert!(!before_spawn(&settings));
    }
}
