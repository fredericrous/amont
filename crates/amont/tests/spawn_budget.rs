//! The commit path's founding argument is subprocess cost — dispatch.rs
//! narrates collapsing 27 processes into one — and nothing guarded it: a
//! change re-introducing a git spawn per check passed every test. This is
//! the deterministic guard the stopwatch test the repo rightly rejected
//! could not be: a PATH-shimmed git that counts its own invocations.

mod common;
use common::Repo;

/// A BUDGET, not an exact pin: tool presence moves the count by a few, and
/// the point is catching the o(checks) regression class — one new spawn per
/// check is +17 at a stroke — not a diff of one.
#[cfg(unix)]
#[test]
fn a_small_commit_stays_inside_the_git_spawn_budget() {
    use std::os::unix::fs::PermissionsExt;
    let r = Repo::new();
    r.stage("a.txt", "hello\n");

    let real = String::from_utf8(
        std::process::Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .expect("sh")
            .stdout,
    )
    .expect("utf8");
    let real = real.trim();
    let shims = r.path(".git/gitshim");
    std::fs::create_dir_all(&shims).expect("mkdir");
    let count = r.path(".git/git-count");
    std::fs::write(
        shims.join("git"),
        format!(
            "#!/bin/sh\nprintf x >> {}\nexec {real} \"$@\"\n",
            count.display()
        ),
    )
    .expect("write");
    std::fs::set_permissions(shims.join("git"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");

    let path = format!(
        "{}:{}",
        shims.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_amont"))
        .args([
            "--hooks-dir",
            &r.path(".git/hooks").to_string_lossy(),
            "pre-commit",
        ])
        .current_dir(&r.dir)
        .env("PATH", path)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run pre-commit");
    assert!(
        out.status.success(),
        "pre-commit failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let spawns = std::fs::read_to_string(&count)
        .map(|s| s.len())
        .unwrap_or(0);
    assert!(
        spawns > 0,
        "the shim never ran — the budget measured nothing"
    );
    assert!(
        spawns <= 25,
        "a one-file commit spawned git {spawns} times — the per-stage snapshot \
         (staged_files/repo_root caching) has regressed, or a check grew its \
         own git habit; raise this budget only with a reason written here"
    );
}

/// post-commit runs on EVERY commit, and the bypass ledger's fast path
/// promises an ungated repository pays zero extra spawns for it. This pins
/// the whole hook — entrypoint bookkeeping included — to a hard budget, so
/// "zero extra" stays a fact rather than an intention.
#[cfg(unix)]
#[test]
fn post_commit_in_an_ungated_repo_stays_near_free() {
    use std::os::unix::fs::PermissionsExt;
    let r = Repo::new();
    r.stage("a.txt", "hello\n");
    r.commit("chore: base");

    let real = String::from_utf8(
        std::process::Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .expect("sh")
            .stdout,
    )
    .expect("utf8");
    let real = real.trim();
    let shims = r.path(".git/gitshim");
    std::fs::create_dir_all(&shims).expect("mkdir");
    let count = r.path(".git/git-count");
    std::fs::write(
        shims.join("git"),
        format!(
            "#!/bin/sh\nprintf x >> {}\nexec {real} \"$@\"\n",
            count.display()
        ),
    )
    .expect("write");
    std::fs::set_permissions(shims.join("git"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");

    let path = format!(
        "{}:{}",
        shims.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_amont"))
        .args([
            "--hooks-dir",
            &r.path(".git/hooks").to_string_lossy(),
            "post-commit",
        ])
        .current_dir(&r.dir)
        .env("PATH", path)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run post-commit");
    assert!(
        out.status.success(),
        "post-commit failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let spawns = std::fs::read_to_string(&count)
        .map(|s| s.len())
        .unwrap_or(0);
    assert!(
        spawns <= 4,
        "post-commit in an ungated repo spawned git {spawns} times — the \
         bypass ledger's zero-spawn fast path (gate_names_declared before any \
         config or diff-tree) has regressed; raise this budget only with a \
         reason written here"
    );
}

/// A repo WITH committed policy pays a bounded toll, not o(checks): trust
/// verification plus one scoped `--show-scope` read per family (severities,
/// settings) and one typed read per consulted setting. The no-policy budget
/// above is the real invariant; this pins the policy path from creeping.
#[cfg(unix)]
#[test]
fn a_policy_repo_stays_inside_a_bounded_budget() {
    use std::os::unix::fs::PermissionsExt;
    let r = Repo::new();
    r.stage(
        "amont.conf",
        "severity lint-json-yaml warn\nskip yamllint\nset largeFileWarn 1\nset commit.subjectMax 50\n",
    );
    let trust = std::process::Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("trust")
        .current_dir(&r.dir)
        .output()
        .expect("amont trust");
    assert!(trust.status.success(), "could not trust the manifest");
    r.stage("a.txt", "hello\n");

    let real = String::from_utf8(
        std::process::Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .expect("sh")
            .stdout,
    )
    .expect("utf8");
    let real = real.trim();
    let shims = r.path(".git/gitshim");
    std::fs::create_dir_all(&shims).expect("mkdir");
    let count = r.path(".git/git-count");
    std::fs::write(
        shims.join("git"),
        format!(
            "#!/bin/sh\nprintf x >> {}\nexec {real} \"$@\"\n",
            count.display()
        ),
    )
    .expect("write");
    std::fs::set_permissions(shims.join("git"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");

    let path = format!(
        "{}:{}",
        shims.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_amont"))
        .args([
            "--hooks-dir",
            &r.path(".git/hooks").to_string_lossy(),
            "pre-commit",
        ])
        .current_dir(&r.dir)
        .env("PATH", path)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run pre-commit");
    assert!(
        out.status.success(),
        "pre-commit failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let spawns = std::fs::read_to_string(&count)
        .map(|s| s.len())
        .unwrap_or(0);
    assert!(
        spawns > 0,
        "the shim never ran — the budget measured nothing"
    );
    assert!(
        spawns <= 35,
        "a one-file commit under committed policy spawned git {spawns} times \
         — the scoped-read caches (key_set_above_policy's OnceLock, the \
         severity fold) have regressed, or a setting grew a per-check read; \
         raise this budget only with a reason written here"
    );
}
