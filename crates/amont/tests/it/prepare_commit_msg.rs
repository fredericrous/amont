//! prepare-commit-msg, ported from its zsh suite.

use crate::common;
use common::Repo;

fn msg_after(r: &Repo, file: &str, source: &[&str]) -> String {
    r.write(file, "subject\n");
    let mut args = vec![file];
    args.extend_from_slice(source);
    r.hook("prepare-commit-msg", &args);
    std::fs::read_to_string(r.path(file)).unwrap_or_default()
}

#[test]
fn appends_a_jira_id_from_the_branch() {
    let r = Repo::new();
    r.git(&["checkout", "-q", "-b", "feat/JIRA-1234-description"]);
    assert!(msg_after(&r, "MSG", &["magic"]).contains("JIRA-1234"));
}

#[test]
fn appends_a_bare_kanbanize_id() {
    let r = Repo::new();
    r.git(&["checkout", "-q", "-b", "fix/1234-something"]);
    assert!(msg_after(&r, "MSG", &["magic"]).contains("#id 1234"));
}

/// The bug found during the port: the shell tested `$?` after a pipeline ending
/// in `head -n 1`, which is head's status and therefore always 0 — so a branch
/// with no digits appended a dangling "Issue: #id " with an empty value.
#[test]
fn appends_nothing_when_the_branch_carries_no_id() {
    let r = Repo::new();
    r.git(&["checkout", "-q", "-b", "feat/no-digits-here"]);
    assert!(!msg_after(&r, "MSG", &["magic"]).contains("Issue"));
}

/// The five known sources mean the text did not come from the author now.
#[test]
fn known_commit_sources_are_left_alone() {
    let r = Repo::new();
    r.git(&["checkout", "-q", "-b", "feat/JIRA-1234-x"]);
    for source in ["message", "template", "merge", "squash", "commit"] {
        assert!(
            !msg_after(&r, "MSG", &[source]).contains("JIRA-1234"),
            "source {source} should be skipped"
        );
    }
}

/// An UNRECOGNISED source falls through to the append — the shell's `case` did,
/// which is why its test passes "magic". Skipping on "not empty" instead would
/// silently disable the hook for any source git adds later.
#[test]
fn an_unrecognised_source_still_appends() {
    let r = Repo::new();
    r.git(&["checkout", "-q", "-b", "feat/JIRA-1234-x"]);
    assert!(msg_after(&r, "MSG", &["magic"]).contains("JIRA-1234"));
}
