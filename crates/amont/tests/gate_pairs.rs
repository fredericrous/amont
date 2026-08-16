//! A gate is a NAME declared at both stages — not an npm vocabulary.
//!
//! `typecheck`/`test:unit`/`test` gated the npm push gate from the start;
//! these tests pin the general form: declare any name at pre-commit (block)
//! and again at pre-push, and the commit-time side earns per-commit stamps
//! the push-time side defers to. A Rust repo's `cargo test`, a Python
//! repo's `pytest` — same contract, no package.json anywhere.

mod common;
use common::{missing, Repo};

use std::io::Write;
use std::process::{Command, Stdio};

/// Run the pre-push DISPATCHER (the pairing lives there, not in a check)
/// over one ref line, capturing output.
fn push_out(r: &Repo, from: &str, to: &str) -> (i32, String) {
    // A feature ref, not main: this drives the whole DISPATCHER, and the
    // built-in branch-protect would (correctly) block a push to main.
    let line = format!("refs/heads/feat/x {to} refs/heads/feat/x {from}\n");
    let mut child = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-push")
        .current_dir(&r.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(line.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("wait");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn head(r: &Repo) -> String {
    String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string()
}

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

/// A commit-run log both sides share: its length says how many times the
/// command executed, wherever from.
fn runs(r: &Repo) -> usize {
    std::fs::read_to_string(r.dir.join("gate.log"))
        .map(|s| s.len())
        .unwrap_or(0)
}

/// A repo whose `check` is declared at BOTH stages — the pair — with a
/// custom name no GATE list mentions, logging each execution.
fn paired_repo(severity: &str) -> (Repo, String) {
    let r = Repo::new();
    r.stage("gate.js", "require('fs').appendFileSync('gate.log','x')\n");
    r.commit("chore: base");
    let base = head(&r);
    r.stage(
        "amont.conf",
        &format!(
            "pre-commit  check  *.txt  {severity}  node gate.js\n\
             pre-push    check  *.txt  block  node gate.js\n"
        ),
    );
    r.commit("chore: the pair");
    trust_and_install(&r);
    (r, base)
}

/// The point: a verified commit earns the stamp, and the push-side twin is
/// skipped BY NAME, out loud.
#[test]
fn a_stamped_pair_is_not_repeated_at_push() {
    if missing("node") {
        return;
    }
    let (r, base) = paired_repo("block");
    r.stage("a.txt", "hello\n");
    let out = r.git(&["commit", "-q", "-m", "feat: through the gate"]);
    assert!(out.status.success(), "verified commit failed");
    assert_eq!(runs(&r), 1, "the commit-time side ran once");

    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("gated at commit instead"),
        "the skip is said out loud: {out}"
    );
    assert_eq!(runs(&r), 1, "the push repeated nothing");
}

/// A `--no-verify` commit has no stamp, so the push-side twin runs — with
/// the same warning the npm gate gives.
#[test]
fn an_unstamped_commit_brings_the_pair_back_at_push() {
    if missing("node") {
        return;
    }
    let (r, base) = paired_repo("block");
    r.stage("a.txt", "hello\n");
    r.commit("feat: dodge the gate"); // Repo::commit IS --no-verify
    assert_eq!(runs(&r), 0, "nothing ran at commit");

    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("no record of it — running it here"),
        "the reason is named: {out}"
    );
    assert_eq!(runs(&r), 1, "the push-side twin ran");
}

/// A WARN commit-time side vouches for nothing: no stamp, push always runs.
#[test]
fn a_warn_severity_pair_cannot_vouch() {
    if missing("node") {
        return;
    }
    let (r, base) = paired_repo("warn");
    r.stage("a.txt", "hello\n");
    let out = r.git(&["commit", "-q", "-m", "feat: warn cannot vouch"]);
    assert!(out.status.success());
    assert_eq!(runs(&r), 1, "the warn check still ran at commit");

    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(
        !out.contains("gated at commit instead"),
        "a warn declaration must not suppress the push: {out}"
    );
    assert_eq!(runs(&r), 2, "the push ran it again");
}

/// An unpaired pre-push declaration runs exactly as it always has.
#[test]
fn an_unpaired_push_declaration_just_runs() {
    if missing("node") {
        return;
    }
    let r = Repo::new();
    r.stage("gate.js", "require('fs').appendFileSync('gate.log','x')\n");
    r.commit("chore: base");
    let base = head(&r);
    r.stage(
        "amont.conf",
        "pre-push  check  *.txt  block  node gate.js\n",
    );
    r.commit("chore: push only");
    trust_and_install(&r);
    r.stage("a.txt", "hello\n");
    r.commit("feat: change");
    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("gated at commit"), "{out}");
    assert_eq!(runs(&r), 1);
}

/// The bypass ledger speaks the general vocabulary too: a dodged CUSTOM
/// gate lands in `.git/amont-bypasses` under its own name.
#[test]
fn a_dodged_custom_gate_reaches_the_bypass_ledger() {
    if missing("node") {
        return;
    }
    let (r, _base) = paired_repo("block");
    r.stage("a.txt", "hello\n");
    r.commit("feat: dodge");
    let ledger = std::fs::read_to_string(r.path(".git/amont-bypasses"))
        .expect("a dodged gate leaves a ledger");
    assert!(ledger.contains(" check"), "{ledger:?}");
}
