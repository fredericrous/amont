//! Commit stamps: a commit-time gate that already ran clean on exactly this
//! staged tree is not run again on the next attempt.
//!
//! The case that motivated it: a ten-minute suite passed at pre-commit, then
//! `commit-msg` refused the subject (three characters over the limit). The
//! retry, on a byte-identical tree, replayed the whole suite. The marker
//! pre-commit leaves for post-commit is bound to the tree and survives the
//! refusal — so the retry can read it and say so, in the push gate's words.
//!
//! Same fixture shape as `gate_pairs.rs`: a declared pre-commit gate that
//! appends to a log, so the log's length says how many times it ran.

mod common;
use common::{missing, Repo};

use std::process::{Command, Stdio};

fn trust_and_install(r: &Repo) {
    for verb in ["trust", "init"] {
        let out = Command::new(env!("CARGO_BIN_EXE_amont"))
            .arg(verb)
            .current_dir(&r.dir)
            .stdin(Stdio::null())
            .output()
            .expect("amont");
        assert!(
            out.status.success(),
            "amont {verb}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn runs(r: &Repo) -> usize {
    std::fs::read_to_string(r.dir.join("gate.log"))
        .map(|s| s.len())
        .unwrap_or(0)
}

/// A hooked commit, with everything the hooks said.
fn commit(r: &Repo, msg: &str) -> (bool, String) {
    let out = r.git(&["commit", "-q", "-m", msg]);
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// A repo with ONE declared, blocking pre-commit gate over `*.txt`, logging
/// each execution, with `a.txt` staged and not yet committed.
fn gated_repo() -> Repo {
    let r = Repo::new();
    r.stage("gate.js", "require('fs').appendFileSync('gate.log','x')\n");
    r.stage(
        "amont.conf",
        "pre-commit  suite  *.txt  block  node gate.js\n",
    );
    r.commit("chore: the gate");
    trust_and_install(&r);
    r.stage("a.txt", "hello\n");
    r
}

const SKIP: &str = "passed on this exact tree earlier — not repeating it here";

/// The point: commit-msg refuses, the tree is unchanged, the gate is not
/// made to run again — and the commit that goes through is stamped as if
/// it had, because it did.
#[test]
fn a_gate_that_passed_before_a_refused_message_is_not_repeated() {
    if missing("node") {
        return;
    }
    let r = gated_repo();
    // the gate runs and passes; then commit-msg refuses the subject
    let (ok, out) = commit(&r, "just a message with no type");
    assert!(!ok, "commit-msg was meant to refuse: {out}");
    assert_eq!(runs(&r), 1, "the gate ran once");
    assert!(!out.contains(SKIP), "nothing was vouched for yet: {out}");

    // the retry, same tree, a subject the hook accepts
    let (ok, out) = commit(&r, "feat: through the gate");
    assert!(ok, "{out}");
    assert!(out.contains(SKIP), "the reuse is said out loud: {out}");
    assert_eq!(runs(&r), 1, "the retry repeated nothing");

    // …and the commit carries the stamp, exactly as a fresh run would
    let head = String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    let note = r.git(&["notes", "--ref", "amont-gate", "show", &head]);
    assert!(
        note.status.success(),
        "the retried commit has no gate stamp"
    );
    assert!(
        String::from_utf8_lossy(&note.stdout).contains("suite"),
        "the stamp names the gate"
    );
}

/// A stamp is a statement about ONE tree: new content runs the gate.
#[test]
fn a_changed_tree_runs_the_gate_again() {
    if missing("node") {
        return;
    }
    let r = gated_repo();
    let (ok, _) = commit(&r, "just a message with no type");
    assert!(!ok);
    r.stage("a.txt", "hello again\n");
    let (ok, out) = commit(&r, "feat: more");
    assert!(ok, "{out}");
    assert!(!out.contains(SKIP), "new content is not vouched for: {out}");
    assert_eq!(runs(&r), 2);
}

/// The tree note is the other record: a commit undone with `reset --soft`
/// and made again stages the same tree, and post-commit stamped that tree
/// the first time round — the gate is not repeated for it either.
#[test]
fn a_re_commit_of_a_stamped_tree_does_not_repeat_the_gate() {
    if missing("node") {
        return;
    }
    let r = gated_repo();
    let (ok, out) = commit(&r, "feat: first try");
    assert!(ok, "{out}");
    assert_eq!(runs(&r), 1);
    // undo the commit, keep the index: the same tree is staged again
    assert!(r.git(&["reset", "-q", "--soft", "HEAD~1"]).status.success());
    let (ok, out) = commit(&r, "feat: same content, new commit");
    assert!(ok, "{out}");
    assert!(
        out.contains(SKIP),
        "the tree note vouches for the re-commit: {out}"
    );
    assert_eq!(runs(&r), 1);
}

/// `amont.commitStamps false` switches the reuse off — every attempt runs.
#[test]
fn the_switch_turns_reuse_off() {
    if missing("node") {
        return;
    }
    let r = gated_repo();
    r.git(&["config", "amont.commitStamps", "false"]);
    let (ok, _) = commit(&r, "just a message with no type");
    assert!(!ok);
    let (ok, out) = commit(&r, "feat: through the gate");
    assert!(ok, "{out}");
    assert!(!out.contains(SKIP), "reuse is off: {out}");
    assert_eq!(runs(&r), 2);
}

/// A gate that FAILED left no marker (pre-commit records an empty list on
/// a block), so the retry runs it again — the stamp never vouches for a
/// tree the gate rejected.
#[test]
fn a_failed_gate_is_not_vouched_for() {
    if missing("node") {
        return;
    }
    let r = Repo::new();
    r.stage(
        "gate.js",
        "require('fs').appendFileSync('gate.log','x'); process.exit(require('fs').existsSync('pass') ? 0 : 1)\n",
    );
    r.stage(
        "amont.conf",
        "pre-commit  suite  *.txt  block  node gate.js\n",
    );
    r.commit("chore: the gate");
    trust_and_install(&r);
    r.stage("a.txt", "hello\n");
    let (ok, _) = commit(&r, "feat: blocked");
    assert!(!ok, "the gate was meant to block");
    assert_eq!(runs(&r), 1);
    // same tree (the `pass` file is untracked, not staged), gate now passes
    r.write("pass", "");
    let (ok, out) = commit(&r, "feat: through");
    assert!(ok, "{out}");
    assert!(!out.contains(SKIP), "a rejection is not a stamp: {out}");
    assert_eq!(runs(&r), 2);
}
