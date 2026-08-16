//! The secrets check, end to end — both halves, and the redaction promise.
//!
//! Every fixture token is ASSEMBLED at runtime so this file never contains
//! a contiguous secret shape: the day this repository's own hooks carry the
//! check, its test suite must not be the first finding.

mod common;
use common::Repo;

use std::io::Write;
use std::process::{Command, Stdio};

fn aws_key() -> String {
    format!("{}{}{}", "AK", "IA", "IOSFODNN7EXAMPLE")
}

fn run_check(r: &Repo, check: &str, stdin_line: Option<String>) -> (i32, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_amont"));
    cmd.arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg(check)
        .current_dir(&r.dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = if stdin_line.is_some() {
        cmd.stdin(Stdio::piped()).spawn().expect("spawn")
    } else {
        cmd.stdin(Stdio::null()).spawn().expect("spawn")
    };
    if let Some(line) = stdin_line {
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(line.as_bytes())
            .unwrap();
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

fn head(r: &Repo) -> String {
    String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string()
}

fn push_line(r: &Repo, from: &str) -> String {
    format!("refs/heads/feat/x {} refs/heads/feat/x {from}\n", head(r))
}

/// A staged credential blocks the commit — and the report is REDACTED:
/// the kind and the place, never the token. A hook that echoes a secret
/// into scrollback has widened the leak it exists to prevent.
#[test]
fn a_staged_secret_blocks_and_is_never_echoed() {
    let r = Repo::new();
    let key = aws_key();
    r.stage("config.ini", &format!("access_key = {key}\n"));
    let (code, out) = run_check(&r, "pre-commit-secrets", None);
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("AWS access key id"), "{out}");
    assert!(out.contains("config.ini:1"), "{out}");
    assert!(!out.contains(&key), "the report leaked the secret: {out}");
}

#[test]
fn clean_staged_content_passes() {
    let r = Repo::new();
    r.stage("a.txt", "nothing secret here\n");
    let (code, out) = run_check(&r, "pre-commit-secrets", None);
    assert_eq!(code, 0, "{out}");
}

/// The pragma is the per-line escape for legitimate fixtures.
#[test]
fn the_allow_pragma_is_honoured() {
    let r = Repo::new();
    r.stage(
        "fixture.txt",
        &format!("{} # {}\n", aws_key(), "amont:allow-secret"),
    );
    let (code, out) = run_check(&r, "pre-commit-secrets", None);
    assert_eq!(code, 0, "{out}");
}

/// The push half is why the check exists twice: a secret committed with
/// --no-verify never met pre-commit, and the push is the last recoverable
/// moment. Redaction holds here too.
#[test]
fn a_no_verify_secret_is_caught_at_push() {
    let r = Repo::new();
    r.stage("a.txt", "base\n");
    r.commit("chore: base");
    let base = head(&r);
    let key = aws_key();
    r.stage("leaked.env", &format!("AWS_KEY={key}\n"));
    r.commit("feat: oops"); // Repo::commit IS --no-verify
    let line = push_line(&r, &base);
    let (code, out) = run_check(&r, "pre-push-secrets", Some(line));
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("AWS access key id"), "{out}");
    assert!(out.contains("leaked.env"), "{out}");
    assert!(out.contains("rotating"), "the remedy is named: {out}");
    assert!(!out.contains(&key), "the report leaked the secret: {out}");
}

/// Added then removed WITHIN the pushed range is still a leak: the secret
/// enters the history being published.
#[test]
fn a_secret_removed_later_in_the_range_is_still_caught() {
    let r = Repo::new();
    r.stage("a.txt", "base\n");
    r.commit("chore: base");
    let base = head(&r);
    r.stage("cfg.txt", &format!("{}\n", aws_key()));
    r.commit("feat: add");
    r.stage("cfg.txt", "rotated away\n");
    r.commit("fix: remove");
    let (code, out) = run_check(&r, "pre-push-secrets", Some(push_line(&r, &base)));
    assert_ne!(code, 0, "history still carries it: {out}");
}

#[test]
fn a_clean_push_passes_and_a_delete_is_nothing() {
    let r = Repo::new();
    r.stage("a.txt", "base\n");
    r.commit("chore: base");
    let base = head(&r);
    r.stage("b.txt", "more\n");
    r.commit("feat: clean");
    let (code, out) = run_check(&r, "pre-push-secrets", Some(push_line(&r, &base)));
    assert_eq!(code, 0, "{out}");

    // A ref delete pushes no content at all.
    let zeros = "0".repeat(40);
    let line = format!("(delete) {zeros} refs/heads/feat/x {}\n", head(&r));
    let (code, out) = run_check(&r, "pre-push-secrets", Some(line));
    assert_eq!(code, 0, "{out}");
}
