//! pre-commit-usual-name, ported from its zsh suite.

use crate::common;
use common::Repo;

#[test]
fn warns_the_first_time_an_identity_commits() {
    let r = Repo::new();
    r.git(&["config", "user.name", "test all mighty"]);
    r.git(&["config", "user.email", "test@domain.test"]);
    r.stage("a.txt", "x\n");
    r.commit("feat: a");
    // same email, DIFFERENT name → not seen before
    r.git(&["config", "user.name", "test mighty"]);
    assert!(r.hook("pre-commit-usual-name", &[]).says("first time"));
}

#[test]
fn stays_quiet_for_an_identity_already_in_the_log() {
    let r = Repo::new();
    r.git(&["config", "user.name", "test all mighty"]);
    r.git(&["config", "user.email", "test@domain.test"]);
    r.stage("a.txt", "x\n");
    r.commit("feat: a");
    assert!(!r.hook("pre-commit-usual-name", &[]).says("first time"));
}

/// As a REGEX, "(dev)" is a group — so a name containing metacharacters would
/// match an author who never committed, and the warning this hook exists to
/// give would be silently withheld. Fixed-string containment cannot do that.
#[test]
fn a_name_with_regex_metacharacters_is_matched_literally() {
    let r = Repo::new();
    r.git(&["config", "user.name", "test dev all mighty"]);
    r.git(&["config", "user.email", "t@x.test"]);
    r.stage("a.txt", "x\n");
    r.commit("feat: a");
    // this name has NOT committed; as a regex it would match the one that did
    r.git(&["config", "user.name", "test (dev) all mighty"]);
    assert!(r.hook("pre-commit-usual-name", &[]).says("first time"));
}

#[test]
fn never_blocks_a_commit() {
    let r = Repo::new();
    r.stage("a.txt", "x\n");
    r.commit("feat: a");
    r.git(&["config", "user.name", "someone new"]);
    assert!(r.hook("pre-commit-usual-name", &[]).passed());
}
