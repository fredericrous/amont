//! Severity: a check that fails without failing the commit.
//!
//! `hook.skip` already existed for "do not run this". Severity is the opposite
//! trade: keep running it, keep reading its output, stop treating its verdict as
//! fatal. The distinction is worth pinning because the cheap implementation of
//! "make it non-blocking" is to skip it, and that silently deletes the signal
//! everyone thought they were keeping.

use crate::common;
use common::Repo;
use std::io::Write;
use std::process::Stdio;

/// Content that makes `pre-commit-merge-conflict` fail. Built rather than
/// written literally, or this file trips the very check it exercises.
fn conflicted() -> String {
    format!(
        "{}\nours\n{}\ntheirs\n{}\n",
        "<".repeat(7),
        "=".repeat(7),
        ">".repeat(7)
    )
}

fn pre_push(r: &Repo) -> (i32, String) {
    let mut child = crate::common::amont_cmd()
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-push")
        .current_dir(&r.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        // NOT refs/heads/main: branch-protect runs first and would block on
        // that, so the chain would end before branch-pattern ever spoke.
        //
        // An all-zero REMOTE oid, because `branch-pattern` judges the ref being
        // pushed rather than the branch checked out, and a non-zero remote oid
        // means the branch is already on the server — nothing left to validate.
        // The old line said `bbb`, which under the corrected check is a pass.
        .write_all(
            b"refs/heads/nonsense-branch-name aaa refs/heads/nonsense-branch-name \
              0000000000000000000000000000000000000000\n",
        )
        .expect("write");
    let out = child.wait_with_output().expect("wait");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// The baseline the downgrade is measured against. Without this, a test that
/// only asserts "exit 0 with the override" would also pass against a check that
/// never failed in the first place.
#[test]
fn the_check_blocks_by_default() {
    let r = Repo::new();
    r.stage("bad.txt", &conflicted());
    let run = r.hook("pre-commit", &[]);
    assert!(!run.passed(), "expected a block, got:\n{}", run.output());
}

#[test]
fn severity_warn_reports_without_blocking() {
    let r = Repo::new();
    r.git(&["config", "amont.severity.pre-commit-merge-conflict", "warn"]);
    r.stage("bad.txt", &conflicted());
    let run = r.hook("pre-commit", &[]);

    assert!(run.passed(), "should not block:\n{}", run.output());
    // Still ran, and still said so — the whole point of preferring this over a
    // skip.
    assert!(
        run.says("conflict"),
        "the check went quiet instead of warning:\n{}",
        run.output()
    );
    assert!(
        run.says("set to warn"),
        "nothing explained why an error message did not block:\n{}",
        run.output()
    );
}

/// `block` is the default, so an explicit `block` can only be proved by writing
/// it over a `warn` — otherwise the assertion passes on an unset key.
#[test]
fn severity_block_overrides_a_broader_warn() {
    let r = Repo::new();
    r.git(&["config", "amont.severity.pre-commit-merge-conflict", "warn"]);
    r.git(&[
        "config",
        "--replace-all",
        "amont.severity.pre-commit-merge-conflict",
        "block",
    ]);
    r.stage("bad.txt", &conflicted());
    assert!(!r.hook("pre-commit", &[]).passed());
}

/// A value that is neither `warn` nor `block` must not silently disable a
/// check. Typos in git config are unreported by git itself.
#[test]
fn an_unrecognised_severity_keeps_the_declared_one() {
    let r = Repo::new();
    r.git(&[
        "config",
        "amont.severity.pre-commit-merge-conflict",
        "advisory",
    ]);
    r.stage("bad.txt", &conflicted());
    let run = r.hook("pre-commit", &[]);
    assert!(
        !run.passed(),
        "a misspelt severity turned the check off:\n{}",
        run.output()
    );
}

/// pre-push stops at the FIRST blocking failure. A downgraded one must not stop
/// it — the loop has to reach the end, which a non-zero exit would disprove.
#[test]
fn a_warn_failure_does_not_end_the_pre_push_chain() {
    let r = Repo::new();
    r.stage("a.txt", "x\n");
    r.commit("init");
    r.git(&["checkout", "-q", "-b", "nonsense-branch-name"]);

    let (code, out) = pre_push(&r);
    assert_ne!(code, 0, "branch-pattern was expected to block:\n{out}");

    r.git(&["config", "amont.severity.pre-push-branch-pattern", "warn"]);
    let (code, out) = pre_push(&r);
    assert_eq!(code, 0, "the chain stopped at a warning:\n{out}");
    assert!(
        out.contains("severity warn"),
        "the downgrade was not announced:\n{out}"
    );
}

/// Severity is per-check. Setting one must not move another.
#[test]
fn the_override_is_scoped_to_one_check() {
    let r = Repo::new();
    r.git(&["config", "amont.severity.pre-commit-ban-terms", "warn"]);
    r.stage("bad.txt", &conflicted());
    assert!(
        !r.hook("pre-commit", &[]).passed(),
        "downgrading one check downgraded another"
    );
}
