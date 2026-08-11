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
