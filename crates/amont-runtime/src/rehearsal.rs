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

/// How long a PUSH may wait for a running rehearsal before giving up and
/// running the gate itself. `0` means no limit.
///
/// [`await_for`] runs inside `pre-push`, which is to say after git has opened
/// its connection to the remote and while that connection sits idle — the
/// exact window this whole feature exists to shorten. Waiting was bounded
/// only by the worker's own deadline, which is `amont.timeout`: an hour by
/// default, and unbounded where somebody has set it to `0`. So the mechanism
/// built to stop a four-minute suite killing a push could hold the same
/// connection open for far longer than the suite would have.
///
/// Five minutes because that is under every idle timeout we have measured —
/// Forgejo's git timeout is six — so a wait that expires still leaves the
/// connection alive for the gate that follows it. `amont rehearse --wait`
/// has no budget: nothing is connected there, and a person who typed it is
/// asking to be told when the suite ends.
const WAIT_BUDGET: &str = "amont.rehearsalWait";

pub fn wait_budget(settings: &crate::config::Settings) -> Option<Duration> {
    match crate::config::integer_or(settings, WAIT_BUDGET, 300, 0..=86_400) {
        0 => None,
        secs => Some(Duration::from_secs(secs as u64)),
    }
}

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
pub fn on_commit_enabled(settings: &crate::config::Settings) -> bool {
    crate::config::boolean_or(settings, ON_COMMIT, false)
}

/// What a push does about a gate-paired check when a pushed commit carries
/// no record of it — rewritten by a rebase or amend, committed with
/// `--no-verify`, made where amont is not installed:
///
/// - `run` (the default): run the suite in the push, as always;
/// - `refuse`: block the push and say how to get the stamp instead
///   (`amont rehearse --wait`). The suite then never runs while git holds
///   its connection to the remote open — the one place a slow suite can
///   kill a push whose checks all passed. A rehearsal still RUNNING for the
///   tip is waited for first (`amont.rehearsalWait`), and its stamp is
///   honoured; a rehearsal snapshot itself always runs, since that is where
///   the stamp is earned.
const UNSTAMPED_PUSH: &str = "amont.unstampedPush";

/// Does this repository refuse a push the gate did not vouch for?
pub fn refuse_unstamped(settings: &crate::config::Settings) -> bool {
    crate::config::enumerated_or(settings, UNSTAMPED_PUSH, &["run", "refuse"], "run") == "refuse"
}

/// Is this process the pre-push run inside a rehearsal snapshot?
///
/// Read ONCE, and the variable is removed from the environment as it is
/// read: everything this process spawns — the test suite above all — must
/// not inherit it. amont's own suite pushes through a real pre-push in
/// fixture repositories, and with the variable inherited that pre-push
/// believed itself a rehearsal and skipped branch-protect, which is how the
/// first rehearsal of this very feature failed its own gate.
///
/// **`main` primes this before anything else runs, and must keep doing so.**
/// `remove_var` is not thread-safe — that is why Rust 2024 made it `unsafe`
/// — and this is reachable from `pushed_tree::where_to_run`, i.e. from
/// inside a check, and `dispatch` runs checks concurrently. Settling the
/// `OnceLock` on the main thread means the racy call has already happened by
/// the time any thread exists. Do not remove that call, and do not let a
/// caller reach here first.
pub fn in_snapshot() -> bool {
    snapshot_env().is_some()
}

/// The prefix of the variable's value that names the gates the rehearsal gave
/// to the cluster (ADR-0017): `detached=<id>,<id>`. A bare `1` names none.
const DETACHED_PREFIX: &str = "detached=";

/// What the variable said, parsed where it is settled: the gates the
/// rehearsal that made this snapshot handed to the cluster.
struct SnapshotEnv {
    detached: Vec<String>,
    /// The rehearsal was started by land's pre-stage: someone is waiting on
    /// it, so a warn gate's slot wait has a ceiling (ADR-0013).
    prestage: bool,
}

/// The leading marker of the variable's value for land's pre-stage:
/// `prestage;1` or `prestage;detached=a,b`. An older amont reads the whole
/// value as no detached gates.
const PRESTAGE_PREFIX: &str = "prestage;";

/// What the variable's value says: whether land's pre-stage made the
/// snapshot, and the gates it handed to the cluster.
fn parse_snapshot_env(raw: &str) -> SnapshotEnv {
    let (prestage, rest) = match raw.strip_prefix(PRESTAGE_PREFIX) {
        Some(rest) => (true, rest),
        None => (false, raw),
    };
    let detached = rest
        .strip_prefix(DETACHED_PREFIX)
        .map(|ids| ids.split(',').map(str::to_string).collect())
        .unwrap_or_default();
    SnapshotEnv { detached, prestage }
}

/// The variable, settled and parsed once for the same reason [`in_snapshot`]
/// is.
fn snapshot_env() -> &'static Option<SnapshotEnv> {
    static IN: std::sync::OnceLock<Option<SnapshotEnv>> = std::sync::OnceLock::new();
    IN.get_or_init(|| {
        let raw = std::env::var_os(ENV).map(|v| v.to_string_lossy().into_owned())?;
        std::env::remove_var(ENV);
        Some(parse_snapshot_env(&raw))
    })
}

/// Is this snapshot the one land's pre-stage made? Settled with
/// [`in_snapshot`], never read from the variable.
pub fn in_prestage() -> bool {
    snapshot_env().as_ref().is_some_and(|env| env.prestage)
}

/// The gates the rehearsal that started this snapshot handed to the cluster:
/// this run does not repeat them, their passes come back as cluster tokens at
/// the next commit or push. Empty outside a snapshot. Read by `main`, which
/// carries it on `Ctx` to the pre-push loop.
pub fn handed_to_cluster() -> &'static [String] {
    snapshot_env()
        .as_ref()
        .map_or(&[], |env| env.detached.as_slice())
}

/// The variable `amont land` sets on its own `git push` (ADR-0018):
/// `<sha> <remote tip>`, what its pre-stage ran the push gates against. Read
/// once and removed, for the reason [`in_snapshot`] gives: a gate's own
/// fixture pushes must not inherit it. `main` primes it on the main thread.
pub const LAND_ENV: &str = "AMONT_LAND";

/// What land's pre-stage saw: the sha it ran the gates on, and the remote tip
/// it ran them against (all zeros for a ref the remote lacks).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandLine {
    pub sha: String,
    pub remote_tip: String,
}

/// [`LAND_ENV`], parsed where it is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LandEnv {
    /// Not land's push.
    Absent,
    Valid(LandLine),
    /// Set, but not two object names: the hook refuses rather than guess.
    Malformed(String),
}

/// A full object name: 40 (sha-1) or 64 (sha-256) hex digits.
fn is_oid(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

impl LandEnv {
    pub fn parse(raw: &str) -> LandEnv {
        let fields: Vec<&str> = raw.split_whitespace().collect();
        match fields.as_slice() {
            [sha, remote_tip] if is_oid(sha) && is_oid(remote_tip) => LandEnv::Valid(LandLine {
                sha: sha.to_string(),
                remote_tip: remote_tip.to_string(),
            }),
            _ => LandEnv::Malformed(raw.to_string()),
        }
    }
}

/// The land pre-stage's line, when this process is the hook of the push
/// `amont land` makes. `main` reads it and carries it on `Ctx`.
pub fn land_env() -> &'static LandEnv {
    static IN: std::sync::OnceLock<LandEnv> = std::sync::OnceLock::new();
    IN.get_or_init(|| match std::env::var_os(LAND_ENV) {
        None => LandEnv::Absent,
        Some(raw) => {
            std::env::remove_var(LAND_ENV);
            LandEnv::parse(&raw.to_string_lossy())
        }
    })
}

/// Set by `amont land` on the `amont rehearse --wait` it runs as its
/// pre-stage (ADR-0018): the real ref line of the push it is about to make,
/// `<local ref> <sha> <remote ref> <remote tip>`, so the rehearsal selects
/// exactly the gates that push will owe — not the ones a possibly stale
/// tracking ref suggests. Also means "never cancel another tree's rehearsal:
/// wait for it". Read once and removed, like [`ENV`].
pub const PRESTAGE_ENV: &str = "AMONT_LAND_PRESTAGE";

/// [`PRESTAGE_ENV`], parsed where it is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prestage {
    /// A plain rehearsal of HEAD.
    Absent,
    /// Land's pre-stage, of this push line.
    Of(crate::pushrefs::PushRef),
    /// Set, but not a push line: `rehearse` refuses rather than fall back to
    /// an ordinary rehearsal that might cancel somebody else's.
    Malformed(String),
}

impl Prestage {
    pub fn parse(raw: &str) -> Prestage {
        let fields: Vec<&str> = raw.split_whitespace().collect();
        match fields.as_slice() {
            [local_ref, local_oid, remote_ref, remote_oid]
                if is_oid(local_oid) && is_oid(remote_oid) =>
            {
                Prestage::Of(crate::pushrefs::PushRef {
                    local_ref: local_ref.to_string(),
                    local_oid: local_oid.to_string(),
                    remote_ref: remote_ref.to_string(),
                    remote_oid: remote_oid.to_string(),
                })
            }
            _ => Prestage::Malformed(raw.to_string()),
        }
    }

    fn line(&self) -> Option<&crate::pushrefs::PushRef> {
        match self {
            Prestage::Of(line) => Some(line),
            Prestage::Absent | Prestage::Malformed(_) => None,
        }
    }
}

/// The working tree this rehearsal snapshot was taken from, when this process
/// is the snapshot's pre-push (`AMONT_SOURCE_WORKTREE`). Read once and removed
/// like [`ENV`]; `main` primes it and carries it on `Ctx`.
pub fn source_worktree() -> Option<&'static std::path::Path> {
    static IN: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    IN.get_or_init(|| {
        let raw = std::env::var_os(crate::pushed_tree::SOURCE_WORKTREE_ENV)?;
        std::env::remove_var(crate::pushed_tree::SOURCE_WORKTREE_ENV);
        Some(std::path::PathBuf::from(raw))
    })
    .as_deref()
}

/// This process's pre-stage, settled once. Primed by `main`; read by
/// `rehearse`, which passes it down.
pub fn prestage() -> &'static Prestage {
    static IN: std::sync::OnceLock<Prestage> = std::sync::OnceLock::new();
    IN.get_or_init(|| match std::env::var_os(PRESTAGE_ENV) {
        None => Prestage::Absent,
        Some(raw) => {
            std::env::remove_var(PRESTAGE_ENV);
            Prestage::parse(&raw.to_string_lossy())
        }
    })
}

/// What a push does about a heavy gate (a scoped one its range touches) with
/// no stamp or vouch for the pushed tree (ADR-0018):
///
/// - `refuse` (the default): refuse at once, before anything runs on the
///   connection git already opened, naming `amont land`;
/// - `inline`: run it in the push, as amont did before 2.0.
const PUSH_GATES: &str = "amont.pushGates";

/// Does a push refuse an unstamped heavy gate instead of running it?
pub fn refuse_push_gates(settings: &crate::config::Settings) -> bool {
    crate::config::enumerated_or(settings, PUSH_GATES, &["refuse", "inline"], "refuse") == "refuse"
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
    /// What a running worker is doing — `preparing` the snapshot or
    /// `testing` it. A key rather than a phase value: an older amont reading
    /// this file ignores a key it does not know, where an unknown phase would
    /// read as no rehearsal at all.
    pub step: Option<String>,
    /// Why a rehearsal failed without the gate ever running.
    pub reason: Option<String>,
    /// Land's pre-stage (ADR-0018): a rehearsal that meets it waits for it
    /// instead of cancelling it, since land is about to push what it proves.
    pub prestage: bool,
    /// The signal that stopped its push gate, when one did: no verdict, so a
    /// follower reports it interrupted, not failed.
    pub signal: Option<i32>,
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
        ) + &[("step", &self.step), ("reason", &self.reason)]
            .iter()
            .filter_map(|(k, v)| v.as_ref().map(|v| format!("{k}={}\n", one_line(v))))
            .collect::<String>()
            + if self.prestage { "kind=prestage\n" } else { "" }
            + &self
                .signal
                .map(|n| format!("signal={n}\n"))
                .unwrap_or_default()
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
        let (mut step, mut reason, mut prestage, mut signal) = (None, None, false, None);
        for line in lines {
            let (k, v) = line.split_once('=')?;
            match k {
                "pid" => pid = v.parse().ok(),
                "commit" => commit = Some(v.to_string()),
                "tree" => tree = Some(v.to_string()),
                "started" => started = v.parse().ok(),
                "snapshot" => snapshot = Some(PathBuf::from(v)),
                "phase" => phase = Phase::parse(v),
                "step" => step = Some(v.to_string()),
                "reason" => reason = Some(v.to_string()),
                "kind" => prestage = v == "prestage",
                "signal" => signal = v.parse().ok(),
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
            step,
            reason,
            prestage,
            signal,
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

/// A value the one-line-per-key state file can hold.
fn one_line(v: &str) -> String {
    v.replace(['\n', '\r'], " ")
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
        // the index hints of a commit this process may be inside (see
        // `hooks::common::strip_git_env`)
        crate::hooks::common::INDEX_ENV,
        crate::hooks::common::INDEX_GIT_DIR_ENV,
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
///
/// Unix only. On Windows a child inherits every inheritable handle of its
/// parent — Rust's `Command` cannot restrict that today — so a worker
/// started from a hook whose output is piped (an agent driving `git commit`)
/// would hold the pipe until the suite ended, and the "background" run
/// would block the very command it was meant to free. Said plainly instead
/// of spawned: `amont rehearse --wait` runs the same thing in the foreground.
#[cfg(not(unix))]
pub fn spawn_detached() -> Result<u32, String> {
    Err("background rehearsal is not available on Windows yet — \
         `amont rehearse --wait` runs it in the foreground"
        .to_string())
}

#[cfg(unix)]
pub fn spawn_detached() -> Result<u32, String> {
    let log = log_path().ok_or("not inside a git repository")?;
    spawn_amont(&["rehearse", "--worker"], &log)
}

/// Start `amont <args…>` in the background: its own process group, stdin
/// closed, stdout and stderr on `log` (truncated). Every detached worker —
/// the rehearsal, the tree-gate warm-up — goes through here, each with its
/// OWN log, so one never truncates another's.
#[cfg(unix)]
pub fn spawn_amont(args: &[&str], log: &std::path::Path) -> Result<u32, String> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate amont: {e}"))?;
    let file =
        std::fs::File::create(log).map_err(|e| format!("cannot open {}: {e}", log.display()))?;
    let err = file
        .try_clone()
        .map_err(|e| format!("cannot open {}: {e}", log.display()))?;
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(Stdio::from(err));
    strip_repo_env(&mut cmd);
    cmd.process_group(0);
    let child = cmd
        .spawn()
        .map_err(|e| format!("cannot start amont {}: {e}", args.join(" ")))?;
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
    /// Every gate left to run went to the cluster (ADR-0017); the next commit
    /// or push collects the result.
    Detached,
    Passed,
    Failed,
    /// The snapshot's push gate was stopped by this signal: no verdict.
    Interrupted(i32),
}

/// The signal that ended a child, if one did.
pub(crate) fn signal_of(status: &std::process::ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    {
        std::os::unix::process::ExitStatusExt::signal(status)
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}

/// Is the rehearsal `state` recorded still the one running now?
fn still_running(state: &State) -> bool {
    read().is_some_and(|now| now.alive() && now.pid == state.pid && now.started == state.started)
}

/// How long a rehearsal waits behind land's pre-stage, or land's pre-stage
/// behind another rehearsal, before giving up (ADR-0018). Nothing holds a
/// connection open meanwhile; the bound is for a worker that hangs.
const PRESTAGE_WAIT: Duration = Duration::from_secs(3 * 60 * 60);

/// What a rehearsal of `HEAD` has to decide with.
struct Plan {
    root: String,
    head: String,
    tree: String,
    push_ref: crate::pushrefs::PushRef,
    manifest: crate::manifest::Manifest,
    settings: crate::config::Settings,
    /// The scoped push gates with work to do.
    gates: Vec<String>,
    /// Tree gates this tree has not proven yet (ADR-0024).
    tree_todo: Vec<crate::manifest::TreeGate>,
    /// The audit reads this plan made, asked again by `vouched`.
    audits: crate::hooks::audit::Ledger,
}

fn plan(prestage: Option<&crate::pushrefs::PushRef>) -> Result<Plan, String> {
    let root = crate::hooks::common::repo_root_checked()?;
    let repo = Path::new(&root);
    // Land's pre-stage names the commit it will push (a tag's, for
    // `land --tag`); a plain rehearsal tests HEAD.
    let head = match prestage {
        Some(line) => crate::git::stdout(&["rev-parse", &format!("{}^{{commit}}", line.local_oid)])
            .ok_or("could not resolve the commit to pre-stage")?,
        None => crate::git::stdout(&["rev-parse", "HEAD"]).ok_or("could not resolve HEAD")?,
    };
    let tree = crate::git::stdout(&["rev-parse", &format!("{head}^{{tree}}")])
        .ok_or("could not resolve the tree to rehearse")?;
    // The branch and its upstream, read HERE in the primary worktree: the
    // snapshot is a detached HEAD and could not answer.
    let push_ref = match prestage {
        Some(line) => line.clone(),
        None => crate::pushrefs::synthetic_from_upstream()?,
    };
    let manifest = crate::manifest::load(repo);
    let settings = crate::config::Settings::new(manifest.policy.clone());
    let changed = crate::pushrefs::changed_files(std::slice::from_ref(&push_ref));
    let mut gates = crate::dispatch::scoped_push_gates(&settings, &manifest, &changed);
    // Audits are owed whatever the range touches, until a fresh record
    // answers for them (ADR-0018).
    let audits = crate::hooks::audit::Ledger::default();
    for audit in crate::dispatch::owed_audits(
        &settings,
        &manifest,
        &audits,
        std::slice::from_ref(&push_ref),
    ) {
        if !gates.contains(&audit) {
            gates.push(audit);
        }
    }
    // Tree gates this tree has not proven yet (ADR-0024). Resolved BEFORE
    // the nothing-to-do exit: a push whose only gates are lint must still be
    // rehearsed, or a rebased branch could never be attested.
    let proven_here = crate::gate_stamp::tree_tokens(&tree);
    let tree_todo: Vec<crate::manifest::TreeGate> = if crate::tree_lint::enabled(&settings) {
        manifest
            .tree
            .iter()
            .filter(|g| !proven_here.contains(&g.stamp_key()))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    Ok(Plan {
        root,
        head,
        tree,
        push_ref,
        manifest,
        settings,
        gates,
        tree_todo,
        audits,
    })
}

impl Plan {
    /// Is `gate` already vouched for on HEAD: by a local token, or by a
    /// cluster token (ADR-0016, ADR-0017)?
    fn vouched(&self, gate: &str) -> bool {
        if crate::hooks::audit::is_audit(gate) {
            return !crate::hooks::audit::owed(
                &self.settings,
                &self.audits,
                gate,
                std::slice::from_ref(&self.push_ref),
            );
        }
        vouched_by(&self.settings, &self.manifest, &self.head, gate)
    }

    /// Hand the gates that are not vouched yet to the cluster, as far as it
    /// may take them. The gates now on the cluster.
    fn detach(&self, ask: crate::cluster::Ask) -> Vec<String> {
        let unstamped: Vec<String> = self
            .gates
            .iter()
            .filter(|gate| !self.vouched(gate))
            .cloned()
            .collect();
        crate::cluster::detach(&self.settings, &self.manifest, &unstamped, ask).gates
    }
}

/// Who asked for a rehearsal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// A push's own pre-push.
    Push,
    /// Land's pre-stage.
    Prestage,
}

impl Origin {
    /// Land's pre-stage hands the rehearsal its push line; any other
    /// rehearsal has none.
    fn of(prestage: Option<&crate::pushrefs::PushRef>) -> Origin {
        match prestage {
            Some(_) => Origin::Prestage,
            None => Origin::Push,
        }
    }
}

/// The value of [`ENV`] for a run that gave `on_cluster` to the cluster.
/// Land's pre-stage marks it with a leading `prestage;`.
fn snapshot_env_value(on_cluster: &[String], origin: Origin) -> String {
    let marker = match origin {
        Origin::Push => "",
        Origin::Prestage => PRESTAGE_PREFIX,
    };
    if on_cluster.is_empty() {
        format!("{marker}1")
    } else {
        format!("{marker}{DETACHED_PREFIX}{}", on_cluster.join(","))
    }
}

/// [`Plan::vouched`] for a plan taken apart.
fn vouched_by(
    settings: &crate::config::Settings,
    manifest: &crate::manifest::Manifest,
    head: &str,
    gate: &str,
) -> bool {
    let tips = [head.to_string()];
    let stamped = crate::gate_stamp::stamps_for(&tips);
    let local = crate::gate_identity::token(&manifest.externals, gate)
        .is_some_and(|t| stamped.get(head).is_some_and(|s| s.contains(&t)));
    local
        || manifest
            .externals
            .iter()
            .find(|e| e.id == gate)
            .is_some_and(|e| crate::cluster::vouched_on_every(settings, e, &tips))
}

/// THE worker. Runs in the foreground of whoever called it — the detached
/// process `spawn_detached` started, or `amont rehearse --wait` when nothing
/// was running — and returns only when the verdict is in.
pub fn worker(
    ask: crate::cluster::Ask,
    prestage: Option<&crate::pushrefs::PushRef>,
) -> Result<Outcome, String> {
    let planned = plan(prestage)?;
    let repo = Path::new(&planned.root);
    let (head, tree) = (planned.head.clone(), planned.tree.clone());
    let short = head.get(..8).unwrap_or(&head);
    if planned.gates.is_empty() && planned.tree_todo.is_empty() {
        println!("nothing to rehearse: no test gate has work to do for what {short} would push");
        return Ok(Outcome::NothingToDo);
    }
    if planned.gates.iter().all(|gate| planned.vouched(gate)) && planned.tree_todo.is_empty() {
        println!(
            "{} {} already stamped on this tree — nothing to rehearse",
            valid_sign(),
            highlight(&planned.gates.join(" "))
        );
        return Ok(Outcome::AlreadyStamped);
    }
    // Another tree's rehearsal is cancelled — the newer tree is what matters —
    // except where land is involved: land's pre-stage never cancels anyone,
    // and is never cancelled; whoever meets it waits for it, within a bound
    // (ADR-0018). Then the state is read again, since another worker may
    // have registered meanwhile.
    let mut settled = false;
    for _ in 0..3 {
        let Some(prev) = read() else {
            settled = true;
            break;
        };
        if !prev.alive() {
            if prev.phase == Phase::Running {
                // Died without a verdict. Its snapshot is nobody's now.
                remove_snapshot(repo, &prev.snapshot);
            }
            settled = true;
            break;
        }
        if prev.tree == tree {
            println!(
                "a rehearsal of this tree is already running (pid {}, started {} ago)",
                prev.pid,
                human_secs(prev.age_secs())
            );
            return Ok(Outcome::AlreadyRunning(prev.pid));
        }
        if prestage.is_some() || prev.prestage {
            eprintln!(
                "waiting for the rehearsal of {} (pid {}) before rehearsing {short}",
                prev.short(),
                prev.pid,
            );
            if follow(&prev, Some(PRESTAGE_WAIT)).is_none() && still_running(&prev) {
                return Err(format!(
                    "the rehearsal of {} (pid {}) is still running after {}; \
                     not starting another beside it",
                    prev.short(),
                    prev.pid,
                    human_secs(PRESTAGE_WAIT.as_secs())
                ));
            }
            continue;
        }
        eprintln!(
            "cancelling the rehearsal of {} (pid {}) — {} is the tree that matters now",
            prev.short(),
            prev.pid,
            short
        );
        cancel(&prev, repo);
        settled = true;
        break;
    }
    if !settled {
        return Err("other rehearsals kept starting in this worktree; try again".to_string());
    }
    // Gates the cluster may take go there (ADR-0017); the snapshot below runs
    // the rest, and its pre-push is told which gates not to repeat.
    let on_cluster = planned.detach(ask);
    let Plan {
        push_ref,
        manifest,
        settings,
        gates,
        tree_todo,
        ..
    } = planned;
    let local_work = gates
        .iter()
        .any(|gate| !on_cluster.contains(gate) && !vouched_by(&settings, &manifest, &head, gate));
    if !local_work && tree_todo.is_empty() {
        eprintln!(
            "{} every gate left to rehearse went to the cluster ({}) — the next commit or push collects the result",
            valid_sign(),
            highlight(&on_cluster.join(" "))
        );
        return Ok(Outcome::Detached);
    }
    let snapshot = crate::pushed_tree::PushedTree::checkout(repo, &head)
        .ok_or("could not check out HEAD into a snapshot worktree")?;
    // Registered BEFORE preparing: an install can take minutes, and in that
    // time `--status`, a push looking for this tree, and a newer commit that
    // must cancel it all need to know it exists.
    let mut me = State {
        pid: std::process::id(),
        commit: head.clone(),
        tree,
        started: now(),
        snapshot: snapshot.path().to_path_buf(),
        phase: Phase::Running,
        step: Some("preparing".to_string()),
        reason: None,
        prestage: prestage.is_some(),
        signal: None,
    };
    // Checking out took time: a worker that registered since is not
    // overwritten when land is on either side (ADR-0018).
    if let Some(other) = read().filter(|s| s.alive() && s.pid != me.pid) {
        if prestage.is_some() || other.prestage {
            drop(snapshot);
            return Err(format!(
                "the rehearsal of {} (pid {}) started meanwhile; try again",
                other.short(),
                other.pid
            ));
        }
        cancel(&other, repo);
    }
    write(&me);
    if let Err(why) = snapshot.prepare(&settings) {
        write(&State {
            phase: Phase::Failed,
            step: None,
            reason: Some(format!("could not prepare the snapshot: {why}")),
            ..me
        });
        drop(snapshot);
        return Err(format!("could not prepare the snapshot: {why}"));
    }
    if !tree_todo.is_empty() {
        me.step = Some("tree lint".to_string());
        write(&me);
        crate::tree_lint::rehearse(&settings, snapshot.path(), &me.tree, &tree_todo);
    }
    if gates.is_empty() {
        // Lint was all there was: the push gate has nothing to run here.
        write(&State {
            phase: Phase::Passed,
            step: None,
            ..me
        });
        drop(snapshot);
        println!(
            "{} rehearsal of {short} done (tree lint only)",
            valid_sign()
        );
        return Ok(Outcome::Passed);
    }
    me.step = Some("testing".to_string());
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
        .env(ENV, snapshot_env_value(&on_cluster, Origin::of(prestage)))
        // A rehearsal is how a gate runs with no push waiting on it, so it
        // waits for a slot rather than defer: a rehearsal whose gates all
        // deferred would report "passed" having run nothing (ADR-0013).
        .env(crate::host_slots::ON_BUSY_ENV, "wait")
        // where the source's installed environment lives (a virtualenv the
        // Python audit reads; ADR-0018)
        .env(crate::pushed_tree::SOURCE_WORKTREE_ENV, repo)
        .stdin(Stdio::piped());
    strip_repo_env(&mut cmd);
    let line = format!(
        "{} {} {} {}\n",
        push_ref.local_ref, push_ref.local_oid, push_ref.remote_ref, push_ref.remote_oid
    );
    let status = (|| -> std::io::Result<std::process::ExitStatus> {
        let mut child = cmd.spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(line.as_bytes())?;
        }
        child.wait()
    })()
    .map_err(|e| format!("could not run the push gate in the snapshot: {e}"))?;
    let passed = status.success();
    let done = State {
        phase: if passed { Phase::Passed } else { Phase::Failed },
        step: None,
        reason: signal_of(&status).map(|n| format!("the push gate was stopped by signal {n}")),
        signal: signal_of(&status),
        ..me
    };
    write(&done);
    drop(snapshot);
    if let Some(n) = signal_of(&status) {
        eprintln!(
            "{} rehearsal of {short} stopped by signal {n}",
            warning_sign()
        );
        return Ok(Outcome::Interrupted(n));
    }
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
pub fn await_for(settings: &crate::config::Settings, tips: &[String]) -> Option<Phase> {
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
    // what happens next is the push's call, and under `refuse` it is not a
    // run here — say so rather than promise one
    let then = if refuse_unstamped(settings) {
        "fix it and rehearse again (`amont rehearse --wait`)"
    } else {
        "running the gate here"
    };
    match state.phase {
        Phase::Passed => None, // the stamps say it all
        Phase::Failed => {
            crate::say!(
                "{} the background rehearsal of this tree failed {} ago{}{log} — {then}",
                warning_sign(),
                human_secs(state.age_secs()),
                state
                    .reason
                    .as_deref()
                    .map(|r| format!(" ({r})"))
                    .unwrap_or_default(),
            );
            None
        }
        Phase::Running if !process_alive(state.pid) => {
            crate::say!(
                "{} a background rehearsal of this tree died without a verdict{log} — {then}",
                warning_sign(),
            );
            None
        }
        Phase::Running => {
            let budget = wait_budget(settings);
            crate::say!(
                "{} a background rehearsal of this tree started {} ago is still running — \
                 waiting{} for it rather than starting the suite over{log}",
                warning_sign(),
                human_secs(state.age_secs()),
                match budget {
                    Some(b) => format!(" up to {}", human_secs(b.as_secs())),
                    None => String::new(),
                },
            );
            let followed = follow(&state, budget).map(|(phase, _)| phase);
            if followed.is_none() && still_running(&state) {
                // Left RUNNING on purpose: it may still finish and stamp the
                // tree for the next push. We simply stop holding an open
                // connection open for it.
                warn(&format!(
                    "the rehearsal is still running after {} — running the gate here \
                     rather than holding the push open (`git config \
                     amont.rehearsalWait <secs>`, or 0 to wait indefinitely)",
                    human_secs(state.age_secs())
                ));
            }
            followed
        }
    }
}

/// Poll a running rehearsal to its end. `None` when it vanished — a new
/// worker took over for a different tree, the process died, or `budget`
/// expired.
///
/// `budget` is `None` for a caller with nothing on the wire; see
/// [`WAIT_BUDGET`] for why the push path always passes one.
fn follow(state: &State, budget: Option<Duration>) -> Option<(Phase, State)> {
    let mut last_word = std::time::Instant::now();
    let deadline = budget.map(|b| std::time::Instant::now() + b);
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
                if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
                    // The caller says what stopping the wait means for it.
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
                    format!(
                        "{} rehearsal failed{}",
                        warning_sign(),
                        now_state
                            .reason
                            .as_deref()
                            .map(|r| format!(" ({r})"))
                            .unwrap_or_default()
                    )
                };
                crate::say!("{verb} after {}", human_secs(now_state.age_secs()));
                return Some((phase, now_state));
            }
        }
    }
}

/// `amont rehearse [--wait|--status|--stop|--worker] [--cluster]`.
pub fn command(args: &[OsString], prestage: &Prestage) -> i32 {
    let flag = |f: &str| args.iter().any(|a| a == f);
    let ask = if flag("--cluster") {
        crate::cluster::Ask::Flag
    } else {
        crate::cluster::Ask::Setting
    };
    let prestage = match prestage {
        Prestage::Malformed(raw) => {
            eprintln!("amont: {PRESTAGE_ENV} is not a push line ({raw:?}); nothing was rehearsed");
            return 2;
        }
        line => line.line(),
    };
    if flag("--worker") {
        return match worker(ask, prestage) {
            Ok(Outcome::Failed) => 1,
            Ok(Outcome::Interrupted(n)) => 128 + n,
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
        // The Workflows go first: they are what outlives this process.
        let terminated = crate::cluster::stop_pending();
        for line in terminated.done.iter().chain(&terminated.failed) {
            eprintln!("{line}");
        }
        match read() {
            Some(s) if s.alive() => {
                cancel(&s, Path::new(&root));
                clear();
                println!("stopped the rehearsal of {} (pid {})", s.short(), s.pid);
            }
            _ if terminated.done.is_empty() && terminated.failed.is_empty() => {
                println!("no rehearsal is running in this worktree");
            }
            _ => {}
        }
        // A Workflow that is still running after `--stop` is a failure of it.
        return i32::from(!terminated.failed.is_empty());
    }
    if flag("--cluster") && !flag("--wait") {
        // Submit in the foreground — one line per Workflow, back in seconds —
        // then let a worker run what is left here.
        match plan(prestage) {
            Ok(planned) => {
                let on_cluster = planned.detach(crate::cluster::Ask::Flag);
                let left = planned
                    .gates
                    .iter()
                    .any(|gate| !on_cluster.contains(gate) && !planned.vouched(gate));
                if !left && planned.tree_todo.is_empty() && !on_cluster.is_empty() {
                    return 0;
                }
            }
            Err(e) => {
                // It ran and failed (no upstream, no HEAD): not a usage error.
                eprintln!("amont: {e}");
                return 1;
            }
        }
    }
    let head = crate::git::stdout(&["rev-parse", "HEAD^{tree}"]);
    let running_here = read().filter(|s| s.alive() && Some(s.tree.as_str()) == head.as_deref());
    if flag("--wait") {
        // A LOOP, not recursion. `worker()` answering `AlreadyRunning` means
        // somebody else claimed the tree between our own look and its own,
        // and the answer is to follow theirs — but re-entering `command`
        // from inside itself made that a stack frame per lost race with no
        // ceiling on it. Two passes is all the race can honestly need: one
        // to lose it, one to find the worker that won. A third would mean
        // the state file is churning, and running the gate ourselves is a
        // better answer than spinning on it.
        //
        // No budget here: nothing is connected, and somebody typed this
        // command to be told when the suite ends. See `WAIT_BUDGET`.
        // The tree to follow is the one being rehearsed: HEAD's, or the
        // commit land's pre-stage names (a tag's may be older than HEAD).
        let target = match prestage {
            Some(line) => format!("{}^{{tree}}", line.local_oid),
            None => "HEAD^{tree}".to_string(),
        };
        for _ in 0..3 {
            let running = read().filter(|s| {
                s.alive()
                    && Some(s.tree.as_str())
                        == crate::git::stdout(&["rev-parse", &target]).as_deref()
            });
            let Some(state) = running else {
                // Nothing running: do it here, in the foreground, and say so.
                match worker(ask, prestage) {
                    Ok(Outcome::Failed) => return 1,
                    Ok(Outcome::Interrupted(n)) => return 128 + n,
                    Ok(Outcome::AlreadyRunning(_)) => continue,
                    Ok(_) => return 0,
                    Err(e) => {
                        eprintln!("amont: {e}");
                        return 2;
                    }
                }
            };
            eprintln!(
                "following the rehearsal of {} (pid {}, started {} ago)",
                state.short(),
                state.pid,
                human_secs(state.age_secs())
            );
            match follow(&state, None) {
                // Its stamps say what IT selected. Plan again: another base
                // can owe another set, and the worker answers at once when
                // the followed run covered everything (ADR-0018).
                Some((Phase::Passed, _)) => continue,
                // Stopped by a signal: no verdict, so not a failure either.
                Some((
                    _,
                    State {
                        signal: Some(n), ..
                    },
                )) => return 128 + n,
                Some(_) => {
                    show_log_tail();
                    return 1;
                }
                None => {
                    eprintln!("amont: the rehearsal ended without a verdict");
                    return 2;
                }
            }
        }
        eprintln!("amont: another rehearsal keeps claiming this tree — run the gate yourself");
        return 2;
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
    // What the cluster finished since the last hook is collected first, so the
    // lines below are today's. Asked for, so stdout: this is the answer.
    let collected = crate::cluster::collect();
    let mut detached = crate::cluster::settled_lines(&collected.settled);
    detached.extend(crate::cluster::status_lines());
    for line in &detached {
        println!("{line}");
    }
    if let Some(trouble) = &collected.trouble {
        eprintln!("cluster runs not refreshed — {trouble}");
    }
    let Some(state) = read() else {
        if detached.is_empty() {
            println!("no rehearsal recorded in this worktree");
        }
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
                "rehearsal of {which}: running for {age} (pid {}{})",
                state.pid,
                state
                    .step
                    .as_deref()
                    .map(|s| format!(", {s}"))
                    .unwrap_or_default()
            )
        }
        Phase::Running => {
            format!("rehearsal of {which}: died without a verdict, started {age} ago")
        }
        Phase::Passed => format!("rehearsal of {which}: passed, started {age} ago"),
        Phase::Failed => format!(
            "rehearsal of {which}: FAILED, started {age} ago{}",
            state
                .reason
                .as_deref()
                .map(|r| format!(" — {r}"))
                .unwrap_or_default()
        ),
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
            step: Some("preparing".to_string()),
            reason: None,
            prestage: false,
            signal: None,
        }
    }

    #[test]
    fn the_prestage_marker_leaves_the_detached_gates_intact() {
        let marked = parse_snapshot_env("prestage;detached=a,b");
        assert!(marked.prestage);
        assert_eq!(marked.detached, ["a", "b"]);
        let plain = parse_snapshot_env("detached=a,b");
        assert!(!plain.prestage);
        assert_eq!(plain.detached, ["a", "b"]);
        let bare = parse_snapshot_env("1");
        assert!(!bare.prestage);
        assert!(bare.detached.is_empty());
        let marked_bare = parse_snapshot_env("prestage;1");
        assert!(marked_bare.prestage);
        assert!(marked_bare.detached.is_empty());
    }

    /// A pre-stage is recorded as one, so a rehearsal that meets it waits
    /// instead of cancelling it (ADR-0018); an older file reads as not one.
    #[test]
    fn a_prestage_survives_the_state_file() {
        let staged = State {
            prestage: true,
            signal: Some(9),
            ..sample()
        };
        assert_eq!(State::parse(&staged.render()), Some(staged));
        assert_eq!(State::parse(&sample().render()), Some(sample()));
    }

    /// The land variables are parsed where they are read: a malformed value
    /// is never mistaken for a remote that moved, or for a plain rehearsal.
    #[test]
    fn the_land_variables_take_object_names_only() {
        let (a, b) = ("a".repeat(40), "0".repeat(40));
        assert_eq!(
            LandEnv::parse(&format!("{a} {b}")),
            LandEnv::Valid(LandLine {
                sha: a.clone(),
                remote_tip: b.clone()
            })
        );
        for bad in [
            "",
            "abc",
            a.as_str(),
            &format!("{a} {b} extra"),
            &format!("{a} xyz"),
        ] {
            assert!(
                matches!(LandEnv::parse(bad), LandEnv::Malformed(_)),
                "{bad:?}"
            );
        }
        assert!(matches!(
            Prestage::parse(&format!("refs/heads/x {a} refs/heads/x {b}")),
            Prestage::Of(_)
        ));
        for bad in [
            format!("refs/heads/x {a} refs/heads/x"),
            "x y z w".to_string(),
        ] {
            assert!(
                matches!(Prestage::parse(&bad), Prestage::Malformed(_)),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_state_survives_the_round_trip() {
        let s = sample();
        assert_eq!(State::parse(&s.render()), Some(s));
        let done = State {
            phase: Phase::Failed,
            step: None,
            reason: Some("could not prepare the snapshot: npm ci failed\nin web/".to_string()),
            ..sample()
        };
        let back = State::parse(&done.render()).expect("parses");
        assert_eq!(
            back.reason.as_deref(),
            Some("could not prepare the snapshot: npm ci failed in web/"),
            "a reason is kept on one line"
        );
        assert_eq!(back.phase, Phase::Failed);
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
