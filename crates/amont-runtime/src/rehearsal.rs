//! Rehearse the push gate in the background, on a snapshot of `HEAD`.
//!
//! `amont run pre-push` (v1.27) moved the suite off the wire: run it first,
//! stamp the tree, and the push that follows holds the remote for seconds.
//! It is still a four-minute foreground wait that somebody has to remember
//! to start. This module starts it for them, at the moment the content
//! exists — `post-commit` — and gets out of the way.
//!
//! ## The shape
//!
//! - `post-commit` (opted in with `git config amont.rehearseOnCommit true`)
//!   spawns a detached WORKER: its own process group, stdio on a log file,
//!   no terminal. git never waits for it.
//! - The worker checks out `HEAD` into a throwaway worktree — the same
//!   [`crate::pushed_tree`] machinery `amont.testPushedTree` uses — and
//!   drives the ordinary pre-push dispatcher there, with the branch and its
//!   upstream as the ref line. The snapshot is what makes this safe to run
//!   while you keep editing: the suite reads a tree nobody is touching, and
//!   the stamp it earns is for exactly that tree.
//! - The stamp is the whole hand-off. A pre-push that finds it skips the
//!   suite; there is no second record to keep in step.
//! - Latest wins. A worker that finds another one running on a DIFFERENT
//!   tree kills it (the whole process group, suite included) and removes its
//!   snapshot; one running on the same tree is left alone. A rebase replaying
//!   ten commits does not queue ten suites.
//! - A push that arrives while the rehearsal of its tree is still running
//!   WAITS for it rather than starting the suite over: less remaining work
//!   than a fresh run, and no extra CPU. The connection stays open either
//!   way; waiting is the shorter of the two.
//!
//! ## What is written where
//!
//! `$GIT_DIR/amont-rehearsal` — the worktree-PRIVATE git dir, like the gate
//! marker: the commit that started this happened in this worktree, and the
//! push that will ask about it comes from here too. One small text file:
//! pid, commit, tree, start time, snapshot path, phase. Its log sits beside
//! it as `amont-rehearsal.log`. Nothing here is a second source of truth
//! about whether a gate passed — that is the stamp's job; this file only
//! says whether someone is still working on it, so a push can decide
//! between waiting and running.
//!
//! ## Failure direction
//!
//! Every path that goes wrong — a worker that died, a state file that will
//! not parse, a snapshot that could not be prepared — ends in the gate
//! running at push time exactly as it would have without this module. The
//! background run can only ever REMOVE work from the push; it never lets
//! the push skip work nobody did.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::hooks::common::{human_secs, warn};
use crate::ui::{highlight, valid_sign, warning_sign};

/// The opt-in: rehearse after every commit.
const ON_COMMIT: &str = "amont.rehearseOnCommit";

/// The state file, inside `$GIT_DIR`.
const STATE: &str = "amont-rehearsal";
/// The detached worker's stdout and stderr, beside it.
const LOG: &str = "amont-rehearsal.log";
/// First line of the state file — bump when the layout changes, and an
/// older binary reads the file as "no rehearsal", which re-runs the gate.
const FORMAT: &str = "amont-rehearsal-v1";

/// Set in the environment of the pre-push run a worker drives inside the
/// snapshot. The dispatcher reads it to run the content gates only — the
/// push-shaped checks (branch-protect, secrets, the auto-rebase) ask about a
/// push that is not happening — and to skip the wait below, which would
/// otherwise be a process waiting for itself.
pub const ENV: &str = "AMONT_REHEARSAL";

/// Does this repository rehearse after every commit?
pub fn on_commit_enabled() -> bool {
    crate::config::boolean_or(ON_COMMIT, false)
}

/// Is this process the pre-push run inside a rehearsal snapshot?
///
/// Read ONCE, and the variable is removed from the environment as it is
/// read: everything this process spawns — the test suite above all — must
/// not inherit it. amont's own suite pushes through a real pre-push in
/// fixture repositories, and with the variable inherited that pre-push
/// believed itself a rehearsal and skipped branch-protect, which is how the
/// first rehearsal of this very feature failed its own gate.
pub fn in_snapshot() -> bool {
    static IN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *IN.get_or_init(|| {
        let set = std::env::var_os(ENV).is_some();
        if set {
            std::env::remove_var(ENV);
        }
        set
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Running,
    Passed,
    Failed,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Phase::Running => "running",
            Phase::Passed => "passed",
            Phase::Failed => "failed",
        }
    }

    fn parse(s: &str) -> Option<Phase> {
        match s {
            "running" => Some(Phase::Running),
            "passed" => Some(Phase::Passed),
            "failed" => Some(Phase::Failed),
            _ => None,
        }
    }
}

/// One rehearsal, as the state file records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub pid: u32,
    pub commit: String,
    pub tree: String,
    pub started: u64,
    pub snapshot: PathBuf,
    pub phase: Phase,
}

impl State {
    fn render(&self) -> String {
        format!(
            "{FORMAT}\npid={}\ncommit={}\ntree={}\nstarted={}\nsnapshot={}\nphase={}\n",
            self.pid,
            self.commit,
            self.tree,
            self.started,
            self.snapshot.display(),
            self.phase.as_str(),
        )
    }

    /// `None` for anything this version did not write — including a file
    /// from a future layout — which the callers all treat as "no rehearsal".
    pub fn parse(body: &str) -> Option<State> {
        let mut lines = body.lines();
        if lines.next()? != FORMAT {
            return None;
        }
        let (mut pid, mut commit, mut tree, mut started, mut snapshot, mut phase) =
            (None, None, None, None, None, None);
        for line in lines {
            let (k, v) = line.split_once('=')?;
            match k {
                "pid" => pid = v.parse().ok(),
                "commit" => commit = Some(v.to_string()),
                "tree" => tree = Some(v.to_string()),
                "started" => started = v.parse().ok(),
                "snapshot" => snapshot = Some(PathBuf::from(v)),
                "phase" => phase = Phase::parse(v),
                _ => {}
            }
        }
        Some(State {
            pid: pid?,
            commit: commit?,
            tree: tree?,
            started: started?,
            snapshot: snapshot?,
            phase: phase?,
        })
    }

    /// Still running, and the process is really there. A `running` record
    /// whose pid is gone is a worker that was killed or crashed before it
    /// could write its verdict.
    pub fn alive(&self) -> bool {
        self.phase == Phase::Running && process_alive(self.pid)
    }

    pub fn age_secs(&self) -> u64 {
        now().saturating_sub(self.started)
    }

    fn short(&self) -> &str {
        self.commit.get(..8).unwrap_or(&self.commit)
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// `$GIT_DIR`, absolute — the worktree-private one, deliberately (see the
/// module doc).
fn git_dir() -> Option<PathBuf> {
    crate::git::stdout(&["rev-parse", "--absolute-git-dir"]).map(PathBuf::from)
}

fn state_path() -> Option<PathBuf> {
    git_dir().map(|d| d.join(STATE))
}

/// Where a detached worker writes. Reported to the user whenever the
/// outcome might send them looking.
pub fn log_path() -> Option<PathBuf> {
    git_dir().map(|d| d.join(LOG))
}

/// The recorded rehearsal for this worktree, if any.
pub fn read() -> Option<State> {
    let body = std::fs::read_to_string(state_path()?).ok()?;
    State::parse(&body)
}

fn write(state: &State) {
    if let Some(path) = state_path() {
        // Best-effort: a state file that could not be written costs, at
        // most, a push that runs the gate instead of waiting — the safe
        // direction.
        let _ = std::fs::write(path, state.render());
    }
}

fn clear() {
    if let Some(path) = state_path() {
        let _ = std::fs::remove_file(path);
    }
}

/// Is a process with this pid running? A wrong answer in the "no" direction
/// re-runs a gate; a wrong "yes" would make a push wait on nothing, so the
/// poll loops below also give up when the state stops changing.
pub fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
            .unwrap_or(false)
    }
}

/// End a worker and everything it started: the suite runs in the worker's
/// process group, which is what makes "cancel" mean the tests too rather
/// than an orphaned vitest carrying on against a worktree about to vanish.
fn kill_group(pid: u32) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-TERM", "--", &format!("-{pid}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(unix))]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Stop the recorded worker, if it is running, and take its snapshot with
/// it. A killed worker never reaches its own cleanup (`Drop` does not run
/// on a signal), so whoever kills it owns the leftovers.
fn cancel(state: &State, repo: &Path) {
    if state.alive() {
        kill_group(state.pid);
        // Give the group a moment to go, so the worktree below is not
        // removed under a test still writing into it.
        for _ in 0..20 {
            if !process_alive(state.pid) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    remove_snapshot(repo, &state.snapshot);
}

fn remove_snapshot(repo: &Path, path: &Path) {
    if path.as_os_str().is_empty() || !path.exists() {
        let _ = crate::git::succeeds_in(repo, &["worktree", "prune"]);
        return;
    }
    let _ = crate::git::succeeds_in(
        repo,
        &[
            "worktree",
            "remove",
            "--force",
            path.to_str().unwrap_or_default(),
        ],
    );
    let _ = std::fs::remove_dir_all(path);
    let _ = crate::git::succeeds_in(repo, &["worktree", "prune"]);
}

/// The environment git hands a hook — `GIT_DIR`, `GIT_INDEX_FILE` and the
/// rest — pins every git command to the hook's own repository and, for
/// `commit -a`, to a temporary index that is gone by the time a worker
/// reads it. The worker runs git in the primary tree and in a snapshot; it
/// must see neither. `GIT_CONFIG_*` stays: it says where config lives, not
/// which repository this is, and the test harness relies on it.
fn strip_repo_env(cmd: &mut Command) {
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_PREFIX",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_REFLOG_ACTION",
        "GIT_EXEC_PATH",
    ] {
        cmd.env_remove(key);
    }
}

/// Start a worker for `HEAD` in the background and return at once.
///
/// Its own process group (so it can be cancelled as a unit), stdin closed,
/// stdout and stderr on the log. The worker does all the deciding —
/// whether there is anything to rehearse, whether the tree is already
/// stamped, whether another worker has it — so the caller (a post-commit
/// hook, or `amont rehearse`) pays one `spawn` and nothing else.
pub fn spawn_detached() -> Result<u32, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate amont: {e}"))?;
    let log = log_path().ok_or("not inside a git repository")?;
    let file =
        std::fs::File::create(&log).map_err(|e| format!("cannot open {}: {e}", log.display()))?;
    let err = file
        .try_clone()
        .map_err(|e| format!("cannot open {}: {e}", log.display()))?;
    let mut cmd = Command::new(exe);
    cmd.arg("rehearse")
        .arg("--worker")
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(Stdio::from(err));
    strip_repo_env(&mut cmd);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = cmd
        .spawn()
        .map_err(|e| format!("cannot start the rehearsal: {e}"))?;
    Ok(child.id())
}

/// What a worker found, or did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// No scoped gate has work to do for what `HEAD` would push.
    NothingToDo,
    /// Every gate that would run is already stamped on this tree.
    AlreadyStamped,
    /// Another worker is on this exact tree; it was left alone.
    AlreadyRunning(u32),
    Passed,
    Failed,
}

/// THE worker. Runs in the foreground of whoever called it — the detached
/// process `spawn_detached` started, or `amont rehearse --wait` when nothing
/// was running — and returns only when the verdict is in.
pub fn worker() -> Result<Outcome, String> {
    let root = crate::hooks::common::repo_root_checked()?;
    let repo = Path::new(&root);
    let head = crate::git::stdout(&["rev-parse", "HEAD"]).ok_or("could not resolve HEAD")?;
    let tree = crate::git::stdout(&["rev-parse", "HEAD^{tree}"])
        .ok_or("could not resolve the tree of HEAD")?;
    // The branch and its upstream, read HERE in the primary worktree: the
    // snapshot is a detached HEAD and could not answer.
    let push_ref = crate::pushrefs::synthetic_from_upstream()?;
    let manifest = crate::manifest::load(repo);
    crate::policy::install(manifest.policy.clone());
    let changed = crate::pushrefs::changed_files(std::slice::from_ref(&push_ref));
    let gates = crate::dispatch::scoped_push_gates(&manifest, &changed);
    let short = head.get(..8).unwrap_or(&head);
    if gates.is_empty() {
        println!("nothing to rehearse: no test gate has work to do for what {short} would push");
        return Ok(Outcome::NothingToDo);
    }
    let stamped = crate::gate_stamp::stamps_for(std::slice::from_ref(&head));
    let vouched = |g: &String| stamped.get(&head).is_some_and(|s| s.contains(g));
    if gates.iter().all(vouched) {
        println!(
            "{} {} already stamped on this tree — nothing to rehearse",
            valid_sign(),
            highlight(&gates.join(" "))
        );
        return Ok(Outcome::AlreadyStamped);
    }
    if let Some(prev) = read() {
        if prev.alive() {
            if prev.tree == tree {
                println!(
                    "a rehearsal of this tree is already running (pid {}, started {} ago)",
                    prev.pid,
                    human_secs(prev.age_secs())
                );
                return Ok(Outcome::AlreadyRunning(prev.pid));
            }
            println!(
                "cancelling the rehearsal of {} (pid {}) — {} is the tree that matters now",
                prev.short(),
                prev.pid,
                short
            );
            cancel(&prev, repo);
        } else if prev.phase == Phase::Running {
            // Died without a verdict. Its snapshot is nobody's now.
            remove_snapshot(repo, &prev.snapshot);
        }
    }
    let snapshot = crate::pushed_tree::PushedTree::create(repo, &head)
        .ok_or("could not check out HEAD into a snapshot worktree")?;
    let me = State {
        pid: std::process::id(),
        commit: head.clone(),
        tree,
        started: now(),
        snapshot: snapshot.path().to_path_buf(),
        phase: Phase::Running,
    };
    write(&me);
    println!(
        "rehearsing {} for {} in {}",
        highlight(&gates.join(" ")),
        short,
        snapshot.path().display()
    );
    let hooks_dir = crate::git::stdout(&["rev-parse", "--git-path", "hooks"])
        .map(PathBuf::from)
        .map(|p| if p.is_absolute() { p } else { repo.join(p) })
        .ok_or("could not locate the hooks directory")?;
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate amont: {e}"))?;
    let mut cmd = Command::new(exe);
    cmd.arg("--hooks-dir")
        .arg(&hooks_dir)
        .arg("pre-push")
        .current_dir(snapshot.path())
        .env(ENV, "1")
        .stdin(Stdio::piped());
    strip_repo_env(&mut cmd);
    let line = format!(
        "{} {} {} {}\n",
        push_ref.local_ref, push_ref.local_oid, push_ref.remote_ref, push_ref.remote_oid
    );
    let passed = (|| -> std::io::Result<bool> {
        let mut child = cmd.spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(line.as_bytes())?;
        }
        Ok(child.wait()?.success())
    })()
    .map_err(|e| format!("could not run the push gate in the snapshot: {e}"))?;
    let done = State {
        phase: if passed { Phase::Passed } else { Phase::Failed },
        ..me
    };
    write(&done);
    drop(snapshot);
    if passed {
        println!("{} rehearsal of {short} passed", valid_sign());
        Ok(Outcome::Passed)
    } else {
        println!("{} rehearsal of {short} FAILED", warning_sign());
        Ok(Outcome::Failed)
    }
}

/// Where the live pre-push meets the background: if a worker is on the tree
/// of one of these tips right now, wait for it. Returns the phase it ended
/// in, or `None` when there was nothing to wait for — in which case the
/// caller runs the gate as usual.
///
/// A rehearsal that already FAILED is reported, not honoured: the gate runs
/// again here, which is what shows the developer the failure in the
/// terminal they are looking at.
pub fn await_for(tips: &[String]) -> Option<Phase> {
    if in_snapshot() {
        return None;
    }
    let state = read()?;
    let on_a_tip = tips.iter().any(|t| {
        let spec = format!("{t}^{{tree}}");
        crate::git::stdout(&["rev-parse", &spec]).as_deref() == Some(state.tree.as_str())
    });
    if !on_a_tip {
        return None;
    }
    let log = log_path()
        .map(|p| format!(" (log: {})", p.display()))
        .unwrap_or_default();
    match state.phase {
        Phase::Passed => None, // the stamps say it all
        Phase::Failed => {
            crate::say!(
                "{} the background rehearsal of this tree failed {} ago{log} — running the gate here",
                warning_sign(),
                human_secs(state.age_secs()),
            );
            None
        }
        Phase::Running if !process_alive(state.pid) => {
            crate::say!(
                "{} a background rehearsal of this tree died without a verdict{log} — running the gate here",
                warning_sign(),
            );
            None
        }
        Phase::Running => {
            crate::say!(
                "{} a background rehearsal of this tree started {} ago is still running — \
                 waiting for it rather than starting the suite over{log}",
                warning_sign(),
                human_secs(state.age_secs()),
            );
            follow(&state).map(|(phase, _)| phase)
        }
    }
}

/// Poll a running rehearsal to its end. `None` when it vanished — a new
/// worker took over for a different tree, or the process died.
fn follow(state: &State) -> Option<(Phase, State)> {
    let mut last_word = std::time::Instant::now();
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let now_state = read()?;
        if now_state.tree != state.tree || now_state.started != state.started {
            return None;
        }
        match now_state.phase {
            Phase::Running => {
                if !process_alive(now_state.pid) {
                    warn("the rehearsal died without a verdict");
                    return None;
                }
                if last_word.elapsed() >= Duration::from_secs(60) {
                    eprintln!(
                        "  still rehearsing ({} so far)",
                        human_secs(now_state.age_secs())
                    );
                    last_word = std::time::Instant::now();
                }
            }
            phase => {
                let verb = if phase == Phase::Passed {
                    format!("{} rehearsal passed", valid_sign())
                } else {
                    format!("{} rehearsal failed", warning_sign())
                };
                crate::say!("{verb} after {}", human_secs(now_state.age_secs()));
                return Some((phase, now_state));
            }
        }
    }
}

/// `amont rehearse [--wait|--status|--stop|--worker]`.
pub fn command(args: &[OsString]) -> i32 {
    let flag = |f: &str| args.iter().any(|a| a == f);
    if flag("--worker") {
        return match worker() {
            Ok(Outcome::Failed) => 1,
            Ok(_) => 0,
            Err(e) => {
                eprintln!("amont: {e}");
                2
            }
        };
    }
    let root = match crate::hooks::common::repo_root_checked() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("amont: {e}");
            return 2;
        }
    };
    if flag("--status") {
        return status();
    }
    if flag("--stop") {
        return match read() {
            Some(s) if s.alive() => {
                cancel(&s, Path::new(&root));
                clear();
                println!("stopped the rehearsal of {} (pid {})", s.short(), s.pid);
                0
            }
            _ => {
                println!("no rehearsal is running in this worktree");
                0
            }
        };
    }
    let head = crate::git::stdout(&["rev-parse", "HEAD^{tree}"]);
    let running_here = read().filter(|s| s.alive() && Some(s.tree.as_str()) == head.as_deref());
    if flag("--wait") {
        return match running_here {
            Some(state) => {
                println!(
                    "following the rehearsal of {} (pid {}, started {} ago)",
                    state.short(),
                    state.pid,
                    human_secs(state.age_secs())
                );
                match follow(&state) {
                    Some((Phase::Passed, _)) => 0,
                    Some(_) => {
                        show_log_tail();
                        1
                    }
                    None => {
                        eprintln!("amont: the rehearsal ended without a verdict");
                        2
                    }
                }
            }
            // Nothing running: do it here, in the foreground, and say so.
            None => match worker() {
                Ok(Outcome::Failed) => 1,
                Ok(Outcome::AlreadyRunning(_)) => command(&[OsString::from("--wait")]),
                Ok(_) => 0,
                Err(e) => {
                    eprintln!("amont: {e}");
                    2
                }
            },
        };
    }
    if let Some(state) = running_here {
        println!(
            "a rehearsal of this tree is already running (pid {}, started {} ago) — \
             `amont rehearse --wait` follows it",
            state.pid,
            human_secs(state.age_secs())
        );
        return 0;
    }
    match spawn_detached() {
        Ok(pid) => {
            println!(
                "rehearsing the push gate for HEAD in the background (pid {pid}) — \
                 `amont rehearse --wait` follows it, `--status` reports{}",
                log_path()
                    .map(|p| format!("; log: {}", p.display()))
                    .unwrap_or_default()
            );
            0
        }
        Err(e) => {
            eprintln!("amont: {e}");
            2
        }
    }
}

fn status() -> i32 {
    let Some(state) = read() else {
        println!("no rehearsal recorded in this worktree");
        return 0;
    };
    let head = crate::git::stdout(&["rev-parse", "HEAD^{tree}"]);
    let which = if head.as_deref() == Some(state.tree.as_str()) {
        "HEAD's tree".to_string()
    } else {
        format!("{} (not HEAD's tree)", state.short())
    };
    let age = human_secs(state.age_secs());
    let line = match state.phase {
        Phase::Running if state.alive() => {
            format!(
                "rehearsal of {which}: running for {age} (pid {})",
                state.pid
            )
        }
        Phase::Running => {
            format!("rehearsal of {which}: died without a verdict, started {age} ago")
        }
        Phase::Passed => format!("rehearsal of {which}: passed, started {age} ago"),
        Phase::Failed => format!("rehearsal of {which}: FAILED, started {age} ago"),
    };
    println!("{line}");
    if let Some(log) = log_path().filter(|p| p.exists()) {
        println!("log: {}", log.display());
    }
    0
}

/// The last lines of the detached log — the failure a `--wait` just
/// reported happened in another process, and this is where it went.
fn show_log_tail() {
    let Some(log) = log_path() else { return };
    let Ok(body) = std::fs::read_to_string(&log) else {
        return;
    };
    let lines: Vec<&str> = body.lines().collect();
    let start = lines.len().saturating_sub(40);
    eprintln!(
        "--- {} (last {} lines) ---",
        log.display(),
        lines.len() - start
    );
    for l in &lines[start..] {
        eprintln!("{l}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> State {
        State {
            pid: 4242,
            commit: "a".repeat(40),
            tree: "b".repeat(40),
            started: 1_700_000_000,
            snapshot: PathBuf::from("/tmp/amont-push-1-2"),
            phase: Phase::Running,
        }
    }

    #[test]
    fn a_state_survives_the_round_trip() {
        let s = sample();
        assert_eq!(State::parse(&s.render()), Some(s));
        let done = State {
            phase: Phase::Failed,
            ..sample()
        };
        assert_eq!(State::parse(&done.render()), Some(done));
    }

    /// The failure direction: anything this version did not write reads as
    /// "no rehearsal", which runs the gate.
    #[test]
    fn a_foreign_or_partial_file_is_no_rehearsal() {
        assert_eq!(State::parse(""), None);
        assert_eq!(State::parse("amont-rehearsal-v2\npid=1\n"), None);
        assert_eq!(State::parse("amont-rehearsal-v1\npid=1\n"), None);
        assert_eq!(
            State::parse("amont-rehearsal-v1\npid=x\ncommit=a\ntree=b\nstarted=1\nsnapshot=/s\nphase=running\n"),
            None
        );
    }

    /// A pid this box never had is not alive — and a record naming it is a
    /// dead worker, not a running one.
    #[test]
    fn a_dead_pid_is_not_alive() {
        let s = State {
            pid: u32::MAX - 7,
            ..sample()
        };
        assert!(!s.alive());
        let done = State {
            phase: Phase::Passed,
            pid: std::process::id(),
            ..sample()
        };
        assert!(
            !done.alive(),
            "a finished rehearsal is not running whoever's pid it names"
        );
    }
}
