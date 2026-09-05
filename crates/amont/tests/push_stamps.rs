//! Push stamps: the suite runs BEFORE git opens its connection.
//!
//! git connects to the remote, THEN runs pre-push, and holds the connection
//! idle until the gate finishes. A remote that drops idle sessions kills the
//! push after a four-minute suite has already passed. The stamp is the way
//! out: a push-time gate that passed records the tips it vouched for in
//! `refs/notes/amont-gate`, and the next push of the same content — a retry
//! after the wire dropped, or the real push after an `amont run pre-push`
//! rehearsal — skips the gate, out loud.
//!
//! Same fixture shape as `gate_pairs.rs`: a declared pre-push check that
//! appends to a log, so the log's length says how many times it ran.

mod common;
use common::{missing, Repo};

use std::io::Write;
use std::process::{Command, Stdio};

/// Drive the pre-push DISPATCHER over one ref line, capturing output.
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

fn runs(r: &Repo) -> usize {
    std::fs::read_to_string(r.dir.join("gate.log"))
        .map(|s| s.len())
        .unwrap_or(0)
}

/// A repo with ONE declared pre-push gate (no commit-time twin), logging
/// each execution, and one committed `.txt` in its scope.
fn gated_repo() -> (Repo, String) {
    let r = Repo::new();
    r.stage("gate.js", "require('fs').appendFileSync('gate.log','x')\n");
    r.commit("chore: base");
    let base = head(&r);
    r.stage(
        "amont.conf",
        "pre-push    suite  *.txt  block  node gate.js\n",
    );
    r.commit("chore: the gate");
    trust_and_install(&r);
    r.stage("a.txt", "hello\n");
    r.commit("feat: something to push");
    (r, base)
}

const SKIP: &str = "passed on this exact tree earlier — not repeating it here";
const STAMPED: &str = "stamped";

/// The point: a gate that passed stamps the tip, and the next push of the
/// same tip is not made to wait for it again.
#[test]
fn a_passed_gate_is_not_repeated_on_the_next_push_of_the_same_tree() {
    if missing("node") {
        return;
    }
    let (r, base) = gated_repo();
    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert_eq!(runs(&r), 1, "the gate ran once");
    assert!(out.contains(STAMPED), "the stamp is announced: {out}");

    // The retry — the wire dropped, the content did not change.
    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(out.contains(SKIP), "the skip is said out loud: {out}");
    assert_eq!(runs(&r), 1, "the push repeated nothing");
}

/// A stamp is a statement about ONE tree. New content runs the gate.
#[test]
fn a_changed_tree_runs_the_gate_again() {
    if missing("node") {
        return;
    }
    let (r, base) = gated_repo();
    let (code, _) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0);
    r.stage("a.txt", "hello again\n");
    r.commit("feat: more");
    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains(SKIP), "new content is not vouched for: {out}");
    assert_eq!(runs(&r), 2);
}

/// `amont run pre-push` is the rehearsal: same dispatcher, no connection
/// open. It stamps HEAD, and the push that follows skips the suite — which is
/// the whole reason to rehearse. The branch has never been pushed, so the
/// rehearsal measures against `origin/main`.
#[test]
fn a_rehearsal_stamps_head_and_the_push_skips_the_suite() {
    if missing("node") {
        return;
    }
    let (r, base) = gated_repo();
    // A feature branch (the built-in branch-protect would refuse `main`) and
    // a remote-tracking `origin/main` at the base, with no network anywhere.
    // `--no-track`: a global `branch.autoSetupMerge=always` would otherwise
    // make the branch track the local `main`, and the rehearsal would diff
    // against that instead of exercising the never-pushed fallback.
    r.git(&["checkout", "-q", "--no-track", "-b", "feat/x"]);
    r.git(&["update-ref", "refs/remotes/origin/main", &base]);
    let rehearsal = r.run(&["run", "pre-push"]);
    assert!(rehearsal.passed(), "{}", rehearsal.output());
    assert_eq!(
        runs(&r),
        1,
        "the rehearsal ran the gate: {}",
        rehearsal.output()
    );
    assert!(
        rehearsal.says(STAMPED),
        "the rehearsal stamps: {}",
        rehearsal.output()
    );

    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(out.contains(SKIP), "{out}");
    assert_eq!(runs(&r), 1, "the push repeated nothing");
}

/// Working-tree mode tests the WORKING tree. A modified tracked file means
/// what ran is not what is being pushed, so nothing is vouched for.
#[test]
fn a_dirty_working_tree_earns_no_stamp() {
    if missing("node") {
        return;
    }
    let (r, base) = gated_repo();
    r.write("a.txt", "uncommitted\n");
    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains(STAMPED), "a dirty tree is not stamped: {out}");
    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains(SKIP), "{out}");
    assert_eq!(runs(&r), 2);
}

/// The switch: `amont.pushStamps false` neither writes nor honours stamps.
#[test]
fn push_stamps_can_be_switched_off() {
    if missing("node") {
        return;
    }
    let (r, base) = gated_repo();
    r.git(&["config", "amont.pushStamps", "false"]);
    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains(STAMPED), "{out}");
    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains(SKIP), "{out}");
    assert_eq!(runs(&r), 2);
}

/// A push stamp and a commit-time stamp share one note and must not erase
/// each other: the commit-time script name survives the push-time id.
#[test]
fn a_push_stamp_merges_with_a_commit_time_stamp() {
    if missing("node") {
        return;
    }
    let r = Repo::new();
    r.stage("gate.js", "require('fs').appendFileSync('gate.log','x')\n");
    r.commit("chore: base");
    let base = head(&r);
    r.stage(
        "amont.conf",
        "pre-commit  check  *.txt  block  node gate.js\n\
         pre-push    check  *.txt  block  node gate.js\n\
         pre-push    suite  *.txt  block  node gate.js\n",
    );
    r.commit("chore: the pair plus a lone gate");
    trust_and_install(&r);
    r.stage("a.txt", "hello\n");
    let out = r.git(&["commit", "-q", "-m", "feat: through the commit gate"]);
    assert!(out.status.success(), "verified commit failed");
    assert_eq!(runs(&r), 1);

    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("gated at commit instead"), "{out}");
    assert_eq!(runs(&r), 2, "the lone gate ran; the pair did not");

    let note = String::from_utf8_lossy(
        &r.git(&["notes", "--ref", "amont-gate", "show", "HEAD"])
            .stdout,
    )
    .to_string();
    assert!(note.contains("check"), "commit-time token kept: {note}");
    assert!(
        note.contains("pre-push-suite"),
        "push-time token added: {note}"
    );

    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("gated at commit instead"), "{out}");
    assert!(out.contains(SKIP), "{out}");
    assert_eq!(runs(&r), 2, "nothing ran the second time");
}

/// An UNTRACKED file earns no stamp either, and says why.
///
/// The cleanliness test used to pass `--untracked-files=no`, reasoning that
/// untracked files are not in the tree the stamp vouches for. That is the
/// risk, not the reason: they were there while the suite ran. A new test
/// file, a fixture, a local `.env` is exactly the sort of thing that changes
/// a suite's answer, and the stamp would then vouch for a tree that does not
/// contain it.
///
/// Ignored files still cost nobody a stamp — `--porcelain` does not list
/// them — and the one line is worth its noise because the answer changed
/// under people who used to get a stamp here.
#[test]
fn an_untracked_file_earns_no_stamp_and_says_so() {
    if missing("node") {
        return;
    }
    let (r, base) = gated_repo();
    r.write("scratch.txt.notes", "a file git has never seen\n");

    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(
        !out.contains(STAMPED),
        "an untracked file was stamped over: {out}"
    );
    assert!(
        out.contains("untracked files present"),
        "it must say why there is no stamp: {out}"
    );

    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains(SKIP), "{out}");
    assert_eq!(runs(&r), 2, "the gate ran both times: {out}");
}

/// An IGNORED file is not an untracked one. `target/` and `node_modules/`
/// must not cost anybody a stamp, or the strictness above would make the
/// whole feature useless in every real repository.
#[test]
fn an_ignored_file_still_earns_a_stamp() {
    if missing("node") {
        return;
    }
    let (r, base) = gated_repo();
    r.write(".git/info/exclude", "build-output/\n");
    std::fs::create_dir_all(r.dir.join("build-output")).expect("mkdir");
    r.write("build-output/thing.o", "binary-ish\n");

    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(out.contains(STAMPED), "an ignored file cost a stamp: {out}");
}
