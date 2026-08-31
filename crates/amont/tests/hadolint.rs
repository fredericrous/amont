//! pre-commit-hadolint.

mod common;
use common::Repo;

/// A blatantly bad Dockerfile must block, and a clean one must not.
///
/// `FROM alpine` unpinned is DL3006 and `RUN cd` is DL3003; hadolint's default
/// failure threshold is `info`, so either is enough to fail. The clean case
/// pins the tag and does nothing else, so no rule applies.
#[test]
fn a_bad_dockerfile_blocks_and_a_clean_one_does_not() {
    if common::missing("hadolint") {
        return; // the Unavailable path is covered below
    }
    let r = Repo::new();
    r.stage("Dockerfile", "FROM alpine\nRUN cd /tmp && echo hi\n");
    let run = r.hook("pre-commit", &[]);
    assert!(
        !run.passed(),
        "a hadolint finding must block: {}",
        run.output()
    );
    assert!(run.says("hadolint"), "{}", run.output());

    let r = Repo::new();
    r.stage("Dockerfile", "FROM alpine:3.19\nRUN echo hi\n");
    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "a clean Dockerfile passes: {}", run.output());
}

/// Out of scope is SILENT: a repository with no Dockerfile must not hear about
/// a Dockerfile linter at all.
#[test]
fn a_commit_touching_no_dockerfile_says_nothing_about_it() {
    let r = Repo::new();
    r.stage("notes.md", "no docker here\n");
    let run = r.hook("pre-commit", &[]);
    assert!(!run.says("hadolint"), "{}", run.output());
    assert!(run.passed(), "{}", run.output());
}

/// The documented limit, pinned so it stays a decision rather than a surprise:
/// scope name tokens are exact basenames, so a suffixed Dockerfile is NOT
/// matched. Stated in docs/checks.md; asserted here.
#[test]
fn a_suffixed_dockerfile_is_deliberately_not_matched() {
    let r = Repo::new();
    // Bad enough to fail hadolint if it were ever handed to it.
    r.stage("Dockerfile.dev", "FROM alpine\nRUN cd /tmp\n");
    let run = r.hook("pre-commit", &[]);
    assert!(
        !run.says("hadolint"),
        "Dockerfile.dev is out of scope by design: {}",
        run.output()
    );
    assert!(run.passed(), "{}", run.output());
}

/// A missing tool is `Unavailable`, never `Failed` — a check that could not run
/// has not found anything, and blocking a commit over amont's own plumbing is
/// the failure mode `staged_files` documents at length.
#[test]
fn it_is_unavailable_not_failed_without_the_tool() {
    if !common::missing("hadolint") {
        return;
    }
    let r = Repo::new();
    r.stage("Dockerfile", "FROM alpine\nRUN cd /tmp\n");
    let run = r.hook("pre-commit", &[]);
    assert!(
        run.passed(),
        "a missing tool must not block: {}",
        run.output()
    );
    assert!(run.says("not installed"), "{}", run.output());
}
