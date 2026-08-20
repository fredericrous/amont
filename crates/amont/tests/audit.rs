//! The dependency audits, end to end: warn on a branch push, refuse a
//! `v*` tag push carrying known vulnerabilities, never block when the
//! tool is missing. Fake audit tools on a prepended PATH give each test
//! total control of output and exit code — the checks' verdicts come from
//! parsing, and parsing is what these pin.
#![cfg(unix)]

mod common;
use common::Repo;

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

/// A directory of fake tools, prepended to PATH for one invocation.
fn shim(r: &Repo, name: &str, body: &str) {
    let dir = r.path(".git/toolshims");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}")).expect("write");
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// Run ONE audit check with a single pushed ref, fake tools first on PATH.
fn push_check(r: &Repo, check: &str, remote_ref: &str) -> (i32, String) {
    let oid = "a".repeat(40);
    let line = format!("{remote_ref} {oid} {remote_ref} {}\n", "0".repeat(40));
    let path = format!(
        "{}:{}",
        r.path(".git/toolshims").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg(check)
        .current_dir(&r.dir)
        .env("PATH", path)
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

fn repo() -> Repo {
    let r = Repo::new();
    r.stage("a.txt", "x\n");
    r.commit("chore: base");
    r
}

/// The point of the whole design: the same finding is a warning on a
/// branch and a refusal on a release.
#[test]
fn vulnerabilities_warn_on_a_branch_and_refuse_a_v_tag() {
    let r = repo();
    shim(&r, "cargo-audit", "exit 0"); // exists, so the check proceeds
    shim(
        &r,
        "cargo",
        "echo 'Crate: bad'\necho 'ID: RUSTSEC-2025-0001'\necho 'error: 1 vulnerability found'\nexit 1",
    );

    let (code, out) = push_check(&r, "pre-push-audit-rust", "refs/heads/feat/x");
    assert_eq!(code, 0, "a branch push must not block: {out}");
    assert!(out.contains("will BLOCK a v* tag push"), "{out}");
    assert!(out.contains("RUSTSEC-2025-0001"), "{out}");

    let (code, out) = push_check(&r, "pre-push-audit-rust", "refs/tags/v1.0.0");
    assert_ne!(code, 0, "a release does not ship with these: {out}");
    assert!(out.contains("does not ship"), "{out}");
}

/// A tag that merely starts with the letter v is not a release.
#[test]
fn a_vendor_tag_is_not_a_release() {
    let r = repo();
    shim(&r, "cargo-audit", "exit 0");
    shim(&r, "cargo", "echo 'ID: RUSTSEC-2025-0002'\nexit 1");
    let (code, out) = push_check(&r, "pre-push-audit-rust", "refs/tags/vendor-drop");
    assert_eq!(code, 0, "{out}");
}

/// Clean trees pass a release push, and say so.
#[test]
fn a_clean_tree_passes_a_tag_push() {
    let r = repo();
    shim(&r, "cargo-audit", "exit 0");
    shim(&r, "cargo", "echo 'ok, 312 crates checked'\nexit 0");
    let (code, out) = push_check(&r, "pre-push-audit-rust", "refs/tags/v2.0.0");
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("no known vulnerabilities"), "{out}");
}

/// Warning-class advisories never block, even on a release — the tree
/// carries unmaintained crates today and a gate nothing passes gets
/// deleted.
#[test]
fn warning_class_advisories_do_not_block_a_release() {
    let r = repo();
    shim(&r, "cargo-audit", "exit 0");
    shim(
        &r,
        "cargo",
        "echo 'warning: unmaintained RUSTSEC-2024-0436 paste'\nexit 0",
    );
    let (code, out) = push_check(&r, "pre-push-audit-rust", "refs/tags/v3.0.0");
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("RUSTSEC-2024-0436"),
        "named, not hidden: {out}"
    );
}

/// A missing tool is loud and non-blocking — the offline case must not
/// teach --no-verify.
#[test]
fn a_missing_audit_tool_warns_and_never_blocks() {
    let r = repo(); // no shims at all: cargo-audit absent from the fake dir
    std::fs::create_dir_all(r.path(".git/toolshims")).unwrap();
    let (code, out) = push_check(&r, "pre-push-audit-rust", "refs/tags/v1.0.0");
    assert_eq!(code, 0, "{out}");
    // On a machine with a REAL cargo-audit on PATH the shim dir cannot
    // hide it, and the check takes the could-not-check path instead (the
    // fixture repo has no Cargo.toml). Both phrasings honour the same
    // contract this test pins: loud, and never blocking.
    assert!(
        out.contains("did NOT run") || out.contains("could not run") || out.contains("NOT checked"),
        "{out}"
    );
}

/// npm's summary line decides, both ways.
#[test]
fn npm_audit_summary_decides_both_ways() {
    let r = repo();
    shim(
        &r,
        "npm",
        "echo 'found 3 vulnerabilities (1 moderate, 2 high)'\nexit 1",
    );
    let (code, out) = push_check(&r, "pre-push-audit-js", "refs/tags/v1.0.0");
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("found 3 vulnerabilities"), "{out}");

    shim(&r, "npm", "echo 'found 0 vulnerabilities'\nexit 0");
    let (code, out) = push_check(&r, "pre-push-audit-js", "refs/tags/v1.0.0");
    assert_eq!(code, 0, "{out}");
}

/// govulncheck's GO- ids decide; the exit code says whether the analysed
/// code is affected or the finding is informational.
#[test]
fn govulncheck_ids_decide_both_ways() {
    let r = repo();
    shim(
        &r,
        "govulncheck",
        "echo 'Vulnerability #1: GO-2022-0969'\nexit 3",
    );
    let (code, out) = push_check(&r, "pre-push-audit-go", "refs/tags/v1.0.0");
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("GO-2022-0969"), "{out}");

    shim(
        &r,
        "govulncheck",
        "echo 'No vulnerabilities found.'\nexit 0",
    );
    let (code, out) = push_check(&r, "pre-push-audit-go", "refs/tags/v1.0.0");
    assert_eq!(code, 0, "{out}");

    // Informational — the module is vulnerable, the code never calls it:
    // named, never blocking, even on a release tag.
    shim(
        &r,
        "govulncheck",
        "echo '=== Informational ==='\necho 'Vulnerability #1: GO-2023-1840'\nexit 0",
    );
    let (code, out) = push_check(&r, "pre-push-audit-go", "refs/tags/v1.0.0");
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("GO-2023-1840"), "{out}");
}

/// pip-audit's closing sentence decides.
#[test]
fn pip_audit_sentence_decides() {
    let r = repo();
    // The pip-style path: a requirements file is what gets audited.
    std::fs::write(r.path("requirements.txt"), "requests==2.0.0\n").expect("write");
    shim(
        &r,
        "pip-audit",
        "echo 'Found 2 known vulnerabilities in 1 package'\nexit 1",
    );
    let (code, out) = push_check(&r, "pre-push-audit-python", "refs/tags/v1.0.0");
    assert_ne!(code, 0, "{out}");

    shim(
        &r,
        "pip-audit",
        "echo 'No known vulnerabilities found'\nexit 0",
    );
    let (code, out) = push_check(&r, "pre-push-audit-python", "refs/tags/v1.0.0");
    assert_eq!(code, 0, "{out}");
}

/// A uv project has no `requirements.txt` — it has a venv. Before this, the
/// check looked only for the file, found none, and reported "the audit did
/// NOT run" on every push forever; a whole fleet of Python repositories was
/// in that state with nobody told. Driven through the real binary, so the
/// argv it builds is what is being pinned.
#[test]
fn a_uv_project_audits_its_venv_not_a_missing_requirements_file() {
    let r = repo();
    std::fs::write(r.path("pyproject.toml"), "[project]\nname = \"x\"\n").expect("write");
    std::fs::create_dir_all(r.path(".venv/lib/python3.13/site-packages")).expect("mkdir");
    // The shim proves the tool was actually invoked: it only speaks when
    // asked about a --path, so reaching this output means the venv route
    // was taken rather than the requirements one.
    shim(
        &r,
        "pip-audit",
        "case \"$*\" in *--path*) echo 'Found 3 known vulnerabilities in 2 packages'; exit 1;; \
         *) echo 'wrong invocation: '\"$*\"; exit 2;; esac",
    );
    let (code, out) = push_check(&r, "pre-push-audit-python", "refs/tags/v1.0.0");
    assert_ne!(code, 0, "a release must refuse a vulnerable tree: {out}");
    assert!(
        out.contains("Found 3 known vulnerabilities"),
        "the tool's own sentence should decide: {out}"
    );

    // And with neither input, it still says so rather than inventing one.
    let bare = repo();
    shim(
        &bare,
        "pip-audit",
        "echo 'No known vulnerabilities found'\nexit 0",
    );
    let (code, out) = push_check(&bare, "pre-push-audit-python", "refs/tags/v1.0.0");
    assert_eq!(code, 0, "unavailable never blocks: {out}");
    assert!(
        out.contains("did NOT run"),
        "silence would be the bug this fixes: {out}"
    );
}
