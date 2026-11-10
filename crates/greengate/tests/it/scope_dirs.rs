//! What `docs-skip` leaves in the one-shot gate marker, read straight after
//! the pre-commit hook and before any post-commit consumes it. The directory
//! trigger and `docs-skip`'s verdicts are steps of `flows/declared.rs`.
//!
//! Its own binary, for the reason `scope_opt_in.rs` gives: timing-sensitive
//! neighbours in a shared binary compete for scheduler slots.

use crate::common;

use common::Repo;

fn manifest(r: &Repo, body: &str) {
    r.stage("amont.conf", body);
    crate::common::amont_cmd()
        .arg("trust")
        .current_dir(r.path(""))
        .output()
        .expect("amont trust");
}

/// A skipped gate must not be stamped as having passed. The marker is what
/// post-commit binds to the commit, and the push-side pairing and attestations
/// read it as "this gate ran clean on this tree" — so a gate that never ran
/// may not be in it, and one that did run must be.
#[test]
fn docs_skip_earns_no_stamp_and_a_real_run_does() {
    let r = Repo::new();
    manifest(&r, "pre-commit  gate  *  block  docs-skip true\n");
    r.stage("docs/guide.md", "one\n");
    r.stage("src/lib.rs", "code\n");
    r.commit("base");
    let marker = r.path(".git/amont-gate");

    r.stage("docs/guide.md", "two\n");
    let run = r.hook("pre-commit", &[]);
    assert!(run.says("skipped"), "must be skipped: {}", run.output());
    let stamped = std::fs::read_to_string(&marker).unwrap_or_default();
    assert!(
        !stamped
            .lines()
            .any(|l| l == "gate" || l.starts_with("gate@")),
        "a skipped gate must not be stamped: {stamped:?}"
    );
    r.git(&["reset", "-q", "--hard", "HEAD"]);

    r.stage("src/lib.rs", "changed\n");
    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.output());
    let stamped = std::fs::read_to_string(&marker).unwrap_or_default();
    assert!(
        // The complete token: the name AND the identity of what ran.
        stamped.lines().any(|l| l.starts_with("gate@")),
        "a gate that ran clean must be stamped under its identity: {stamped:?}"
    );
}

/// `amont run` rehearses the stage and leaves no marker, on purpose. The
/// marker is one-shot state bound to `git write-tree` of the INDEX and
/// consumed by the commit that follows; a rehearsal is not that commit, and
/// `--all-files` judges the working tree, which is not the tree the marker
/// would name. The hook path is the one that stamps.
#[test]
fn amont_run_leaves_no_marker_but_the_pre_commit_hook_does() {
    let r = Repo::new();
    manifest(&r, "pre-commit  gate  *  block  true\n");
    r.stage("src/lib.rs", "code\n");
    r.commit("base");
    let marker = r.path(".git/amont-gate");

    r.stage("src/lib.rs", "changed\n");
    let run = r.run(&["run"]);
    assert!(run.passed(), "{}", run.output());
    assert!(!marker.exists(), "a rehearsal must not write the marker");

    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.output());
    assert!(marker.exists(), "the hook path stamps");
}
