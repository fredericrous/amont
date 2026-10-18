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
//! The journey (a stamp, its retry, new content, an amend, a dirty or
//! untracked tree, the switch, a rehearsal) is `flows/push.rs`. What stays
//! here is the stamp's identity: a replaced tool, one nobody can identify,
//! and a token from before tokens carried one.
//!
//! Same fixture shape as `gate_pairs.rs`: a declared pre-push check that
//! appends to a log, so the log's length says how many times it ran.

use crate::common;
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

/// A repo whose one pre-push gate runs `mytool`, and the directory a test may
/// put it in. Nothing is put there: on its own this is the repository whose
/// gate names a program nobody can find, which runs on every platform.
fn tooled_repo_without_tool() -> (Repo, String, std::path::PathBuf) {
    let r = Repo::new();
    r.stage("gate.js", "require('fs').appendFileSync('gate.log','x')\n");
    r.commit("chore: base");
    let base = head(&r);
    r.stage(
        "amont.conf",
        "pre-push    suite  *.txt  block  mytool gate.js\n",
    );
    r.commit("chore: the gate");
    trust_and_install(&r);
    r.stage("a.txt", "hello\n");
    r.commit("feat: something to push");
    let bin = r.path(".git/toolbin");
    std::fs::create_dir_all(&bin).expect("mkdir");
    (r, base, bin)
}

/// The same repository with `mytool` written into that directory, so "the
/// tool changed" can be staged without touching the machine. The tool is a
/// shell script, hence unix-only.
#[cfg(unix)]
fn tooled_repo() -> (Repo, String, std::path::PathBuf) {
    let (r, base, bin) = tooled_repo_without_tool();
    common::fixture_exec(&bin.join("mytool"), "#!/bin/sh\nexec node \"$@\"\n");
    (r, base, bin)
}

#[cfg(unix)]
fn push_with_path(r: &Repo, from: &str, to: &str, bin: &std::path::Path) -> (i32, String) {
    let line = format!("refs/heads/feat/x {to} refs/heads/feat/x {from}\n");
    let mut child = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-push")
        .current_dir(&r.dir)
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
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

/// The tree says the content is unchanged; it cannot say the tool that judged
/// it is. A pass made by one binary is not a pass for the one that replaced it.
#[test]
#[cfg(unix)]
fn a_replaced_tool_runs_the_gate_again_and_an_unchanged_one_does_not() {
    if missing("node") {
        return;
    }
    let (r, base, bin) = tooled_repo();
    let (code, out) = push_with_path(&r, &base, &head(&r), &bin);
    assert_eq!(code, 0, "{out}");
    assert_eq!(runs(&r), 1);

    let (code, out) = push_with_path(&r, &base, &head(&r), &bin);
    assert_eq!(code, 0, "{out}");
    assert_eq!(runs(&r), 1, "same tree, same tool: skipped\n{out}");
    assert!(out.contains(SKIP), "{out}");

    // Same tree, a different binary behind the same name.
    common::fixture_exec(
        &bin.join("mytool"),
        "#!/bin/sh\n# a newer build\nexec node \"$@\"\n",
    );
    let (code, out) = push_with_path(&r, &base, &head(&r), &bin);
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        runs(&r),
        2,
        "the tool changed, so the stamp is not its pass\n{out}"
    );
}

/// A tool nobody can identify is never skipped on a stamp: no token is
/// written for it, so the next push runs it again.
#[test]
fn a_gate_whose_tool_cannot_be_identified_earns_no_stamp() {
    if missing("node") {
        return;
    }
    let (r, base, bin) = tooled_repo_without_tool();
    // `mytool` is not on PATH here, so the gate is unavailable and its
    // identity unknown.
    let (_, out) = push_out(&r, &base, &head(&r));
    let tree = String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD^{tree}"]).stdout)
        .trim()
        .to_string();
    for key in ["HEAD", tree.as_str()] {
        // A note may exist (evidence lines record that the gate could not
        // run); what must not exist is a TOKEN vouching for it.
        let shown = r.git(&["notes", "--ref", "amont-gate", "show", key]);
        let text = String::from_utf8_lossy(&shown.stdout).to_string();
        assert!(
            !text.lines().next().unwrap_or("").contains("pre-push-suite"),
            "no stamp token for a gate that could not be identified on {key}:\n{text}\n{out}"
        );
    }
    let _ = bin;
}

/// Stamps written before tokens carried an identity are bare names. They are
/// not honoured (one re-run), they do not break parsing, and the note that
/// results holds both kinds side by side without confusing them.
#[test]
fn a_legacy_bare_token_costs_one_run_and_then_the_namespaced_one_is_honoured() {
    if missing("node") {
        return;
    }
    let (r, base) = gated_repo();
    let tip = head(&r);
    let tree = String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD^{tree}"]).stdout)
        .trim()
        .to_string();
    for key in [tip.as_str(), tree.as_str()] {
        r.git(&[
            "notes",
            "--ref",
            "amont-gate",
            "add",
            "-f",
            "-m",
            "amont-gate-v1 pre-push-suite",
            key,
        ]);
    }
    let (code, out) = push_out(&r, &base, &tip);
    assert_eq!(code, 0, "{out}");
    assert_eq!(runs(&r), 1, "a bare token vouches for nothing:\n{out}");
    assert!(!out.contains(SKIP), "{out}");

    let shown = String::from_utf8_lossy(
        &r.git(&["notes", "--ref", "amont-gate", "show", &tip])
            .stdout,
    )
    .to_string();
    assert!(shown.contains("pre-push-suite@"), "mixed note: {shown}");
    assert!(shown.starts_with("amont-gate-v1"), "{shown}");

    let (code, out) = push_out(&r, &base, &tip);
    assert_eq!(code, 0, "{out}");
    assert_eq!(runs(&r), 1, "the namespaced token is honoured:\n{out}");
    assert!(out.contains(SKIP), "{out}");
}
