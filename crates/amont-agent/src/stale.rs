//! How far this checkout sits behind the remote's default branch.
//!
//! The failure this measures is quiet in a way the other rules are not: no
//! command fails, nothing is refused, no log line says anything. A session
//! opens in a checkout that was last pulled on Tuesday, the model reads the
//! tree it is given, and builds a feature that landed on `origin/main` on
//! Wednesday. The work is correct against the code it can see. It is only
//! wasted against the code that exists.
//!
//! So this module does the one thing the model cannot do for itself: it asks
//! the remote, once, at the moment a session opens, and states the distance as
//! a fact. It never pulls. `git pull` rewrites the working tree under whoever
//! is using it — the exact collision per-task worktrees exist to prevent — and
//! inherits `pull.rebase`/autostash surprises that can leave a conflict state
//! nobody asked for. Moving `refs/remotes/origin/*` is safe in every worktree
//! at once; moving `HEAD` is not.
//!
//! ## The fetch is budgeted and throttled
//!
//! A `SessionStart` hook has ten seconds. A fetch talks to the network, and a
//! network verb hanging is Tuesday — captive portal, VPN split brain, a remote
//! that accepts the connect and says nothing. The fetch gets
//! [`FETCH_BUDGET_SECS`] and is killed at the deadline; the distance is then
//! reported against whatever `origin/main` was at the last successful fetch,
//! and the notice says so. Twenty parallel sessions on one repository must
//! not become twenty fetches, so a `FETCH_HEAD` younger than
//! [`FETCH_FRESH_SECS`] is trusted without a network round-trip.
//!
//! Everything that cannot be established is silence, like the rest of this
//! crate: not a repository, no `origin`, no default branch, git would not
//! answer — nothing is said.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use amont_runtime::{config, git};

/// `amont.agent.fetch` — whether a session opening may touch the network at
/// all. `false` still reports the distance to the last-fetched `origin/main`;
/// it only stops the guard from refreshing it.
pub const KEY_FETCH: &str = "amont.agent.fetch";

/// Wall clock for one fetch. Half the hook's own timeout, so a fetch that is
/// killed still leaves time to compute and emit the notice.
pub const FETCH_BUDGET_SECS: u64 = 5;

/// A `FETCH_HEAD` younger than this is fresh enough. Ten minutes: long enough
/// that a burst of sessions shares one round-trip, short enough that a
/// `--resume` after lunch asks again.
pub const FETCH_FRESH_SECS: u64 = 600;

/// What a fetch attempt came to. Carried into the notice so the reader knows
/// how current the number is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fetch {
    /// Ran and succeeded.
    Done,
    /// Not run: a recent fetch was trusted, or `amont.agent.fetch` is off.
    Skipped,
    /// Ran and failed, or was killed at the budget. The distance is against
    /// the previous fetch.
    Failed,
}

/// The distance from one ref to the remote's default branch.
#[derive(Debug, Clone)]
pub struct Drift {
    /// Repository basename, for the notice. Never the full path.
    pub repo: String,
    /// The branch checked out at `from`, when `from` is `HEAD` and it is one.
    pub branch: Option<String>,
    /// The remote default branch, `origin/main` or whatever `origin/HEAD` says.
    pub base: String,
    pub behind: u32,
    pub ahead: u32,
    /// `<short sha> <subject> (<relative date>)` of the base's tip.
    pub newest: String,
    pub fetched: Fetch,
}

/// Measure `from` against the remote default branch, refreshing the remote
/// ref first if it is stale and the network is allowed.
///
/// `None` is silence: not a repository, no `origin`, nothing to compare to.
pub fn measure(cwd: &Path, from: &str) -> Option<Drift> {
    let top = git::stdout_in(cwd, &["rev-parse", "--show-toplevel"])?;
    let remote = remote_of(cwd)?;
    let base = default_base(cwd, &remote)?;
    let fetched = refresh(cwd, &remote, &base);
    let counts = git::stdout_in(
        cwd,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("{from}...{base}"),
        ],
    )?;
    let mut parts = counts.split_whitespace();
    let ahead: u32 = parts.next()?.parse().ok()?;
    let behind: u32 = parts.next()?.parse().ok()?;
    let newest = git::stdout_in(cwd, &["log", "-1", "--format=%h %s (%cr)", &base])?;
    let branch = if from == "HEAD" {
        git::stdout_in(cwd, &["symbolic-ref", "-q", "--short", "HEAD"])
    } else {
        None
    };
    let repo = Path::new(&top)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "-".to_string());
    Some(Drift {
        repo,
        branch,
        base,
        behind,
        ahead,
        newest,
        fetched,
    })
}

/// Which remote is "the" remote.
///
/// `checkout.defaultRemote` is git's own answer to this question — a
/// repository mid-migration, whose `origin` is a mirror going stale while a
/// second remote carries the truth, sets it and every `git checkout <branch>`
/// disambiguates the same way. Otherwise `origin` when it exists; otherwise
/// the only remote there is. Two remotes and no preference is a guess, and a
/// guess about which remote is authoritative is exactly the wrong thing to
/// state as a fact — so that is silence.
fn remote_of(cwd: &Path) -> Option<String> {
    if let Some(r) = git::stdout_in(cwd, &["config", "--get", "checkout.defaultRemote"]) {
        if !r.is_empty() {
            return Some(r);
        }
    }
    let listed = git::stdout_in(cwd, &["remote"])?;
    let remotes: Vec<&str> = listed
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if remotes.contains(&"origin") {
        return Some("origin".to_string());
    }
    match remotes.as_slice() {
        [only] => Some((*only).to_string()),
        _ => None,
    }
}

/// The remote's default branch as a local ref name: `origin/main`.
///
/// `<remote>/HEAD` is the authority when a clone set it. Older clones and
/// hand-added remotes have none, so fall back to the two names that cover
/// nearly every repository, and give up rather than guess further.
fn default_base(cwd: &Path, remote: &str) -> Option<String> {
    let head_ref = format!("refs/remotes/{remote}/HEAD");
    if let Some(head) = git::stdout_in(cwd, &["symbolic-ref", "-q", "--short", &head_ref]) {
        if !head.is_empty() {
            return Some(head);
        }
    }
    for name in ["main", "master"] {
        let full = format!("refs/remotes/{remote}/{name}");
        if git::succeeds_in(cwd, &["rev-parse", "-q", "--verify", &full]) {
            return Some(format!("{remote}/{name}"));
        }
    }
    None
}

/// Bring `origin/<branch>` up to date if it is worth asking.
///
/// Only the one branch, no tags, no submodules: the question is "has the
/// default branch moved", and the cheapest fetch that answers it is the one
/// that fits the budget on a slow link.
pub fn refresh(cwd: &Path, remote: &str, base: &str) -> Fetch {
    if !config::boolean_or(KEY_FETCH, true) {
        return Fetch::Skipped;
    }
    if fetched_recently(cwd) {
        return Fetch::Skipped;
    }
    let Some(branch) = base.strip_prefix(&format!("{remote}/")) else {
        return Fetch::Skipped;
    };
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(cwd)
        .args([
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-recurse-submodules",
            remote,
            branch,
        ])
        // A credential prompt with nobody to answer it would sit there until
        // the budget kills it. Better to fail at once and say so.
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let Ok(mut child) = cmd.spawn() else {
        return Fetch::Failed;
    };
    let deadline = Instant::now() + Duration::from_secs(FETCH_BUDGET_SECS);
    loop {
        match child.try_wait() {
            Ok(Some(s)) if s.success() => return Fetch::Done,
            Ok(Some(_)) | Err(_) => return Fetch::Failed,
            Ok(None) => {}
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Fetch::Failed;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// `FETCH_HEAD`'s age, via `--git-path` so a linked worktree resolves to the
/// file git would actually write.
fn fetched_recently(cwd: &Path) -> bool {
    let Some(path) = git::stdout_in(cwd, &["rev-parse", "--git-path", "FETCH_HEAD"]) else {
        return false;
    };
    let path = if Path::new(&path).is_absolute() {
        std::path::PathBuf::from(path)
    } else {
        cwd.join(path)
    };
    let Ok(meta) = std::fs::metadata(&path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .map(|age| age.as_secs() < FETCH_FRESH_SECS)
        .unwrap_or(false)
}

/// The session-opening notice: a statement of where this checkout stands.
///
/// A fact, deliberately not an order — see `decision::phrase`. It names what
/// is behind, by how much, what the newest thing over there is, and the two
/// consequences the reader can act on: work that looks missing may already
/// exist, and a branch started from `HEAD` inherits the gap.
pub fn notice(d: &Drift) -> String {
    let where_ = match &d.branch {
        Some(b) => format!("this checkout of {} (branch {b})", d.repo),
        None => format!("this checkout of {}", d.repo),
    };
    let commits = if d.behind == 1 { "commit" } else { "commits" };
    let mut text = format!(
        "{where_} is {} {commits} behind {}; newest there: {}. \
         Work that seems missing here may already exist on {} — \
         `git log HEAD..{} --oneline` lists it — and a branch or worktree \
         started from HEAD inherits the gap; one started from {} does not.",
        d.behind, d.base, d.newest, d.base, d.base, d.base
    );
    if d.ahead > 0 {
        text.push_str(&format!(
            " ({} local {} not on {}.)",
            d.ahead,
            if d.ahead == 1 {
                "commit is"
            } else {
                "commits are"
            },
            d.base
        ));
    }
    if d.fetched == Fetch::Failed {
        text.push_str(&format!(
            " {} is as of the last successful fetch; fetching just now did not \
             complete within {FETCH_BUDGET_SECS}s.",
            d.base
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drift() -> Drift {
        Drift {
            repo: "thing".into(),
            branch: Some("main".into()),
            base: "origin/main".into(),
            behind: 8,
            ahead: 0,
            newest: "d3b2ed5 chore(release): 1.16.0 (3 days ago)".into(),
            fetched: Fetch::Done,
        }
    }

    #[test]
    fn the_notice_states_the_distance_and_the_newest_commit() {
        let n = notice(&drift());
        assert!(n.contains("8 commits behind origin/main"), "{n}");
        assert!(n.contains("d3b2ed5"), "{n}");
        assert!(n.contains("git log HEAD..origin/main"), "{n}");
        assert!(!n.contains("fetch"), "a clean fetch is not mentioned: {n}");
    }

    #[test]
    fn a_failed_fetch_is_disclosed() {
        let d = Drift {
            fetched: Fetch::Failed,
            ..drift()
        };
        assert!(notice(&d).contains("last successful fetch"));
    }

    #[test]
    fn singular_when_one_behind() {
        let d = Drift {
            behind: 1,
            ..drift()
        };
        assert!(notice(&d).contains("1 commit behind"));
    }

    /// Outside a repository there is nothing to measure, and nothing said.
    #[test]
    fn not_a_repository_is_silence() {
        let dir = std::env::temp_dir().join(format!("amont-agent-stale-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(measure(&dir, "HEAD").is_none());
    }
}
