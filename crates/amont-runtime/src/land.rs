//! `amont land` — push the current branch, then prove the remote ref moved.
//!
//! A push that exits 0 says the remote ACCEPTED something. It does not say the
//! branch now points where you pushed it: a forge that loses an accepted ref
//! update, a replica that has not caught up, and a `| tail` that threw the
//! exit status away all end the same way, a green terminal and a stale
//! branch. The proof is one question asked of the remote afterwards, and this
//! verb asks it every time, so nobody has to remember to.
//!
//! It wraps the push and the proof and nothing else. The gates, the stamps,
//! the rehearsal wait and the attestation all stay in the pre-push hook the
//! push runs; `land` neither re-implements nor skips them.
//!
//! Exit codes (`2` stays the binary-wide usage error):
//!
//! | code | status         | meaning                                              |
//! |------|----------------|------------------------------------------------------|
//! | 0    | `landed`       | the remote ref equals the pushed sha                 |
//! | 1    | `refused`      | a hook, the server, or a precondition said no        |
//! | 2    | `usage`        | wrong invocation                                     |
//! | 3    | `unreachable`  | the remote could not be reached, or the push was    |
//! |      | / `unverified` | accepted but could not be checked                    |
//! | 4    | `not-observed` | accepted, but the expected tip was not observed      |
//! | 128+n| `interrupted`  | killed by signal n                                   |
//!
//! `4` is deliberately worded as "not observed". Equality proves the tip was
//! seen; inequality does not prove an update was lost, because another writer
//! may have advanced or replaced the ref in between. The observed sha is
//! reported so the person can tell which.

use std::ffi::OsString;
use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::attest::{classify_push_stderr, PushFailure};
use crate::git::{self, Probe};
use crate::hooks::pull_rebase::PUSH_AGAIN;
use crate::json;

/// Re-reads of the remote after a mismatch, and the pause between them.
/// Spent only when the first read disagrees: at most 6 s.
const REREADS: u32 = 3;
const REREAD_PAUSE: Duration = Duration::from_secs(2);

/// A landing receipt older than this is deleted by the next land.
const RECEIPT_KEEP: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Whether a non-fast-forward may replace the remote ref.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A plain push: the server refuses anything but a fast-forward.
    Plain,
    /// `--force`: replace the ref, but only if it still holds the tip that
    /// was read (`--force-with-lease` on that exact sha).
    Force,
}

/// How the run ended. One value per exit code family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Landed,
    Refused,
    Usage,
    Unreachable,
    Unverified,
    NotObserved,
    /// Killed by this signal number.
    Interrupted(i32),
}

impl Status {
    pub fn code(self) -> i32 {
        match self {
            Status::Landed => 0,
            Status::Refused => 1,
            Status::Usage => 2,
            Status::Unreachable | Status::Unverified => 3,
            Status::NotObserved => 4,
            Status::Interrupted(sig) => 128 + sig,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Landed => "landed",
            Status::Refused => "refused",
            Status::Usage => "usage",
            Status::Unreachable => "unreachable",
            Status::Unverified => "unverified",
            Status::NotObserved => "not-observed",
            Status::Interrupted(_) => "interrupted",
        }
    }
}

/// Everything the output is made of. Filled in as the run learns things, so
/// a run that stops early still prints what it knew.
#[derive(Debug, Clone)]
pub struct Report {
    pub branch: String,
    pub remote: String,
    pub push_url: String,
    pub pushed: String,
    /// `refs/heads/<rb>`, the ref the push writes; empty until known.
    pub dst: String,
    pub remote_before: String,
    pub remote_after: String,
    pub retried: bool,
    pub status: Status,
    /// Commands the person can run next, printed whole, one per line.
    pub next: Vec<String>,
}

impl Report {
    fn new(status: Status) -> Self {
        Report {
            branch: String::new(),
            remote: String::new(),
            push_url: String::new(),
            pushed: String::new(),
            dst: String::new(),
            remote_before: String::new(),
            remote_after: String::new(),
            retried: false,
            status,
            next: Vec::new(),
        }
    }

    fn json(&self) -> String {
        json::object(&[
            json::string_field("branch", &self.branch),
            json::string_field("remote", &self.remote),
            json::string_field("push_url", &self.push_url),
            json::string_field("pushed", &self.pushed),
            json::string_field("remote_before", &self.remote_before),
            json::string_field("remote_after", &self.remote_after),
            json::bool_field("retried", self.retried),
            json::string_field("status", self.status.as_str()),
        ])
    }
}

/// The widest the closing verdict line may be.
const VERDICT_WIDTH: usize = 80;

/// `…` in the middle of `s` so it fits `max` columns. Counts chars, which is
/// right for branch names (ASCII in practice, and a wide glyph only makes the
/// line shorter than the budget, never longer).
pub fn truncate_middle(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    if max <= 1 {
        return "…".chars().take(max).collect();
    }
    let keep = max - 1;
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let front: String = s.chars().take(head).collect();
    let back: String = s.chars().skip(n - tail).collect();
    format!("{front}…{back}")
}

fn short(sha: &str) -> String {
    if sha.is_empty() {
        "-------".to_string()
    } else {
        sha.chars().take(7).collect()
    }
}

/// The last line printed: plain text, no colour, no glyphs, at most 80
/// columns whatever the branch is called. Outcome word, the two shas, then
/// the branch in whatever room is left.
pub fn verdict(status: Status, before: &str, after: &str, branch: &str) -> String {
    verdict_word(status.as_str(), before, after, branch)
}

fn verdict_word(word: &str, before: &str, after: &str, branch: &str) -> String {
    let head = format!("{word} {}..{}", short(before), short(after));
    let room = VERDICT_WIDTH.saturating_sub(head.chars().count() + 1);
    if branch.is_empty() || room == 0 {
        return head;
    }
    format!("{head} {}", truncate_middle(branch, room))
}

/// The tip `ls-remote <url> <ref>` printed; empty when it printed nothing
/// (the ref does not exist).
pub fn parse_tip(ls_remote: &str) -> String {
    ls_remote
        .lines()
        .find_map(|l| l.split_whitespace().next())
        .unwrap_or("")
        .to_string()
}

/// What a read of the remote ref came to.
enum Seen {
    Tip(String),
    Unreachable,
}

fn read_tip(url: &str, rb: &str, budget: u64) -> Seen {
    let refname = format!("refs/heads/{rb}");
    match git::probe_remote_stdout(&["ls-remote", url, &refname], budget) {
        (Probe::Exit(0), out) => Seen::Tip(parse_tip(&out)),
        _ => Seen::Unreachable,
    }
}

/// `amont land [--force] [--json] [--dry-run]`.
pub fn command(args: &[OsString]) -> i32 {
    let json_out = args.iter().any(|a| a == "--json");
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let mode = if args.iter().any(|a| a == "--force") {
        Mode::Force
    } else {
        Mode::Plain
    };
    if args.iter().any(|a| {
        let a = a.to_string_lossy();
        a != "--force" && a != "--json" && a != "--dry-run"
    }) {
        eprintln!("amont: land: takes no arguments, only [--force] [--json] [--dry-run]");
        return finish(&Report::new(Status::Usage), json_out);
    }
    if dry_run {
        return dry_run_command(mode, json_out);
    }
    let started = now_nanos();
    let report = land(mode);
    write_receipt(&report, started, now_nanos());
    finish(&report, json_out)
}

/// The JSON document a usage error prints under `--json`, so a script that
/// parses stdout never has to special-case exit 2.
pub fn usage_json() -> String {
    Report::new(Status::Usage).json()
}

fn finish(report: &Report, json_out: bool) -> i32 {
    for line in &report.next {
        eprintln!("    {line}");
    }
    eprintln!(
        "{}",
        verdict(
            report.status,
            &report.remote_before,
            &report.remote_after,
            &report.branch
        )
    );
    if json_out {
        println!("{}", report.json());
    }
    report.status.code()
}

fn refuse(mut report: Report, why: &str, next: &[&str]) -> Report {
    eprintln!("amont: land: {why}");
    report.status = Status::Refused;
    report.next = next.iter().map(|s| s.to_string()).collect();
    report
}

/// What steps 1 and 2 learned: where the push goes and what it is called there.
struct Resolved {
    remote: String,
    rb: String,
    url: String,
    has_upstream: bool,
}

/// Steps 1 and 2: the branch, its upstream, the refusals, and the one push
/// URL. Reads local git state only: no network, nothing written. `Err` is the
/// refused report, already explained on stderr.
fn resolve() -> Result<(Resolved, Report), Box<Report>> {
    let mut report = Report::new(Status::Refused);

    // 1. What are we pushing, and where to.
    let Some(branch) = git::stdout(&["symbolic-ref", "--quiet", "--short", "HEAD"]) else {
        return Err(Box::new(refuse(
            report,
            "HEAD is detached; there is no branch to land",
            &[],
        )));
    };
    report.branch = branch.clone();
    let upstream_remote = git::stdout(&["config", "--get", &format!("branch.{branch}.remote")]);
    let upstream_merge = git::stdout(&["config", "--get", &format!("branch.{branch}.merge")]);
    // `.` is a branch tracking another LOCAL branch (`branch.autoSetupMerge`
    // does that for `git switch -c`): not a remote upstream, so it names
    // neither where to push nor what to call the branch there.
    let (upstream_remote, upstream_merge) = match upstream_remote.as_deref() {
        Some(".") => (None, None),
        _ => (upstream_remote, upstream_merge),
    };
    let has_upstream = upstream_remote.is_some() && upstream_merge.is_some();
    let remote = upstream_remote.unwrap_or_else(|| "origin".to_string());
    let rb = upstream_merge
        .as_deref()
        .and_then(|m| m.strip_prefix("refs/heads/"))
        .unwrap_or(&branch)
        .to_string();
    report.remote = remote.clone();
    report.dst = format!("refs/heads/{rb}");

    if is_protected(&branch) || is_protected(&rb) {
        return Err(Box::new(refuse(
            report,
            "main and master change by pull request, never by a push",
            &["git switch -c <name>"],
        )));
    }
    if git::stdout(&["status", "--porcelain", "--untracked-files=no"])
        .is_some_and(|s| !s.is_empty())
    {
        return Err(Box::new(refuse(
            report,
            "tracked files have uncommitted changes; the push would not include them",
            &["git status", "git stash"],
        )));
    }

    // 2. The endpoint the push will really use.
    let Some(urls) = git::stdout(&["remote", "get-url", "--push", "--all", &remote]) else {
        return Err(Box::new(refuse(
            report,
            &format!("no remote named {remote:?}"),
            &["git remote -v"],
        )));
    };
    let urls: Vec<&str> = urls.lines().filter(|l| !l.is_empty()).collect();
    if urls.len() != 1 {
        return Err(Box::new(refuse(
            report,
            &format!(
                "{remote:?} has {} push destinations; land verifies exactly one",
                urls.len()
            ),
            &[&format!("git remote get-url --push --all {remote}")],
        )));
    }
    let url = urls[0].to_string();
    report.push_url = url.clone();
    Ok((
        Resolved {
            remote,
            rb,
            url,
            has_upstream,
        },
        report,
    ))
}

fn land(mode: Mode) -> Report {
    let settings = crate::config::Settings::default();
    let budget = crate::hooks::common::network_probe_budget(&settings);
    let (
        Resolved {
            remote,
            rb,
            url,
            has_upstream,
        },
        mut report,
    ) = match resolve() {
        Ok(r) => r,
        Err(refused) => return *refused,
    };

    // Where the ref stands.
    let mut expected_old = match read_tip(&url, &rb, budget) {
        Seen::Tip(t) => t,
        Seen::Unreachable => {
            report.status = Status::Unreachable;
            eprintln!("amont: land: could not read {rb} from {remote}");
            report.next = vec![format!("git ls-remote {remote} refs/heads/{rb}")];
            return report;
        }
    };
    report.remote_before = expected_old.clone();

    // 3. Push, once, and once more only if the hook moved HEAD for us.
    let mut attempt = 0;
    let pushed = loop {
        let Some(sha) = git::stdout(&["rev-parse", "HEAD"]) else {
            return refuse(report, "cannot resolve HEAD", &[]);
        };
        report.pushed = sha.clone();
        let lease = match mode {
            Mode::Force => Some(expected_old.as_str()),
            Mode::Plain => None,
        };
        let run = push(&url, &sha, &rb, lease);
        match run.outcome {
            PushOutcome::Done => break sha,
            PushOutcome::Signal(sig) => {
                report.status = Status::Interrupted(sig);
                return report;
            }
            PushOutcome::Failed => {}
        }
        let head_now = git::stdout(&["rev-parse", "HEAD"]).unwrap_or_default();
        let hook_moved_head = run.seen.contains(PUSH_AGAIN)
            && head_now != sha
            && (expected_old.is_empty()
                || git::succeeds(&["merge-base", "--is-ancestor", &expected_old, &head_now]));
        if attempt == 0 && hook_moved_head {
            attempt = 1;
            report.retried = true;
            eprintln!(
                "amont: land: retrying once: pre-push hook fast-forwarded HEAD {} -> {}",
                short(&sha),
                short(&head_now)
            );
            match read_tip(&url, &rb, budget) {
                Seen::Tip(t) => {
                    expected_old = t;
                    report.remote_before = expected_old.clone();
                }
                Seen::Unreachable => {
                    report.status = Status::Unreachable;
                    report.next = vec![format!("git ls-remote {remote} refs/heads/{rb}")];
                    return report;
                }
            }
            continue;
        }
        report.status = match classify_push_stderr(&run.stderr) {
            PushFailure::Unreachable => Status::Unreachable,
            PushFailure::Raced | PushFailure::Refused => Status::Refused,
        };
        return report;
    };

    // 4. The proof.
    let mut observed = Seen::Unreachable;
    for i in 0..=REREADS {
        if i > 0 {
            std::thread::sleep(REREAD_PAUSE);
        }
        observed = read_tip(&url, &rb, budget);
        if matches!(&observed, Seen::Tip(t) if *t == pushed) {
            break;
        }
    }
    match observed {
        Seen::Tip(t) if t == pushed => {
            report.remote_after = t;
            report.status = Status::Landed;
            move_tracking_ref(&remote, &rb, &pushed);
            if !has_upstream {
                set_upstream(&remote, &rb);
            }
        }
        Seen::Tip(t) => {
            report.remote_after = t;
            report.status = Status::NotObserved;
            eprintln!(
                "amont: land: the remote accepted the push but {rb} reads {}, not {}",
                short(&report.remote_after),
                short(&pushed)
            );
            report.next = vec![format!("git ls-remote {remote} refs/heads/{rb}")];
        }
        Seen::Unreachable => {
            report.status = Status::Unverified;
            eprintln!("amont: land: the push was accepted but {rb} could not be read back");
            report.next = vec![format!("git ls-remote {remote} refs/heads/{rb}")];
        }
    }
    report
}

/// `--dry-run`: steps 1 and 2 of a land and nothing else. No network, no push,
/// no ref written, no receipt.
fn dry_run_command(mode: Mode, json_out: bool) -> i32 {
    let (mut report, would) = match resolve() {
        Ok((_, report)) => (report, true),
        Err(refused) => (*refused, false),
    };
    if would {
        report.pushed = git::stdout(&["rev-parse", "HEAD"]).unwrap_or_default();
    }
    let word = if would { "would-land" } else { "refused" };
    for line in &report.next {
        eprintln!("    {line}");
    }
    eprintln!("{}", verdict_word(word, "", &report.pushed, &report.branch));
    if json_out {
        let dir = receipt_dir()
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_default();
        println!(
            "{}",
            json::object(&[
                json::string_field("branch", &report.branch),
                json::string_field("remote", &report.remote),
                json::string_field("push_url", &report.push_url),
                json::string_field("pushed", &report.pushed),
                json::string_field("dst", &report.dst),
                json::bool_field("retried", false),
                json::string_field("status", word),
                json::string_field("receipt_dir", &dir),
                json::bool_field("lease", mode == Mode::Force),
            ])
        );
    }
    if would {
        0
    } else {
        1
    }
}

fn now_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_nanos()).ok())
        .unwrap_or(0)
}

/// `<git-dir>/amont/land`, absolute and per worktree.
fn receipt_dir() -> Option<PathBuf> {
    git::stdout(&["rev-parse", "--absolute-git-dir"])
        .map(|d| PathBuf::from(d).join("amont").join("land"))
}

/// The receipt is the evidence of what a land pushed, independent of the
/// tracking ref. One per land that got past resolving the branch, written with
/// the final status. A receipt that cannot be written is a warning, never a
/// change of exit code.
fn write_receipt(report: &Report, started: i64, ended: i64) {
    if report.branch.is_empty() {
        return;
    }
    let Some(dir) = receipt_dir() else {
        eprintln!("amont: land: could not find the git directory; no landing receipt written");
        return;
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "amont: land: could not write a landing receipt in {}: {e}",
            dir.display()
        );
        return;
    }
    prune_receipts(&dir);
    let path = dir.join(format!("{started}-{}.json", std::process::id()));
    let body = json::object(&[
        json::int_field("started", started),
        json::int_field("ended", ended),
        json::string_field("branch", &report.branch),
        json::string_field("remote", &report.remote),
        json::string_field("push_url", &report.push_url),
        json::string_field("dst", &report.dst),
        json::string_field("pushed", &report.pushed),
        json::bool_field("retried", report.retried),
        json::string_field("status", report.status.as_str()),
    ]);
    if let Err(e) = std::fs::write(&path, format!("{body}\n")) {
        eprintln!(
            "amont: land: could not write the landing receipt {}: {e}",
            path.display()
        );
    }
}

/// Deletes receipts older than [`RECEIPT_KEEP`].
fn prune_receipts(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map_or(true, |e| e != "json") {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > RECEIPT_KEEP);
        if old {
            if let Err(e) = std::fs::remove_file(&path) {
                eprintln!(
                    "amont: land: could not delete the old receipt {}: {e}",
                    path.display()
                );
            }
        }
    }
}

fn is_protected(branch: &str) -> bool {
    matches!(branch, "main" | "master")
}

/// A push to a URL updates no tracking ref, so after a verified landing the
/// local view of the remote is moved to what was just proven. The landing
/// already happened, so a failure here is a warning, not an exit code.
fn move_tracking_ref(remote: &str, rb: &str, sha: &str) {
    let tracking = format!("refs/remotes/{remote}/{rb}");
    if !git::succeeds(&["update-ref", &tracking, sha]) {
        eprintln!(
            "amont: land: landed, but could not move {tracking} to {}",
            short(sha)
        );
    }
}

/// The first landing of a branch with no upstream sets one.
fn set_upstream(remote: &str, rb: &str) {
    let upstream = format!("{remote}/{rb}");
    if !git::succeeds(&["branch", "--set-upstream-to", &upstream]) {
        eprintln!("amont: land: landed, but could not set the upstream to {upstream}");
    }
}

enum PushOutcome {
    Done,
    Failed,
    Signal(i32),
}

struct PushRun {
    outcome: PushOutcome,
    /// stderr alone, for classification.
    stderr: String,
    /// stdout and stderr, for finding the hook's `push again`.
    seen: String,
}

/// `git push <url> <sha>:refs/heads/<rb>` with the pre-push hooks running.
///
/// An explicit sha, so the commit that is verified is the commit that was
/// pushed even if HEAD moves underneath. Both of the child's streams are
/// copied to OUR stderr as they arrive, in chunks rather than lines so git's
/// `\r` progress still moves, and a copy is kept for classification; stdout
/// of the process stays free for `--json`. No overall deadline: the
/// pre-push hook runs the whole gate, which takes minutes. Without a
/// terminal the push gets the no-prompt environment, which bounds the
/// network legs only: a custom ssh command, a credential helper or an
/// established session can still stall, and Ctrl-C is the answer.
fn push(url: &str, sha: &str, rb: &str, lease: Option<&str>) -> PushRun {
    let mut cmd = Command::new("git");
    if !std::io::stdin().is_terminal() {
        let (pre, env) = git::remote_env(git::configured_ssh());
        cmd.args(pre).envs(env);
    }
    cmd.arg("push");
    if let Some(old) = lease {
        cmd.arg(format!("--force-with-lease=refs/heads/{rb}:{old}"));
    }
    cmd.arg(url)
        .arg(format!("{sha}:refs/heads/{rb}"))
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let why = format!("could not run git push: {e}\n");
            eprint!("amont: land: {why}");
            return PushRun {
                outcome: PushOutcome::Failed,
                stderr: why.clone(),
                seen: why,
            };
        }
    };
    let tee = |mut from: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut kept = Vec::new();
            let mut buf = [0u8; 4096];
            let mut err = std::io::stderr();
            while let Ok(n) = from.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let _ = err.write_all(&buf[..n]);
                let _ = err.flush();
                kept.extend_from_slice(&buf[..n]);
            }
            String::from_utf8_lossy(&kept).into_owned()
        })
    };
    let out_thread = child.stdout.take().map(|s| tee(Box::new(s)));
    let err_thread = child.stderr.take().map(|s| tee(Box::new(s)));
    let status = child.wait();
    let out = out_thread.and_then(|t| t.join().ok()).unwrap_or_default();
    let err = err_thread.and_then(|t| t.join().ok()).unwrap_or_default();
    let outcome = match status {
        Ok(s) if s.success() => PushOutcome::Done,
        Ok(s) => match signal_of(&s) {
            Some(sig) => PushOutcome::Signal(sig),
            None => PushOutcome::Failed,
        },
        Err(_) => PushOutcome::Failed,
    };
    PushRun {
        outcome,
        seen: format!("{out}{err}"),
        stderr: err,
    }
}

#[cfg(unix)]
fn signal_of(s: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    s.signal()
}

#[cfg(not(unix))]
fn signal_of(_: &std::process::ExitStatus) -> Option<i32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_keep_usage_apart_from_the_lost_ref() {
        assert_eq!(Status::Landed.code(), 0);
        assert_eq!(Status::Refused.code(), 1);
        assert_eq!(Status::Usage.code(), 2);
        assert_eq!(Status::Unreachable.code(), 3);
        assert_eq!(Status::Unverified.code(), 3);
        assert_eq!(Status::NotObserved.code(), 4);
        assert_eq!(Status::Interrupted(2).code(), 130);
    }

    #[test]
    fn every_status_has_its_own_word() {
        let all = [
            Status::Landed,
            Status::Refused,
            Status::Usage,
            Status::Unreachable,
            Status::Unverified,
            Status::NotObserved,
            Status::Interrupted(15),
        ];
        let words: std::collections::HashSet<_> = all.iter().map(|s| s.as_str()).collect();
        assert_eq!(words.len(), all.len());
    }

    #[test]
    fn the_verdict_fits_eighty_columns_for_any_branch() {
        let long = "feature/".to_string() + &"x".repeat(120);
        for status in [Status::Landed, Status::NotObserved, Status::Unverified] {
            let line = verdict(status, "0123456789abcdef", "fedcba9876543210", &long);
            assert!(line.chars().count() <= 80, "{line}");
            assert!(line.contains('…'), "{line}");
            assert!(line.starts_with(status.as_str()));
        }
    }

    #[test]
    fn a_short_branch_is_shown_whole_and_a_new_ref_has_a_placeholder() {
        let line = verdict(Status::Landed, "", "fedcba9876543210", "feat/x");
        assert_eq!(line, "landed -------..fedcba9 feat/x");
    }

    #[test]
    fn the_verdict_is_plain_text() {
        let line = verdict(Status::Refused, "a", "b", "feat/ünï");
        assert!(!line.contains('\u{1b}'));
    }

    #[test]
    fn truncation_keeps_both_ends() {
        assert_eq!(truncate_middle("abcdefghij", 5), "ab…ij");
        assert_eq!(truncate_middle("abc", 5), "abc");
        assert_eq!(truncate_middle("abcdefghij", 1), "…");
    }

    #[test]
    fn the_tip_is_the_first_field_and_absence_is_empty() {
        assert_eq!(parse_tip("abc123\trefs/heads/x\n"), "abc123");
        assert_eq!(parse_tip(""), "");
    }

    #[test]
    fn json_carries_every_field_the_contract_names() {
        let mut r = Report::new(Status::NotObserved);
        r.branch = "feat/x".into();
        r.retried = true;
        let j = r.json();
        for key in [
            "branch",
            "remote",
            "push_url",
            "pushed",
            "remote_before",
            "remote_after",
            "retried",
            "status",
        ] {
            assert!(j.contains(&format!("\"{key}\"")), "{key} missing in {j}");
        }
        assert!(j.contains("\"status\":\"not-observed\""), "{j}");
        assert!(j.contains("\"retried\":true"), "{j}");
        assert!(usage_json().contains("\"status\":\"usage\""));
    }
}
