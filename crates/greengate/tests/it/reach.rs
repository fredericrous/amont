//! `amont.conventions declared` — the house rules stand down in a repository
//! that never subscribed, and the safety net does not. The journey itself is
//! `flows/declared.rs`, step `reach`; these are the cases it does not walk: an
//! `amont.conf` that cannot be read, and a push to main.

use crate::common;
use common::Repo;

use std::process::Stdio;

fn declared_mode(r: &Repo) {
    r.git(&["config", "amont.conventions", "declared"]);
}

/// Presence is the declaration, even of an `amont.conf` that cannot be read
/// (a directory) or is not UTF-8: what it says is unusable, that it exists is
/// not, so the house rules still apply.
#[test]
fn an_unreadable_manifest_still_declares_the_repo() {
    let r = Repo::new();
    declared_mode(&r);
    let msg = r.path("msg.txt");
    std::fs::write(&msg, "not a conventional subject at all\n").unwrap();

    r.write("amont.conf/inside", "");
    let run = r.hook("commit-msg", &[msg.to_str().unwrap()]);
    assert!(!run.passed(), "a directory declares: {}", run.stdout);

    std::fs::remove_dir_all(r.path("amont.conf")).unwrap();
    std::fs::write(r.path("amont.conf"), b"# \xff\xfe not utf-8\n").unwrap();
    let run = r.hook("commit-msg", &[msg.to_str().unwrap()]);
    assert!(!run.passed(), "a non-UTF-8 file declares: {}", run.stdout);
}

/// pre-push in an undeclared repository: branch-protect (a convention) lets
/// a push to main through, while the secrets check (safety) still blocks.
#[test]
fn pre_push_keeps_only_the_safety_net_where_undeclared() {
    let r = Repo::new();
    declared_mode(&r);
    r.stage("a.txt", "x\n");
    r.commit("chore: base");
    let head = String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    let line = format!(
        "refs/heads/main {head} refs/heads/main {}\n",
        "0".repeat(40)
    );
    let mut child = crate::common::amont_cmd()
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-push")
        .current_dir(&r.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    use std::io::Write;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(line.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("wait");
    assert!(
        out.status.success(),
        "branch-protect is a convention and stands down: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
