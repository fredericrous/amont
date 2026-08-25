//! pre-commit-branch-protect — the push-time refusal, said at the commit
//! instead of after the work is stacked on `main`.
//!
//! As with `branch_pattern_early`, the states it must stay QUIET in matter
//! more than the one it speaks in: a warning that fires on detached heads
//! or remoteless repositories trains people to stop reading it.

mod common;
use common::Repo;
use std::process::Command;

fn run_check(r: &Repo) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("run")
        .arg("pre-commit-branch-protect")
        .current_dir(&r.dir)
        .output()
        .expect("amont run");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn with_remote(r: &Repo) {
    // A configured remote is all the check consults; it never touches the
    // network, so the URL can be nonsense.
    r.git(&["remote", "add", "origin", "/nowhere/in/particular"]);
}

fn on_main(r: &Repo) {
    r.stage("a.txt", "x\n");
    r.git(&["commit", "-m", "chore: init"]);
    r.git(&["branch", "-M", "main"]);
}

/// The one state it speaks in: on `main`, with a remote that will refuse the
/// push — warned, with the move command, and NOT blocking.
#[test]
fn a_commit_on_main_warns_without_blocking() {
    let r = Repo::new();
    on_main(&r);
    with_remote(&r);
    let (code, out) = run_check(&r);
    assert_eq!(code, 0, "a warning must not block: {out}");
    assert!(out.contains("will be refused"), "{out}");
    assert!(out.contains("git switch -c"), "{out}");
}

#[test]
fn master_is_protected_too() {
    let r = Repo::new();
    on_main(&r);
    r.git(&["branch", "-M", "master"]);
    with_remote(&r);
    let (code, out) = run_check(&r);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("will be refused"), "{out}");
}

/// Any other branch passes, and says so.
#[test]
fn a_feature_branch_passes() {
    let r = Repo::new();
    on_main(&r);
    r.git(&["checkout", "-q", "-b", "feat/good-name"]);
    with_remote(&r);
    let (code, out) = run_check(&r);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("refused"), "{out}");
}

/// A name that merely starts with `main` is not `main`.
#[test]
fn a_lookalike_name_is_not_protected() {
    let r = Repo::new();
    on_main(&r);
    r.git(&["checkout", "-q", "-b", "maintenance"]);
    with_remote(&r);
    let (_, out) = run_check(&r);
    assert!(!out.contains("refused"), "{out}");
}

/// No remote, no push, nothing for the contract to gate.
#[test]
fn a_remoteless_repository_is_quiet() {
    let r = Repo::new();
    on_main(&r);
    let (code, out) = run_check(&r);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("refused"), "{out}");
}

/// A detached head names no branch.
#[test]
fn a_detached_head_is_quiet() {
    let r = Repo::new();
    on_main(&r);
    with_remote(&r);
    r.git(&["checkout", "-q", "--detach"]);
    let (code, out) = run_check(&r);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("refused"), "{out}");
}
