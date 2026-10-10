//! The scope column's directory trigger and the `docs-skip` marker, run through
//! the real pre-commit hook against real staged changes.
//!
//! Its own binary, for the reason `scope_opt_in.rs` gives: timing-sensitive
//! neighbours in a shared binary compete for scheduler slots.

mod common;

use common::Repo;
use std::process::Command;

fn manifest(r: &Repo, body: &str) {
    r.stage("amont.conf", body);
    Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("trust")
        .current_dir(r.path(""))
        .output()
        .expect("amont trust");
}

/// A directory trigger runs its check for a change inside the directory and
/// leaves it alone for one outside, including a sibling whose name only starts
/// with the same letters.
#[test]
fn a_directory_trigger_runs_only_for_a_change_inside_it() {
    let r = Repo::new();
    manifest(
        &r,
        "pre-commit  plugin  claude-plugin/**/*.md  block  sh -c 'echo PLUGIN-RAN; exit 1'\n",
    );

    r.stage("README.md", "readme\n");
    let run = r.hook("pre-commit", &[]);
    assert!(!run.says("PLUGIN-RAN"), "outside the dir: {}", run.output());
    assert!(run.passed(), "and must not block: {}", run.output());

    r.stage("claude-plugin-old/SKILL.md", "lookalike\n");
    let run = r.hook("pre-commit", &[]);
    assert!(!run.says("PLUGIN-RAN"), "a lookalike dir: {}", run.output());

    r.stage("claude-plugin/skills/a/SKILL.md", "inside\n");
    let run = r.hook("pre-commit", &[]);
    assert!(run.says("PLUGIN-RAN"), "inside the dir: {}", run.output());
    assert!(!run.passed(), "and the check's failure blocks");
}

/// `docs-skip` lets a documentation-only edit to an existing file skip the
/// check. A new file, a deleted one, or any code change is judged as before.
#[test]
fn docs_skip_passes_an_edit_to_existing_docs_and_nothing_else() {
    let r = Repo::new();
    manifest(
        &r,
        "pre-commit  gate  *  block  docs-skip sh -c 'echo GATE-RAN; exit 1'\n",
    );
    r.stage("docs/guide.md", "one\n");
    r.stage("src/lib.rs", "code\n");
    r.commit("base");

    // An edit to an existing document: skipped, and says so.
    r.stage("docs/guide.md", "two\n");
    let run = r.hook("pre-commit", &[]);
    assert!(
        !run.says("GATE-RAN"),
        "docs edit must skip: {}",
        run.output()
    );
    assert!(
        run.says("skipped"),
        "and the skip is said out loud: {}",
        run.output()
    );
    assert!(run.passed());
    r.git(&["reset", "-q", "--hard", "HEAD"]);

    // A new document is an added file: it can break a citation, so it runs.
    r.stage("docs/new.md", "new\n");
    let run = r.hook("pre-commit", &[]);
    assert!(
        run.says("GATE-RAN"),
        "an added doc must run: {}",
        run.output()
    );
    r.git(&["reset", "-q", "--hard", "HEAD"]);

    // A code change is never a documentation edit.
    r.stage("src/lib.rs", "changed\n");
    let run = r.hook("pre-commit", &[]);
    assert!(
        run.says("GATE-RAN"),
        "a code edit must run: {}",
        run.output()
    );
}

/// A decision record under `adr/` is a documentation extension that the
/// decision graph is built from, so editing it is never a docs-only change.
#[test]
fn docs_skip_never_passes_an_edit_to_a_decision_record() {
    let r = Repo::new();
    manifest(
        &r,
        "pre-commit  gate  *  block  docs-skip sh -c 'echo GATE-RAN; exit 1'\n",
    );
    r.stage("adr/0001-x.md", "status: active\n");
    r.commit("base");

    r.stage("adr/0001-x.md", "status: superseded\n");
    let run = r.hook("pre-commit", &[]);
    assert!(
        run.says("GATE-RAN"),
        "an ADR edit must run: {}",
        run.output()
    );
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
        !stamped.lines().any(|l| l == "gate"),
        "a skipped gate must not be stamped: {stamped:?}"
    );
    r.git(&["reset", "-q", "--hard", "HEAD"]);

    r.stage("src/lib.rs", "changed\n");
    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.output());
    let stamped = std::fs::read_to_string(&marker).unwrap_or_default();
    assert!(
        stamped.lines().any(|l| l == "gate"),
        "a gate that ran clean must be stamped: {stamped:?}"
    );
}
