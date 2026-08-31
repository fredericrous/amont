//! The Python test gate — a fake pytest on a shimmed PATH gives each test
//! the exit code, and the gate's own contract does the rest.
#![cfg(unix)]

mod common;
use common::Repo;

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

fn shim(r: &Repo, body: &str) {
    let dir = r.path(".git/toolshims");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let p = dir.join("pytest");
    std::fs::write(&p, format!("#!/bin/sh\n{body}")).expect("write");
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// A pytest inside the project's own `.venv`, which is where a Python project
/// keeps the interpreter that can actually import it.
fn venv_shim(r: &Repo, body: &str) {
    let dir = r.path(".venv/bin");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let p = dir.join("pytest");
    std::fs::write(&p, format!("#!/bin/sh\n{body}")).expect("write");
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

fn push(r: &Repo, from: &str) -> (i32, String) {
    let head = String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    let line = format!("refs/heads/feat/x {head} refs/heads/feat/x {from}\n");
    let path = format!(
        "{}:{}",
        r.path(".git/toolshims").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-push-pytest")
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

fn repo() -> (Repo, String) {
    let r = Repo::new();
    r.stage("conftest.py", "# pytest setup\n");
    r.commit("chore: base");
    let base = String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    r.stage("test_x.py", "def test_x():\n    assert True\n");
    r.commit("feat: python change");
    (r, base)
}

/// A green suite passes the push and says so.
#[test]
fn a_green_suite_passes() {
    let (r, base) = repo();
    shim(&r, "exit 0");
    let (code, out) = push(&r, &base);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("Python tests passed"), "{out}");
}

/// A red suite aborts the push, fail-fast.
#[test]
fn a_red_suite_aborts_the_push() {
    let (r, base) = repo();
    shim(&r, "echo 'FAILED test_x.py::test_x'\nexit 1");
    let (code, out) = push(&r, &base);
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("Python tests failed"), "{out}");
}

/// A ref pushing no Python runs nothing — the suite is not a toll.
#[test]
fn a_push_without_python_runs_nothing() {
    let r = Repo::new();
    r.stage("conftest.py", "# setup\n");
    r.commit("chore: base");
    let base = String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    r.stage("README.md", "docs only\n");
    r.commit("docs: change");
    shim(&r, "echo SHOULD-NOT-RUN\nexit 1");
    let (code, out) = push(&r, &base);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("SHOULD-NOT-RUN"), "{out}");
}

/// The project's own `.venv/bin/pytest` beats whatever is first on `PATH`.
///
/// This gate used to resolve with a bare `which("pytest")`, unlike `ruff` and
/// `pyright` beside it, which go through `resolve_python_tool`. A project's
/// tests import the project, and a `PATH` pytest belongs to whatever
/// interpreter happens to be first there — in a `src/` layout it cannot see
/// the package at all. The gate then reported `ModuleNotFoundError` as a test
/// FAILURE and blocked the push, in a repository whose suite passes under
/// `uv run pytest`. A gate that cannot tell "your tests broke" from "I ran the
/// wrong interpreter" is worse than no gate.
#[test]
fn the_projects_own_venv_beats_whatever_is_first_on_path() {
    let (r, from) = repo();
    // The one on PATH would fail the push, and says so if it is ever reached.
    shim(&r, "echo FROM-PATH-PYTEST; exit 1\n");
    // The one that belongs to the project passes.
    venv_shim(&r, "echo FROM-VENV-PYTEST; exit 0\n");

    let (code, out) = push(&r, &from);
    assert!(
        out.contains("FROM-VENV-PYTEST"),
        "the venv's pytest must be the one that runs: {out}"
    );
    assert!(
        !out.contains("FROM-PATH-PYTEST"),
        "the PATH pytest must not be reached: {out}"
    );
    assert_eq!(code, 0, "and its green result is the verdict: {out}");
}
