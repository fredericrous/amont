//! Shared plumbing for the linter-orchestration hooks.
//!
//! Nine of them do the same four things: collect staged files of some kind,
//! bail out if there are none, resolve a tool, run it. In shell that was ~65
//! lines apiece, mostly duplicated; here it is a handful of helpers and each
//! hook keeps only what is actually specific to it.

use crate::git;
use crate::ui::{error_sign, valid_sign, warning_sign};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// Staged files, deletions excluded, whose name ends with one of `exts`.
/// The file set every check asks about, when it is not the staged one.
///
/// Set at most once, before any check runs, by `amont run --all-files`. A
/// process-level override rather than a parameter because a check's signature
/// is `(&[OsString])` — it never sees a `Ctx` — and threading a file set
/// through twenty of them to serve one mode would be a worse trade than a
/// value that is written once and read many times.
///
/// Same shape as `PushRefs`: read once, lent to every check that asks.
static OVERRIDE: OnceLock<Vec<String>> = OnceLock::new();

/// Set once the file set stops being the index.
///
/// `restage`'s own doc says what makes re-staging safe: the pre-commit stage
/// holds the unstaged changes aside, so the tree contains the staged content
/// and nothing else, and anything a formatter touched is by definition part of
/// this commit. `amont run --all-files` replaces the file set with every
/// tracked path — which is that precondition being FALSE.
///
/// With `amont.fix true`, every fixer's `restage(&files)` would then `git
/// add` everything in the working tree that differs from the index, turning a
/// read-only "does my tree pass" query into `git add .`. That is the hazard §2
/// of docs/index-fidelity-and-run-modes.md names.
///
/// The gate hangs off the OVERRIDE rather than off a flag threaded through
/// twenty check signatures, because the override IS the fact that matters. It
/// therefore covers built-ins and `manifest::External::run` (which consults
/// `fixing_enabled` in two places) in one change, and a future check cannot
/// forget it.
static NOT_THE_INDEX: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Make every subsequent `staged_files` answer from `files` instead of the
/// index. Only the first call counts.
pub fn override_file_set(files: Vec<String>) {
    // Set unconditionally, even if a set already won the `OnceLock`: the
    // statement "the file set is not the index" is true from the first call
    // onwards regardless of which one supplied the paths.
    NOT_THE_INDEX.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = OVERRIDE.set(files);
}

/// Whether the file set every check sees is something other than the index.
pub fn not_the_index() -> bool {
    NOT_THE_INDEX.load(std::sync::atomic::Ordering::SeqCst)
}

/// An empty `exts` returns them all.
///
/// The UNFILTERED list is read from git ONCE per process and lent to every
/// caller — the per-stage snapshot. Eleven of the pre-commit checks ask this
/// question, concurrently, and each used to pay its own `git diff` spawn for
/// an answer that cannot change while the stage runs: the index-fidelity
/// hold pins the tree, and a fixer's `restage()` re-adds only paths already
/// on this list. `PushRefs` ("read once and lent") and `Overrides` ("ONE
/// subprocess for the whole stage") are the same pattern; this was the last
/// hot question still answered per asker.
pub fn staged_files(exts: &[&str]) -> Vec<String> {
    if let Some(all) = OVERRIDE.get() {
        return all
            .iter()
            .filter(|f| exts.is_empty() || exts.iter().any(|e| f.ends_with(e)))
            .cloned()
            .collect();
    }
    static INDEX: OnceLock<Vec<String>> = OnceLock::new();
    INDEX
        .get_or_init(|| {
            match git::stdout_paths(&["diff", "--diff-filter=d", "--cached", "--name-only"]) {
                Some(files) => files,
                // The third member of a bug family (`repo_hooks`, the push
                // gates): git FAILING is not git answering "empty", and a
                // stage that judges an empty set on a git failure reports
                // clean having verified nothing. Say so — once, this cache
                // being the once — and still fail open: pre-commit's job is
                // never to block a commit over its own plumbing.
                None => {
                    warn(
                        "git would not list the staged files — the checks are judging \
                         an EMPTY set, not a verified one",
                    );
                    Vec::new()
                }
            }
        })
        .iter()
        .filter(|f| exts.is_empty() || exts.iter().any(|e| f.ends_with(e)))
        .cloned()
        .collect()
}

/// Every path the index holds — what the repository CARRIES, as opposed to
/// what the current change touches.
///
/// The one honest source for a [`Scope`](crate::check::Scope) opt-in marker.
/// [`staged_files`] answers a different question, and answering the opt-in one
/// with it makes a `+marker` row fire only when the marker itself is in the
/// change — which is never, in ordinary work.
///
/// `ls-files` reads the INDEX, not `HEAD`, so a marker being added by this very
/// commit already counts. A marker sitting untracked on disk does not, which is
/// the same rule the manifest itself lives by: commit it, or it is not real.
///
/// Fails OPEN, unlike `staged_files`, and the asymmetry is deliberate. There,
/// an empty list means the checks judge nothing and say so. Here, an empty list
/// would silently switch every gated check OFF — a check that has quietly never
/// run is the one failure this design is arranged against — so a git failure
/// reports the check as opted in and lets the command itself be the judge. A
/// command that then finds no project fails to spawn, which is `Unavailable`:
/// a warning, never a block.
/// `None` when git would not answer — which is NOT the same as an empty
/// repository, and the caller must not flatten the two. An empty `Vec` opts
/// every gated check OUT; `None` means "unverified", and the gate opts them IN.
pub fn tracked_files() -> Option<Vec<String>> {
    static TRACKED: OnceLock<Option<Vec<String>>> = OnceLock::new();
    TRACKED
        .get_or_init(|| match git::stdout_paths(&["ls-files"]) {
            Some(files) => Some(files),
            None => {
                warn(
                    "git would not list the repository's files — opt-in gated checks \
                     will run rather than be skipped on an unverified answer",
                );
                None
            }
        })
        .clone()
}

/// Repo root, or "." when git cannot say.
///
/// **For CHECK BODIES ONLY.** The fallback is safe there and nowhere else: git
/// invokes a hook with the working tree as the current directory, so a check
/// that reaches this line is already standing in the repository, and "." is the
/// right answer rather than a guess.
///
/// Anything a user types — `amont agents-md`, `install`, `trust`, `restore`
/// — can be typed from any directory on the machine, and there the fallback is
/// not a fallback but a wrong answer that reads as a right one. Use
/// [`repo_root_checked`] at every command entry point.
pub fn repo_root() -> String {
    // Cached: the answer is a property of the process's repository, and
    // every check asked it through its own subprocess.
    static ROOT: OnceLock<String> = OnceLock::new();
    ROOT.get_or_init(|| {
        git::stdout(&["rev-parse", "--show-toplevel"]).unwrap_or_else(|| ".".into())
    })
    .clone()
}

/// Repo root, or an error naming the problem.
///
/// The same question as [`repo_root`] without the "." — because "." is a
/// PLAUSIBLE root, and that is what made it dangerous. `amont agents-md`
/// run outside a repository did not fail; it resolved the root to the current
/// directory and wrote `./AGENTS.md` into whatever directory the user happened
/// to be standing in, then printed `wrote ./AGENTS.md` as if that were the
/// answer. Same shape in `install`'s two prompts, in `trust` (which then
/// looked for a manifest, and would have recorded trust, under `.`) and in
/// `restore`.
///
/// Every one of those is a command somebody types, and a command somebody
/// types is a command they can type from `~`. There is no correct behaviour
/// available to this function when git cannot answer, so it does not invent
/// one.
pub fn repo_root_checked() -> Result<String, String> {
    git::stdout(&["rev-parse", "--show-toplevel"])
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "not inside a git repository".to_string())
}

/// Resolve a tool, preferring the repo's PINNED copy so the hook matches CI.
///
///
/// Order: `<root>/node_modules/.bin/<tool>`, then the MAIN worktree's (a linked
/// worktree has no node_modules of its own — this is why the shell version
/// consulted the git common dir), then PATH.
pub fn resolve_tool(root: &str, tool: &str) -> Option<Vec<String>> {
    // Same extension problem as `which`: an npm-installed binary is `eslint.cmd`
    // on Windows, so the bare name misses the repo's PINNED copy and the hook
    // silently falls through to an ambient one.
    if let Some(p) = in_bin_dir(&format!("{root}/node_modules/.bin"), tool) {
        return Some(vec![p]);
    }
    if let Some(common) = git::stdout(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
    {
        if let Some(main) = Path::new(&common).parent() {
            if let Some(p) = in_bin_dir(&main.join("node_modules/.bin").to_string_lossy(), tool) {
                return Some(vec![p]);
            }
        }
    }
    if let Some(full) = which(tool) {
        return Some(vec![full]);
    }
    // `npx --no-install`: never silently download a random latest version — a
    // hook that quietly pulls a different linter than CI uses is worse than one
    // that skips.
    if which("npx").is_some()
        && Command::new(program("npx"))
            .args(["--no-install", tool, "--version"])
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    {
        return Some(vec![
            program("npx"),
            "--no-install".to_string(),
            tool.to_string(),
        ]);
    }
    None
}

/// First match for `tool` on PATH.
///
/// Windows executables carry an extension — `git` is `git.exe`, an npm-installed
/// `eslint` is `eslint.cmd` — so the bare name finds nothing there. PATHEXT is
/// the OS's own list of what counts as executable; fall back to the usual set
/// when it is unset. Found by the Windows CI job on its first run, where
/// `which("git")` returned None on a machine that plainly has git.
pub fn which(tool: &str) -> Option<String> {
    which_on(&std::env::var_os("PATH")?, tool)
}

/// [`which`] against an EXPLICIT path list — the seam its own test needs.
///
/// The test that pins the Windows extension order used to `set_var("PATH")`
/// around the call, which is process-global: for the length of that call
/// every OTHER test in the binary — 340 of them, running in parallel, many
/// spawning git — had a PATH containing one fake tool and nothing else. A
/// git spawned in that window fails with "not found", which is not a
/// transient `git::retrying` may retry (correctly: it is a hard error), so
/// the caller reads it as git's ANSWER. In `gate_stamp` that answer is
/// "nothing is stamped". Passing the path in deletes the shared state
/// rather than guarding it — a lock only protects the callers who remember
/// to take it, and every future test here would have to remember.
pub fn which_on(path: &std::ffi::OsStr, tool: &str) -> Option<String> {
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(|e| e.to_lowercase())
            .collect()
    } else {
        Vec::new()
    };
    for dir in std::env::split_paths(path) {
        // On Windows the EXTENSION forms come first. A node install ships both
        // `npm` (an extensionless shell script, for MSYS) and `npm.cmd` in the
        // same directory; preferring the bare name hands CreateProcess a shell
        // script it cannot execute — "%1 is not a valid Win32 application" —
        // and the hook reports an installed tool as broken.
        for e in &exts {
            let c = dir.join(format!("{tool}{e}"));
            if c.is_file() {
                return Some(c.to_string_lossy().into_owned());
            }
        }
        let bare = dir.join(tool);
        if bare.is_file() {
            return Some(bare.to_string_lossy().into_owned());
        }
    }
    None
}

/// `<dir>/<tool>`, trying the Windows executable extensions too.
fn in_bin_dir(dir: &str, tool: &str) -> Option<String> {
    let bare = Path::new(dir).join(tool);
    if bare.is_file() {
        return Some(bare.to_string_lossy().into_owned());
    }
    if cfg!(windows) {
        for e in [".cmd", ".exe", ".bat", ".ps1"] {
            let c = Path::new(dir).join(format!("{tool}{e}"));
            if c.is_file() {
                return Some(c.to_string_lossy().into_owned());
            }
        }
    }
    None
}

/// Resolve a tool name to a full path for spawning.
///
/// `Command::new("npm")` cannot execute `npm.cmd`: Rust does no PATHEXT
/// resolution, so on Windows every bare-name spawn fails with "program not
/// found" and the hook reports the tool as broken rather than absent. Found by
/// the Windows job on its first FULL-suite run — the smoke never spawned a
/// tool, so it could not have surfaced this.
///
/// Falls back to the name unchanged, so a caller still gets a sensible error.
pub fn program(name: &str) -> String {
    which(name).unwrap_or_else(|| name.to_string())
}

/// The first of `names` that exists at the repo root — how these hooks decide
/// a repo has opted into a tool.
pub fn first_existing(root: &str, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find(|n| Path::new(root).join(n).exists())
        .map(|n| (*n).to_string())
}

/// Strip git's own environment before handing a Command to another tool.
///
/// git exports GIT_DIR, GIT_INDEX_FILE, GIT_WORK_TREE and friends to every
/// hook. Those OVERRIDE the working directory, so any tool that shells out to
/// git operates on the hook's repository no matter where it was launched.
///
/// That is not hypothetical: `pre-push-cargo-test` runs a project's test suite,
/// and this repo's own suite creates throwaway repos and commits to them. With
/// GIT_DIR inherited, `git commit` in a test wrote into the REAL repository —
/// an actual stray commit, authored by the test fixture, pushed to a branch.
///
/// A test suite should behave exactly as it does when run by hand, which means
/// seeing no git environment at all.
pub fn strip_git_env(cmd: &mut Command) {
    for (k, _) in std::env::vars_os() {
        let key = k.to_string_lossy();
        if key.starts_with("GIT_") {
            cmd.env_remove(&k);
        }
    }
}

/// The wall-clock CEILING for one check's spawned command, in seconds.
///
/// `amont.timeout`, default 3600. This used to be 600 and to be the only
/// clock, which made it answer two different questions with one number: "is
/// this tool stuck?" and "is this suite slow?". A stuck tool is silent, and
/// [`idle_timeout`] catches it in minutes; what is left for the ceiling is
/// the tool that keeps printing and never finishes, which is rare enough to
/// afford an hour. `0` disables. Read once per process: twenty concurrent
/// checks must not each spawn a `git config` to learn the same number.
pub fn check_timeout(settings: &crate::config::Settings) -> u64 {
    *settings.timeout.get_or_init(|| {
        crate::config::integer_or(settings, "amont.timeout", 3600, 0..=86_400) as u64
    })
}

/// The SILENCE budget: how long a spawned command may go without writing a
/// byte before it is judged stuck, in seconds.
///
/// `amont.idleTimeout`, default 120. A hang is silent; a slow test suite
/// talks — `cargo test` prints a line per test. Not every one does (vitest
/// without a terminal prints only its summary), so where CPU can be measured
/// the budget counts silence AND an idle process tree — see [`Activity`] and
/// ADR-0008. Killing on silence catches
/// the captive portal, the deadlocked lock file and the tool waiting on a
/// prompt nobody will answer FASTER than a ten-minute wall clock did, while
/// letting a chatty twenty-five-minute suite finish. Only applies where the
/// output is observed (the captured runners); a command inheriting the
/// terminal directly answers to the ceiling alone. `0` disables.
pub fn idle_timeout(settings: &crate::config::Settings) -> u64 {
    *settings.idle.get_or_init(|| {
        crate::config::integer_or(settings, "amont.idleTimeout", 120, 0..=86_400) as u64
    })
}

/// Whether a silent check that is measurably working on CPU is kept alive
/// past the silence budget — `amont.idleCpuCredit`, default true (ADR-0008,
/// `hooks.liveness`). `false` restores the silence-only rule everywhere.
/// Read once per `Settings`, like the two clocks.
pub fn idle_cpu_credit(settings: &crate::config::Settings) -> bool {
    *settings
        .idle_cpu
        .get_or_init(|| crate::config::boolean_or(settings, "amont.idleCpuCredit", true))
}

/// How long a command may sit in a DECLARED wait — a line that says it is
/// blocked on a lock ([`crate::hooks::wait::marker`]) — before it is killed,
/// in seconds. `amont.lockWait`, default 600; `0` means until the ceiling.
/// The silence clock does not run during such a wait: the tool has said
/// what it is doing, and a lock held by another cargo for a minute on a
/// loaded machine is not a hang (ADR-0009). Read once per `Settings`.
pub fn lock_wait(settings: &crate::config::Settings) -> LockWait {
    match *settings.lock_wait.get_or_init(|| {
        crate::config::integer_or(settings, "amont.lockWait", 600, 0..=86_400) as u64
    }) {
        0 => LockWait::UntilCeiling,
        s => LockWait::Secs(s),
    }
}

/// The budget for a declared wait, as [`lock_wait`] reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockWait {
    Secs(u64),
    /// `amont.lockWait 0`: the ceiling alone bounds a declared wait — and
    /// when the ceiling is off too, the extended silence budget does, so
    /// nothing is ever unbounded.
    UntilCeiling,
}

/// `secs` as people read it: `12s`, `8m12s`, `1h02m`.
pub fn human_secs(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m{:02}s", s / 60, s % 60),
        s => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// How much of a tool's last stderr line a message may quote.
pub const LAST_LINE_CHARS: usize = 200;

/// A work window of at least this many thousandths of one core counts as the
/// tree doing something (ADR-0008): 0.1 core. A hang — a prompt, a lock, a
/// dead network — sits near zero; a test suite runs at whole cores.
pub const BUSY_MILLI_CORES: u32 = 100;

/// What the CPU side of [`Activity`] knows. Stored as a `u8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuState {
    /// Not sampled: `amont.idleCpuCredit false`, no silence budget, or a
    /// platform that cannot measure. The silence-only rule applies.
    Off = 0,
    /// Sampling, but nothing measured yet (the check has not been quiet
    /// long enough, or it has only a baseline).
    Waiting = 1,
    /// Consecutive complete snapshots are being compared.
    Measuring = 2,
    /// The last snapshot was incomplete; the next complete one re-baselines.
    Unavailable = 3,
}

/// What a spawned command has been doing, shared between the reader threads
/// that see its bytes, the CPU sampler, the wait loop that judges it, and the
/// progress displays. Every field is an atomic offset from `base`, so the
/// 80 ms repaint and the 25 ms wait loop read it without a lock.
///
/// Two clocks, kept apart on purpose: `last_out` is when it last WROTE (what
/// the messages report as "last output"), `last_busy` is the start of the
/// last window in which its process tree did measurable CPU work. The kill
/// decision uses the later of the two ([`Activity::still_for`]).
pub struct Activity {
    base: std::time::Instant,
    last_out: std::sync::atomic::AtomicU64,
    last_busy: std::sync::atomic::AtomicU64,
    /// Offset + 1 of the start of the current unbroken run of complete
    /// measurements; 0 when there is none.
    measured_since: std::sync::atomic::AtomicU64,
    /// Offset + 1 of the end of the last complete window; 0 when none.
    last_measured: std::sync::atomic::AtomicU64,
    rate_milli: std::sync::atomic::AtomicU32,
    interval_ms: std::sync::atomic::AtomicU32,
    cpu: std::sync::atomic::AtomicU8,
    /// Offset + 1 of the start of the current declared wait (ADR-0009); 0
    /// when the last stderr line was not a wait marker.
    wait_since: std::sync::atomic::AtomicU64,
    /// [`crate::hooks::wait::WaitKind::code`] of that wait; 0 when none.
    wait_kind: std::sync::atomic::AtomicU8,
    /// Nanoseconds spent in declared waits that have ENDED — what the note
    /// after a slow-but-passing check reports.
    waited_ns: std::sync::atomic::AtomicU64,
    /// The code of the most recent wait, kept after it ends, so the note can
    /// name what was waited for.
    last_wait_kind: std::sync::atomic::AtomicU8,
    /// The silence budget the wait loop is applying right now, in seconds,
    /// after the host's load stretched it; 0 while it is the configured one
    /// (ADR-0009). What the displays count toward.
    budget_secs: std::sync::atomic::AtomicU32,
    /// The load that stretched it: average × 1000, cores, factor × 1000.
    load_avg_milli: std::sync::atomic::AtomicU32,
    load_cores: std::sync::atomic::AtomicU32,
    load_factor_milli: std::sync::atomic::AtomicU32,
    /// The last complete, non-empty stderr line, colour stripped and
    /// clipped: what a kill message may quote, and what decides a retry.
    last_line: std::sync::Mutex<String>,
}

impl Activity {
    pub fn new() -> std::sync::Arc<Activity> {
        std::sync::Arc::new(Activity {
            base: std::time::Instant::now(),
            last_out: Default::default(),
            last_busy: Default::default(),
            measured_since: Default::default(),
            last_measured: Default::default(),
            rate_milli: Default::default(),
            interval_ms: Default::default(),
            cpu: std::sync::atomic::AtomicU8::new(CpuState::Off as u8),
            wait_since: Default::default(),
            wait_kind: Default::default(),
            waited_ns: Default::default(),
            last_wait_kind: Default::default(),
            budget_secs: Default::default(),
            load_avg_milli: Default::default(),
            load_cores: Default::default(),
            load_factor_milli: Default::default(),
            last_line: std::sync::Mutex::new(String::new()),
        })
    }

    /// What the most recent declared wait, ended or not, was for.
    pub fn last_wait_kind(&self) -> Option<crate::hooks::wait::WaitKind> {
        crate::hooks::wait::WaitKind::from_code(
            self.last_wait_kind
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// One complete stderr line arrived. A wait marker starts a declared
    /// wait, or continues the one in progress (cargo prints `package cache`
    /// and then `build directory`; the budget covers the run of them, not
    /// each); any other line ends it. The line is kept for the messages.
    pub fn stderr_line(&self, raw: &str) {
        use std::sync::atomic::Ordering::Relaxed;
        let line = crate::hooks::wait::strip_csi(raw);
        if !line.trim().is_empty() {
            let mut keep = self.last_line.lock().unwrap_or_else(|p| p.into_inner());
            keep.clear();
            keep.extend(line.trim().chars().take(LAST_LINE_CHARS));
        }
        match crate::hooks::wait::marker(&line) {
            Some(kind) => {
                self.wait_kind.store(kind.code(), Relaxed);
                self.last_wait_kind.store(kind.code(), Relaxed);
                // `compare_exchange` from 0: a second marker keeps the
                // original start.
                let _ = self
                    .wait_since
                    .compare_exchange(0, self.now() + 1, Relaxed, Relaxed);
            }
            None => self.end_wait(),
        }
    }

    /// The tool wrote something that is not a wait marker: whatever wait
    /// was in progress is over, and its length is banked for the note.
    pub fn end_wait(&self) {
        use std::sync::atomic::Ordering::Relaxed;
        let since = self.wait_since.swap(0, Relaxed);
        if since != 0 {
            let spent = self.now().saturating_sub(since - 1);
            self.waited_ns.fetch_add(spent, Relaxed);
        }
        self.wait_kind.store(0, Relaxed);
    }

    /// The declared wait in progress, and how long it has lasted.
    pub fn waiting(&self) -> Option<(crate::hooks::wait::WaitKind, std::time::Duration)> {
        use std::sync::atomic::Ordering::Relaxed;
        let since = self.wait_since.load(Relaxed);
        if since == 0 {
            return None;
        }
        let kind = crate::hooks::wait::WaitKind::from_code(self.wait_kind.load(Relaxed))?;
        Some((kind, self.since(since - 1)))
    }

    /// Time spent in declared waits that have ended, plus the one in
    /// progress: what a passing check waited for in total.
    pub fn waited_total(&self) -> std::time::Duration {
        use std::sync::atomic::Ordering::Relaxed;
        let banked = std::time::Duration::from_nanos(self.waited_ns.load(Relaxed));
        banked + self.waiting().map_or(std::time::Duration::ZERO, |(_, d)| d)
    }

    /// The last non-empty stderr line, if any.
    pub fn last_line(&self) -> Option<String> {
        let keep = self.last_line.lock().unwrap_or_else(|p| p.into_inner());
        (!keep.is_empty()).then(|| keep.clone())
    }

    /// The wait loop read the host's load and is applying `budget_secs`.
    pub fn set_load(&self, budget_secs: u64, load: crate::load::Load, factor_milli: u32) {
        use std::sync::atomic::Ordering::Relaxed;
        self.budget_secs
            .store(u32::try_from(budget_secs).unwrap_or(u32::MAX), Relaxed);
        self.load_avg_milli.store(load.avg1_milli, Relaxed);
        self.load_cores.store(load.cores, Relaxed);
        self.load_factor_milli.store(factor_milli, Relaxed);
    }

    /// The load-stretched budget in force, with the load behind it, when
    /// the host's load stretched it at all.
    pub fn load_scale(&self) -> Option<(u64, crate::load::Load, u32)> {
        use std::sync::atomic::Ordering::Relaxed;
        let factor = self.load_factor_milli.load(Relaxed);
        if factor <= 1000 {
            return None;
        }
        Some((
            u64::from(self.budget_secs.load(Relaxed)),
            crate::load::Load {
                avg1_milli: self.load_avg_milli.load(Relaxed),
                cores: self.load_cores.load(Relaxed),
            },
            factor,
        ))
    }
    fn offset(&self, at: std::time::Instant) -> u64 {
        u64::try_from(at.saturating_duration_since(self.base).as_nanos()).unwrap_or(u64::MAX)
    }
    fn now(&self) -> u64 {
        self.offset(std::time::Instant::now())
    }
    fn since(&self, offset: u64) -> std::time::Duration {
        std::time::Duration::from_nanos(self.now().saturating_sub(offset))
    }
    /// It wrote something.
    pub fn touch(&self) {
        self.last_out
            .fetch_max(self.now(), std::sync::atomic::Ordering::Relaxed);
    }
    /// How long since it last wrote a byte — the true output silence, what
    /// the displays report as "last output".
    pub fn quiet_for(&self) -> std::time::Duration {
        self.since(self.last_out.load(std::sync::atomic::Ordering::Relaxed))
    }
    /// The silence the clock counts: zero while the command is in a
    /// declared wait (it has said what it is doing), the output silence
    /// otherwise. What [`Activity::still_for`] and the sampler's start gate
    /// read; the displays keep [`Activity::quiet_for`].
    pub fn silence_for(&self) -> std::time::Duration {
        if self.waiting().is_some() {
            std::time::Duration::ZERO
        } else {
            self.quiet_for()
        }
    }
    /// How long it has been BOTH silent and idle on CPU — the number the
    /// silence budget is judged against. Equals [`Activity::silence_for`]
    /// whenever CPU is not sampled.
    pub fn still_for(&self) -> std::time::Duration {
        let busy = self.last_busy.load(std::sync::atomic::Ordering::Relaxed);
        self.silence_for().min(self.since(busy))
    }
    pub fn cpu_state(&self) -> CpuState {
        match self.cpu.load(std::sync::atomic::Ordering::Relaxed) {
            1 => CpuState::Waiting,
            2 => CpuState::Measuring,
            3 => CpuState::Unavailable,
            _ => CpuState::Off,
        }
    }
    fn set_cpu_state(&self, s: CpuState) {
        self.cpu
            .store(s as u8, std::sync::atomic::Ordering::Relaxed);
    }
    /// The last measured rate in thousandths of a core, while it is fresh:
    /// `None` once two sampling intervals have passed without a complete
    /// window, so a display never keeps showing "busy" on stale data.
    pub fn fresh_rate(&self) -> Option<u32> {
        if self.cpu_state() != CpuState::Measuring {
            return None;
        }
        let end = self
            .last_measured
            .load(std::sync::atomic::Ordering::Relaxed);
        if end == 0 {
            return None;
        }
        let every = u64::from(self.interval_ms.load(std::sync::atomic::Ordering::Relaxed));
        let stale = std::time::Duration::from_millis(2 * every.max(1));
        (self.since(end - 1) <= stale)
            .then(|| self.rate_milli.load(std::sync::atomic::Ordering::Relaxed))
    }
    /// What the CPU side can honestly say at a kill. "Measured idle" names
    /// the unbroken span of complete measurements it rests on — sampling
    /// starts only after a stretch of silence, so that span is always shorter
    /// than the silence itself, and nothing is claimed about the rest.
    pub fn verdict(&self) -> CpuVerdict {
        match self.cpu_state() {
            CpuState::Off => return CpuVerdict::NotSampled,
            CpuState::Waiting | CpuState::Unavailable => return CpuVerdict::Unmeasured,
            CpuState::Measuring => {}
        }
        if let Some(rate) = self.fresh_rate().filter(|r| *r >= BUSY_MILLI_CORES) {
            return CpuVerdict::BusyAtKill(rate);
        }
        let since = self
            .measured_since
            .load(std::sync::atomic::Ordering::Relaxed);
        match (since, self.fresh_rate()) {
            (s, Some(_)) if s != 0 => CpuVerdict::MeasuredIdle(self.since(s - 1).as_secs()),
            _ => CpuVerdict::Unmeasured,
        }
    }
    /// Feed one sampler observation in. `interval` is the sampling period,
    /// kept so displays can tell a fresh rate from a stale one.
    pub fn record(
        &self,
        obs: crate::proctree::Observation,
        at: std::time::Instant,
        interval: std::time::Duration,
    ) {
        use std::sync::atomic::Ordering::Relaxed;
        self.interval_ms.store(
            u32::try_from(interval.as_millis()).unwrap_or(u32::MAX),
            Relaxed,
        );
        match obs {
            crate::proctree::Observation::Baseline => {
                self.measured_since.store(self.offset(at) + 1, Relaxed);
                self.set_cpu_state(CpuState::Waiting);
            }
            crate::proctree::Observation::Window(w) => {
                let milli = w.milli_cores();
                self.rate_milli.store(milli, Relaxed);
                self.last_measured.store(self.offset(w.end) + 1, Relaxed);
                if milli >= BUSY_MILLI_CORES {
                    // The window's START: a burst buys one window, not a
                    // whole new budget.
                    self.last_busy.fetch_max(self.offset(w.start), Relaxed);
                }
                if w.gapped {
                    // Busy is still busy, but a gap is not a measurement:
                    // the measured-idle span starts over at the gap's end,
                    // so the claim never covers what was not seen.
                    self.measured_since.store(self.offset(w.end) + 1, Relaxed);
                }
                self.set_cpu_state(CpuState::Measuring);
            }
            // A skipped sample changes nothing: the rate ages and goes
            // stale on its own if the next complete one is late.
            crate::proctree::Observation::Skipped => {}
            crate::proctree::Observation::Unmeasured => {
                self.measured_since.store(0, Relaxed);
                self.set_cpu_state(CpuState::Unavailable);
            }
        }
    }
    /// Sampling is on for this command.
    pub fn enable_cpu(&self) {
        self.set_cpu_state(CpuState::Waiting);
    }
    /// Which silence budget applies right now, from what the sampler can
    /// claim: see [`CpuGate`].
    pub fn gate(&self) -> CpuGate {
        match self.verdict() {
            CpuVerdict::NotSampled => CpuGate::NotSampled,
            CpuVerdict::MeasuredIdle(_) | CpuVerdict::BusyAtKill(_) => CpuGate::Measured,
            CpuVerdict::Unmeasured => CpuGate::Unmeasured,
        }
    }
}

/// What the kill decision may rest on from the CPU side (ADR-0009). Not
/// sampled at all: the silence-only rule, as ever. Measured: the silence
/// budget, because "silent and idle" was actually observed. Unmeasured —
/// sampling is on but the last sample was late, partial, or has not come
/// yet: the EXTENDED budget, `amont.idleTimeout × amont.idleLoadScale`,
/// which is bounded even when the ceiling is off, and never the bare
/// silence budget, because that would kill on a measurement nobody made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuGate {
    NotSampled,
    Measured,
    Unmeasured,
}

/// What the CPU sampler could say when a command was killed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuVerdict {
    /// Not sampled here (knob off, no budget, platform): silence alone.
    NotSampled,
    /// An unbroken run of complete measurements, this many seconds long and
    /// ending at the kill, all under [`BUSY_MILLI_CORES`].
    MeasuredIdle(u64),
    /// Sampling was on but could not measure the whole budget.
    Unmeasured,
    /// Its tree was measurably busy at the kill, at this many thousandths
    /// of a core.
    BusyAtKill(u32),
}

/// `milli` thousandths of a core as people read it: `~3.9 cores`.
pub fn cores(milli: u32) -> String {
    format!("~{}.{} cores", milli / 1000, (milli % 1000) / 100)
}

/// The deadline for a network PROBE — an `ls-remote` asked before the real
/// work, not the work itself. Capped at 30s below [`check_timeout`]: a
/// probe answers in a second or two when the network is there at all, and
/// a healthy `amont.timeout` of ten minutes is sized for a test suite, not
/// for deciding whether the remote is reachable. Shrinking `amont.timeout`
/// below the cap shrinks this too, and `0` keeps meaning no deadline —
/// somebody who disabled the clock disabled all of it.
pub fn network_probe_budget(settings: &crate::config::Settings) -> u64 {
    match check_timeout(settings) {
        0 => 0,
        t => t.min(30),
    }
}

/// Which clock killed a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// The wall-clock ceiling, `amont.timeout`, in seconds.
    Ceiling(u64),
    /// The silence budget, `amont.idleTimeout`, in seconds.
    Silence(u64),
    /// A declared wait outlived `amont.lockWait` (or, with that at 0 and
    /// the ceiling off, the extended silence budget), in seconds.
    Waited(crate::hooks::wait::WaitKind, u64),
}

/// A command killed by a clock — what happened, said with enough to tell
/// "slow" from "stuck", which is the whole reason there are two clocks.
#[derive(Debug, Clone)]
pub struct Killed {
    pub why: Why,
    /// How long it had been running.
    pub ran_secs: u64,
    /// How long since its last output; `None` when the output was not ours
    /// to observe (inherited stdio).
    pub quiet_secs: Option<u64>,
    /// What its CPU was doing, as far as it was measured.
    pub cpu: CpuVerdict,
    /// The silence budget it ran under, in seconds (0 = off). With
    /// `quiet_secs` it says whether CPU work is what kept a silent command
    /// alive past that budget.
    pub idle_secs: u64,
    /// The declared wait it was in at the kill, and for how long.
    pub waiting: Option<(crate::hooks::wait::WaitKind, u64)>,
    /// Its last non-empty stderr line, colour stripped and clipped to
    /// [`LAST_LINE_CHARS`]: what decides a retry, and what the message may
    /// quote.
    pub last_line: Option<String>,
    /// The host's load at the kill, with the factor it stretched the
    /// silence budget by, when it stretched it at all.
    pub load: Option<(crate::load::Load, u32)>,
}

/// The budgets one wait is judged under — the inputs to [`judge`] that do
/// not move while the command runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clocks {
    /// `amont.timeout`; `None` when off.
    pub ceiling: Option<u64>,
    /// `amont.idleTimeout`; `None` when off or when nobody watches the
    /// output.
    pub silence: Option<u64>,
    /// `amont.lockWait`.
    pub lock_wait: LockWait,
    /// The bound a declared wait falls back to when `lock_wait` says "until
    /// the ceiling" and the ceiling is off: the extended silence budget,
    /// `None` when silence is off too.
    pub extended: Option<u64>,
    /// `amont.idleLoadScale`: how far the host's load may stretch
    /// `silence`; 1 means not at all.
    pub scale: u64,
}

impl Clocks {
    /// The ceiling alone: inherited stdio, where nobody sees the bytes.
    pub fn ceiling_only(wall_secs: u64) -> Clocks {
        Clocks {
            ceiling: (wall_secs > 0).then_some(wall_secs),
            silence: None,
            lock_wait: LockWait::UntilCeiling,
            extended: None,
            scale: 1,
        }
    }

    /// Every clock, from the settings. `idle_secs` is the silence budget
    /// the caller resolved (0 = off).
    pub fn from_settings(settings: &crate::config::Settings, idle_secs: u64) -> Clocks {
        let wall = check_timeout(settings);
        let scale = idle_load_scale(settings);
        Clocks {
            ceiling: (wall > 0).then_some(wall),
            silence: (idle_secs > 0).then_some(idle_secs),
            lock_wait: lock_wait(settings),
            extended: (idle_secs > 0).then(|| idle_secs.saturating_mul(scale)),
            scale,
        }
    }
}

/// How often the wait loop re-reads the host's load.
const LOAD_EVERY: std::time::Duration = std::time::Duration::from_secs(5);

/// `amont.idleLoadScale`: how far the silence budget may stretch, as a
/// factor — under load (ADR-0009, the load-scaled budget) and whenever the
/// CPU could not be measured (the extended budget, `idleTimeout × this`).
/// Default 4, `1` disables the stretch, range 1..=16. A HOST key: read from
/// global or system config only, never from a repository, because it
/// describes the machine and two repositories must not disagree about it.
pub fn idle_load_scale(settings: &crate::config::Settings) -> u64 {
    *settings.idle_load_scale.get_or_init(|| {
        crate::config::host_integer_or(settings, "amont.idleLoadScale", 4, 1..=16) as u64
    })
}

/// What became of a command run under the deadline.
pub enum Ran {
    Status(std::process::ExitStatus),
    /// Killed by a clock; see [`Killed`].
    TimedOut(Killed),
}

/// `cmd.status()`, bounded by [`check_timeout`].
///
/// Without a bound, one hung tool — a linter deadlocked on a lock file, a
/// plugin doing network I/O — blocked the commit FOREVER, and it hung inside
/// the index-fidelity hold: the user's unstaged changes parked in `$GIT_DIR`,
/// their tree showing staged content only, for as long as they were willing
/// to wait. The learned response to that is `--no-verify`, permanently —
/// which disarms every check to escape one.
///
/// The kill reaches the direct child only. A grandchild that detached
/// survives, orphaned — but the COMMIT is no longer hostage to it, which is
/// the property that matters.
pub fn status_within(
    settings: &crate::config::Settings,
    cmd: &mut Command,
) -> std::io::Result<Ran> {
    // The same host-slot wait as the observed runners: a check run without
    // a live stage (one check by name, `amont.progress false`) still queues.
    if crate::host_slots::before_spawn(settings) {
        cmd.env(crate::host_slots::HELD_ENV, "held");
    }
    status_within_secs(cmd, check_timeout(settings))
}

/// [`status_within`] with an explicit ceiling — the testable seam. The
/// output is inherited, so nobody sees the bytes and the silence budget
/// cannot apply; the ceiling is the only clock.
pub fn status_within_secs(cmd: &mut Command, budget_secs: u64) -> std::io::Result<Ran> {
    if budget_secs == 0 {
        return cmd.status().map(Ran::Status);
    }
    let mut child = cmd.spawn()?;
    wait_within(&mut child, Clocks::ceiling_only(budget_secs), None)
}

/// Spawn `cmd` with both streams piped, hand every chunk to `on_output` as
/// it arrives, and wait under BOTH clocks — the reader threads are what
/// make the silence budget observable. The shared runner behind the
/// streamed, captured and discarded variants.
fn run_observed(
    settings: &crate::config::Settings,
    cmd: &mut Command,
    on_output: impl Fn(&[u8]) + Send + Sync + 'static,
) -> std::io::Result<Ran> {
    // A heavy check's first tool waits here for a host slot (ADR-0009);
    // its clocks start at the spawn below, after the wait.
    if crate::host_slots::before_spawn(settings) {
        cmd.env(crate::host_slots::HELD_ENV, "held");
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let activity = Activity::new();
    let on_output = std::sync::Arc::new(on_output);
    let mut child = cmd.spawn()?;
    // The silence is the CHILD's, so its clock starts when the child does:
    // a spawn that itself took a second on a loaded machine is not a
    // second the tool spent saying nothing.
    activity.touch();
    let mut readers = Vec::new();
    // stderr is where cargo and uv print their status, so only its lines
    // are read for wait markers: a test that prints the words on stdout
    // must not pause its own clock. A stdout chunk still ENDS a wait — the
    // tool is visibly alive.
    let pipes = [
        (
            child
                .stdout
                .take()
                .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
            false,
        ),
        (
            child
                .stderr
                .take()
                .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
            true,
        ),
    ];
    for (pipe, is_stderr) in pipes {
        let Some(pipe) = pipe else { continue };
        let activity = std::sync::Arc::clone(&activity);
        let on_output = std::sync::Arc::clone(&on_output);
        readers.push(std::thread::spawn(move || {
            let mut pipe = pipe;
            let mut chunk = [0u8; 4096];
            let mut framer = crate::hooks::wait::LineFramer::default();
            loop {
                match std::io::Read::read(&mut pipe, &mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        activity.touch();
                        if is_stderr {
                            for line in framer.feed(&chunk[..n]) {
                                activity.stderr_line(&line);
                            }
                        } else {
                            activity.end_wait();
                        }
                        on_output(&chunk[..n]);
                    }
                }
            }
        }));
    }
    // The displays read the same clocks the kill decision does.
    let _attached = crate::live::current_sink()
        .map(|(stage, idx)| stage.attach(idx, std::sync::Arc::clone(&activity)));
    let idle = idle_timeout(settings);
    let sampler = (idle > 0 && idle_cpu_credit(settings) && crate::proctree::SUPPORTED)
        .then(|| CpuSampler::start(child.id(), std::sync::Arc::clone(&activity), idle));
    let ran = wait_within(
        &mut child,
        Clocks::from_settings(settings, idle),
        Some(&activity),
    );
    if let Some(s) = sampler {
        s.stop();
    }
    for r in readers {
        let _ = r.join();
    }
    // A check that passed after sitting on a lock says so once: the
    // reader of a slow commit learns where the minutes went, and a run that
    // would once have been killed leaves a trace of why it no longer is.
    if let Ok(Ran::Status(_)) = &ran {
        let waited = activity.waited_total();
        if waited >= std::time::Duration::from_secs(1) {
            let what = activity
                .last_wait_kind()
                .map_or("a lock", crate::hooks::wait::WaitKind::describe);
            say(&format!(
                "  (waited {} for {what})",
                human_secs(waited.as_secs())
            ));
        }
    }
    ran
}

/// The CPU half of the silence budget (ADR-0008): a thread that, while the
/// command is silent, snapshots its process tree and records whether it is
/// doing measurable work. It runs BESIDE the wait loop, never inside it —
/// the loop only reads what this publishes — so nothing a snapshot does can
/// delay the ceiling.
struct CpuSampler {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: std::thread::JoinHandle<()>,
}

impl CpuSampler {
    fn start(pid: u32, activity: std::sync::Arc<Activity>, idle_secs: u64) -> CpuSampler {
        activity.enable_cpu();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("amont-cpu".into())
            .spawn(move || sample_loop(pid, &activity, idle_secs, &flag))
            .expect("spawn the CPU sampler thread");
        CpuSampler { stop, handle }
    }

    /// Ask it to stop and wait a bounded second for it. A snapshot is itself
    /// bounded, so it always stops sooner; if it somehow did not, it is left
    /// to finish on its own rather than holding the check.
    fn stop(self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while !self.handle.is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if self.handle.is_finished() {
            let _ = self.handle.join();
        }
    }
}

/// When sampling starts and how often it repeats, from the silence budget:
/// quiet for `max(250 ms, min(30 s, budget/3))`, then every
/// `clamp(budget/4, 250 ms, 10 s)` — so even a one-second budget is sampled
/// several times before it runs out.
pub fn sampling_schedule(idle_secs: u64) -> (std::time::Duration, std::time::Duration) {
    let ms = std::time::Duration::from_millis;
    let budget = std::time::Duration::from_secs(idle_secs);
    let first = (budget / 3)
        .min(std::time::Duration::from_secs(30))
        .max(ms(250));
    let every = (budget / 4).clamp(ms(250), std::time::Duration::from_secs(10));
    (first, every)
}

fn sample_loop(
    pid: u32,
    activity: &Activity,
    idle_secs: u64,
    stop: &std::sync::atomic::AtomicBool,
) {
    use std::sync::atomic::Ordering::Relaxed;
    let (first, every) = sampling_schedule(idle_secs);
    let mut limits = crate::proctree::Limits::default();
    // A diagnostic, and the seam the timing tests use to force a partial
    // snapshot: cap the walk at this many processes.
    if let Some(n) = std::env::var("AMONT_CPU_MAX_PROCS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
    {
        limits.max_procs = n;
    }
    let trace = std::env::var_os("AMONT_CPU_TRACE");
    let mut tracker = crate::proctree::Tracker::default();
    let nap = |d: std::time::Duration| {
        let until = std::time::Instant::now() + d;
        while !stop.load(Relaxed) && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    };
    while !stop.load(Relaxed) {
        // The silence the clock counts, not the raw output silence: during
        // a declared wait there is nothing to prove, and no CPU is sampled.
        if activity.silence_for() < first {
            // Talking: nothing to prove, and the next quiet stretch starts
            // from a fresh baseline rather than a stale one.
            tracker = crate::proctree::Tracker::default();
            nap(std::time::Duration::from_millis(100));
            continue;
        }
        let snap = crate::proctree::snapshot(pid, &tracker.seen(), &limits);
        let at = std::time::Instant::now();
        let traced = trace.as_ref().map(|_| match &snap {
            crate::proctree::Snapshot::Complete(procs) => procs.clone(),
            _ => Vec::new(),
        });
        let obs = tracker.observe(at, snap);
        if let (Some(path), Some(procs)) = (&trace, traced) {
            trace_sample(path, pid, &procs, &obs);
        }
        activity.record(obs, at, every);
        nap(every);
    }
}

/// `AMONT_CPU_TRACE=<file>`: append what each sample saw — one
/// `pid start ppid cpu_ns` line per process of a complete snapshot, then a
/// `# root <pid> <observation>` line. A diagnostic, and the seam the timing
/// tests use to know a worker was seen before they orphan it (they match
/// the leading pid).
fn trace_sample(
    path: &std::ffi::OsStr,
    root: u32,
    procs: &[crate::proctree::Proc],
    obs: &crate::proctree::Observation,
) {
    use std::io::Write;
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    let mut text = String::new();
    for p in procs {
        text.push_str(&format!(
            "{} {} {} {}\n",
            p.id.pid, p.id.start, p.ppid, p.cpu_ns
        ));
    }
    let obs = match obs {
        crate::proctree::Observation::Window(w) => {
            format!("window {} milli-cores", w.milli_cores())
        }
        other => format!("{other:?}").to_lowercase(),
    };
    text.push_str(&format!("# root {root} {obs}\n"));
    let _ = f.write_all(text.as_bytes());
}

/// [`status_within`], with the child's stdout and stderr CAPTURED into the
/// calling check's slot instead of inherited — the other half of one-check-
/// one-block: a linter's twelve lines used to land on the shared terminal
/// between two other checks' lines. Falls back to plain [`status_within`]
/// when no slot is installed on this thread (`amont.progress false`, or a
/// spawn outside a stage), which is byte-for-byte the old behaviour.
///
/// stdout and stderr merge in ARRIVAL order inside the block, which is what
/// the terminal showed before. The readers are threads, not processes, and
/// they are joined before the status is returned so a block can never grow
/// after its check finished.
pub fn status_streamed(
    settings: &crate::config::Settings,
    cmd: &mut Command,
) -> std::io::Result<Ran> {
    let Some((stage, idx)) = crate::live::current_sink() else {
        return status_within(settings, cmd);
    };
    if crate::live::watching() {
        // The block lands on a real terminal but the tool sees a pipe and
        // would strip its colors; the big three opt-in knobs put them back.
        cmd.env("FORCE_COLOR", "1")
            .env("CLICOLOR_FORCE", "1")
            .env("CARGO_TERM_COLOR", "always");
    }
    run_observed(settings, cmd, move |bytes| stage.append_raw(idx, bytes))
}

/// Run to completion under the `amont.timeout` deadline with stdout and
/// stderr CAPTURED into a string the caller can parse — what the audit
/// checks need: their verdict lives in the tool's output, not its exit
/// code alone. Arrival-ordered merge of both streams, like
/// [`status_streamed`]'s blocks. `None` when the child cannot be spawned.
pub fn capture_within(
    settings: &crate::config::Settings,
    cmd: &mut Command,
) -> Option<(Ran, String)> {
    let text = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let sink = std::sync::Arc::clone(&text);
    let ran = run_observed(settings, cmd, move |bytes| {
        sink.lock()
            .unwrap_or_else(|p| p.into_inner())
            .push_str(&String::from_utf8_lossy(bytes));
    })
    .ok()?;
    let text = std::sync::Arc::try_unwrap(text)
        .map(|m| m.into_inner().unwrap_or_else(|p| p.into_inner()))
        .unwrap_or_default();
    Some((ran, text))
}

/// The wait over an already-spawned child — shared by every runner. The
/// silence budget in `clocks` applies only when there is an [`Activity`] to
/// consult (piped output); with inherited stdio the ceiling is the only
/// clock, and with that off too the child is simply waited for.
pub(crate) fn wait_within(
    child: &mut std::process::Child,
    clocks: Clocks,
    activity: Option<&Activity>,
) -> std::io::Result<Ran> {
    let started = std::time::Instant::now();
    let clocks = match activity {
        Some(_) => clocks,
        None => Clocks {
            silence: None,
            extended: None,
            ..clocks
        },
    };
    if clocks.ceiling.is_none() && clocks.silence.is_none() {
        return child.wait().map(Ran::Status);
    }
    // The clocks as judged: `silence` stretched by the host's load, re-read
    // every few seconds. The configured value stays in `clocks` for the
    // message.
    let mut live = clocks;
    let mut load: Option<(crate::load::Load, u32)> = None;
    let mut next_load = started;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Ran::Status(status));
        }
        let now = std::time::Instant::now();
        if let (Some(idle), Some(a)) = (clocks.silence, activity) {
            if clocks.scale > 1 && now >= next_load {
                next_load = now + LOAD_EVERY;
                if let Some(l) = crate::load::read() {
                    let factor = l.factor_milli(clocks.scale);
                    let applied = crate::load::scaled_budget(idle, l, clocks.scale, clocks.ceiling);
                    live.silence = Some(applied);
                    a.set_load(applied, l, factor);
                    load = (factor > 1000).then_some((l, factor));
                }
            }
        }
        // Judged on "silent AND idle on CPU"; equal to plain silence when CPU
        // is not sampled, and zero during a declared wait.
        let quiet = activity.map(|a| a.still_for());
        let waiting = activity.and_then(Activity::waiting);
        let gate = activity.map_or(CpuGate::NotSampled, Activity::gate);
        let why = judge(now.duration_since(started), quiet, waiting, gate, &live);
        if let Some(why) = why {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(Ran::TimedOut(Killed {
                why,
                ran_secs: now.duration_since(started).as_secs(),
                // The true OUTPUT silence, for the message — not the
                // still-time the verdict was judged on.
                quiet_secs: activity.map(|a| a.quiet_for().as_secs()),
                cpu: activity.map_or(CpuVerdict::NotSampled, Activity::verdict),
                idle_secs: clocks.silence.unwrap_or(0),
                waiting: waiting.map(|(k, d)| (k, d.as_secs())),
                last_line: activity.and_then(Activity::last_line),
                load,
            }));
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

/// Which clock, if any, has fired — the decision, with no process or
/// clock of its own so it can be tested to the second.
///
/// `ran` is how long the command has been running; `quiet` how long it has
/// been silent (and idle, where CPU is sampled), `None` when nobody is
/// watching its output; `waiting` the declared wait in progress, with its
/// length. The ceiling wins when several have fired: it is the larger
/// claim, and the message for it carries the other figures anyway. A
/// declared wait answers to `lock_wait`, never to the silence budget — the
/// tool has said what it is doing — and with `lock_wait` deferring to a
/// ceiling that is off, to the extended silence budget, so that nothing is
/// unbounded. `cpu` picks which silence budget applies: the plain one when
/// idleness was measured or is not sampled at all, the extended one while
/// the sampler cannot say ([`CpuGate`]).
pub fn judge(
    ran: std::time::Duration,
    quiet: Option<std::time::Duration>,
    waiting: Option<(crate::hooks::wait::WaitKind, std::time::Duration)>,
    cpu: CpuGate,
    clocks: &Clocks,
) -> Option<Why> {
    if let Some(wall) = clocks.ceiling {
        if ran >= std::time::Duration::from_secs(wall) {
            return Some(Why::Ceiling(wall));
        }
    }
    if let Some((kind, waited)) = waiting {
        let budget = match clocks.lock_wait {
            LockWait::Secs(s) => Some(s),
            LockWait::UntilCeiling if clocks.ceiling.is_some() => None,
            LockWait::UntilCeiling => clocks.extended,
        };
        return match budget {
            Some(b) if waited >= std::time::Duration::from_secs(b) => Some(Why::Waited(kind, b)),
            _ => None,
        };
    }
    let budget = match cpu {
        CpuGate::NotSampled | CpuGate::Measured => clocks.silence,
        CpuGate::Unmeasured => clocks.extended.or(clocks.silence),
    };
    if let (Some(idle), Some(q)) = (budget, quiet) {
        if q >= std::time::Duration::from_secs(idle) {
            return Some(Why::Silence(idle));
        }
    }
    None
}

/// Say a command was killed, by which clock, and what that tells you.
///
/// The two clocks exist to answer two different questions, so the message
/// answers the one that was asked: silence means stuck — look at the tool;
/// the ceiling with recent output means slow — raise the ceiling.
pub fn say_timed_out(what: &str, k: Killed) {
    match k.why {
        Why::Silence(budget) => {
            // Why the budget is not the configured one, when it is not:
            // the extended budget while CPU could not be measured, or the
            // configured one stretched by the host's load.
            let stretched = match (k.cpu, k.load) {
                _ if budget == k.idle_secs => String::new(),
                (CpuVerdict::Unmeasured, _) => format!(
                    " (the extended budget, {} × {}: its CPU could not be measured)",
                    human_secs(k.idle_secs),
                    hl("amont.idleLoadScale")
                ),
                (_, Some((load, factor))) => format!(
                    " ({} stretched {} by a load average of {} on {} cores — {})",
                    human_secs(k.idle_secs),
                    crate::load::factor_text(factor),
                    load.avg1_text(),
                    load.cores,
                    hl("amont.idleLoadScale")
                ),
                _ => String::new(),
            };
            fail(&match k.cpu {
                CpuVerdict::MeasuredIdle(covered) => format!(
                    "{} printed nothing for {}{stretched} and did no measurable CPU work \
                     (< 0.1 core) in the last {} of it; killed after {} — a tool this idle \
                     is stuck, not slow. {} raises the silence budget (0 disables)",
                    hl(what),
                    human_secs(budget),
                    human_secs(covered.max(1)),
                    human_secs(k.ran_secs),
                    hl("git config amont.idleTimeout <secs>")
                ),
                _ => format!(
                    "{} printed nothing for {}{stretched}{} and was killed after {} — a tool \
                     this quiet is usually stuck, not slow. {} raises the silence budget (0 \
                     disables)",
                    hl(what),
                    human_secs(budget),
                    if k.cpu == CpuVerdict::Unmeasured && stretched.is_empty() {
                        " (CPU not measured)"
                    } else {
                        ""
                    },
                    human_secs(k.ran_secs),
                    hl("git config amont.idleTimeout <secs>")
                ),
            })
        }
        Why::Waited(kind, budget) => fail(&format!(
            "{} waited {} for {} and was killed after {} — {}. {} raises the wait budget \
             (0: until the ceiling)",
            hl(what),
            human_secs(budget),
            kind.describe(),
            human_secs(k.ran_secs),
            kind.holders(),
            hl("git config amont.lockWait <secs>")
        )),
        Why::Ceiling(budget) => {
            let verdict = match (k.quiet_secs, k.cpu) {
                (_, _) if k.waiting.is_some() => {
                    let (kind, w) = k.waiting.expect("checked");
                    format!(
                        " It was waiting for {} for the last {} — {}.",
                        kind.describe(),
                        human_secs(w),
                        kind.holders()
                    )
                }
                (Some(q), CpuVerdict::BusyAtKill(m)) if k.idle_secs > 0 && q >= k.idle_secs => {
                    format!(
                        " It printed nothing for the last {} but kept its CPU busy ({}), so the \
                     silence budget did not stop it: a busy loop, or a tool that prints only \
                     at the end (give it a per-file reporter). {} kills quiet runs at the \
                     silence budget whatever their CPU.",
                        human_secs(q),
                        cores(m),
                        hl("git config amont.idleCpuCredit false")
                    )
                }
                (Some(q), CpuVerdict::Unmeasured) if k.idle_secs > 0 && q >= k.idle_secs => {
                    format!(
                        " It printed nothing for the last {} and its CPU could not be measured, \
                         so only the extended silence budget applied. {} kills quiet runs at \
                         the silence budget whatever their CPU.",
                        human_secs(q),
                        hl("git config amont.idleCpuCredit false")
                    )
                }
                (Some(q), _) if q < 30 => format!(
                    " It was still printing ({} since its last line): slow, not stuck.",
                    human_secs(q)
                ),
                (Some(q), _) => format!(" Its last output was {} ago.", human_secs(q)),
                (None, _) => String::new(),
            };
            fail(&format!(
                "{} timed out: ran for {} and was killed at the ceiling. {} raises it \
                 (0 disables).{verdict}",
                hl(what),
                human_secs(budget),
                hl("git config amont.timeout <secs>")
            ))
        }
    }
}

/// [`status_within`], collapsed to "did it exit 0" — the shape the one-shot
/// tool spawns want. A timeout says so, names `what`, and reads as failure.
pub fn bounded_success(settings: &crate::config::Settings, cmd: &mut Command, what: &str) -> bool {
    match status_streamed(settings, cmd) {
        Ok(Ran::Status(s)) => s.success(),
        Ok(Ran::TimedOut(b)) => {
            say_timed_out(what, b);
            false
        }
        Err(_) => false,
    }
}

/// Run `argv` from `root`, inheriting stdio. True when it exits 0.
pub fn run(
    settings: &crate::config::Settings,
    root: &str,
    argv: &[String],
    extra: &[String],
) -> bool {
    let Some((program, rest)) = argv.split_first() else {
        return true;
    };
    let mut cmd = Command::new(program);
    cmd.args(rest)
        .args(extra)
        .current_dir(root)
        .stdin(Stdio::null());
    strip_git_env(&mut cmd);
    bounded_success(settings, &mut cmd, program)
}

/// As [`run`], but with the tool's own output discarded.
///
/// For a pass whose only job is to decide something — prettier's `--check`,
/// ruff's `--fix` sweep — where the offenders are printed once, by the pass
/// that reports them, rather than twice.
pub fn run_quiet(
    settings: &crate::config::Settings,
    root: &str,
    argv: &[String],
    extra: &[String],
) -> bool {
    let Some((program, rest)) = argv.split_first() else {
        return true;
    };
    let mut cmd = Command::new(program);
    cmd.args(rest)
        .args(extra)
        .current_dir(root)
        .stdin(Stdio::null());
    strip_git_env(&mut cmd);
    // Deliberately NOT the streamed runner: this helper's contract is that
    // the output is discarded, and capture would resurrect it into the
    // block. Observed and dropped instead of `/dev/null`, so the silence
    // clock still sees whether the tool is alive.
    match run_observed(settings, &mut cmd, |_| {}) {
        Ok(Ran::Status(s)) => s.success(),
        Ok(Ran::TimedOut(b)) => {
            say_timed_out(program, b);
            false
        }
        Err(_) => false,
    }
}

/// Whether the user asked for checks to repair what they find.
///
/// OFF by default. `git config amont.fix true` turns it on, per repository,
/// because a hook that edits your files without being asked is a larger
/// surprise than one that complains — and because with index fidelity in place
/// the repair lands in the commit you are making, which is a bigger claim to
/// make on somebody's behalf than printing an error.
pub fn fixing_enabled(settings: &crate::config::Settings) -> bool {
    // Never while the file set is not the index — see `NOT_THE_INDEX`.
    !not_the_index() && fixing_requested(settings)
}

/// What the CONFIG says, ignoring whether the current run may act on it.
///
/// Split out so `run_all` can tell the difference between "fixing is off" and
/// "you asked for fixing and this mode will not do it", and say the second out
/// loud instead of silently ignoring the key.
pub fn fixing_requested(settings: &crate::config::Settings) -> bool {
    *settings
        .fixing
        .get_or_init(|| crate::config::boolean_or(settings, "amont.fix", false))
}

/// What a re-stage actually did. THREE answers, because the old `bool`
/// conflated two of them and the conflation shipped unformatted code.
///
/// `prettier.rs` read `if run_quiet(write) && restage(&files) { … Fixed }`. When
/// `git add` FAILED, `restage` returned `false` — indistinguishable from
/// "nothing needed staging" — so control fell through to a second `--check`
/// pass, which inspected the NOW-FORMATTED WORKING TREE, passed, printed
/// "Prettier passed" and returned `Outcome::Passed`. The index still held the
/// unformatted content, so the commit contained unformatted code and the hook
/// said it had passed. `manifest.rs` had the same shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Restaged {
    /// No path differed from the index — nothing to do, and nothing wrong.
    Nothing,
    /// `git add` succeeded; the index now holds the repair.
    Staged,
    /// `git add` failed, carrying the paths it could not stage. The index
    /// holds content the fixer has already replaced on disk, so this MUST be
    /// loud at every call site — and naming the files is the difference
    /// between a message somebody can act on and one they cannot.
    Failed(Vec<String>),
}

/// Serialises this process's own `git add` calls.
///
/// pre-commit runs its checks concurrently (`dispatch.rs`), and up to three of
/// them can re-stage. git takes `$GIT_DIR/index.lock` exclusively, so two
/// concurrent `git add`s in the same repository make one of them fail — which,
/// before `Restaged`, was silently read as "nothing moved". Holding this across
/// the `git add` removes self-contention entirely; the retry below is only for
/// OTHER processes.
static INDEX_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Re-stage exactly the paths a fixer rewrote, and say what happened.
///
/// Safe ONLY because the pre-commit stage holds unstaged changes aside: the
/// tree contains the staged content and nothing else, so anything a formatter
/// touched is by definition part of this commit. Without that, re-staging would
/// sweep in work the author deliberately kept back.
pub fn restage(paths: &[String]) -> Restaged {
    // Belt and braces alongside `fixing_enabled`: a future fixer that forgets
    // the gate still cannot turn `amont run --all-files` into `git add .`.
    if not_the_index() {
        return Restaged::Nothing;
    }
    let changed: Vec<String> = paths
        .iter()
        .filter(|p| !git::succeeds(&["diff", "--quiet", "--", p]))
        .cloned()
        .collect();
    if changed.is_empty() {
        return Restaged::Nothing;
    }
    let mut args = vec!["add", "--"];
    args.extend(changed.iter().map(String::as_str));

    let _serialised = INDEX_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Another PROCESS can hold `index.lock` — a `git status` from an editor, a
    // second hook in a linked worktree. Back off and retry rather than
    // reporting a transient collision as a failed repair. `git add` of the same
    // paths is idempotent: it records the paths' current worktree content, so
    // running it twice records the same thing twice and cannot double-stage.
    const BACKOFF_MS: [u64; 3] = [50, 150, 400];
    if git::succeeds(&args) {
        return Restaged::Staged;
    }
    for wait in BACKOFF_MS {
        std::thread::sleep(std::time::Duration::from_millis(wait));
        if git::succeeds(&args) {
            return Restaged::Staged;
        }
    }
    Restaged::Failed(changed)
}

/// A check passed. THE funnel for every success line, which is what lets
/// `amont.quiet` swallow them in one place — see [`crate::live::quiet`].
pub fn ok(settings: &crate::config::Settings, msg: &str) {
    if crate::live::quiet(settings) {
        return;
    }
    crate::live::say(&format!("{} {msg}", valid_sign()));
}
pub fn fail(msg: &str) {
    crate::live::say(&format!("{} {msg}", error_sign()));
}
pub fn warn(msg: &str) {
    crate::live::say(&format!("{} {msg}", warning_sign()));
}
/// A line with no sign of its own — what a check's direct `println!` becomes,
/// so it lands in the check's block instead of interleaving. See `live::say`.
pub fn say(msg: &str) {
    crate::live::say(msg);
}

/// Orange, for the fragments these hooks highlight.
pub fn hl(s: &str) -> String {
    crate::ui::highlight(s)
}

#[cfg(test)]
mod tests {

    /// A window of `milli` thousandths of a core between `start` and `end`
    /// (a zero-length window still reports `milli`: `milli_cores` floors the
    /// wall time at 1 ns).
    fn window(
        start: std::time::Instant,
        end: std::time::Instant,
        milli: u64,
    ) -> crate::proctree::Observation {
        let wall = u64::try_from(end.duration_since(start).as_nanos())
            .unwrap_or(u64::MAX)
            .max(1);
        let gain = wall * milli / 1000;
        crate::proctree::Observation::Window(crate::proctree::Window {
            start,
            end,
            gain_ns: gain,
            gapped: false,
        })
    }

    /// A gapped window resets the measured-idle span to its own end: the
    /// claim never covers the gap, while a busy gapped window still counts
    /// as busy.
    #[test]
    fn a_gapped_window_is_never_claimed_as_measured_idle() {
        use super::{Activity, CpuVerdict};
        use crate::proctree::{Observation, Window};
        let s = std::time::Duration::from_secs;
        let every = s(10);
        let a = Activity::new();
        a.enable_cpu();
        let t0 = std::time::Instant::now() - s(30);
        a.record(Observation::Baseline, t0, every);
        a.record(Observation::Skipped, t0 + s(10), every);
        let gap_end = std::time::Instant::now();
        a.record(
            Observation::Window(Window {
                start: t0,
                end: gap_end,
                gain_ns: 0,
                gapped: true,
            }),
            gap_end,
            every,
        );
        // Idle, but the span rests only on what followed the gap: 0 s.
        assert_eq!(a.verdict(), CpuVerdict::MeasuredIdle(0));
        assert_eq!(a.gate(), super::CpuGate::Measured);

        let busy = Activity::new();
        busy.enable_cpu();
        busy.record(Observation::Baseline, t0, every);
        busy.record(Observation::Skipped, t0 + s(10), every);
        let now = std::time::Instant::now();
        busy.record(window_gapped(t0, now, 3000), now, every);
        assert_eq!(busy.verdict(), CpuVerdict::BusyAtKill(3000));
        assert!(busy.still_for() < s(1));
    }

    fn window_gapped(
        start: std::time::Instant,
        end: std::time::Instant,
        milli: u64,
    ) -> crate::proctree::Observation {
        match window(start, end, milli) {
            crate::proctree::Observation::Window(w) => {
                crate::proctree::Observation::Window(crate::proctree::Window { gapped: true, ..w })
            }
            o => o,
        }
    }

    /// Even the shortest budget is sampled several times before it runs out,
    /// and the default one starts at 30 s and repeats every 10 s.
    #[test]
    fn the_sampling_schedule_follows_the_budget_within_floors() {
        use super::sampling_schedule;
        let ms = std::time::Duration::from_millis;
        let third = |secs: u64| std::time::Duration::from_secs(secs) / 3;
        assert_eq!(sampling_schedule(1), (third(1), ms(250)));
        assert_eq!(sampling_schedule(2), (third(2), ms(500)));
        assert_eq!(sampling_schedule(120), (ms(30_000), ms(10_000)));
        assert_eq!(sampling_schedule(0), (ms(250), ms(250)));
    }

    /// Without CPU data the still-time IS the silence; a busy window pulls it
    /// back to the window's start, and the output clock is left alone.
    #[test]
    fn busy_work_resets_the_still_time_but_not_the_output_silence() {
        use super::Activity;
        let a = Activity::new();
        a.touch();
        std::thread::sleep(std::time::Duration::from_millis(60));
        let quiet = a.quiet_for();
        assert!(a.still_for() >= quiet.saturating_sub(std::time::Duration::from_millis(5)));
        a.enable_cpu();
        let now = std::time::Instant::now();
        a.record(
            window(now - std::time::Duration::from_millis(10), now, 2000),
            now,
            std::time::Duration::from_secs(1),
        );
        assert!(a.still_for() < std::time::Duration::from_millis(40));
        assert!(a.quiet_for() >= std::time::Duration::from_millis(60));
    }

    /// What the kill message may claim. "Measured idle" names the unbroken
    /// span of complete measurements behind it; a broken run is unmeasured;
    /// a fresh busy window is reported as busy; no sampling says nothing.
    #[test]
    fn the_cpu_verdict_claims_only_what_was_measured() {
        use super::{Activity, CpuVerdict};
        use crate::proctree::Observation;
        let s = std::time::Duration::from_secs;
        let every = s(10);

        let off = Activity::new();
        assert_eq!(off.verdict(), CpuVerdict::NotSampled);

        let waiting = Activity::new();
        waiting.enable_cpu();
        assert_eq!(waiting.verdict(), CpuVerdict::Unmeasured);

        let now = std::time::Instant::now();
        let before = now - s(1);
        // A real second of complete, idle measurement: the claim names it.
        let idle = Activity::new();
        idle.enable_cpu();
        let t0 = std::time::Instant::now();
        idle.record(Observation::Baseline, t0, every);
        std::thread::sleep(std::time::Duration::from_millis(1050));
        let t1 = std::time::Instant::now();
        idle.record(window(t0, t1, 20), t1, every);
        assert_eq!(idle.verdict(), CpuVerdict::MeasuredIdle(1));

        let busy = Activity::new();
        busy.enable_cpu();
        busy.record(Observation::Baseline, before, every);
        busy.record(window(before, now, 3900), now, every);
        assert_eq!(busy.verdict(), CpuVerdict::BusyAtKill(3900));

        let broken = Activity::new();
        broken.enable_cpu();
        broken.record(Observation::Baseline, before, every);
        broken.record(window(before, now, 20), now, every);
        broken.record(Observation::Unmeasured, now, every);
        assert_eq!(broken.verdict(), CpuVerdict::Unmeasured);
        assert_eq!(broken.gate(), super::CpuGate::Unmeasured);
        assert_eq!(off.gate(), super::CpuGate::NotSampled);
        assert_eq!(idle.gate(), super::CpuGate::Measured);
        assert_eq!(busy.gate(), super::CpuGate::Measured);
    }

    /// A rate older than two sampling intervals is not shown, and cannot
    /// back a "busy" or "idle" claim.
    #[test]
    fn a_stale_rate_expires() {
        use super::{Activity, CpuVerdict};
        use crate::proctree::Observation;
        let a = Activity::new();
        a.enable_cpu();
        let then = std::time::Instant::now();
        let tick = std::time::Duration::from_millis(5);
        let earlier = then - std::time::Duration::from_secs(1);
        a.record(Observation::Baseline, earlier, tick);
        a.record(window(earlier, then, 3000), then, tick);
        std::thread::sleep(std::time::Duration::from_millis(40));
        assert_eq!(a.fresh_rate(), None);
        assert_eq!(a.verdict(), CpuVerdict::Unmeasured);
    }

    #[test]
    fn cores_read_as_one_decimal() {
        assert_eq!(super::cores(3900), "~3.9 cores");
        assert_eq!(super::cores(420), "~0.4 cores");
        assert_eq!(super::cores(100), "~0.1 cores");
    }

    /// The two clocks, decided to the second. A chatty command outlives any
    /// silence budget however long it runs; a silent one dies at the budget
    /// however short; a command nobody watches answers to the ceiling only.
    #[test]
    fn the_clocks_judge_silence_and_ceiling_separately() {
        use super::{judge, CpuGate, Why};
        use std::time::Duration as D;
        let s = D::from_secs;
        let c = |ceiling: Option<u64>, silence: Option<u64>| super::Clocks {
            ceiling,
            silence,
            lock_wait: super::LockWait::Secs(600),
            extended: silence,
            scale: 1,
        };
        // Chatty and long: past a five-second silence budget, still fine.
        assert_eq!(
            judge(
                s(900),
                Some(s(0)),
                None,
                CpuGate::NotSampled,
                &c(Some(3600), Some(5))
            ),
            None
        );
        assert_eq!(
            judge(
                s(900),
                Some(s(4)),
                None,
                CpuGate::NotSampled,
                &c(Some(3600), Some(5))
            ),
            None
        );
        // Silent for the budget: killed, and the silence is blamed.
        assert_eq!(
            judge(
                s(30),
                Some(s(5)),
                None,
                CpuGate::NotSampled,
                &c(Some(3600), Some(5))
            ),
            Some(Why::Silence(5))
        );
        // Unobserved output: the silence clock cannot run at all.
        assert_eq!(
            judge(
                s(900),
                None,
                None,
                CpuGate::NotSampled,
                &c(Some(3600), Some(5))
            ),
            None
        );
        // The ceiling fires on elapsed time whatever the output is doing.
        assert_eq!(
            judge(
                s(3600),
                Some(s(0)),
                None,
                CpuGate::NotSampled,
                &c(Some(3600), Some(120))
            ),
            Some(Why::Ceiling(3600))
        );
        // Both fired at once: the ceiling is the answer.
        assert_eq!(
            judge(
                s(3600),
                Some(s(600)),
                None,
                CpuGate::NotSampled,
                &c(Some(3600), Some(120))
            ),
            Some(Why::Ceiling(3600))
        );
        // Both off: nothing ever fires.
        assert_eq!(
            judge(
                s(86_400),
                Some(s(86_400)),
                None,
                CpuGate::NotSampled,
                &c(None, None)
            ),
            None
        );
        // Only silence on: no ceiling, however long it runs.
        assert_eq!(
            judge(
                s(86_400),
                Some(s(1)),
                None,
                CpuGate::NotSampled,
                &c(None, Some(120))
            ),
            None
        );
    }

    /// A declared wait answers to `amont.lockWait` and never to the silence
    /// budget; at 0 it answers to the ceiling, and with the ceiling off too,
    /// to the extended silence budget — never to nothing.
    #[test]
    fn a_declared_wait_answers_to_its_own_budget() {
        use super::{judge, Clocks, CpuGate, LockWait, Why};
        use crate::hooks::wait::{CargoLockWhat, WaitKind};
        use std::time::Duration as D;
        let s = D::from_secs;
        let kind = WaitKind::CargoLock(CargoLockWhat::BuildDirectory);
        let clocks = Clocks {
            ceiling: Some(3600),
            silence: Some(120),
            lock_wait: LockWait::Secs(600),
            extended: Some(480),
            scale: 4,
        };
        // Silent for ten times the budget, but in a declared wait: fine.
        assert_eq!(
            judge(
                s(1300),
                Some(s(0)),
                Some((kind, s(599))),
                CpuGate::Measured,
                &clocks
            ),
            None
        );
        // The wait outlives its budget: killed, and the wait is blamed.
        assert_eq!(
            judge(
                s(1300),
                Some(s(0)),
                Some((kind, s(600))),
                CpuGate::Measured,
                &clocks
            ),
            Some(Why::Waited(kind, 600))
        );
        // The ceiling still wins.
        assert_eq!(
            judge(
                s(3600),
                Some(s(0)),
                Some((kind, s(3000))),
                CpuGate::Measured,
                &clocks
            ),
            Some(Why::Ceiling(3600))
        );
        // lockWait 0 with a ceiling: the ceiling alone bounds the wait.
        let until = Clocks {
            lock_wait: LockWait::UntilCeiling,
            ..clocks
        };
        assert_eq!(
            judge(
                s(3000),
                Some(s(0)),
                Some((kind, s(2900))),
                CpuGate::Measured,
                &until
            ),
            None
        );
        // lockWait 0 and no ceiling: the extended budget bounds it.
        let open = Clocks {
            ceiling: None,
            ..until
        };
        assert_eq!(
            judge(
                s(3000),
                Some(s(0)),
                Some((kind, s(479))),
                CpuGate::Measured,
                &open
            ),
            None
        );
        assert_eq!(
            judge(
                s(3000),
                Some(s(0)),
                Some((kind, s(480))),
                CpuGate::Measured,
                &open
            ),
            Some(Why::Waited(kind, 480))
        );
    }

    /// While the CPU is unmeasured the silence budget is the extended one:
    /// no kill before `idle × scale`, a kill at it even with the ceiling
    /// off; measured and not-sampled keep the plain budget.
    #[test]
    fn an_unmeasured_cpu_answers_to_the_extended_budget() {
        use super::{judge, Clocks, CpuGate, LockWait, Why};
        use std::time::Duration as D;
        let s = D::from_secs;
        let clocks = Clocks {
            ceiling: None,
            silence: Some(120),
            lock_wait: LockWait::Secs(600),
            extended: Some(480),
            scale: 4,
        };
        assert_eq!(
            judge(s(500), Some(s(300)), None, CpuGate::Unmeasured, &clocks),
            None
        );
        assert_eq!(
            judge(s(500), Some(s(479)), None, CpuGate::Unmeasured, &clocks),
            None
        );
        assert_eq!(
            judge(s(500), Some(s(480)), None, CpuGate::Unmeasured, &clocks),
            Some(Why::Silence(480))
        );
        assert_eq!(
            judge(s(500), Some(s(120)), None, CpuGate::Measured, &clocks),
            Some(Why::Silence(120))
        );
        assert_eq!(
            judge(s(500), Some(s(120)), None, CpuGate::NotSampled, &clocks),
            Some(Why::Silence(120))
        );
        // No extended budget configured (silence off): nothing fires.
        let off = Clocks {
            silence: None,
            extended: None,
            ..clocks
        };
        assert_eq!(
            judge(s(86_400), Some(s(86_400)), None, CpuGate::Unmeasured, &off),
            None
        );
    }

    /// A marker line starts a wait that a second marker continues and any
    /// other line ends; the silence the clock counts is zero meanwhile, the
    /// output silence is not, and the time is banked for the note.
    #[test]
    fn a_marker_line_pauses_the_silence_clock_until_another_line() {
        use super::Activity;
        use crate::hooks::wait::{CargoLockWhat, WaitKind};
        let a = Activity::new();
        a.touch();
        assert_eq!(a.waiting(), None);
        a.stderr_line("    Blocking waiting for file lock on package cache");
        std::thread::sleep(std::time::Duration::from_millis(30));
        let (kind, waited) = a.waiting().expect("a wait is in progress");
        assert_eq!(kind, WaitKind::CargoLock(CargoLockWhat::PackageCache));
        assert!(waited >= std::time::Duration::from_millis(30));
        assert_eq!(a.silence_for(), std::time::Duration::ZERO);
        assert_eq!(a.still_for(), std::time::Duration::ZERO);
        assert!(a.quiet_for() >= std::time::Duration::from_millis(30));
        // A second marker keeps the original start, and names the new lock.
        a.stderr_line("\u{1b}[1m    Blocking\u{1b}[0m waiting for file lock on build directory");
        let (kind, again) = a.waiting().expect("still waiting");
        assert_eq!(kind, WaitKind::CargoLock(CargoLockWhat::BuildDirectory));
        assert!(again >= waited);
        // Any other line ends it, and the time spent is banked. The reader
        // touches the output clock before it frames, as here.
        a.touch();
        a.stderr_line("    Checking foo v0.1.0");
        assert_eq!(a.waiting(), None);
        assert!(a.waited_total() >= std::time::Duration::from_millis(30));
        assert_eq!(a.last_wait_kind(), Some(kind));
        assert_eq!(a.last_line().as_deref(), Some("Checking foo v0.1.0"));
        assert!(a.silence_for() < std::time::Duration::from_millis(20));
        // A stdout chunk ends a wait too.
        a.stderr_line("Waiting to acquire write lock for `x`");
        assert!(a.waiting().is_some());
        a.end_wait();
        assert_eq!(a.waiting(), None);
    }

    /// The deadline kills what outlives it and reports what finished.
    #[cfg(unix)]
    #[test]
    fn the_deadline_kills_a_sleeper_and_spares_a_finisher() {
        let started = std::time::Instant::now();
        let mut slow = Command::new(program("sleep"));
        slow.arg("300").stdin(Stdio::null());
        match status_within_secs(&mut slow, 1) {
            Ok(Ran::TimedOut(Killed {
                why: Why::Ceiling(1),
                quiet_secs: None,
                ..
            })) => {}
            other => panic!("expected TimedOut(1), got {:?}", other.map(|_| "ran")),
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(60),
            "the kill did not happen at the deadline"
        );

        let mut quick = Command::new(program("true"));
        quick.stdin(Stdio::null());
        match status_within_secs(&mut quick, 60) {
            Ok(Ran::Status(s)) => assert!(s.success()),
            other => panic!("expected a clean exit, got {:?}", other.map(|_| "?")),
        }
    }

    use super::*;

    #[test]
    fn which_finds_a_real_binary_and_not_a_fake_one() {
        assert!(which("git").is_some());
        assert!(which("definitely-not-a-real-binary-xyz").is_none());
    }

    /// On Windows a tool can exist BOTH as an extensionless shell script and as
    /// a .cmd/.exe in the same directory; only the latter is executable by
    /// CreateProcess, so the extension forms must win.
    #[test]
    #[cfg(windows)]
    fn windows_prefers_an_executable_extension_over_a_bare_file() {
        let dir = std::env::temp_dir().join("amont-which-order");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("faketool"), "#!/bin/sh\n").unwrap();
        std::fs::write(dir.join("faketool.cmd"), "@echo off\n").unwrap();
        // The path is PASSED, never installed into this process: see
        // `which_on`. The old spelling swapped the real PATH out from under
        // every other test in this binary for the length of the call.
        let found = which_on(dir.as_os_str(), "faketool").unwrap();
        assert!(found.ends_with(".cmd"), "got {found}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// "Nothing moved" and "`git add` FAILED" are different answers, and the
    /// old `bool` gave the same one for both.
    ///
    /// That conflation is what shipped unformatted code: `prettier.rs` read
    /// `if wrote && restage(&files)`, so a failed `git add` fell through to a
    /// second `--check` against the now-formatted WORKING TREE, which passed —
    /// while the INDEX still held the unformatted content the commit would
    /// carry.
    ///
    /// An absolute path outside any repository is a `git add` git will always
    /// refuse, which is the only way to reach the failing branch without
    /// sabotaging a real index.
    #[test]
    fn restage_distinguishes_nothing_from_failure() {
        // `restage` runs `git add` in the PROCESS cwd, so this test depends
        // on that cwd as surely as one that moves it — see `crate::TEST_CWD`.
        // Without the lock it ran inside whatever fixture `gate_stamp` had
        // moved into, and took that repository's index.lock out from under
        // its own commit.
        let _cwd = crate::TEST_CWD.lock().unwrap_or_else(|p| p.into_inner());
        let outside = std::env::temp_dir()
            .join("amont-restage-outside-any-repo")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            restage(std::slice::from_ref(&outside)),
            Restaged::Failed(vec![outside]),
            "a `git add` git refuses must report Failed, never Nothing"
        );
        assert_eq!(
            restage(&[]),
            Restaged::Nothing,
            "no paths is nothing to do, and nothing wrong"
        );
    }

    /// No check may hand `Command` a bare program name.
    ///
    /// `Command::new` does NO PATHEXT resolution, so `Command::new("npm")`
    /// cannot execute `npm.cmd` and `Command::new("uvx")` cannot execute
    /// `uvx.exe`: the spawn fails with "program not found" and a
    /// `Severity::Block` check reports an installed tool as broken. That is the
    /// incident `program()` exists for, and it kept recurring — `yamllint` and
    /// three sites in `python_tools` were still doing it, THREE OF THEM after
    /// `which()` had already succeeded and discarded the answer.
    ///
    /// A source scan rather than a runtime assertion because the failure only
    /// reproduces on Windows, and the whole point is to catch the next one on
    /// every platform. Comment lines are skipped: `program()`'s own doc quotes
    /// the offending call. The needle is assembled from two pieces so this
    /// module — which the scan also reads — does not match itself.
    #[test]
    fn no_hook_spawns_a_bare_program_name() {
        let needle = concat!("Command", "::new(");
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/hooks");
        let mut scanned = 0usize;
        for entry in std::fs::read_dir(dir).expect("hooks dir").flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            scanned += 1;
            let src = std::fs::read_to_string(&path).expect("read a hook module");
            for (n, line) in src.lines().enumerate() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                let Some(after) = line.split_once(needle) else {
                    continue;
                };
                assert!(
                    !after.1.starts_with('"'),
                    "{}:{} spawns a bare name — route it through `program()` or \
                     the path `which()` already resolved: {}",
                    path.display(),
                    n + 1,
                    line.trim()
                );
            }
        }
        assert!(
            scanned > 10,
            "the scan found almost nothing: {scanned} files"
        );
    }

    #[test]
    fn first_existing_picks_the_earliest_present_name() {
        let dir = std::env::temp_dir().join("amont-first-existing-test");
        let _ = std::fs::create_dir_all(&dir);
        let root = dir.to_string_lossy().into_owned();
        let _ = std::fs::write(dir.join("second"), "x");
        assert_eq!(
            first_existing(&root, &["first", "second", "third"]).as_deref(),
            Some("second")
        );
        assert_eq!(first_existing(&root, &["nope"]), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
