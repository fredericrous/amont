//! pre-push-branch-protect, driven by REAL git pushing to a REAL remote.
//!
//! Every other test of this check hands the dispatcher a ref line that a Rust
//! test wrote, which asserts the check's logic against our own belief about
//! git's `pre-push` protocol. That belief was wrong in one place for as long
//! as the check existed: git sends 40 zeros as the REMOTE oid when the branch
//! does not exist on the remote yet, the check never read that field, and so
//! it refused the FIRST push of every new repository — a push whose advice
//! ("Open a Pull Request") cannot be followed, because there is no base
//! branch to open one against. The only way out was `--no-verify`, which
//! switches off every other pre-push gate too.
//!
//! A unit test could not catch it, because the fixture that would have caught
//! it is the one that was wrong. So this file uses no fixture: git decides
//! what goes on stdin.

mod common;
use common::Repo;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A bare repository to push into, beside the working one.
fn bare_remote(r: &Repo) -> PathBuf {
    let remote = r.dir.join("..").join(format!(
        "{}-origin.git",
        r.dir.file_name().unwrap().to_string_lossy()
    ));
    let _ = std::fs::remove_dir_all(&remote);
    let out = Command::new("git")
        .args([
            "init",
            "-q",
            "--bare",
            "--template=",
            "--initial-branch=main",
        ])
        .arg(&remote)
        .output()
        .expect("git init --bare");
    assert!(out.status.success(), "init bare: {out:?}");
    remote
}

/// Install our real `pre-push` shim, the one git will actually run.
fn install_pre_push(r: &Repo) {
    let hooks = r.dir.join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).expect("hooks dir");
    let bin = Path::new(env!("CARGO_BIN_EXE_amont"));
    let shim = format!(
        "#!/bin/sh\nexec '{}' --hooks-dir \"$(dirname \"$0\")\" pre-push \"$@\"\n",
        bin.display()
    );
    let path = hooks.join("pre-push");
    std::fs::write(&path, shim).expect("write shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
}

fn push(r: &Repo) -> (bool, String) {
    let out = r.git(&["push", "origin", "main"]);
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// The whole bug, in one test: the first push creates `main` on the remote
/// and must be allowed; the second updates it and must be refused.
///
/// Both halves matter. The first alone would pass if somebody switched the
/// check off entirely.
#[cfg(unix)] // the shim is `#!/bin/sh`; Windows resolves hooks differently
#[test]
fn the_push_that_creates_main_is_allowed_and_the_next_one_is_not() {
    let r = Repo::new();
    let remote = bare_remote(&r);
    r.stage("a.txt", "one\n");
    r.git(&["commit", "-m", "chore: init"]);
    r.git(&["remote", "add", "origin", &remote.display().to_string()]);
    install_pre_push(&r);

    let (ok, out) = push(&r);
    assert!(
        ok,
        "the first push of a new repository creates main on the remote — \
         there is no history to protect and no PR to open against:\n{out}"
    );

    // And it really landed, rather than being a push of nothing.
    let ls = Command::new("git")
        .args([
            "ls-remote",
            &remote.display().to_string(),
            "refs/heads/main",
        ])
        .output()
        .expect("ls-remote");
    assert!(
        !String::from_utf8_lossy(&ls.stdout).trim().is_empty(),
        "main should now exist on the remote"
    );

    r.stage("b.txt", "two\n");
    r.git(&["commit", "-m", "chore: more"]);
    let (ok, out) = push(&r);
    assert!(
        !ok,
        "once main exists on the remote, a direct push to it is refused:\n{out}"
    );
    assert!(out.contains("forbidden"), "{out}");
    assert!(out.contains("Pull Request"), "{out}");

    let _ = std::fs::remove_dir_all(&remote);
}

/// A feature branch is created and updated freely — the check is about
/// `main`, not about creation.
#[cfg(unix)]
#[test]
fn a_feature_branch_pushes_twice_without_complaint() {
    let r = Repo::new();
    let remote = bare_remote(&r);
    r.stage("a.txt", "one\n");
    r.git(&["commit", "-m", "chore: init"]);
    r.git(&["remote", "add", "origin", &remote.display().to_string()]);
    r.git(&["switch", "-c", "feat/x"]);
    install_pre_push(&r);

    for round in 1..=2 {
        r.stage(&format!("f{round}.txt"), "x\n");
        r.git(&["commit", "-m", "chore: work"]);
        let out = r.git(&["push", "origin", "feat/x"]);
        assert!(
            out.status.success(),
            "round {round}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let _ = std::fs::remove_dir_all(&remote);
}
