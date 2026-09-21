//! The evidence half of the gate record, end to end.
//!
//! `gate_stamp`'s unit tests prove the note keeps both halves apart. These
//! prove the dispatcher fills the evidence half at all — including on the
//! path a FAILING push leaves by, which is the half a report is mostly made
//! of — and that `amont.order evidence` reorders the gates it finds there
//! without ever dropping one.
//!
//! Same fixture shape as `push_stamps.rs`: declared pre-push gates that
//! append to a log, so the log says how many times each ran and in which
//! order.

mod common;
use common::{missing, Repo};

use std::io::Write;
use std::process::{Command, Stdio};

fn head(r: &Repo) -> String {
    String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string()
}

fn tree(r: &Repo) -> String {
    String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD^{tree}"]).stdout)
        .trim()
        .to_string()
}

fn push_out(r: &Repo, from: &str, to: &str) -> (i32, String) {
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

fn trust_and_install(r: &Repo) {
    for verb in ["trust", "init"] {
        let out = Command::new(env!("CARGO_BIN_EXE_amont"))
            .arg(verb)
            .current_dir(&r.dir)
            .stdin(Stdio::null())
            .output()
            .expect("amont");
        assert!(out.status.success(), "amont {verb}");
    }
}

/// The note our ref holds for `HEAD^{tree}`.
fn note(r: &Repo) -> String {
    let t = tree(r);
    String::from_utf8_lossy(&r.git(&["notes", "--ref", "amont-gate", "show", &t]).stdout)
        .to_string()
}

fn log(r: &Repo) -> String {
    std::fs::read_to_string(r.dir.join("gate.log")).unwrap_or_default()
}

/// Two declared pre-push gates over `*.txt`, `alpha` then `beta` in the file,
/// each appending its name to `gate.log`. `beta` fails when `BETA_FAILS`
/// exists, so a test can decide what the push does without a second fixture.
fn two_gates() -> (Repo, String) {
    let r = Repo::new();
    r.stage(
        "gate.js",
        "const fs=require('fs');\n\
         const who=process.argv[2];\n\
         fs.appendFileSync('gate.log', who + '\\n');\n\
         if (who === 'beta' && fs.existsSync('BETA_FAILS')) process.exit(1);\n",
    );
    r.commit("chore: base");
    let base = head(&r);
    r.stage(
        "amont.conf",
        "pre-push    alpha  *.txt  block  node gate.js alpha\n\
         pre-push    beta   *.txt  block  node gate.js beta\n",
    );
    r.commit("chore: the gates");
    trust_and_install(&r);
    r.stage("a.txt", "hello\n");
    r.commit("feat: something to push");
    (r, base)
}

/// A push records what each gate cost and how it ended, keyed by the content
/// it judged. Without this the report has no dataset at all.
#[test]
fn a_push_records_a_run_line_per_gate() {
    if missing("node") {
        return;
    }
    let (r, base) = two_gates();
    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    let note = note(&r);
    assert!(
        note.lines()
            .next()
            .is_some_and(|l| l.starts_with("amont-gate-v1")),
        "line one is still the stamp: {note}"
    );
    for gate in ["alpha", "beta"] {
        assert!(
            note.lines().any(|l| l.starts_with("run ")
                && l.contains(&format!("pre-push-{gate} "))
                && l.ends_with(|c: char| c.is_ascii_digit())),
            "no run line for {gate}: {note}"
        );
    }
    assert!(
        note.lines().filter(|l| l.starts_with("run ")).count() >= 2,
        "{note}"
    );
}

/// The path that matters most: a push the gate REFUSED still leaves a
/// record. A dataset of only the pushes that succeeded could never show a
/// failure rate, which is the number the ordering is computed from.
#[test]
fn a_blocked_push_records_the_failure_it_blocked_on() {
    if missing("node") {
        return;
    }
    let (r, base) = two_gates();
    r.write("BETA_FAILS", "");
    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 1, "the push was supposed to be refused: {out}");
    let note = note(&r);
    assert!(
        note.contains(" pre-push-beta fail "),
        "the failure is on the record: {note}"
    );
    assert!(
        note.contains(" pre-push-alpha pass "),
        "so is what ran before it: {note}"
    );
}

/// `amont.order evidence`: the gate the record says fails is attempted
/// first, so a push that is going to be refused is refused sooner. The
/// declared order is alpha, beta; the seeded record says beta fails.
#[test]
fn evidence_ordering_attempts_the_failing_gate_first() {
    if missing("node") {
        return;
    }
    let (r, base) = two_gates();
    r.git(&["config", "amont.order", "evidence"]);
    // A record for content that is NOT this tree — ordering is a claim about
    // the gates, not about the content in front of it. The token line is
    // empty on purpose: this seeds evidence, never a stamp.
    // Yesterday, not a fixed date: the ordering looks at a RECENT window, and
    // a hard-coded epoch is a test that quietly stops testing anything the
    // day it falls out of it.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        - 86_400;
    let body = format!(
        "amont-gate-v1\n\
         run {} pre-push-alpha pass 100\n\
         run {} pre-push-beta pass 100\n\
         run {} pre-push-beta fail 100\n",
        now,
        now + 1,
        now + 2
    );
    let empty = String::from_utf8_lossy(
        &r.git(&["hash-object", "-w", "-t", "tree", "--stdin"])
            .stdout,
    )
    .trim()
    .to_string();
    let key = if empty.is_empty() { tree(&r) } else { empty };
    let out = r.git(&[
        "notes",
        "--ref",
        "amont-gate",
        "add",
        "-f",
        "-m",
        &body,
        &key,
    ]);
    assert!(out.status.success(), "seeding the record failed");

    r.write("BETA_FAILS", "");
    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 1, "{out}");
    assert_eq!(
        log(&r).lines().collect::<Vec<_>>(),
        vec!["beta"],
        "beta ran first and alpha never had to run: {out}"
    );
}

/// The default is unchanged, and it is the declared order. A repository that
/// says nothing gets exactly what it got before this existed.
#[test]
fn without_the_key_the_declared_order_is_kept() {
    if missing("node") {
        return;
    }
    let (r, base) = two_gates();
    r.write("BETA_FAILS", "");
    let (code, _) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 1);
    assert_eq!(
        log(&r).lines().collect::<Vec<_>>(),
        vec!["alpha", "beta"],
        "the file's order, whatever the record says"
    );
}
