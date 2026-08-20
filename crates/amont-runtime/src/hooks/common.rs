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
    let path = std::env::var_os("PATH")?;
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
    for dir in std::env::split_paths(&path) {
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

/// The wall-clock budget for one check's spawned command, in seconds.
///
/// `amont.timeout`, default 600 — ten minutes, the figure the generated
/// agent guidance already tells tooling to allow a whole commit or push; a
/// single check that outlives it is not slow, it is stuck. `0` disables.
/// Read once per process: twenty concurrent checks must not each spawn a
/// `git config` to learn the same number.
pub fn check_timeout() -> u64 {
    static TIMEOUT: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *TIMEOUT.get_or_init(|| crate::config::integer_or("amont.timeout", 600, 0..=86_400) as u64)
}

/// The deadline for a network PROBE — an `ls-remote` asked before the real
/// work, not the work itself. Capped at 30s below [`check_timeout`]: a
/// probe answers in a second or two when the network is there at all, and
/// a healthy `amont.timeout` of ten minutes is sized for a test suite, not
/// for deciding whether the remote is reachable. Shrinking `amont.timeout`
/// below the cap shrinks this too, and `0` keeps meaning no deadline —
/// somebody who disabled the clock disabled all of it.
pub fn network_probe_budget() -> u64 {
    match check_timeout() {
        0 => 0,
        t => t.min(30),
    }
}

/// What became of a command run under the deadline.
pub enum Ran {
    Status(std::process::ExitStatus),
    /// Killed at the deadline; carries the budget it exceeded, in seconds.
    TimedOut(u64),
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
pub fn status_within(cmd: &mut Command) -> std::io::Result<Ran> {
    status_within_secs(cmd, check_timeout())
}

/// [`status_within`] with an explicit budget — the testable seam.
pub fn status_within_secs(cmd: &mut Command, budget_secs: u64) -> std::io::Result<Ran> {
    if budget_secs == 0 {
        return cmd.status().map(Ran::Status);
    }
    let mut child = cmd.spawn()?;
    wait_within(&mut child, budget_secs)
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
pub fn status_streamed(cmd: &mut Command) -> std::io::Result<Ran> {
    let Some((stage, idx)) = crate::live::current_sink() else {
        return status_within(cmd);
    };
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    if crate::live::watching() {
        // The block lands on a real terminal but the tool sees a pipe and
        // would strip its colors; the big three opt-in knobs put them back.
        cmd.env("FORCE_COLOR", "1")
            .env("CLICOLOR_FORCE", "1")
            .env("CARGO_TERM_COLOR", "always");
    }
    let budget = check_timeout();
    let mut child = cmd.spawn()?;
    let mut readers = Vec::new();
    for pipe in [
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let stage = std::sync::Arc::clone(&stage);
        readers.push(std::thread::spawn(move || {
            let mut pipe = pipe;
            let mut chunk = [0u8; 4096];
            loop {
                match std::io::Read::read(&mut pipe, &mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => stage.append_raw(idx, &chunk[..n]),
                }
            }
        }));
    }
    let ran = wait_within(&mut child, budget)?;
    for r in readers {
        let _ = r.join();
    }
    Ok(ran)
}

/// Run to completion under the `amont.timeout` deadline with stdout and
/// stderr CAPTURED into a string the caller can parse — what the audit
/// checks need: their verdict lives in the tool's output, not its exit
/// code alone. Arrival-ordered merge of both streams, like
/// [`status_streamed`]'s blocks. `None` when the child cannot be spawned.
pub fn capture_within(cmd: &mut Command) -> Option<(Ran, String)> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let budget = check_timeout();
    let mut child = cmd.spawn().ok()?;
    let text = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let mut readers = Vec::new();
    for pipe in [
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let text = std::sync::Arc::clone(&text);
        readers.push(std::thread::spawn(move || {
            let mut pipe = pipe;
            let mut chunk = [0u8; 4096];
            loop {
                match std::io::Read::read(&mut pipe, &mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let piece = String::from_utf8_lossy(&chunk[..n]).into_owned();
                        text.lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .push_str(&piece);
                    }
                }
            }
        }));
    }
    let ran = wait_within(&mut child, budget).ok()?;
    for r in readers {
        let _ = r.join();
    }
    let text = std::sync::Arc::try_unwrap(text)
        .map(|m| m.into_inner().unwrap_or_else(|p| p.into_inner()))
        .unwrap_or_default();
    Some((ran, text))
}

/// The deadline loop over an already-spawned child — shared by the
/// inherited and captured runners.
pub(crate) fn wait_within(
    child: &mut std::process::Child,
    budget_secs: u64,
) -> std::io::Result<Ran> {
    if budget_secs == 0 {
        return child.wait().map(Ran::Status);
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(budget_secs);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Ran::Status(status));
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(Ran::TimedOut(budget_secs));
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

/// Say a command was killed at the deadline, and how to change the deadline.
pub fn say_timed_out(what: &str, budget_secs: u64) {
    fail(&format!(
        "{} timed out after {budget_secs}s — killed. {} raises the budget",
        hl(what),
        hl("git config amont.timeout <secs>")
    ));
}

/// [`status_within`], collapsed to "did it exit 0" — the shape the one-shot
/// tool spawns want. A timeout says so, names `what`, and reads as failure.
pub fn bounded_success(cmd: &mut Command, what: &str) -> bool {
    match status_streamed(cmd) {
        Ok(Ran::Status(s)) => s.success(),
        Ok(Ran::TimedOut(b)) => {
            say_timed_out(what, b);
            false
        }
        Err(_) => false,
    }
}

/// Run `argv` from `root`, inheriting stdio. True when it exits 0.
pub fn run(root: &str, argv: &[String], extra: &[String]) -> bool {
    let Some((program, rest)) = argv.split_first() else {
        return true;
    };
    let mut cmd = Command::new(program);
    cmd.args(rest)
        .args(extra)
        .current_dir(root)
        .stdin(Stdio::null());
    strip_git_env(&mut cmd);
    bounded_success(&mut cmd, program)
}

/// As [`run`], but with the tool's own output discarded.
///
/// For a pass whose only job is to decide something — prettier's `--check`,
/// ruff's `--fix` sweep — where the offenders are printed once, by the pass
/// that reports them, rather than twice.
pub fn run_quiet(root: &str, argv: &[String], extra: &[String]) -> bool {
    let Some((program, rest)) = argv.split_first() else {
        return true;
    };
    let mut cmd = Command::new(program);
    cmd.args(rest)
        .args(extra)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    strip_git_env(&mut cmd);
    // Deliberately NOT the streamed runner: this helper's contract is that
    // the output is discarded, and capture would resurrect it into the block.
    match status_within(&mut cmd) {
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
pub fn fixing_enabled() -> bool {
    // Never while the file set is not the index — see `NOT_THE_INDEX`.
    !not_the_index() && fixing_requested()
}

/// What the CONFIG says, ignoring whether the current run may act on it.
///
/// Split out so `run_all` can tell the difference between "fixing is off" and
/// "you asked for fixing and this mode will not do it", and say the second out
/// loud instead of silently ignoring the key.
pub fn fixing_requested() -> bool {
    crate::config::boolean_or("amont.fix", false)
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

pub fn ok(msg: &str) {
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

    /// The deadline kills what outlives it and reports what finished.
    #[cfg(unix)]
    #[test]
    fn the_deadline_kills_a_sleeper_and_spares_a_finisher() {
        let started = std::time::Instant::now();
        let mut slow = Command::new(program("sleep"));
        slow.arg("300").stdin(Stdio::null());
        match status_within_secs(&mut slow, 1) {
            Ok(Ran::TimedOut(1)) => {}
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
        let saved = std::env::var_os("PATH");
        std::env::set_var("PATH", &dir);
        let found = which("faketool").unwrap();
        if let Some(p) = saved {
            std::env::set_var("PATH", p);
        }
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
