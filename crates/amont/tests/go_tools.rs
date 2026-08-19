//! The Go track — fake `gofmt`/`go` binaries on a shimmed PATH give each
//! test the behaviour, and the checks' own contracts do the rest.
#![cfg(unix)]

mod common;
use common::Repo;

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

fn shim(r: &Repo, name: &str, body: &str) {
    let dir = r.path(".git/toolshims");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}")).expect("write");
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

fn shimmed_path(r: &Repo) -> String {
    format!(
        "{}:{}",
        r.path(".git/toolshims").display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

fn commit_check(r: &Repo, check: &str) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg(check)
        .current_dir(&r.dir)
        .env("PATH", shimmed_path(r))
        .stdin(Stdio::null())
        .output()
        .expect("run");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn push_check(r: &Repo, check: &str, from: &str) -> (i32, String) {
    let head = String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    let line = format!("refs/heads/feat/x {head} refs/heads/feat/x {from}\n");
    let mut child = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg(check)
        .current_dir(&r.dir)
        .env("PATH", shimmed_path(r))
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

/// A repo with a Go module and one staged `.go` file.
fn go_repo() -> Repo {
    let r = Repo::new();
    r.stage("go.mod", "module example.com/x\n");
    r.stage("main.go", "package main\n");
    r
}

// ---- gofmt ----

/// `gofmt -l` printing nothing is a clean verdict.
#[test]
fn clean_formatting_passes() {
    let r = go_repo();
    shim(&r, "gofmt", "exit 0");
    let (code, out) = commit_check(&r, "pre-commit-gofmt");
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("Go formatting is clean"), "{out}");
}

/// The LISTING decides, not the exit code — gofmt exits 0 either way.
#[test]
fn a_listed_file_fails_with_its_name() {
    let r = go_repo();
    shim(&r, "gofmt", "echo main.go\nexit 0");
    let (code, out) = commit_check(&r, "pre-commit-gofmt");
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("Unformatted Go"), "{out}");
    assert!(out.contains("main.go"), "{out}");
}

/// With `amont.fix true`, `-w` rewrites and the result is re-staged.
#[test]
fn fixing_rewrites_and_restages() {
    let r = go_repo();
    r.git(&["config", "amont.fix", "true"]);
    // -l lists the file; -w actually writes the formatted content.
    shim(
        &r,
        "gofmt",
        "case \"$1\" in\n-l) echo main.go ;;\n-w) printf 'package main\\n\\nfunc main() {}\\n' > main.go ;;\nesac\nexit 0",
    );
    let (code, out) = commit_check(&r, "pre-commit-gofmt");
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("Go reformatted and re-staged"), "{out}");
    let staged = String::from_utf8_lossy(&r.git(&["show", ":main.go"]).stdout).to_string();
    assert!(
        staged.contains("func main"),
        "index holds the fix: {staged}"
    );
}

/// No `go.mod` above the staged file — not a Go module, nothing to check.
#[test]
fn a_moduleless_go_file_is_out_of_scope() {
    let r = Repo::new();
    r.stage("scripts/loose.go", "package main\n");
    shim(&r, "gofmt", "echo SHOULD-NOT-RUN\nexit 1");
    let (code, out) = commit_check(&r, "pre-commit-gofmt");
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("SHOULD-NOT-RUN"), "{out}");
}

/// A missing toolchain warns and never blocks — Unavailable's contract.
#[test]
fn a_missing_gofmt_warns_and_never_blocks() {
    let r = go_repo();
    // PATH is the shim dir ALONE, holding only a symlink to the real git —
    // so git works and gofmt is deterministically absent, whether or not the
    // machine running this test has a Go toolchain.
    let dir = r.path(".git/toolshims");
    std::fs::create_dir_all(&dir).unwrap();
    let git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|d| d.join("git"))
        .find(|p| p.is_file())
        .expect("git on PATH");
    std::os::unix::fs::symlink(git, dir.join("git")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-commit-gofmt")
        .current_dir(&r.dir)
        .env("PATH", r.path(".git/toolshims").display().to_string())
        .stdin(Stdio::null())
        .output()
        .expect("run");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(text.contains("gofmt is not installed"), "{text}");
}

// ---- go vet ----

#[test]
fn a_clean_vet_passes() {
    let r = go_repo();
    shim(&r, "go", "exit 0");
    let (code, out) = commit_check(&r, "pre-commit-go-vet");
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("go vet passed"), "{out}");
}

#[test]
fn vet_findings_block_the_commit() {
    let r = go_repo();
    shim(&r, "go", "echo 'main.go:1:1: unreachable code' >&2\nexit 1");
    let (code, out) = commit_check(&r, "pre-commit-go-vet");
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("go vet"), "{out}");
}

/// A dependency bump alone — `go.mod` with no `.go` staged — still vets.
#[test]
fn a_dependency_bump_alone_still_vets() {
    let r = Repo::new();
    r.stage("main.go", "package main\n");
    r.commit("chore: base");
    r.stage(
        "go.mod",
        "module example.com/x\n\nrequire example.com/dep v1.2.3\n",
    );
    shim(&r, "go", "echo VET-RAN >&2\nexit 0");
    let (code, out) = commit_check(&r, "pre-commit-go-vet");
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("VET-RAN"), "{out}");
}

// ---- go test ----

fn pushed_go_repo() -> (Repo, String) {
    let r = Repo::new();
    r.stage("go.mod", "module example.com/x\n");
    r.commit("chore: base");
    let base = String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    r.stage("main.go", "package main\n");
    r.commit("feat: go change");
    (r, base)
}

#[test]
fn a_green_suite_passes_the_push() {
    let (r, base) = pushed_go_repo();
    shim(&r, "go", "exit 0");
    let (code, out) = push_check(&r, "pre-push-go-test", &base);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("Go tests passed"), "{out}");
}

#[test]
fn a_red_suite_aborts_the_push() {
    let (r, base) = pushed_go_repo();
    shim(&r, "go", "echo '--- FAIL: TestX' \nexit 1");
    let (code, out) = push_check(&r, "pre-push-go-test", &base);
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("Go tests failed"), "{out}");
}

/// A ref pushing no Go runs nothing — the suite is not a toll.
#[test]
fn a_push_without_go_runs_nothing() {
    let r = Repo::new();
    r.stage("go.mod", "module example.com/x\n");
    r.commit("chore: base");
    let base = String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    r.stage("README.md", "docs only\n");
    r.commit("docs: change");
    shim(&r, "go", "echo SHOULD-NOT-RUN\nexit 1");
    let (code, out) = push_check(&r, "pre-push-go-test", &base);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("SHOULD-NOT-RUN"), "{out}");
}
