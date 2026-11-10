//! pre-commit-merge-conflict, ported from its zsh suite plus the scoping cases
//! added in #29.

use crate::common;
use common::Repo;

/// BUILT, not written. A file that tests for conflict markers cannot contain
/// them literally or the hook flags it — which is exactly what happened on the
/// first commit of this file. The implementation solves it the same way; an
/// exclusion list would just be one more thing to keep current.
fn markers() -> (String, String, String) {
    ("<".repeat(7), "=".repeat(7), ">".repeat(7))
}

fn conflicted() -> String {
    let (lt, eq, gt) = markers();
    format!("a\n{lt} HEAD\nb\n{eq}\nc\n{gt} other\n")
}

#[test]
fn rejects_a_staged_file_with_markers() {
    let r = Repo::new();
    r.stage("bad.txt", &conflicted());
    assert!(!r.hook("pre-commit-merge-conflict", &[]).passed());
}

#[test]
fn passes_when_nothing_staged_has_markers() {
    let r = Repo::new();
    r.stage("ok.txt", "clean\n");
    assert!(r.hook("pre-commit-merge-conflict", &[]).passed());
}

/// The scope change (#29): a marker in a file this commit does not touch is not
/// this commit's problem. Under the old whole-index grep one bad tracked file
/// blocked every commit in the repo.
#[test]
fn ignores_markers_in_a_file_this_commit_does_not_stage() {
    let r = Repo::new();
    r.stage("untouched.txt", &conflicted());
    r.commit("seed a conflicted file");
    r.stage("other.txt", "unrelated\n");
    assert!(r.hook("pre-commit-merge-conflict", &[]).passed());
}

/// A file must carry ALL THREE markers — a lone rule of `=` in a document is
/// not a conflict.
#[test]
fn one_marker_alone_is_not_a_conflict() {
    let r = Repo::new();
    let (_, eq, _) = markers();
    r.stage("doc.md", &format!("title\n{eq}\nbody\n"));
    assert!(r.hook("pre-commit-merge-conflict", &[]).passed());
}
