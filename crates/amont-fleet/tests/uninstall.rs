//! `amont-fleet uninstall` deletes shims independently of `fix`/`apply` —
//! it is a third code path that reimplements "delete our files" — so it must
//! repeat the guard those two treat as mandatory: never delete a path git
//! tracks, however `.git/hooks` was reached. `fix.rs`'s doc explains why that
//! guard exists at all: `~/.config/git/git-templates` is commonly a SYMLINK to
//! a checkout, and an install/uninstall step that only checked "is this ours"
//! and not "is this tracked" destroyed tracked source before.

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

fn shim_text() -> String {
    std::fs::read_to_string(repo_root().join("templates/hooks/pre-commit"))
        .expect("template")
        .replace("__AMONT_BIN__", "/bin/fake-amont")
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[cfg(unix)]
#[test]
fn uninstall_refuses_a_shim_reached_through_a_symlink_into_a_tracked_checkout() {
    let root = std::env::temp_dir().join(format!("fleet-uninstall-symlink-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("mkdir");

    // A real checkout, unrelated to the fleet root, that TRACKS what looks
    // like a hooks directory — exactly the shape of `git-templates` itself.
    let checkout = root.join("checkout");
    std::fs::create_dir_all(checkout.join("hooks")).expect("mkdir");
    git(&checkout, &["init", "-q", "--template=", "."]);
    git(&checkout, &["config", "user.email", "t@t"]);
    git(&checkout, &["config", "user.name", "t"]);
    std::fs::write(checkout.join("hooks/pre-commit"), shim_text()).expect("write");
    git(&checkout, &["add", "hooks/pre-commit"]);
    git(
        &checkout,
        &["commit", "-q", "--no-verify", "-m", "chore: seed"],
    );

    // A "repository" in the fleet whose `.git` is a SYMLINK into that
    // checkout — so `<victim>/.git/hooks/pre-commit` physically IS the
    // tracked file above, even though nothing under `<victim>` says so.
    let victim = root.join("victim");
    std::fs::create_dir_all(&victim).expect("mkdir");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&checkout, victim.join(".git")).expect("symlink");

    let out = Command::new(env!("CARGO_BIN_EXE_amont-fleet"))
        .args(["uninstall", "--root"])
        .arg(&root)
        .output()
        .expect("amont-fleet uninstall");
    assert!(out.status.success(), "{:?}", out);

    let tracked_still_there = std::fs::read_to_string(checkout.join("hooks/pre-commit"))
        .map(|c| c == shim_text())
        .unwrap_or(false);
    assert!(
        tracked_still_there,
        "uninstall deleted a shim that was tracked source reached through a symlink:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// root ignores permission bits, so a test built on them proves nothing there.
#[cfg(unix)]
fn running_as_root() -> bool {
    extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() == 0 }
}

/// VERIFIED: with a read-only `.git/hooks`, `uninstall` printed
/// `0 shims removed from 0 repositories`, exited 0, and left all four shims
/// installed and running. The repository was not even listed.
///
/// The whole failure was one `&&` chain:
///
/// ```text
/// if ours && !is_tracked(&path) && std::fs::remove_file(&path).is_ok() { here += 1; }
/// ```
///
/// "not ours", "tracked" and "the unlink failed" are three different outcomes
/// and that expression has one. A tool reporting success for work it did not do
/// is worse than one that fails, because the user stops looking.
#[cfg(unix)]
#[test]
fn a_hooks_directory_we_cannot_write_to_is_reported_and_exits_nonzero() {
    use std::os::unix::fs::PermissionsExt;
    if running_as_root() {
        return;
    }
    let root =
        std::env::temp_dir().join(format!("fleet-uninstall-readonly-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let hooks = root.join("victim/.git/hooks");
    std::fs::create_dir_all(&hooks).expect("mkdir");
    for name in [
        "commit-msg",
        "post-commit",
        "pre-commit",
        "pre-push",
        "prepare-commit-msg",
    ] {
        std::fs::write(hooks.join(name), shim_text()).expect("write");
    }
    std::fs::set_permissions(&hooks, std::fs::Permissions::from_mode(0o555)).expect("chmod");

    let out = Command::new(env!("CARGO_BIN_EXE_amont-fleet"))
        .args(["uninstall", "--root"])
        .arg(&root)
        .output()
        .expect("amont-fleet uninstall");
    let _ = std::fs::set_permissions(&hooks, std::fs::Permissions::from_mode(0o755));

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(1),
        "shims still installed after `uninstall` is not success:\n{stdout}"
    );
    assert!(
        stdout.contains("FAILED to remove"),
        "and the failure must be a block, not a missing row:\n{stdout}"
    );
    assert!(
        stdout.contains("pre-commit"),
        "naming what it could not remove:\n{stdout}"
    );
    assert!(
        hooks.join("pre-commit").exists(),
        "fixture: the shim must genuinely still be there"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// README promises that a hook somebody else wrote is never taken. Keeping that
/// promise SILENTLY is indistinguishable from having missed the file: the user
/// asked for four shims to go and got three, with nothing said about the
/// fourth.
#[test]
fn a_hook_we_did_not_write_is_named_not_silently_skipped() {
    let root = std::env::temp_dir().join(format!("fleet-uninstall-theirs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let hooks = root.join("victim/.git/hooks");
    std::fs::create_dir_all(&hooks).expect("mkdir");
    std::fs::write(hooks.join("pre-commit"), shim_text()).expect("write");
    let theirs = "#!/bin/sh\n# my own commit-msg, thanks\nexec my-linter \"$@\"\n";
    std::fs::write(hooks.join("commit-msg"), theirs).expect("write");

    let out = Command::new(env!("CARGO_BIN_EXE_amont-fleet"))
        .args(["uninstall", "--root"])
        .arg(&root)
        .output()
        .expect("amont-fleet uninstall");
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(out.status.success(), "{stdout}");
    assert!(
        !hooks.join("pre-commit").exists(),
        "ours must still be removed:\n{stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(hooks.join("commit-msg"))
            .ok()
            .as_deref(),
        Some(theirs),
        "and theirs must be untouched"
    );
    assert!(
        stdout.contains("left alone (not ours)") && stdout.contains("commit-msg"),
        "leaving it alone silently is the other half of the same bug:\n{stdout}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// The ordinary case, unaffected: an untracked shim in a plain (non-symlinked)
/// repo is still removed.
#[test]
fn uninstall_still_removes_an_ordinary_untracked_shim() {
    let root = std::env::temp_dir().join(format!("fleet-uninstall-plain-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let hooks = root.join("victim/.git/hooks");
    std::fs::create_dir_all(&hooks).expect("mkdir");
    std::fs::write(hooks.join("pre-commit"), shim_text()).expect("write");

    let out = Command::new(env!("CARGO_BIN_EXE_amont-fleet"))
        .args(["uninstall", "--root"])
        .arg(&root)
        .output()
        .expect("amont-fleet uninstall");
    assert!(out.status.success(), "{:?}", out);
    assert!(
        !hooks.join("pre-commit").exists(),
        "an ordinary, untracked shim must still be removed"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// The fleet removed shims from 200 repositories and forgot nothing in any of
/// them: 200 stamp refs, ledgers and trust records describing hooks that were
/// no longer there. It now sweeps the same list `amont uninstall` does — and
/// stops at the same line, held work included.
#[test]
fn uninstall_forgets_our_bookkeeping_but_not_the_users_work() {
    let root = std::env::temp_dir().join(format!("fleet-uninstall-forget-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("managed");
    std::fs::create_dir_all(&repo).expect("mkdir");
    git(&repo, &["init", "-q"]);
    let hooks = repo.join(".git/hooks");
    std::fs::create_dir_all(&hooks).expect("mkdir");
    std::fs::write(hooks.join("pre-commit"), shim_text()).expect("write");

    // Ours: a trust record, a ledger, a skew marker.
    git(&repo, &["config", "amont.trusted", "deadbeef"]);
    git(&repo, &["config", "hook.skip", "pre-commit-clippy"]);
    std::fs::write(repo.join(".git/amont-bypasses"), "amont-bypasses-v1\n").expect("write");
    // Theirs: parked work an interrupted run left behind.
    std::fs::create_dir_all(repo.join(".git/amont-held")).expect("mkdir");
    std::fs::write(repo.join(".git/amont-held/index"), b"amont-held-v1\0").expect("write");

    let out = Command::new(env!("CARGO_BIN_EXE_amont-fleet"))
        .args(["uninstall", "--root"])
        .arg(&root)
        .output()
        .expect("amont-fleet uninstall");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}");

    let config = |key: &str| -> String {
        String::from_utf8_lossy(
            &Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["config", "--local", "--get", key])
                .output()
                .expect("git")
                .stdout,
        )
        .trim()
        .to_string()
    };
    assert!(
        config("amont.trusted").is_empty(),
        "trust survived a fleet uninstall:\n{stdout}"
    );
    assert!(
        !repo.join(".git/amont-bypasses").exists(),
        "the ledger outlived the hooks it describes:\n{stdout}"
    );
    assert!(
        stdout.contains("forgot"),
        "sweeping 200 repositories must say so:\n{stdout}"
    );
    assert_eq!(
        config("hook.skip"),
        "pre-commit-clippy",
        "the user's own policy is not ours to drop:\n{stdout}"
    );
    assert!(
        repo.join(".git/amont-held/index").exists(),
        "PARKED WORK was deleted — the one thing uninstall must never take:\n{stdout}"
    );

    let _ = std::fs::remove_dir_all(&root);
}
