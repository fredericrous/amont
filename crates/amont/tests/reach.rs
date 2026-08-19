//! `amont.conventions declared` — the house rules stand down in a repository
//! that never subscribed, and the safety net does not.

mod common;
use common::Repo;

use std::process::{Command, Stdio};

fn declared_mode(r: &Repo) {
    r.git(&["config", "amont.conventions", "declared"]);
}

/// The default is `everywhere`: without the mode set, nothing changes.
#[test]
fn the_default_keeps_todays_behaviour() {
    let r = Repo::new();
    r.stage("bad.json", "{ not json\n");
    let run = r.hook("pre-commit", &[]);
    assert!(!run.passed(), "lint-json-yaml still blocks: {}", run.stdout);
}

/// In declared mode, an undeclared repository holds the conventions back —
/// announced once, count not names — and a convention finding no longer
/// blocks.
#[test]
fn an_undeclared_repo_holds_the_conventions_back() {
    let r = Repo::new();
    declared_mode(&r);
    r.stage("bad.json", "{ not json\n");
    let run = r.hook("pre-commit", &[]);
    assert!(
        run.passed(),
        "a house rule must not fire here: {}",
        run.stdout
    );
    assert!(
        run.says("convention check(s) held back"),
        "held back, not silent: {}",
        run.stdout
    );
    assert!(
        run.says("safety net still runs"),
        "the reader is told what still protects them: {}",
        run.stdout
    );
}

/// The safety net is not held back: a debug leftover still blocks the commit
/// in a repository that never declared amont.
#[test]
fn the_safety_net_still_blocks_in_an_undeclared_repo() {
    let r = Repo::new();
    declared_mode(&r);
    r.stage("f.ts", "debugger;\n");
    let run = r.hook("pre-commit", &[]);
    assert!(!run.passed(), "ban-terms is safety: {}", run.stdout);
}

/// A committed `amont.conf` — even an empty one — is the declaration, and
/// brings the house rules back.
#[test]
fn an_empty_manifest_declares_the_repo() {
    let r = Repo::new();
    declared_mode(&r);
    r.stage("amont.conf", "# this repository subscribes to amont\n");
    r.stage("bad.json", "{ not json\n");
    let run = r.hook("pre-commit", &[]);
    assert!(
        !run.passed(),
        "declared repo gets the house rules: {}",
        run.stdout
    );
    assert!(
        !run.says("held back"),
        "nothing is held back in a declared repo: {}",
        run.stdout
    );
}

/// A typo in the mode is inert, not a surprise policy change.
#[test]
fn a_misspelt_mode_is_inert() {
    let r = Repo::new();
    r.git(&["config", "amont.conventions", "declred"]);
    r.stage("bad.json", "{ not json\n");
    let run = r.hook("pre-commit", &[]);
    assert!(
        !run.passed(),
        "a typo must not soften anything: {}",
        run.stdout
    );
}

/// commit-msg is pure convention: in declared mode an undeclared repository
/// accepts any subject, and a declared one still enforces the shape.
#[test]
fn commit_msg_stands_down_only_where_undeclared() {
    let r = Repo::new();
    declared_mode(&r);
    let msg = r.path("msg.txt");
    std::fs::write(&msg, "not a conventional subject at all\n").unwrap();
    let run = r.hook("commit-msg", &[msg.to_str().unwrap()]);
    assert!(run.passed(), "conventions held back: {}", run.stdout);

    r.stage("amont.conf", "# declared\n");
    let run = r.hook("commit-msg", &[msg.to_str().unwrap()]);
    assert!(!run.passed(), "declared repo enforces: {}", run.stdout);
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

/// `amont list` says the mode out loud, in text and in JSON.
#[test]
fn list_reports_the_held_back_state() {
    let r = Repo::new();
    declared_mode(&r);
    let out = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("list")
        .current_dir(&r.dir)
        .stdin(Stdio::null())
        .output()
        .expect("run");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(text.contains("conventions held back"), "{text}");

    let out = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("list")
        .arg("--json")
        .current_dir(&r.dir)
        .stdin(Stdio::null())
        .output()
        .expect("run");
    let json = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(json.contains("\"conventions_apply\":false"), "{json}");
}
