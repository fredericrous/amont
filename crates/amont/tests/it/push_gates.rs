//! ADR-0018: heavy gates run before the push connection.
//!
//! - A bare push whose heavy gates (scoped ones its range touches) carry no
//!   stamp refuses at once under `amont.pushGates refuse` (the default), runs
//!   none of them, and names `amont land`. `inline` keeps the old behaviour.
//! - The hook of `amont land`'s own push (`AMONT_LAND`) refuses with a
//!   sentinel land matches: `remote-moved` only when the range's remote side is
//!   not what the pre-stage saw, `evidence-missing` otherwise.
//! - `amont land` runs the owed gates first, then pushes; a failing gate or
//!   `amont.pushStamps false` pushes nothing.
//! - An unwalkable range fails closed; a deletion is never refused.
//!
//! Gates are a node script appending to an ABSOLUTE log: inside a rehearsal
//! snapshot the working directory is the snapshot, not the clone.

use crate::common::{self, missing, Repo};
use std::process::Stdio;

const SKIP: &str = "passed on this exact tree earlier — not repeating it here";

fn head(r: &Repo) -> String {
    String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string()
}

/// Run the pre-push hook on one ref line, with `env` added. Returns the exit
/// code, stdout and stderr.
fn hook(r: &Repo, line: &str, env: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = common::amont_cmd();
    cmd.arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-push")
        .current_dir(&r.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn");
    {
        use std::io::Write;
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(line.as_bytes())
            .expect("write");
    }
    let out = child.wait_with_output().expect("wait");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn push_line(from: &str, to: &str) -> String {
    format!("refs/heads/feat/x {to} refs/heads/feat/x {from}\n")
}

fn runs(log: &std::path::Path) -> usize {
    std::fs::read_to_string(log).map(|s| s.len()).unwrap_or(0)
}

/// A repo with one pre-push gate scoped to `*.txt` (severity `sev`, exiting
/// `exit`), logging each run to an absolute path, and one `.txt` commit to
/// push. Returns the repo, the base sha and the log path.
fn gated(sev: &str, exit: i32) -> (Repo, String, std::path::PathBuf) {
    let r = Repo::new();
    let log = r.path(".git/gate.log");
    r.stage(
        "gate.js",
        &format!(
            "require('fs').appendFileSync({:?},'x'); process.exit({exit})\n",
            log.to_str().expect("utf8")
        ),
    );
    r.commit("chore: base");
    let base = head(&r);
    // What a rehearsal or `run --stamp` diffs against when the branch has no
    // upstream: the remote's default branch, here at the base commit.
    r.git(&["update-ref", "refs/remotes/origin/main", &base]);
    r.stage(
        "amont.conf",
        &format!("pre-push    suite  *.txt  {sev}  node gate.js\n"),
    );
    r.commit("chore: the gate");
    crate::tree_gates::trust_and_install(&r);
    r.stage("a.txt", "hello\n");
    r.commit("feat: something to push");
    (r, base, log)
}

#[test]
fn a_bare_push_refuses_an_unstamped_heavy_gate_and_runs_none() {
    if missing("node") {
        return;
    }
    let (r, base, log) = gated("block", 0);
    let (code, out, err) = hook(&r, &push_line(&base, &head(&r)), &[]);
    assert_ne!(code, 0, "{out}{err}");
    assert_eq!(runs(&log), 0, "no heavy gate starts in a refused push");
    assert!(err.contains("push gate has not run on"), "{err}");
    assert!(err.contains("pre-push-suite"), "{err}");
    assert!(err.contains("amont land"), "{err}");
    assert!(err.contains("amont rehearse --wait"), "{err}");
    for line in err.lines().filter(|l| l.contains("amont land")) {
        assert!(line.chars().count() <= 80, "{line:?}");
    }
}

#[test]
fn inline_runs_the_gate_in_the_push_as_before() {
    if missing("node") {
        return;
    }
    let (r, base, log) = gated("block", 0);
    r.git(&["config", "amont.pushGates", "inline"]);
    let (code, out, err) = hook(&r, &push_line(&base, &head(&r)), &[]);
    assert_eq!(code, 0, "{out}{err}");
    assert_eq!(runs(&log), 1);
}

#[test]
fn a_stamped_tree_passes_a_refusing_push_without_running_the_gate() {
    if missing("node") {
        return;
    }
    let (r, base, log) = gated("block", 0);
    // Earn the stamp the way a person would: rehearse.
    let out = common::amont_cmd()
        .args(["run", "--stamp"])
        .current_dir(&r.dir)
        .stdin(Stdio::null())
        .output()
        .expect("amont run --stamp");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let before = runs(&log);
    let (code, so, se) = hook(&r, &push_line(&base, &head(&r)), &[]);
    assert_eq!(code, 0, "{so}{se}");
    assert!(so.contains(SKIP) || se.contains(SKIP), "{so}{se}");
    assert_eq!(runs(&log), before, "the stamped gate is not run again");
}

#[test]
fn stamp_reuse_off_names_the_setting_and_never_offers_a_rehearsal() {
    if missing("node") {
        return;
    }
    let (r, base, log) = gated("block", 0);
    r.git(&["config", "amont.pushStamps", "false"]);
    let (code, out, err) = hook(&r, &push_line(&base, &head(&r)), &[]);
    assert_ne!(code, 0, "{out}{err}");
    assert_eq!(runs(&log), 0);
    assert!(err.contains("amont.pushStamps"), "{err}");
    assert!(!err.contains("amont rehearse --wait"), "{err}");
}

#[test]
fn lands_own_push_with_an_unchanged_range_reports_missing_evidence_not_a_moved_remote() {
    if missing("node") {
        return;
    }
    let (r, base, log) = gated("block", 0);
    let tip = head(&r);
    let land = format!("{tip} {base}");
    let (code, out, err) = hook(&r, &push_line(&base, &tip), &[("AMONT_LAND", &land)]);
    assert_ne!(code, 0, "{out}{err}");
    assert_eq!(runs(&log), 0);
    assert!(err.contains("amont-land: evidence-missing"), "{err}");
    assert!(!err.contains("amont-land: remote-moved"), "{err}");
}

#[test]
fn lands_own_push_whose_remote_side_moved_says_so() {
    if missing("node") {
        return;
    }
    let (r, base, log) = gated("block", 0);
    let tip = head(&r);
    // The pre-stage saw the branch as new; the push finds `base` there.
    let land = format!("{tip} 0000000000000000000000000000000000000000");
    let (code, out, err) = hook(&r, &push_line(&base, &tip), &[("AMONT_LAND", &land)]);
    assert_ne!(code, 0, "{out}{err}");
    assert_eq!(runs(&log), 0);
    assert!(err.contains("amont-land: remote-moved"), "{err}");
}

#[test]
fn a_deletion_is_never_refused() {
    if missing("node") {
        return;
    }
    let (r, base, log) = gated("block", 0);
    let zeros = "0000000000000000000000000000000000000000";
    let line = format!("(delete) {zeros} refs/heads/feat/x {base}\n");
    let (code, out, err) = hook(&r, &line, &[]);
    assert_eq!(code, 0, "{out}{err}");
    assert_eq!(runs(&log), 0);
}

#[test]
fn an_unwalkable_range_fails_closed_and_refuses() {
    if missing("node") {
        return;
    }
    let (r, _base, log) = gated("block", 0);
    // A remote tip this clone never fetched: the range cannot be walked, and
    // "nothing changed" would skip every gate.
    let ghost = "1234567890123456789012345678901234567890";
    let (code, out, err) = hook(&r, &push_line(ghost, &head(&r)), &[]);
    assert_ne!(code, 0, "{out}{err}");
    assert_eq!(runs(&log), 0);
    assert!(err.contains("pre-push-suite"), "{err}");
}

#[test]
fn a_warning_gate_rehearsed_earlier_is_not_asked_for_again_and_is_not_a_pass() {
    if missing("node") {
        return;
    }
    let (r, base, log) = gated("warn", 1);
    let out = common::amont_cmd()
        .args(["rehearse", "--wait"])
        .current_dir(&r.dir)
        .stdin(Stdio::null())
        .output()
        .expect("amont rehearse");
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let after_rehearsal = runs(&log);
    assert_eq!(after_rehearsal, 1, "the rehearsal ran the gate once");
    let (code, so, se) = hook(&r, &push_line(&base, &head(&r)), &[]);
    assert_eq!(code, 0, "a gate that completed is not refused: {so}{se}");
    assert_eq!(runs(&log), after_rehearsal, "and not run again");
    // Never a pass: no push stamp names it.
    let tree = String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD^{tree}"]).stdout)
        .trim()
        .to_string();
    // The note's first line holds the pass tokens; later lines are run
    // evidence, which names every gate that ran, failures included.
    let stamp = r.git(&["notes", "--ref", "amont-gate", "show", &tree]);
    let body = String::from_utf8_lossy(&stamp.stdout).into_owned();
    let tokens = body.lines().next().unwrap_or("");
    assert!(
        !tokens.contains("pre-push-suite"),
        "a warning is recorded as completed, never stamped as passed: {body}"
    );
    let done = r.git(&["notes", "--ref", "amont-gate-done", "show", &tree]);
    assert!(
        String::from_utf8_lossy(&done.stdout).contains("pre-push-suite"),
        "the completion is recorded in its own ref"
    );
}

/// A land fixture: a branch whose push owes the `*.txt` gate, hooks installed.
fn land_fixture(exit: i32) -> (crate::land::Fx, std::path::PathBuf) {
    let fx = crate::land::fixture("feat/gated");
    let log = fx.r.path(".git/gate.log");
    fx.r.stage(
        "gate.js",
        &format!(
            "require('fs').appendFileSync({:?},'x'); process.exit({exit})\n",
            log.to_str().expect("utf8")
        ),
    );
    fx.r.stage(
        "amont.conf",
        "pre-push    suite  *.txt  block  node gate.js\n",
    );
    fx.r.commit("chore: the gate");
    crate::tree_gates::trust_and_install(&fx.r);
    (fx, log)
}

#[test]
fn land_runs_the_gate_before_the_push_and_the_push_does_not_repeat_it() {
    if missing("node") {
        return;
    }
    let (fx, log) = land_fixture(0);
    let ran = crate::land::land(&fx, &["--json"]);
    assert_eq!(ran.code, 0, "{}{}", ran.stdout, ran.stderr);
    assert_eq!(runs(&log), 1, "once, in the pre-stage: {}", ran.stderr);
    assert!(ran.stderr.contains("before git connects"), "{}", ran.stderr);
    assert!(
        ran.stderr.contains(SKIP),
        "the push's hook skips it: {}",
        ran.stderr
    );
    assert_eq!(crate::land::tip(&fx, "feat/gated"), crate::land::head(&fx));
    // stdout is the report alone, one JSON document.
    let doc = ran.stdout.trim();
    assert!(doc.starts_with('{') && doc.ends_with('}'), "{doc}");
    assert!(
        doc.contains("\"gates\":{\"owed\":[\"pre-push-suite\"],\"prestage\":\"ran\"}"),
        "{doc}"
    );
}

#[test]
fn a_failing_gate_in_the_pre_stage_pushes_nothing() {
    if missing("node") {
        return;
    }
    let (fx, log) = land_fixture(1);
    let ran = crate::land::land(&fx, &["--json"]);
    assert_eq!(ran.code, 1, "{}{}", ran.stdout, ran.stderr);
    assert_eq!(runs(&log), 1);
    let pushed = std::process::Command::new("git")
        .args(["rev-parse", "--verify", "-q", "refs/heads/feat/gated"])
        .current_dir(&fx.bare)
        .output()
        .expect("git");
    assert!(!pushed.status.success(), "nothing reached the origin");
    assert!(
        ran.stdout.contains("\"prestage\":\"failed\""),
        "{}",
        ran.stdout
    );
}

#[test]
fn land_with_stamp_reuse_off_refuses_before_any_gate_runs() {
    if missing("node") {
        return;
    }
    let (fx, log) = land_fixture(0);
    fx.r.git(&["config", "amont.pushStamps", "false"]);
    let ran = crate::land::land(&fx, &[]);
    assert_eq!(ran.code, 1, "{}{}", ran.stdout, ran.stderr);
    assert_eq!(runs(&log), 0);
    assert!(ran.stderr.contains("amont.pushStamps"), "{}", ran.stderr);
}

#[test]
fn a_tag_push_of_an_unstamped_commit_refuses_until_run_stamp_stamps_it() {
    if missing("node") {
        return;
    }
    let (r, _base, log) = gated("block", 0);
    let tip = head(&r);
    let zeros = "0000000000000000000000000000000000000000";
    let line = format!("refs/tags/v1.0.0 {tip} refs/tags/v1.0.0 {zeros}\n");
    let (code, out, err) = hook(&r, &line, &[]);
    assert_ne!(code, 0, "{out}{err}");
    assert_eq!(runs(&log), 0);
    assert!(err.contains("pre-push-suite"), "{err}");
    let stamped = common::amont_cmd()
        .args(["run", "--stamp"])
        .current_dir(&r.dir)
        .stdin(Stdio::null())
        .output()
        .expect("amont run --stamp");
    assert!(
        stamped.status.success(),
        "{}",
        String::from_utf8_lossy(&stamped.stderr)
    );
    let (code, out, err) = hook(&r, &line, &[]);
    assert_eq!(code, 0, "the stamped tag push passes: {out}{err}");
}
