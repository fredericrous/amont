//! pre-push-pull-rebase, ported from its zsh suite.
//!
//! The hardening is the point and is preserved exactly: never touch a dirty
//! tree, rebase only onto the branch's OWN upstream (an older version used
//! `origin HEAD`, which resolves to the remote's default branch and silently
//! rebased every push onto main), and abort cleanly on conflict.
//!
//! The states a push journey passes through (no upstream, a dirty tree, in
//! sync, diverged after an amend, behind with `amont.autoRebase` off and
//! on, an upstream merged away) are steps of `flows/push.rs`, driven by
//! real pushes. What stays here are the states that need a setup of their
//! own: a local upstream, a worktree marker, a remote not named `origin`,
//! offline, a hung remote, a tag push.

use crate::common;
use common::Repo;

fn with_origin() -> Repo {
    let r = Repo::new();
    r.stage("a.txt", "x\n");
    r.commit("init");
    // The bare origin goes under .git/, NOT in the working tree. Anywhere in
    // the tree it shows as untracked, the tree is dirty, and every case below
    // silently takes the dirty-tree early exit instead of the path it claims
    // to test. The zsh suite hit this and worked around it with a committed
    // .gitignore; putting the repo somewhere git never scans removes the
    // hazard rather than papering over it.
    let origin = r.path(".git/test-origin.git");
    r.git(&["init", "-q", "--bare", origin.to_str().unwrap()]);
    r.git(&["remote", "add", "origin", origin.to_str().unwrap()]);
    assert!(
        String::from_utf8_lossy(&r.git(&["status", "--porcelain"]).stdout)
            .trim()
            .is_empty(),
        "the fixture itself must leave a CLEAN tree, or these tests prove nothing"
    );
    r
}

/// The incident this guard exists for: `git worktree add -b <new> <path>
/// main` sets the new branch's upstream to the LOCAL branch `main` — no `/`
/// in `@{u}` at all. The old code split on `/` and fell back to `("origin",
/// upstream)`, so it silently treated "main" as "origin/main" and ran a bare
/// `pull --rebase`, which actually synced from LOCAL main per
/// `branch.*.remote`/`.merge` — not origin, whatever the messages said. If
/// local main is later moved (a `reset --hard` in another worktree, say),
/// the next push rebases onto wherever it ended up, unannounced. The fix
/// does not merely hope a same-history rebase is harmless here — it never
/// attempts one at all when the upstream is not a real remote.
#[test]
fn a_branch_tracking_a_local_branch_is_not_silently_synced_as_origin() {
    let r = with_origin();
    r.git(&["checkout", "-q", "-b", "feat/from-local"]);
    r.stage("b.txt", "extra work\n");
    r.commit("extra work");
    // No `-u origin`: track LOCAL main instead, exactly what
    // `git worktree add -b <new> <path> main` does by default.
    r.git(&["branch", "--set-upstream-to=main", "feat/from-local"]);
    let before = String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();

    let run = r.hook("pre-push-pull-rebase", &[]);
    assert!(run.passed(), "{}", run.output());
    assert!(
        run.says("not a remote-tracking branch"),
        "a local-branch upstream must be named and skipped, not guessed at as origin: {}",
        run.output()
    );

    let after = String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    assert_eq!(
        before, after,
        "the branch must not be touched at all when its upstream is not a real remote"
    );
}

/// Step 4's advisory has to survive the WORKTREE MARKER.
///
/// Since git 2.23, `git branch` prints `+ main` for a branch checked out in
/// another worktree, not `  main`. `lists_branch` stripped only spaces, tabs
/// and `*`, so in exactly the layout this project's own workflow uses — work in
/// a linked worktree while `main` stays checked out in the primary — it
/// answered false for both `main` and `master` and the whole default-branch
/// advisory silently never fired.
#[test]
fn the_default_branch_advisory_survives_a_worktree_marker() {
    let r = with_origin();
    r.stage("a.txt", "one\n");
    r.commit("chore: seed");
    r.git(&["push", "-q", "--no-verify", "-u", "origin", "main"]);

    // The work happens in a linked worktree; `main` stays checked out here, so
    // `git branch` over there prints `+ main`.
    let wt = r.worktree("feat/adv");
    let mut push = std::process::Command::new("git");
    push.args(["push", "-q", "--no-verify", "-u", "origin", "feat/adv"])
        .current_dir(&wt);
    Repo::strip_git_env_impl(&mut push);
    assert!(push.status().expect("push").success());

    // Move the default branch ahead on the server.
    r.stage("a.txt", "two\n");
    r.commit("chore: move main on");
    r.git(&["push", "-q", "--no-verify", "-u", "origin", "main"]);

    let listed = String::from_utf8_lossy(
        &std::process::Command::new("git")
            .args(["branch"])
            .current_dir(&wt)
            .output()
            .expect("git branch")
            .stdout,
    )
    .into_owned();
    assert!(
        listed.contains("+ main"),
        "fixture: expected the worktree marker, got {listed:?}"
    );

    let run = r.hook_at(&wt, "pre-push-pull-rebase", &[]);
    assert!(run.passed(), "{}", run.output());
    assert!(
        run.says("is ahead by 1 commit"),
        "the advisory never fired past the worktree marker:\n{}",
        run.output()
    );
}

/// Step 2 spends thirteen lines of comment establishing `remote` as the
/// VERIFIED remote for this branch, and step 4 then hardcoded `origin`. In a
/// repository whose remote is called `upstream`, the fetch failed (ignored) and
/// `rev-list origin/<branch>...HEAD` returned `None`, so the advisory was
/// silently skipped.
///
/// The branch is `test/zz` on purpose: it sorts AFTER `main`, so `git branch`
/// lists the default branch FIRST — which is the arrangement that also exposes
/// the trim hazard in step 4 (`git::stdout` trimmed the whole buffer and ate
/// the first line's indentation, so `lists_branch`'s decoration guard rejected
/// it). Whether the advisory fired used to depend on the alphabetical position
/// of the branch you happened to be on.
#[test]
fn the_advisory_uses_the_branch_s_own_remote() {
    let r = Repo::new();
    r.stage("a.txt", "one\n");
    r.commit("chore: seed");
    let remote = r.path(".git/test-upstream.git");
    r.git(&["init", "-q", "--bare", remote.to_str().unwrap()]);
    r.git(&["remote", "add", "upstream", remote.to_str().unwrap()]);
    r.git(&["push", "-q", "--no-verify", "-u", "upstream", "main"]);

    r.git(&["checkout", "-q", "-b", "test/zz"]);
    r.git(&["push", "-q", "--no-verify", "-u", "upstream", "test/zz"]);

    // Move the default branch ahead on the server, then come back.
    r.git(&["checkout", "-q", "main"]);
    r.stage("a.txt", "two\n");
    r.commit("chore: move main on");
    r.git(&["push", "-q", "--no-verify", "upstream", "main"]);
    r.git(&["checkout", "-q", "test/zz"]);

    let listed = String::from_utf8_lossy(&r.git(&["branch"]).stdout).into_owned();
    assert!(
        listed.starts_with("  main"),
        "fixture: main must sort first for this case to bite, got {listed:?}"
    );

    let run = r.hook("pre-push-pull-rebase", &[]);
    assert!(run.passed(), "{}", run.output());
    assert!(
        run.says("upstream/main is ahead by 1 commit"),
        "the advisory looked for a remote this repo does not have:\n{}",
        run.output()
    );
}

/// Offline is not "your branch was deleted". `ls-remote --exit-code` exits 2
/// for a missing ref and 128 for a failed connection, and the hook used to
/// read both as the first — a wrong diagnosis someone acts on.
#[test]
fn offline_reads_as_unreachable_not_deleted() {
    let r = with_origin();
    r.git(&["checkout", "-q", "-b", "feat/offline"]);
    r.git(&["push", "-q", "--no-verify", "-u", "origin", "feat/offline"]);
    // The upstream is configured and healthy; now the network "goes away".
    r.git(&["remote", "set-url", "origin", "/nonexistent/nowhere.git"]);
    let run = r.hook("pre-push-pull-rebase", &[]);
    assert!(run.passed(), "{}", run.output());
    assert!(
        run.says("Could not reach"),
        "offline must read as unreachable:\n{}",
        run.output()
    );
    assert!(
        !run.says("no longer exists"),
        "offline must NOT read as a deleted upstream:\n{}",
        run.output()
    );
}

/// A remote that accepts the connection and then says nothing must not hold
/// the push hostage: the probe is killed at its deadline and the sync is
/// skipped, honestly. `amont.timeout 1` shrinks the probe budget to 1s, so
/// the hung shim's 30s sleep proves the kill rather than the wait.
#[cfg(unix)]
#[test]
fn a_hung_remote_is_cut_at_the_deadline() {
    let r = with_origin();
    r.git(&["checkout", "-q", "-b", "feat/hung"]);
    r.git(&["push", "-q", "--no-verify", "-u", "origin", "feat/hung"]);
    r.git(&["config", "amont.timeout", "1"]);

    let real = String::from_utf8(
        std::process::Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .expect("sh")
            .stdout,
    )
    .expect("utf8");
    let real = real.trim().to_string();
    let shims = r.path(".git/gitshim");
    std::fs::create_dir_all(&shims).expect("mkdir");
    common::fixture_exec(
        &shims.join("git"),
        &format!("#!/bin/sh\ncase \"$1\" in ls-remote) sleep 30 ;; esac\nexec {real} \"$@\"\n"),
    );

    let started = std::time::Instant::now();
    let mut cmd = crate::common::amont_cmd();
    cmd.arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-push-pull-rebase")
        .current_dir(&r.dir)
        .stdin(std::process::Stdio::null())
        .env(
            "PATH",
            format!(
                "{}:{}",
                shims.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        );
    let out = cmd.output().expect("run hook");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains("did not answer within"),
        "the deadline must be named:\n{text}"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(15),
        "the probe was not killed at its deadline (took {:?})",
        started.elapsed()
    );
}

/// Run the hook as git does, with `line` on stdin: the refs this push carries.
fn hook_with_push(r: &Repo, line: &str) -> (i32, String) {
    use std::io::Write;
    use std::process::Stdio;
    let mut cmd = crate::common::amont_cmd();
    cmd.arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-push-pull-rebase")
        .current_dir(&r.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    Repo::strip_git_env_impl(&mut cmd);
    Repo::pin_host_env(&mut cmd);
    let mut child = cmd.spawn().expect("run amont");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(line.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// A push of a tag publishes no branch, so the checkout is not this push's
/// business. Pushing release tags from the live checkout on `main` used to
/// rebase `main` and refuse the tag with "push again" (2026-10-15, three
/// times). A push that carries the branch still syncs it.
#[test]
fn a_tag_push_leaves_a_behind_branch_alone() {
    let r = with_origin();
    r.stage("b.txt", "ahead on the remote\n");
    r.commit("chore: remote-side commit");
    r.git(&["push", "-q", "--no-verify", "-u", "origin", "main"]);
    r.git(&["reset", "--hard", "-q", "HEAD~1"]);
    r.git(&["fetch", "-q", "origin"]);
    let head = |r: &Repo| {
        String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
            .trim()
            .to_string()
    };
    let before = head(&r);
    let zero = "0".repeat(before.len());

    let tag = format!("refs/tags/v1 {before} refs/tags/v1 {zero}\n");
    let (code, out) = hook_with_push(&r, &tag);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("push again"), "{out}");
    assert_eq!(head(&r), before, "the checkout was not touched");

    let branch = format!("refs/heads/main {before} refs/heads/main {zero}\n");
    let (code, out) = hook_with_push(&r, &branch);
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("push again"), "{out}");
}
