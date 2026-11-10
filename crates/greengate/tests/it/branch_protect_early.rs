//! pre-commit-branch-protect — the push-time refusal, said at the commit
//! instead of after the work is stacked on `main`.
//!
//! As with `branch_pattern_early`, the states it must stay QUIET in matter
//! more than the one it speaks in: a warning that fires on detached heads
//! or remoteless repositories trains people to stop reading it.
//!
//! The states a push journey passes through (remoteless, never pushed, on
//! `main` once the remote has it, a feature branch the remote has) are
//! steps of `flows/push.rs`; the edge cases stay here.

use crate::common;
use common::Repo;

fn run_check(r: &Repo) -> (i32, String) {
    let out = crate::common::amont_cmd()
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

fn on_main(r: &Repo) {
    r.stage("a.txt", "x\n");
    r.git(&["commit", "-m", "chore: init"]);
    r.git(&["branch", "-M", "main"]);
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
