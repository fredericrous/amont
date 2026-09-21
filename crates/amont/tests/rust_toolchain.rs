//! The Rust checks and the toolchain pin: the cargo that answers must be the
//! one the repository pins, or the check says so rather than judging with
//! another.
#![cfg(unix)]

mod common;
use common::Repo;

use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

/// A fake `cargo` that reports one version and accepts every subcommand.
fn shim_cargo(r: &Repo, version: &str) {
    let dir = r.path(".git/toolshims");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let p = dir.join("cargo");
    std::fs::write(
        &p,
        format!(
            "#!/bin/sh\ncase \"$1\" in --version) echo \"cargo {version} (fake)\";; esac\nexit 0\n"
        ),
    )
    .expect("write");
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// A fake `rustup` whose `which cargo` names the shim cargo: a rustup that
/// honours the pin. Without it the host's rustup — when there is one — would
/// answer for its own toolchains, and the test would be about the host.
fn shim_rustup(r: &Repo) {
    let dir = r.path(".git/toolshims");
    let cargo = dir.join("cargo");
    let p = dir.join("rustup");
    std::fs::write(
        &p,
        format!(
            "#!/bin/sh\n[ \"$1 $2\" = \"which cargo\" ] && echo \"{}\" && exit 0\nexit 1\n",
            cargo.display()
        ),
    )
    .expect("write");
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// A `rustup` that answers nothing: the fallback-to-`which` path.
fn shim_no_rustup(r: &Repo) {
    let p = r.path(".git/toolshims/rustup");
    std::fs::write(&p, "#!/bin/sh\nexit 1\n").expect("write");
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// The shim directory first on PATH; the real PATH follows for git.
fn shimmed_path(r: &Repo) -> String {
    format!(
        "{}:{}",
        r.path(".git/toolshims").display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

fn clippy(r: &Repo) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-commit-clippy")
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

fn rust_repo(pin: &str) -> Repo {
    let r = Repo::new();
    r.stage(
        "Cargo.toml",
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    r.stage("src/lib.rs", "pub fn x() {}\n");
    r.stage(
        "rust-toolchain.toml",
        &format!("[toolchain]\nchannel = \"{pin}\"\n"),
    );
    r
}

#[test]
fn a_cargo_that_is_not_the_pinned_toolchain_fails_the_check() {
    let r = rust_repo("9.99.0");
    shim_cargo(&r, "1.0.0");
    shim_rustup(&r);
    let (code, out) = clippy(&r);
    assert_ne!(code, 0, "must not judge with the wrong toolchain: {out}");
    assert!(
        out.contains("cargo is 1.0.0") && out.contains("pins 9.99.0"),
        "names both versions: {out}"
    );
    assert!(out.contains("rustup shim"), "names the fix: {out}");
}

#[test]
fn the_pinned_toolchain_runs_the_check() {
    let r = rust_repo("1.0.0");
    shim_cargo(&r, "1.0.0");
    shim_rustup(&r);
    let (code, out) = clippy(&r);
    assert_eq!(
        code, 0,
        "the pin matches, clippy (the shim) runs and passes: {out}"
    );
    assert!(!out.contains("pins"), "no mismatch talk: {out}");
}

#[test]
fn a_channel_name_is_left_to_rustup() {
    let r = rust_repo("stable");
    shim_cargo(&r, "1.0.0");
    shim_rustup(&r);
    let (code, out) = clippy(&r);
    assert_eq!(code, 0, "nothing to hold `stable` against: {out}");
}

/// No rustup to ask: the first cargo on PATH is used, and still held against
/// the pin.
#[test]
fn without_rustup_the_path_cargo_is_used_and_still_checked() {
    let r = rust_repo("1.0.0");
    shim_cargo(&r, "1.0.0");
    shim_no_rustup(&r);
    let (code, out) = clippy(&r);
    assert_eq!(code, 0, "fallback cargo matches the pin: {out}");

    let r = rust_repo("2.0.0");
    shim_cargo(&r, "1.0.0");
    shim_no_rustup(&r);
    let (code, out) = clippy(&r);
    assert_ne!(code, 0, "fallback cargo does not match the pin: {out}");
    assert!(
        out.contains("cargo is 1.0.0") && out.contains("pins 2.0.0"),
        "{out}"
    );
}
