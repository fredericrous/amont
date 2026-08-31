//! pre-commit-hadolint.

mod common;
use common::Repo;

#[test]
fn a_dockerfile_is_judged_and_anything_else_is_not() {
    let r = Repo::new();
    r.stage("Dockerfile", "FROM alpine:3.19\nRUN echo hi\n");
    let run = r.hook("pre-commit", &[]);
    // Whether it passes depends on the tool being installed; what must hold
    // either way is that a missing tool never BLOCKS.
    if common::missing("hadolint") {
        assert!(
            run.passed(),
            "a missing tool must not block: {}",
            run.output()
        );
        assert!(run.says("not installed"), "{}", run.output());
    }

    let r = Repo::new();
    r.stage("notes.md", "no docker here\n");
    let run = r.hook("pre-commit", &[]);
    assert!(
        !run.says("hadolint"),
        "silent out of scope: {}",
        run.output()
    );
    assert!(run.passed(), "{}", run.output());
}

/// The documented limit, pinned so it is a decision rather than a surprise:
/// the scope's name tokens are exact basenames, so a suffixed Dockerfile is
/// NOT matched. Stated in docs/checks.md; asserted here.
#[test]
fn a_suffixed_dockerfile_is_deliberately_not_matched() {
    let r = Repo::new();
    r.stage("Dockerfile.dev", "FROM alpine\nRUN echo hi\n");
    let run = r.hook("pre-commit", &[]);
    assert!(
        !run.says("hadolint"),
        "Dockerfile.dev is out of scope by design: {}",
        run.output()
    );
    assert!(run.passed(), "{}", run.output());
}
