//! #289: a pass earned in a dirty working tree is never signed onto the
//! pushed commit's tree. The gate tested what was on disk, not the commit.
//!
//! Unix only: the fixture is trusted and installed through `tree_gates`,
//! which is Unix only, and the key comes from `ssh-keygen`.
#![cfg(unix)]

use crate::common::{self, missing, Repo};
use std::process::{Command, Stdio};

fn head(r: &Repo) -> String {
    String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string()
}

/// A repo with a passing pre-push gate on `*.txt`, inline, signing on, a
/// bare `origin`, and one `.txt` commit to push. The repo, the base sha and
/// the origin's path. `None` where node or ssh-keygen is missing.
fn signing_repo() -> Option<(Repo, String, std::path::PathBuf)> {
    if missing("node") || missing("ssh-keygen") {
        return None;
    }
    let r = Repo::new();
    r.stage("gate.js", "process.exit(0)\n");
    r.commit("chore: base");
    let base = head(&r);
    r.git(&["update-ref", "refs/remotes/origin/main", &base]);
    r.stage(
        "amont.conf",
        "pre-push    suite  *.txt  block  node gate.js\n",
    );
    r.commit("chore: the gate");
    crate::tree_gates::trust_and_install(&r);
    r.stage("a.txt", "hello\n");
    r.commit("feat: something to push");
    r.git(&["config", "amont.pushGates", "inline"]);
    let key = r.path(".git/attest-key");
    let made = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "t@example.org", "-f"])
        .arg(&key)
        .status()
        .expect("ssh-keygen")
        .success();
    assert!(made, "ssh-keygen");
    let origin = r.path(".git/origin.git");
    r.git(&["init", "-q", "--bare", &origin.display().to_string()]);
    r.git(&["remote", "add", "origin", &origin.display().to_string()]);
    r.git(&["config", "amont.attest", "true"]);
    r.git(&["config", "amont.attestKey", &key.display().to_string()]);
    Some((r, base, origin))
}

/// The pre-push hook pushing HEAD to `origin`: exit code and output.
fn push_hook(r: &Repo, base: &str, origin: &std::path::Path) -> (i32, String) {
    let line = format!("refs/heads/feat/x {} refs/heads/feat/x {base}\n", head(r));
    let mut cmd = common::amont_cmd();
    cmd.arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-push")
        .arg("origin")
        .arg(origin)
        .current_dir(&r.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn");
    {
        use std::io::Write;
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(line.as_bytes())
            .expect("write");
    }
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

/// The `gates` line the origin's attestation note on `tip` carries, if any.
fn attested_gates(origin: &std::path::Path, tip: &str) -> String {
    let note = Command::new("git")
        .args(["notes", "--ref", "amont-attest", "show", tip])
        .current_dir(origin)
        .output()
        .expect("git notes on the origin");
    String::from_utf8_lossy(&note.stdout)
        .lines()
        .filter(|l| l.starts_with("gates "))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_gate_that_passed_in_a_dirty_tree_is_not_attested() {
    let Some((r, base, origin)) = signing_repo() else {
        return;
    };
    // A tracked change the commit does not carry: the gate reads this.
    r.write("a.txt", "not what is committed\n");
    let (code, out) = push_hook(&r, &base, &origin);
    assert_eq!(code, 0, "{out}");
    let gates = attested_gates(&origin, &head(&r));
    assert!(
        !gates.contains("pre-push-suite"),
        "a dirty-tree pass was signed onto the commit: {gates}\n{out}"
    );
}

#[test]
fn a_gate_that_passed_in_a_clean_tree_is_attested() {
    let Some((r, base, origin)) = signing_repo() else {
        return;
    };
    let (code, out) = push_hook(&r, &base, &origin);
    assert_eq!(code, 0, "{out}");
    let gates = attested_gates(&origin, &head(&r));
    assert!(gates.contains("pre-push-suite"), "{gates}\n{out}");
}
