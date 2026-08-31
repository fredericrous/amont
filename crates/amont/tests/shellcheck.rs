//! pre-commit-shellcheck.
//!
//! Its own binary, not a few more cases in `external.rs`: that file is at its
//! parallelism limit, and `tests/timing.rs` documents what happens when it is
//! pushed past it.

mod common;
use common::Repo;

fn have_shellcheck() -> bool {
    !common::missing("shellcheck")
}

#[test]
fn a_bad_script_blocks_and_a_good_one_does_not() {
    if !have_shellcheck() {
        return; // covered by `it_is_unavailable_not_failed_without_the_tool`
    }
    let r = Repo::new();
    // SC2086: unquoted $1. shellcheck's default rules catch this.
    r.stage("bad.sh", "#!/bin/sh\nrm -rf $1\n");
    let run = r.hook("pre-commit", &[]);
    assert!(
        !run.passed(),
        "a shellcheck finding must block: {}",
        run.output()
    );
    assert!(run.says("shellcheck"), "{}", run.output());

    let r = Repo::new();
    r.stage("good.sh", "#!/bin/sh\nrm -rf \"$1\"\n");
    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "a clean script passes: {}", run.output());
}

/// Out of scope is SILENT, not merely non-blocking: a repository with no shell
/// must not hear about a shell linter at all.
#[test]
fn a_commit_touching_no_shell_says_nothing_about_it() {
    let r = Repo::new();
    r.stage("notes.md", "no shell here\n");
    let run = r.hook("pre-commit", &[]);
    assert!(!run.says("shellcheck"), "{}", run.output());
    assert!(run.passed(), "{}", run.output());
}

/// A missing tool is `Unavailable`, never `Failed` — a check that cannot run
/// has not found anything, and blocking a commit over amont's own plumbing is
/// the failure mode `staged_files` documents at length.
#[test]
fn it_is_unavailable_not_failed_without_the_tool() {
    if have_shellcheck() {
        return;
    }
    let r = Repo::new();
    r.stage("bad.sh", "#!/bin/sh\nrm -rf $1\n");
    let run = r.hook("pre-commit", &[]);
    assert!(
        run.passed(),
        "a missing tool must not block: {}",
        run.output()
    );
    assert!(run.says("not installed"), "{}", run.output());
}
