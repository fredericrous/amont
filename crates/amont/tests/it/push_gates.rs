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

// ---------------------------------------------------------------------------
// Phase 1b: dependency audits off the connection (ADR-0018). A repository
// tracking `go.sum` is opted into `audit-go`; a fake `govulncheck` logs each
// run to an absolute path and answers from marker files: `VULN` → exit 1 with
// a GO- id (a vulnerability), `OFFLINE` → exit 1 with no id (could not check),
// neither → exit 0 (clean).
// ---------------------------------------------------------------------------

/// A land fixture opted into audit-go, and the fake tool's bin dir and log.
fn audit_fixture() -> (crate::land::Fx, std::path::PathBuf, std::path::PathBuf) {
    let fx = crate::land::fixture("feat/audited");
    let bin = fx.r.path(".git/fakebin");
    std::fs::create_dir_all(&bin).expect("mkdir");
    let log = fx.r.path(".git/audit.log");
    let marks = fx.r.path(".git");
    let tool = bin.join("govulncheck");
    std::fs::write(
        &tool,
        format!(
            "#!/bin/sh\nprintf x >> {log:?}\n\
             if [ -e {m:?}/VULN ]; then echo 'Vulnerability #1: GO-2026-0001'; exit 3; fi\n\
             if [ -e {m:?}/OFFLINE ]; then echo 'fetching vulndb: no route to host'; exit 1; fi\n\
             echo 'No vulnerabilities found.'\n",
            log = log.to_str().expect("utf8"),
            m = marks.to_str().expect("utf8"),
        ),
    )
    .expect("write tool");
    common::fixture_exec(&tool, &std::fs::read_to_string(&tool).expect("read"));
    fx.r.stage("go.sum", "example.com/m v1.0.0 h1:abc=\n");
    fx.r.stage("go.mod", "module example.com/x\n");
    fx.r.commit("chore: go module");
    crate::tree_gates::trust_and_install(&fx.r);
    (fx, bin, log)
}

fn audit_records(r: &Repo) -> String {
    String::from_utf8_lossy(&r.git(&["notes", "--ref", "amont-audit", "list"]).stdout).into_owned()
}

#[test]
fn land_runs_the_audit_once_and_a_later_docs_push_replays_it() {
    let (fx, bin, log) = audit_fixture();
    let ran = crate::land::land_with(&fx, &["--json"], Some(&bin));
    assert_eq!(ran.code, 0, "{}{}", ran.stdout, ran.stderr);
    assert_eq!(runs(&log), 1, "once, in the pre-stage: {}", ran.stderr);
    assert!(
        ran.stderr.contains("replaying the audit recorded"),
        "{}",
        ran.stderr
    );
    assert!(!audit_records(&fx.r).is_empty(), "a record was written");
    // A docs-only commit: same lockfiles, same key — a bare push replays.
    fx.r.stage("notes.md", "docs\n");
    fx.r.commit("docs: notes");
    let tip = crate::land::head(&fx);
    let base = crate::land::tip(&fx, "feat/audited");
    let (code, out, err) = hook_in(&fx.r, &push_line(&base, &tip), &bin, &[]);
    assert_eq!(code, 0, "{out}{err}");
    assert_eq!(runs(&log), 1, "the tool did not run again: {out}{err}");
}

#[test]
fn a_stale_record_is_owed_again_and_a_bare_push_refuses() {
    let (fx, bin, log) = audit_fixture();
    let ran = crate::land::land_with(&fx, &[], Some(&bin));
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    // Age every record by two days.
    for line in audit_records(&fx.r).lines() {
        let object = line.split_whitespace().nth(1).expect("object");
        let body = String::from_utf8_lossy(
            &fx.r
                .git(&["notes", "--ref", "amont-audit", "show", object])
                .stdout,
        )
        .into_owned();
        let mut fields: Vec<String> = body
            .lines()
            .next()
            .unwrap()
            .split(' ')
            .map(str::to_string)
            .collect();
        let at: i64 = fields[1].parse().unwrap();
        fields[1] = (at - 2 * 24 * 3600).to_string();
        let aged = std::iter::once(fields.join(" "))
            .chain(body.lines().skip(1).map(str::to_string))
            .collect::<Vec<_>>()
            .join("\n");
        fx.r.git(&[
            "notes",
            "--ref",
            "amont-audit",
            "add",
            "-f",
            "-m",
            &aged,
            object,
        ]);
    }
    let tip = crate::land::head(&fx);
    let (code, out, err) = hook_in(&fx.r, &push_line(&tip, &tip), &bin, &[]);
    let _ = out;
    assert_ne!(code, 0, "a stale audit is owed: {err}");
    assert!(err.contains("pre-push-audit-go"), "{err}");
    assert_eq!(runs(&log), 1, "and was not run on the connection");
}

#[test]
fn a_branch_record_does_not_answer_for_a_release_tag() {
    let (fx, bin, log) = audit_fixture();
    let ran = crate::land::land_with(&fx, &[], Some(&bin));
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    let tip = crate::land::head(&fx);
    let zeros = "0000000000000000000000000000000000000000";
    let line = format!("refs/tags/v1.0.0 {tip} refs/tags/v1.0.0 {zeros}\n");
    let (code, _out, err) = hook_in(&fx.r, &line, &bin, &[]);
    assert_ne!(code, 0, "{err}");
    assert!(err.contains("pre-push-audit-go"), "{err}");
    assert_eq!(runs(&log), 1);
}

#[test]
fn land_tag_runs_release_audits_on_the_tagged_commit_and_blocks_a_vulnerability() {
    let (fx, bin, log) = audit_fixture();
    fx.r.git(&["tag", "v1.0.0"]);
    std::fs::write(fx.r.path(".git/VULN"), "").expect("mark");
    let ran = crate::land::land_with(&fx, &["--tag", "v1.0.0"], Some(&bin));
    assert_eq!(
        ran.code, 1,
        "a v* tag does not ship a vulnerability: {}",
        ran.stderr
    );
    assert_eq!(runs(&log), 1);
    let on_remote = std::process::Command::new("git")
        .args(["rev-parse", "--verify", "-q", "refs/tags/v1.0.0"])
        .current_dir(&fx.bare)
        .output()
        .expect("git");
    assert!(!on_remote.status.success(), "nothing pushed");
}

#[test]
fn land_tag_lands_a_clean_tag_and_refuses_one_already_on_the_remote() {
    let (fx, bin, _log) = audit_fixture();
    // Tag the commit BEFORE head, so the pre-stage must snapshot the tag, not HEAD.
    let tagged = crate::land::head(&fx);
    fx.r.stage("later.md", "after the tag\n");
    fx.r.commit("docs: after");
    fx.r.git(&["tag", "v1.0.0", &tagged]);
    let ran = crate::land::land_with(&fx, &["--tag", "v1.0.0", "--json"], Some(&bin));
    assert_eq!(ran.code, 0, "{}{}", ran.stdout, ran.stderr);
    let remote = crate::land::plain_git(&fx.bare, &["rev-parse", "refs/tags/v1.0.0"]);
    assert_eq!(remote, tagged);
    let again = crate::land::land_with(&fx, &["--tag", "v1.0.0"], Some(&bin));
    assert_eq!(again.code, 1, "{}", again.stderr);
    assert!(again.stderr.contains("already exists"), "{}", again.stderr);
}

#[test]
fn an_audit_that_could_not_check_never_blocks_a_land() {
    let (fx, bin, _log) = audit_fixture();
    std::fs::write(fx.r.path(".git/OFFLINE"), "").expect("mark");
    let ran = crate::land::land_with(&fx, &[], Some(&bin));
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(ran.stderr.contains("NOT checked"), "{}", ran.stderr);
}

#[test]
fn a_missing_audit_tool_never_blocks_a_land() {
    let (fx, _bin, _log) = audit_fixture();
    // An empty bin dir first on PATH, and no govulncheck anywhere on it.
    let empty = fx.r.path(".git/emptybin");
    std::fs::create_dir_all(&empty).expect("mkdir");
    if std::process::Command::new("sh")
        .args(["-c", "command -v govulncheck"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return; // this machine has a real govulncheck on PATH
    }
    let ran = crate::land::land_with(&fx, &[], Some(&empty));
    assert_eq!(ran.code, 0, "{}", ran.stderr);
}

#[test]
fn a_dirty_lockfile_writes_no_record() {
    let (fx, bin, log) = audit_fixture();
    fx.r.git(&["config", "amont.pushGates", "inline"]);
    std::fs::write(fx.r.path("go.sum"), "example.com/m v1.0.1 h1:def=\n").expect("dirty");
    let tip = crate::land::head(&fx);
    let (code, out, err) = hook_in(&fx.r, &push_line(&tip, &tip), &bin, &[]);
    assert_eq!(code, 0, "{out}{err}");
    assert_eq!(runs(&log), 1, "inline, the audit ran");
    assert!(
        audit_records(&fx.r).is_empty(),
        "but nothing was recorded for a tree it did not read"
    );
}

/// `hook` with `bin` first on PATH.
fn hook_in(
    r: &Repo,
    line: &str,
    bin: &std::path::Path,
    env: &[(&str, &str)],
) -> (i32, String, String) {
    let rest = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(bin.to_path_buf()).chain(std::env::split_paths(&rest)),
    )
    .expect("PATH");
    let path = path.to_string_lossy().into_owned();
    let mut all: Vec<(&str, &str)> = vec![("PATH", path.as_str())];
    all.extend_from_slice(env);
    hook(r, line, &all)
}

#[test]
fn neither_land_variable_reaches_a_gate() {
    if missing("node") {
        return;
    }
    let fx = crate::land::fixture("feat/env");
    let seen = fx.r.path(".git/env.log");
    fx.r.stage(
        "gate.js",
        &format!(
            "require('fs').appendFileSync({:?}, (process.env.AMONT_LAND||'-') + ' ' + (process.env.AMONT_LAND_PRESTAGE||'-') + '\\n')\n",
            seen.to_str().expect("utf8")
        ),
    );
    fx.r.stage(
        "amont.conf",
        "pre-push    suite  *.txt  block  node gate.js\n",
    );
    fx.r.commit("chore: the gate");
    crate::tree_gates::trust_and_install(&fx.r);
    let ran = crate::land::land(&fx, &[]);
    assert_eq!(ran.code, 0, "{}{}", ran.stdout, ran.stderr);
    let log = std::fs::read_to_string(&seen).expect("the gate ran");
    for line in log.lines() {
        assert_eq!(line, "- -", "a gate child saw a land variable: {log}");
    }
}

/// Age every audit record by `secs`.
fn age_audit_records(r: &Repo, secs: i64) {
    for line in audit_records(r).lines() {
        let object = line.split_whitespace().nth(1).expect("object");
        let body = String::from_utf8_lossy(
            &r.git(&["notes", "--ref", "amont-audit", "show", object])
                .stdout,
        )
        .into_owned();
        let mut fields: Vec<String> = body
            .lines()
            .next()
            .unwrap()
            .split(' ')
            .map(str::to_string)
            .collect();
        let at: i64 = fields[1].parse().unwrap();
        fields[1] = (at - secs).to_string();
        let aged = std::iter::once(fields.join(" "))
            .chain(body.lines().skip(1).map(str::to_string))
            .collect::<Vec<_>>()
            .join("\n");
        r.git(&[
            "notes",
            "--ref",
            "amont-audit",
            "add",
            "-f",
            "-m",
            &aged,
            object,
        ]);
    }
}

#[test]
fn a_lockfile_change_owes_the_audit_again() {
    let (fx, bin, log) = audit_fixture();
    let ran = crate::land::land_with(&fx, &[], Some(&bin));
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    let base = crate::land::head(&fx);
    fx.r.stage("go.sum", "example.com/m v1.0.1 h1:def=\n");
    fx.r.commit("fix(deps): bump");
    let (code, _out, err) = hook_in(&fx.r, &push_line(&base, &crate::land::head(&fx)), &bin, &[]);
    assert_ne!(code, 0, "a new lockfile has no record: {err}");
    assert!(err.contains("pre-push-audit-go"), "{err}");
    assert_eq!(runs(&log), 1, "and the tool did not run on the connection");
}

#[test]
fn a_could_not_check_record_expires_after_an_hour() {
    let (fx, bin, _log) = audit_fixture();
    std::fs::write(fx.r.path(".git/OFFLINE"), "").expect("mark");
    let ran = crate::land::land_with(&fx, &[], Some(&bin));
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    let tip = crate::land::head(&fx);
    // Fresh: the push replays "NOT checked".
    let (code, _o, err) = hook_in(&fx.r, &push_line(&tip, &tip), &bin, &[]);
    assert_eq!(code, 0, "{err}");
    // Two hours later it is owed again, though a clean record would still hold.
    age_audit_records(&fx.r, 2 * 3600);
    let (code, _o, err) = hook_in(&fx.r, &push_line(&tip, &tip), &bin, &[]);
    assert_ne!(code, 0, "{err}");
    assert!(err.contains("pre-push-audit-go"), "{err}");
}
