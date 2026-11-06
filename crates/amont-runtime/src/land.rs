//! `amont land` — push the current branch, then prove the remote ref moved.
//!
//! A push that exits 0 says the remote ACCEPTED something. It does not say the
//! branch now points where you pushed it: a forge that loses an accepted ref
//! update, a replica that has not caught up, and a `| tail` that threw the
//! exit status away all end the same way, a green terminal and a stale
//! branch. The proof is one question asked of the remote afterwards, and this
//! verb asks it every time, so nobody has to remember to.
//!
//! Before every push attempt it runs a pre-stage (ADR-0018): the heavy gates
//! the push owes run in a child `amont rehearse --wait`, told the push's real
//! ref line, with no connection open; the push's own hook then finds them
//! stamped and runs only the unscoped checks. The stamps, the attestation and
//! those checks stay in the pre-push hook; `land` neither re-implements nor
//! skips them.
//!
//! Exit codes (`2` stays the binary-wide usage error):
//!
//! | code | status         | meaning                                              |
//! |------|----------------|------------------------------------------------------|
//! | 0    | `landed`       | the remote ref equals the pushed sha                 |
//! | 1    | `refused`      | a gate, a hook, the server, or a precondition said no |
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
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::attest::{classify_push_stderr, PushFailure};
use crate::git::{self, Probe};
use crate::hooks::pull_rebase::PUSH_AGAIN;
use crate::json;
use crate::rehearsal::signal_of;

/// The line the hook of land's own push prints, first, when the pushed
/// range's remote side is not the one land's pre-stage ran against: the
/// remote moved under land (ADR-0018). Matched in the push's output, never
/// the prose after it.
pub const LAND_REMOTE_MOVED: &str = "amont-land: remote-moved";
/// The line that hook prints, first, when the range is the pre-stage's but a
/// gate it owes carries no evidence: a defect, named with the gates.
pub const LAND_EVIDENCE_MISSING: &str = "amont-land: evidence-missing";

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
    /// The push gates the pushed range owes (ADR-0018), and what the
    /// pre-stage did about them.
    pub gates_owed: Vec<String>,
    pub prestage: PrestageState,
}

/// What land's pre-stage did (ADR-0018), as `--json` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrestageState {
    /// It did not get that far.
    NotRun,
    /// Every owed gate already had its evidence: nothing ran.
    Stamped,
    /// It ran the owed gates, and they passed.
    Ran,
    /// A gate failed: nothing was pushed.
    Failed,
    /// It could not run, or was not allowed to: nothing was pushed.
    Refused,
    /// A signal stopped it: nothing was pushed.
    Interrupted,
}

impl PrestageState {
    pub fn as_str(self) -> &'static str {
        match self {
            PrestageState::NotRun => "",
            PrestageState::Stamped => "stamped",
            PrestageState::Ran => "ran",
            PrestageState::Failed => "failed",
            PrestageState::Refused => "refused",
            PrestageState::Interrupted => "interrupted",
        }
    }
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
            gates_owed: Vec::new(),
            prestage: PrestageState::NotRun,
        }
    }

    fn json(&self) -> String {
        json::object(&[
            json::string_field("branch", &self.branch),
            json::string_field("remote", &self.remote),
            json::string_field("push_url", &self.push_url),
            json::string_field("pushed", &self.pushed),
            json::string_field("ref", &self.dst),
            json::string_field("remote_before", &self.remote_before),
            json::string_field("remote_after", &self.remote_after),
            json::bool_field("retried", self.retried),
            json::string_field("status", self.status.as_str()),
            format!(
                "\"gates\":{}",
                json::object(&[
                    json::string_array_field("owed", &self.gates_owed),
                    json::string_field("prestage", self.prestage.as_str()),
                ])
            ),
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
    read_ref(url, &format!("refs/heads/{rb}"), budget)
}

/// Where `refname` stands on `url`: empty when absent.
fn read_ref(url: &str, refname: &str, budget: u64) -> Seen {
    match git::probe_remote_stdout(&["ls-remote", url, refname], budget) {
        (Probe::Exit(0), out) => Seen::Tip(parse_tip(&out)),
        _ => Seen::Unreachable,
    }
}

/// `amont land [--force] [--json] [--dry-run]`, or
/// `amont land --tag <name> [--json] [--dry-run]`.
pub fn command(args: &[OsString]) -> i32 {
    let json_out = args.iter().any(|a| a == "--json");
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let mode = if args.iter().any(|a| a == "--force") {
        Mode::Force
    } else {
        Mode::Plain
    };
    let mut tag: Option<String> = None;
    let mut it = args.iter().map(|a| a.to_string_lossy().into_owned());
    while let Some(a) = it.next() {
        match a.as_str() {
            "--force" | "--json" | "--dry-run" => {}
            "--tag" => match it.next() {
                Some(name) if !name.is_empty() && !name.starts_with('-') => tag = Some(name),
                _ => {
                    eprintln!("amont: land: --tag needs a tag name");
                    return finish(&Report::new(Status::Usage), json_out);
                }
            },
            other if other.starts_with("--tag=") => match &other["--tag=".len()..] {
                "" => {
                    eprintln!("amont: land: --tag needs a tag name");
                    return finish(&Report::new(Status::Usage), json_out);
                }
                name => tag = Some(name.to_string()),
            },
            _ => {
                eprintln!(
                    "amont: land: takes no arguments, only [--force] [--json] [--dry-run] [--tag <name>]"
                );
                return finish(&Report::new(Status::Usage), json_out);
            }
        }
    }
    if let Some(name) = tag {
        if mode == Mode::Force {
            eprintln!("amont: land: a tag is never forced; --tag takes no --force");
            return finish(&Report::new(Status::Usage), json_out);
        }
        if dry_run {
            let (report, verdict) = dry_run_tag(&name);
            say_dry_run(&report, verdict);
            if json_out {
                println!("{}", dry_run_json(&report, verdict, mode));
            }
            return i32::from(verdict != "would-land");
        }
        let started = now_nanos();
        let report = land_tag(&name);
        write_receipt(&report, started, now_nanos());
        return finish(&report, json_out);
    }
    if dry_run {
        let (report, verdict) = dry_run_branch();
        say_dry_run(&report, verdict);
        if json_out {
            println!("{}", dry_run_json(&report, verdict, mode));
        }
        return i32::from(verdict != "would-land");
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
            shown_ref(report)
        )
    );
    if json_out {
        println!("{}", report.json());
    }
    report.status.code()
}

/// What the verdict line names: the branch, or for a tag land (which has none)
/// the tag.
fn shown_ref(report: &Report) -> &str {
    if report.branch.is_empty() {
        report.dst.strip_prefix("refs/tags/").unwrap_or(&report.dst)
    } else {
        &report.branch
    }
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

/// The one push URL of `remote`, or the refusal saying why there is not one.
fn one_push_url(remote: &str, mut report: Report) -> Result<(String, Report), Box<Report>> {
    let Some(urls) = git::stdout(&["remote", "get-url", "--push", "--all", remote]) else {
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
    report.push_url = redact_url(&url);
    Ok((url, report))
}

/// `url` as a report may print it: a password, or any user part of an
/// http(s) URL (a token rides there), becomes `***`. The push itself uses
/// the URL unchanged.
fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string(); // scp-like `git@host:path` or a path: no secret
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let (authority, path) = rest.split_at(authority_end);
    let Some((user, host)) = authority.rsplit_once('@') else {
        return url.to_string();
    };
    let user = if scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https") {
        "***".to_string()
    } else {
        match user.split_once(':') {
            Some((name, _)) => format!("{name}:***"),
            None => user.to_string(),
        }
    };
    format!("{scheme}://{user}@{host}{path}")
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
    // A branch cut from `main` tracks `main` (`git worktree add -b <b>
    // origin/main` does that): its base, not where it publishes. Following
    // it would refuse every such branch as a push to `main`; plain `git
    // push` refuses the name mismatch too. So it lands under its own name on
    // that remote, and the upstream is set to it afterwards, as `git push -u`
    // would. A renamed upstream that is not `main` or `master` still decides.
    let upstream_merge = upstream_merge.filter(|m| {
        let tracked = m.strip_prefix("refs/heads/").unwrap_or(m);
        !is_protected(tracked) || is_protected(&branch)
    });
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
    let (url, report) = one_push_url(&remote, report)?;
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
        // ADR-0018: every heavy gate this push owes runs HERE, before git
        // connects — on every attempt, so a hook that moved HEAD gets its new
        // sha pre-staged too.
        let local_ref = format!("refs/heads/{}", report.branch);
        let dst = format!("refs/heads/{rb}");
        report = match prestage(&local_ref, &sha, &dst, &expected_old, (&remote, &url))
            .into_report(report)
        {
            ControlFlow::Continue(report) => report,
            ControlFlow::Break(stopped) => return *stopped,
        };
        let land_env = format!("{sha} {}", or_zeros(&expected_old));
        let run = push(&url, &sha, &dst, lease, &land_env);
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
        // The hook of land's own push refused (ADR-0018): matched on its
        // sentinel lines, never on the prose.
        if run.seen.contains(crate::land::LAND_REMOTE_MOVED) {
            report.status = Status::Refused;
            report.next = vec!["amont land".to_string()];
            return report;
        }
        if run.seen.contains(crate::land::LAND_EVIDENCE_MISSING) {
            report.status = Status::Refused;
            report.next = vec![
                "amont rehearse --wait".to_string(),
                "amont land".to_string(),
            ];
            return report;
        }
        report.status = status_of_failed_push(&run.stderr);
        return report;
    };

    // 4. The proof.
    let mut observed = Seen::Unreachable;
    for i in 0..=REREADS {
        if i > 0 {
            std::thread::sleep(crate::clock::scaled(REREAD_PAUSE));
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

/// An absent remote tip, as git's ref lines spell it.
const ZEROS: &str = "0000000000000000000000000000000000000000";

fn or_zeros(tip: &str) -> &str {
    if tip.is_empty() {
        ZEROS
    } else {
        tip
    }
}

/// What land's pre-stage ended in.
enum Prestage {
    /// Every gate the push owes has its evidence: push.
    Ready,
    /// Do not push; why, for the person, and what to run next.
    Refused(String, Vec<String>),
    /// The pre-stage was stopped by this signal; nothing was pushed.
    Interrupted(i32),
}

/// The pre-stage's result: what the push owed, what was done about it, and
/// whether to go on.
struct Staged {
    owed: Vec<String>,
    state: PrestageState,
    end: Prestage,
}

impl Staged {
    fn ended(owed: Vec<String>, state: PrestageState, end: Prestage) -> Staged {
        Staged { owed, state, end }
    }

    fn refused(owed: Vec<String>, state: PrestageState, why: String) -> Staged {
        Staged::ended(owed, state, Prestage::Refused(why, Vec::new()))
    }

    /// `report` with this folded in: `Continue` when land may push, `Break`
    /// with the finished report when it must stop.
    fn into_report(self, mut report: Report) -> ControlFlow<Box<Report>, Report> {
        report.gates_owed = self.owed;
        report.prestage = self.state;
        match self.end {
            Prestage::Ready => ControlFlow::Continue(report),
            Prestage::Refused(why, next) => {
                eprintln!("amont: land: {why}");
                report.status = Status::Refused;
                report.next = next;
                ControlFlow::Break(Box::new(report))
            }
            Prestage::Interrupted(sig) => {
                report.status = Status::Interrupted(sig);
                ControlFlow::Break(Box::new(report))
            }
        }
    }
}

/// A push git refused, as land reports it.
fn status_of_failed_push(stderr: &str) -> Status {
    match classify_push_stderr(stderr) {
        PushFailure::Unreachable => Status::Unreachable,
        PushFailure::Raced | PushFailure::Refused => Status::Refused,
        // `PushFailure` is another crate's `#[non_exhaustive]` enum: a kind
        // it adds later is a refusal until land learns it.
        _ => Status::Refused,
    }
}

/// The rehearsal this worktree recorded last: `(pid, started, running)`.
/// What tells whether gates ran during the pre-stage: one started by it, or
/// one already running that it followed.
fn last_rehearsal() -> Option<(u32, u64, bool)> {
    crate::rehearsal::read().map(|s| (s.pid, s.started, s.alive()))
}

/// Run every heavy gate the push of `sha` onto `dst` (whose remote tip is
/// `remote_tip`, empty when the ref is new) will owe, before git opens a
/// connection (ADR-0018). In a child `amont rehearse --wait`, told the real
/// ref line, whose stdout goes to this process's stderr so a `--json` report
/// stays the only thing on stdout.
/// `remote` is the remote's name, for what land tells the person: its URL
/// may carry credentials, and is only ever handed to git.
fn prestage(
    local_ref: &str,
    sha: &str,
    dst: &str,
    remote_tip: &str,
    (remote, url): (&str, &str),
) -> Staged {
    // What to run again after a refusal: the same land.
    let again = match dst.strip_prefix("refs/tags/") {
        Some(tag) => format!("amont land --tag {tag}"),
        None => "amont land".to_string(),
    };
    let root = match crate::hooks::common::repo_root_checked() {
        Ok(root) => root,
        Err(e) => return Staged::refused(Vec::new(), PrestageState::Refused, e),
    };
    let manifest = crate::manifest::load(std::path::Path::new(&root));
    let settings = crate::config::Settings::new(manifest.policy.clone());
    // The range starts at the remote's tip: a tip this clone never fetched
    // (a force push elsewhere) would make it unwalkable. Fetch it, or refuse.
    // A fetch moves objects, so it gets the check budget (`amont.timeout`),
    // not the few seconds a reachability probe gets.
    if !remote_tip.is_empty() && !has_commit(remote_tip) {
        eprintln!(
            "amont: land: fetching {} ({dst}) from the remote",
            short(remote_tip)
        );
        let budget = crate::hooks::common::check_timeout(&settings);
        let (probe, _) = git::probe_remote_stdout(&["fetch", "-q", "--no-tags", url, dst], budget);
        if !has_commit(remote_tip) {
            let how = match probe {
                Probe::Exit(0) => "it fetched, but not that commit".to_string(),
                Probe::Exit(code) => format!("git fetch exited {code}"),
                Probe::TimedOut(secs) => format!("git fetch did not finish within {secs}s"),
                Probe::Failed => "git fetch did not run".to_string(),
            };
            return Staged::ended(
                Vec::new(),
                PrestageState::Refused,
                Prestage::Refused(
                    format!(
                        "the remote's {dst} is at {}, a commit this clone does not have ({how})",
                        short(remote_tip)
                    ),
                    // git's own words, which the bounded fetch could not keep
                    vec![format!("git fetch --no-tags {remote} {dst}"), again],
                ),
            );
        }
    }
    let line = crate::pushrefs::PushRef {
        local_ref: local_ref.to_string(),
        local_oid: sha.to_string(),
        remote_ref: dst.to_string(),
        remote_oid: or_zeros(remote_tip).to_string(),
    };
    let changed = crate::pushrefs::changed_files(std::slice::from_ref(&line));
    let mut owed = crate::dispatch::scoped_push_gates(&settings, &manifest, &changed);
    let audits = crate::hooks::audit::Ledger::default();
    for audit in
        crate::dispatch::owed_audits(&settings, &manifest, &audits, std::slice::from_ref(&line))
    {
        if !owed.contains(&audit) {
            owed.push(audit);
        }
    }
    if owed.is_empty() {
        return Staged::ended(owed, PrestageState::Stamped, Prestage::Ready);
    }
    if !crate::gate_stamp::push_stamps_enabled(&settings) {
        return Staged::ended(
            owed,
            PrestageState::Refused,
            Prestage::Refused(
                "stamp reuse is off (amont.pushStamps false), so the gates land runs \
                 cannot count at the push"
                    .to_string(),
                vec!["git config amont.pushStamps true".to_string(), again],
            ),
        );
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            return Staged::refused(
                owed,
                PrestageState::Refused,
                format!("cannot locate the amont binary for the pre-stage: {e}"),
            )
        }
    };
    let names: Vec<&str> = owed.iter().map(String::as_str).collect();
    eprintln!(
        "amont: land: {} push gate{} to check before git connects:",
        owed.len(),
        if owed.len() == 1 { "" } else { "s" }
    );
    for wrapped in crate::dispatch::wrap_names(&names) {
        eprintln!("{wrapped}");
    }
    let before = last_rehearsal();
    let status = Command::new(exe)
        .args(["rehearse", "--wait"])
        .env(
            crate::rehearsal::PRESTAGE_ENV,
            format!(
                "{} {} {} {}",
                line.local_ref, line.local_oid, line.remote_ref, line.remote_oid
            ),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::io::stderr()))
        .status();
    let status = match status {
        Ok(status) => status,
        Err(e) => {
            return Staged::refused(
                owed,
                PrestageState::Refused,
                format!("could not start the pre-stage: {e}"),
            )
        }
    };
    if let Some(sig) = signal_of(&status) {
        return Staged::ended(owed, PrestageState::Interrupted, Prestage::Interrupted(sig));
    }
    match status.code() {
        Some(0) => {
            // `ran` when gates ran meanwhile: a rehearsal this pre-stage
            // started, or one already running that it followed. `stamped`
            // when every gate already had its evidence.
            let followed = before.is_some_and(|(_, _, running)| running);
            let state = if followed
                || last_rehearsal().map(|(p, t, _)| (p, t)) != before.map(|(p, t, _)| (p, t))
            {
                PrestageState::Ran
            } else {
                PrestageState::Stamped
            };
            Staged::ended(owed, state, Prestage::Ready)
        }
        Some(1) => Staged::ended(
            owed,
            PrestageState::Failed,
            Prestage::Refused(
                "a push gate failed before the push (named above); nothing was pushed".to_string(),
                vec!["amont rehearse --status".to_string(), again],
            ),
        ),
        // `rehearse` reports a gate stopped by signal n as 128+n.
        Some(code) if code > 128 => Staged::ended(
            owed,
            PrestageState::Interrupted,
            Prestage::Interrupted(code - 128),
        ),
        code => Staged::refused(
            owed,
            PrestageState::Refused,
            format!(
                "the pre-stage could not run (exit {}); nothing was pushed",
                code.map_or("by signal".to_string(), |c| c.to_string())
            ),
        ),
    }
}

fn has_commit(oid: &str) -> bool {
    git::succeeds(&["cat-file", "-e", &format!("{oid}^{{commit}}")])
}

/// `amont land --tag <name>` (ADR-0018): pre-stage the push of one existing
/// local tag (release-mode audits for a `v*` tag, on a snapshot of the tagged
/// commit), push it, and read it back. A tag already on the remote is refused:
/// a tag is never moved.
/// Steps 1 and 2 of a tag land: the local tag and the one push URL. The
/// report so far, the URL and the tag's oid, or the finished refusal.
fn resolve_tag(name: &str) -> Result<(Report, String, String), Box<Report>> {
    let mut report = Report::new(Status::Refused);
    let dst = format!("refs/tags/{name}");
    report.dst = dst.clone();
    let Some(oid) = git::stdout(&["rev-parse", "--verify", "-q", &dst]) else {
        return Err(Box::new(refuse(
            report,
            &format!("there is no local tag {name:?}"),
            &["git tag --list"],
        )));
    };
    report.pushed = oid.clone();
    let remote = git::stdout(&["symbolic-ref", "--quiet", "--short", "HEAD"])
        .and_then(|b| git::stdout(&["config", &format!("branch.{b}.remote")]))
        .filter(|r| r != ".")
        .unwrap_or_else(|| "origin".to_string());
    report.remote = remote.clone();
    let (url, report) = one_push_url(&remote, report)?;
    Ok((report, url, oid))
}

/// `amont land --tag <name> --dry-run`: steps 1 and 2 only. Nothing is read
/// from the network and nothing is pushed, so the verdict is `would-land`.
fn dry_run_tag(name: &str) -> (Report, &'static str) {
    match resolve_tag(name) {
        Ok((report, _, _)) => (report, "would-land"),
        Err(refused) => (*refused, "refused"),
    }
}

fn land_tag(name: &str) -> Report {
    let settings = crate::config::Settings::default();
    let budget = crate::hooks::common::network_probe_budget(&settings);
    let (mut report, url, oid) = match resolve_tag(name) {
        Ok(resolved) => resolved,
        Err(refused) => return *refused,
    };
    let (dst, remote) = (report.dst.clone(), report.remote.clone());
    match read_ref(&url, &dst, budget) {
        Seen::Tip(t) if t.is_empty() => {}
        Seen::Tip(t) => {
            report.remote_before = t.clone();
            return refuse(
                report,
                &format!("{dst} already exists on {remote}; a tag is never moved"),
                &[&format!("git ls-remote {remote} {dst}")],
            );
        }
        Seen::Unreachable => {
            report.status = Status::Unreachable;
            eprintln!("amont: land: could not read {dst} from {remote}");
            report.next = vec![format!("git ls-remote {remote} {dst}")];
            return report;
        }
    }
    report = match prestage(&dst, &oid, &dst, "", (&remote, &url)).into_report(report) {
        ControlFlow::Continue(report) => report,
        ControlFlow::Break(stopped) => return *stopped,
    };
    let land_env = format!("{oid} {ZEROS}");
    let run = push(&url, &dst, &dst, None, &land_env);
    match run.outcome {
        PushOutcome::Done => {}
        PushOutcome::Signal(sig) => {
            report.status = Status::Interrupted(sig);
            return report;
        }
        PushOutcome::Failed => {
            report.status = status_of_failed_push(&run.stderr);
            if run.seen.contains(LAND_EVIDENCE_MISSING) {
                // A rehearsal tests HEAD, never the tagged commit's release
                // audits: land the tag again, which runs them first.
                report.next = vec![format!("amont land --tag {name}")];
            }
            return report;
        }
    }
    let mut observed = Seen::Unreachable;
    for i in 0..=REREADS {
        if i > 0 {
            std::thread::sleep(crate::clock::scaled(REREAD_PAUSE));
        }
        observed = read_ref(&url, &dst, budget);
        if matches!(&observed, Seen::Tip(t) if *t == oid) {
            break;
        }
    }
    match observed {
        Seen::Tip(t) if t == oid => {
            report.remote_after = t;
            report.status = Status::Landed;
        }
        Seen::Tip(t) => {
            report.remote_after = t;
            report.status = Status::NotObserved;
            report.next = vec![format!("git ls-remote {remote} {dst}")];
        }
        Seen::Unreachable => {
            report.status = Status::Unverified;
            report.next = vec![format!("git ls-remote {remote} {dst}")];
        }
    }
    report
}

/// `--dry-run`: steps 1 and 2 of a land and nothing else. No network, no push,
/// no ref written, no receipt.
fn dry_run_branch() -> (Report, &'static str) {
    match resolve() {
        Ok((_, mut report)) => {
            report.pushed = git::stdout(&["rev-parse", "HEAD"]).unwrap_or_default();
            (report, "would-land")
        }
        Err(refused) => (*refused, "refused"),
    }
}

/// A dry run's talk, branch or tag alike: `next`, then the headline.
fn say_dry_run(report: &Report, verdict: &str) {
    for line in &report.next {
        eprintln!("    {line}");
    }
    eprintln!(
        "{}",
        verdict_word(verdict, "", &report.pushed, shown_ref(report))
    );
}

/// A dry run's document: the same fields for a branch and a tag.
fn dry_run_json(report: &Report, verdict: &str, mode: Mode) -> String {
    let dir = receipt_dir()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    json::object(&[
        json::string_field("branch", &report.branch),
        json::string_field("remote", &report.remote),
        json::string_field("push_url", &report.push_url),
        json::string_field("pushed", &report.pushed),
        json::string_field("dst", &report.dst),
        json::string_field("ref", &report.dst),
        json::bool_field("retried", false),
        json::string_field("status", verdict),
        json::string_field("receipt_dir", &dir),
        json::bool_field("lease", mode == Mode::Force),
    ])
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
    if report.branch.is_empty() && report.dst.is_empty() {
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
fn push(url: &str, src: &str, dst: &str, lease: Option<&str>, land_env: &str) -> PushRun {
    let mut cmd = Command::new("git");
    // What the pre-stage ran against, for the hook of THIS push (ADR-0018).
    cmd.env(crate::rehearsal::LAND_ENV, land_env);
    if !std::io::stdin().is_terminal() {
        let (pre, env) = git::remote_env(git::configured_ssh());
        cmd.args(pre).envs(env);
    }
    cmd.arg("push");
    if let Some(old) = lease {
        cmd.arg(format!("--force-with-lease={dst}:{old}"));
    }
    cmd.arg(url)
        .arg(format!("{src}:{dst}"))
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

#[cfg(test)]
mod tests {

    /// A credential in the push URL never reaches `land --json`.
    #[test]
    fn a_push_url_is_reported_without_its_credentials() {
        let r = super::redact_url;
        assert_eq!(
            r("https://x:tok@github.com/o/r.git"),
            "https://***@github.com/o/r.git"
        );
        assert_eq!(
            r("https://tok@github.com/o/r.git"),
            "https://***@github.com/o/r.git"
        );
        assert_eq!(r("ssh://git:pw@host/o/r"), "ssh://git:***@host/o/r");
        assert_eq!(
            r("ssh://git@github.com/o/r.git"),
            "ssh://git@github.com/o/r.git"
        );
        assert_eq!(r("git@github.com:o/r.git"), "git@github.com:o/r.git");
        assert_eq!(
            r("https://github.com/o/r.git"),
            "https://github.com/o/r.git"
        );
        assert_eq!(r("/tmp/bare.git"), "/tmp/bare.git");
    }
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
