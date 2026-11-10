//! pre-commit-branch-pattern — the push-time contract, said at the first
//! commit instead of after the work is stacked on a name that has to change.
//!
//! The states it must stay QUIET in matter more than the one it speaks in:
//! a warning that fires on detached heads, remoteless repositories or
//! branches the server already accepted trains people to stop reading it —
//! `usual-name`'s lesson, which this check is a second application of.
//!
//! A conforming name and an off-contract one are steps of `flows/push.rs`;
//! the quiet states stay here.

use crate::common;
use common::Repo;

fn run_check(r: &Repo) -> (i32, String) {
    let out = crate::common::amont_cmd()
        .arg("run")
        .arg("pre-commit-branch-pattern")
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

/// No remote, no contract to gate: a purely local repository stays quiet.
#[test]
fn a_remoteless_repository_is_quiet() {
    let r = Repo::new();
    r.stage("a.txt", "x\n");
    r.git(&["commit", "-m", "chore: init"]);
    r.git(&["checkout", "-q", "-b", "off-pattern"]);
    let (code, out) = run_check(&r);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("refused"), "{out}");
}

/// pre-push authorises a branch the server already has by its non-zero
/// remote oid; the remote-tracking ref is the local mirror of that fact, so
/// an off-contract name that predates the contract keeps committing quietly.
#[test]
fn a_branch_the_remote_already_has_is_quiet() {
    let r = Repo::new();
    r.stage("a.txt", "x\n");
    r.git(&["commit", "-m", "chore: init"]);
    r.git(&["checkout", "-q", "-b", "legacy-name"]);
    with_remote(&r);
    r.git(&["update-ref", "refs/remotes/origin/legacy-name", "HEAD"]);
    let (code, out) = run_check(&r);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("refused"), "{out}");
}

/// A detached head names no branch — rebase, cherry-pick and bisect all
/// commit from one, and none of those is the moment to discuss naming.
#[test]
fn a_detached_head_is_quiet() {
    let r = Repo::new();
    r.stage("a.txt", "x\n");
    r.git(&["commit", "-m", "chore: init"]);
    with_remote(&r);
    r.git(&["checkout", "-q", "--detach", "HEAD"]);
    let (code, out) = run_check(&r);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("refused"), "{out}");
}
