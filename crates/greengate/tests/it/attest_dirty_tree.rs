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
    r.git(&["config", "greengate.pushGates", "inline"]);
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
    r.git(&["config", "greengate.attest", "true"]);
    r.git(&["config", "greengate.attestKey", &key.display().to_string()]);
    Some((r, base, origin))
}

/// The pre-push hook pushing HEAD to `origin`: exit code and output.
fn push_hook(r: &Repo, base: &str, origin: &std::path::Path) -> (i32, String) {
    push_hook_env(r, base, origin, &[])
}

/// [`push_hook`] with extra environment variables.
fn push_hook_env(
    r: &Repo,
    base: &str,
    origin: &std::path::Path,
    envs: &[(&str, &std::path::Path)],
) -> (i32, String) {
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
    for (k, v) in envs {
        cmd.env(k, v);
    }
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
        .args(["notes", "--ref", "greengate-attest", "show", tip])
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

/// Every ref under `refs/notes` that `git` in `dir` lists, one per line.
fn note_refs(dir: &std::path::Path) -> String {
    let out = Command::new("git")
        .args(["for-each-ref", "--format=%(refname)", "refs/notes"])
        .current_dir(dir)
        .output()
        .expect("git for-each-ref");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// An old-name ref, locally or on the remote.
fn old_name_refs(dir: &std::path::Path) -> String {
    let out = Command::new("git")
        .args([
            "for-each-ref",
            "--format=%(refname)",
            "refs/notes/amont-*",
            "refs/notes/attest-local/amont-*",
        ])
        .current_dir(dir)
        .output()
        .expect("git for-each-ref");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Writes a provenance payload for HEAD where greengate looks for it
/// (`$CLAUDE_CONFIG_DIR/amont-agent/provenance/<repo>/<commit>`) and returns
/// the config dir and the payload file.
fn leave_provenance(r: &Repo, header: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let tip = head(r);
    let tree = String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD^{tree}"]).stdout)
        .trim()
        .to_string();
    let config = r.path("claude-config");
    let name = r.dir.file_name().expect("repo dir name");
    let dir = config.join("amont-agent").join("provenance").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(&tip);
    std::fs::write(&file, format!("{header}\ncommit {tip}\ntree {tree}\n")).unwrap();
    (config, file)
}

/// An attest-enabled push that carries a provenance payload writes only
/// green names: the attestation and the provenance land under
/// `greengate-*`, and no `amont-*` ref exists locally or on the remote.
#[test]
fn a_push_with_provenance_writes_no_amont_named_ref() {
    let Some((r, base, origin)) = signing_repo() else {
        return;
    };
    let (config, file) = leave_provenance(&r, "greengate-provenance-v1");
    let (code, out) = push_hook_env(&r, &base, &origin, &[("CLAUDE_CONFIG_DIR", &config)]);
    assert_eq!(code, 0, "{out}");
    let remote = note_refs(&origin);
    assert!(
        remote.contains("refs/notes/greengate-attest"),
        "{remote}\n{out}"
    );
    assert!(
        remote.contains("refs/notes/greengate-provenance"),
        "{remote}\n{out}"
    );
    assert!(!file.exists(), "a published payload is consumed: {out}");
    assert_eq!(old_name_refs(&origin), "", "remote: {out}");
    assert_eq!(old_name_refs(&r.dir), "", "local: {out}");
}

/// A payload an older greenpiste wrote is kept, named in the output, and
/// nothing is published for it, under any name.
#[test]
fn a_legacy_provenance_payload_is_kept_and_no_amont_ref_is_written() {
    let Some((r, base, origin)) = signing_repo() else {
        return;
    };
    let (config, file) = leave_provenance(&r, "amont-provenance-v1");
    let (code, out) = push_hook_env(&r, &base, &origin, &[("CLAUDE_CONFIG_DIR", &config)]);
    assert_eq!(code, 0, "{out}");
    assert!(file.exists(), "kept: {out}");
    assert!(out.contains("written by an older greenpiste"), "{out}");
    assert!(out.contains(&file.display().to_string()), "{out}");
    assert!(
        !note_refs(&origin).contains("provenance"),
        "nothing published for it: {}",
        note_refs(&origin)
    );
    assert_eq!(old_name_refs(&origin), "", "remote: {out}");
    assert_eq!(old_name_refs(&r.dir), "", "local: {out}");
}
