//! Linters run at ZERO warnings. A warning-level finding that exits 0 is a
//! list nobody is forced to read — a human scrolls past it, an agent reads
//! "passed" and moves on — so every linter with a warning class is invoked
//! with the flag that makes warnings fail: eslint `--max-warnings 0`,
//! yamllint `--strict`, pyright `--warnings` (clippy has always run with
//! `-D warnings`). Fake tools on a shimmed PATH record their argv; these
//! tests pin that the flags actually reach them.
#![cfg(unix)]

mod common;
use common::Repo;

use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

/// A fake tool that answers `--version`, records every other invocation's
/// argv to `argv.txt`, and exits 0.
fn recording_shim(r: &Repo, name: &str) {
    let dir = r.path(".git/toolshims");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let log = r.path(".git/toolshims/argv.txt");
    let p = dir.join(name);
    std::fs::write(
        &p,
        format!(
            "#!/bin/sh\ncase \"$1\" in --version) echo 1.0.0; exit 0;; esac\necho \"$@\" >> {}\nexit 0\n",
            log.display()
        ),
    )
    .expect("write");
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

fn run_check(r: &Repo, check: &str) -> String {
    // PATH is the shim dir ALONE, holding the recording shim plus a symlink
    // to the real git — so resolution cannot wander off to uv, npx, a venv,
    // or a real tool the machine happens to have. The CI runners resolve
    // pyright through uv where a laptop falls through to PATH; a test about
    // WHICH FLAGS reach the tool must not depend on that difference.
    let dir = r.path(".git/toolshims");
    let git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|d| d.join("git"))
        .find(|p| p.is_file())
        .expect("git on PATH");
    let _ = std::os::unix::fs::symlink(git, dir.join("git"));
    let out = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg(check)
        .current_dir(&r.dir)
        .env("PATH", dir.display().to_string())
        .stdin(Stdio::null())
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "the shim exits 0, so the check passes: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::read_to_string(r.path(".git/toolshims/argv.txt")).unwrap_or_default()
}

#[test]
fn eslint_is_held_to_zero_warnings() {
    let r = Repo::new();
    r.stage(".eslintrc.json", "{}\n");
    r.stage("a.js", "let x = 1\n");
    recording_shim(&r, "eslint");
    let argv = run_check(&r, "pre-commit-lint-js");
    assert!(argv.contains("--max-warnings 0"), "argv was: {argv}");
}

#[test]
fn yamllint_runs_strict() {
    let r = Repo::new();
    r.write(".yamllint", "rules:\n  trailing-spaces: enable\n");
    r.stage("a.yaml", "a: 1\n");
    recording_shim(&r, "yamllint");
    let argv = run_check(&r, "pre-commit-yamllint");
    assert!(argv.contains("--strict"), "argv was: {argv}");
}

#[test]
fn pyright_fails_on_warning_diagnostics_too() {
    let r = Repo::new();
    r.stage("pyrightconfig.json", "{}\n");
    r.stage("a.py", "x = 1\n");
    recording_shim(&r, "pyright");
    let argv = run_check(&r, "pre-commit-pyright");
    assert!(argv.contains("--warnings"), "argv was: {argv}");
}
