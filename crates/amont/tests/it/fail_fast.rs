//! Pre-commit runs its Light checks first and starts its Heavy ones only when
//! none of them blocked (ADR-0020, `hooks.pre-commit-fail-fast`).
//!
//! relais #394: `cargo-fmt` failed in 4 s and `module-cycles` in 2 s, and
//! `cargo test` still ran its whole 13 minutes for a commit already refused.
//! Each test here declares a Heavy gate that appends to `gate.log`, so its
//! run count is the evidence, and a Light gate whose exit code is the knob.

#![cfg(unix)]

use crate::common;
use common::Repo;

use std::process::Stdio;

/// The Heavy gate and a Light one, both on `*.txt`. `light_exit` decides the
/// Light gate's verdict; `light_severity` whether that verdict blocks.
fn repo(light_severity: &str, light_exit: u8, extra: &str) -> Repo {
    let r = Repo::new();
    r.stage("gate.sh", "printf x >> gate.log\n");
    r.stage("light.sh", &format!("exit {light_exit}\n"));
    r.stage(
        "amont.conf",
        &format!(
            "{extra}\
             pre-commit  light  *.txt  {light_severity}  sh light.sh\n\
             pre-commit  gate   *.txt  block  heavy sh gate.sh\n"
        ),
    );
    r.commit("chore: base");
    trust(&r);
    r
}

fn trust(r: &Repo) {
    let out = common::amont_cmd()
        .arg("trust")
        .current_dir(&r.dir)
        .stdin(Stdio::null())
        .output()
        .expect("amont trust");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Run the pre-commit DISPATCHER on what is staged; `env` adds variables.
fn pre_commit(r: &Repo, env: &[(&str, &str)]) -> (i32, String) {
    let mut cmd = common::amont_cmd();
    cmd.arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-commit")
        .current_dir(&r.dir)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("amont pre-commit");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn runs(r: &Repo) -> usize {
    std::fs::read_to_string(r.dir.join("gate.log"))
        .map(|s| s.len())
        .unwrap_or(0)
}

/// The commit-time stamp marker pre-commit leaves for post-commit.
fn marker(r: &Repo) -> String {
    std::fs::read_to_string(r.path(".git/amont-gate")).unwrap_or_default()
}

#[test]
fn a_blocking_light_failure_skips_the_heavy_gate_and_says_so() {
    let r = repo("block", 1, "");
    r.stage("a.txt", "one\n");
    let (code, out) = pre_commit(&r, &[]);
    assert_ne!(code, 0, "the Light failure still refuses the commit: {out}");
    assert_eq!(runs(&r), 0, "the Heavy gate must not start: {out}");
    assert!(
        out.contains("1 check(s) skipped because pre-commit-light failed: pre-commit-gate"),
        "{out}"
    );
    assert!(
        out.contains("to run one now:") && out.contains("    amont run pre-commit-gate"),
        "the next step names the command: {out}"
    );
    // The roll-up comes before the blame list, which stays last.
    let skip = out.find("check(s) skipped").expect("skip line");
    let blame = out.find("Error raised by").expect("blame list");
    assert!(skip < blame, "{out}");
    assert!(
        !marker(&r).lines().any(|t| t.starts_with("gate@")),
        "a skipped gate earns no stamp: {:?}",
        marker(&r)
    );
}

#[test]
fn passing_light_checks_let_the_heavy_gate_run_once() {
    let r = repo("block", 0, "");
    r.stage("a.txt", "one\n");
    let (code, out) = pre_commit(&r, &[]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(runs(&r), 1, "{out}");
    assert!(!out.contains("skipped"), "{out}");
    assert!(
        marker(&r).lines().any(|t| t.starts_with("gate@")),
        "the gate that ran is stamped: {:?}",
        marker(&r)
    );
}

#[test]
fn a_light_failure_at_warn_does_not_skip_the_heavy_gate() {
    let r = repo("warn", 1, "");
    r.stage("a.txt", "one\n");
    let (code, out) = pre_commit(&r, &[]);
    assert_eq!(code, 0, "a warn failure does not block: {out}");
    assert_eq!(runs(&r), 1, "{out}");
    assert!(!out.contains("check(s) skipped"), "{out}");
}

#[test]
fn fail_fast_off_runs_every_check_as_before() {
    let r = repo("block", 1, "");
    r.git(&["config", "amont.failFast", "false"]);
    r.stage("a.txt", "one\n");
    let (code, out) = pre_commit(&r, &[]);
    assert_ne!(code, 0, "{out}");
    assert_eq!(runs(&r), 1, "one wave: the Heavy gate ran: {out}");
    assert!(!out.contains("check(s) skipped"), "{out}");
}

#[test]
fn the_environment_outranks_the_manifest_for_one_run() {
    let r = repo("block", 1, "set failFast true\n");
    r.stage("a.txt", "one\n");
    let (code, out) = pre_commit(&r, &[("AMONT_FAIL_FAST", "false")]);
    assert_ne!(code, 0, "{out}");
    assert_eq!(runs(&r), 1, "AMONT_FAIL_FAST=false runs one wave: {out}");
    // And the manifest's word holds without it.
    let (_, out) = pre_commit(&r, &[]);
    assert!(out.contains("check(s) skipped"), "{out}");
    assert_eq!(runs(&r), 1, "{out}");
}

#[test]
fn a_manifest_can_turn_fail_fast_off() {
    let r = repo("block", 1, "set failFast false\n");
    r.stage("a.txt", "one\n");
    let (_, out) = pre_commit(&r, &[]);
    assert_eq!(runs(&r), 1, "{out}");
    assert!(!out.contains("check(s) skipped"), "{out}");
}

#[test]
fn quiet_mode_still_says_what_was_skipped() {
    let r = repo("block", 1, "");
    r.git(&["config", "amont.quiet", "always"]);
    r.stage("a.txt", "one\n");
    let (_, out) = pre_commit(&r, &[]);
    assert_eq!(runs(&r), 0, "{out}");
    assert!(out.contains("check(s) skipped"), "{out}");
}

#[test]
fn the_progress_view_ends_cleanly_with_a_skipped_gate() {
    for progress in ["true", "false"] {
        let r = repo("block", 1, "");
        r.git(&["config", "amont.progress", progress]);
        r.stage("a.txt", "one\n");
        let (code, out) = pre_commit(&r, &[]);
        assert_ne!(code, 0, "progress={progress}: {out}");
        assert_eq!(runs(&r), 0, "progress={progress}: {out}");
        assert!(
            out.contains("check(s) skipped"),
            "progress={progress}: {out}"
        );
    }
}

/// A Light fixer has restaged its repair when the Heavy wave starts, so the
/// Heavy gate judges the repaired index — the tree to be committed — and its
/// stamp, filed on that tree, is now true. With one wave the gate could run
/// beside the fixer, judge the unrepaired content, and still be stamped on
/// the repaired tree.
#[test]
fn the_heavy_gate_judges_what_a_light_fixer_restaged() {
    let r = Repo::new();
    crate::fixing::rewriter(&r, "rewrite.sh");
    r.stage("gate.sh", "git show :a.js >> seen.log\n");
    r.stage(
        "amont.conf",
        "pre-commit  fmt   *.js  block  fix ./rewrite.sh\n\
         pre-commit  gate  *.js  block  heavy sh gate.sh\n",
    );
    r.commit("chore: base");
    trust(&r);
    r.git(&["config", "amont.fix", "true"]);
    r.stage("a.js", "const a = BAD\n");
    let (code, out) = pre_commit(&r, &[]);
    assert_eq!(code, 0, "{out}");
    let seen = std::fs::read_to_string(r.dir.join("seen.log")).unwrap_or_default();
    assert_eq!(seen, "const a = GOOD\n", "{out}");
    assert!(
        marker(&r).lines().any(|t| t.starts_with("gate@")),
        "the gate judged the restaged tree, so it is stamped: {:?}",
        marker(&r)
    );
}

/// `amont run` shares the stage body, so the rehearsal skips as the commit
/// does.
#[test]
fn amont_run_skips_the_heavy_gate_the_same_way() {
    let r = repo("block", 1, "");
    r.stage("a.txt", "one\n");
    let out = common::amont_cmd()
        .arg("run")
        .current_dir(&r.dir)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .expect("amont run");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "{text}");
    assert_eq!(runs(&r), 0, "{text}");
    assert!(text.contains("check(s) skipped"), "{text}");
}

/// The wave ledger: one line per hook attempt with a Heavy check, naming
/// what was skipped. `amont run` writes nothing there.
#[test]
fn the_hook_records_the_wave_and_a_rehearsal_does_not() {
    let r = repo("block", 1, "");
    r.stage("a.txt", "one\n");
    let ledger = r.path(".git/amont-waves");
    let _rehearsal = common::amont_cmd() // its exit code is not under test
        .arg("run")
        .current_dir(&r.dir)
        .stdin(Stdio::null())
        .output()
        .expect("amont run");
    assert!(!ledger.exists(), "a rehearsal is not a commit");
    let _ = pre_commit(&r, &[]); // its exit code is not under test
    let text = std::fs::read_to_string(&ledger).expect("ledger");
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some("amont-waves-v1"), "{text}");
    let row: Vec<&str> = lines.next().expect("a row").split(' ').collect();
    assert_eq!(row.len(), 4, "{text}");
    assert_eq!(row[2], "1", "one Heavy check: {text}");
    assert_eq!(row[3], "pre-commit-gate", "{text}");
}
