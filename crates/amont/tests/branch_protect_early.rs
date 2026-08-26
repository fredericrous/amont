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

/// A remote that already carries this branch — the state in which a push
/// really would be refused, and therefore the only state the warning may
/// speak in.
///
/// The remote-tracking ref is written directly rather than fetched: the
/// check reads `refs/remotes/*/<branch>` and never touches the network, so
/// the URL can be nonsense and the ref can be forged. What matters is that
/// the two halves of the state are set together — a test that configured a
/// remote and stopped was asserting a warning about a refusal that would
/// not have happened.
fn with_remote(r: &Repo) {
    r.git(&["remote", "add", "origin", "/nowhere/in/particular"]);
    let out = |args: &[&str]| {
        String::from_utf8_lossy(&r.git(args).stdout)
            .trim()
            .to_string()
    };
    let head = out(&["rev-parse", "HEAD"]);
    let branch = out(&["symbolic-ref", "--short", "HEAD"]);
    r.git(&[
        "update-ref",
        &format!("refs/remotes/origin/{branch}"),
        &head,
    ]);
}

/// A remote is configured, but this branch has never been pushed to it.
fn with_remote_never_pushed(r: &Repo) {
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

/// The first commit of a new repository, before anything has been pushed.
///
/// `pre-push-branch-protect` allows the push that CREATES `main` on the
/// remote — there is no history there to protect and no PR to open against
/// — so a commit-time warning that the push "will be refused" is simply
/// false here, and would send somebody to `git switch -c` to escape a
/// refusal that is not coming.
#[test]
fn a_branch_never_pushed_anywhere_is_quiet() {
    let r = Repo::new();
    on_main(&r);
    with_remote_never_pushed(&r);
    let (code, out) = run_check(&r);
    assert_eq!(code, 0, "{out}");
    assert!(
        !out.contains("will be refused"),
        "nothing is refused until the branch exists on the remote: {out}"
    );
}
