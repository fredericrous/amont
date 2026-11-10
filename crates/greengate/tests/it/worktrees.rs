//! Hooks dispatch from the COMMON `.git/hooks` — shared across every linked
//! worktree — but a git operation's markers (`MERGE_HEAD`, ...) and this
//! tool's own held-changes store live in the WORKTREE-PRIVATE gitdir under
//! `.git/worktrees/<name>`. Code that assumed `hooks_dir`'s parent IS
//! `$GIT_DIR` silently broke both the "never touch a tree mid-operation"
//! guard and the restore half of index fidelity for every linked worktree —
//! discovered when it happened to this very repository's own commit while
//! this file was being written: an unrelated file's unstaged fix was parked
//! in the common directory, `restore()` looked in the worktree-private one,
//! found nothing, and gave up silently.

use crate::common;
use common::Repo;
use std::process::Command;

/// A linked worktree of `r`, on a fresh branch. `git worktree add` never
/// installs hooks there — every worktree dispatches through the COMMON
/// directory's `.git/hooks`, which is the whole point of this file.
fn add_worktree(r: &Repo, branch: &str) -> std::path::PathBuf {
    let path = r.dir.parent().expect("parent").join(format!(
        "{}-{branch}",
        r.dir.file_name().expect("name").to_string_lossy()
    ));
    let _ = std::fs::remove_dir_all(&path);
    let out = r.git(&[
        "worktree",
        "add",
        "-q",
        "-b",
        branch,
        path.to_str().expect("utf8"),
    ]);
    assert!(
        out.status.success(),
        "git worktree add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    path
}

/// The worktree's OWN gitdir — `.git` there is a FILE, not a directory, so a
/// fixture that wrote through `wt.join(".git/...")` would be writing into a
/// path that does not exist as a directory at all. Asked from git directly,
/// the same way `restore()` and `restore_command()` already do.
fn worktree_git_dir(wt: &std::path::Path) -> std::path::PathBuf {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(wt)
        .args(["rev-parse", "--path-format=absolute", "--git-dir"]);
    Repo::strip_git_env_impl(&mut cmd);
    let out = cmd.output().expect("git rev-parse --git-dir");
    std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim())
}

/// `git -C <dir> add <path>`, with the same GIT_* stripping every other git
/// call in this file needs (see `common::Repo::strip_git_env_impl`).
fn git_add(dir: &std::path::Path, path: &str) -> bool {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(["add", path]);
    Repo::strip_git_env_impl(&mut cmd);
    cmd.output().expect("git add").status.success()
}

/// Run `amont pre-commit` exactly the way the real shim would from a
/// linked worktree: `--hooks-dir` is the COMMON directory's hooks folder
/// (where the dispatching shim script actually lives), while the process
/// itself runs with its CWD in the WORKTREE, exactly as git invokes it there.
fn hook_from_worktree(
    main_hooks_dir: &std::path::Path,
    worktree: &std::path::Path,
) -> (i32, String) {
    let mut cmd = crate::common::amont_cmd();
    cmd.arg("--hooks-dir")
        .arg(main_hooks_dir)
        .arg("pre-commit")
        .current_dir(worktree);
    Repo::strip_git_env_impl(&mut cmd);
    let out = cmd.output().expect("run amont pre-commit");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// The guard `docs`/`staged_only.rs` call "never mid-operation": a merge
/// (or cherry-pick, rebase, revert) already holds work that is not the
/// author's, so content checks must pause rather than judge it. Must hold in
/// a linked worktree exactly as it does in the main one.
#[test]
fn a_merge_in_progress_in_a_worktree_still_pauses_content_checks() {
    let r = Repo::new();
    r.stage("x.json", "{\n  \"a\": 1\n}\n");
    r.commit("chore: seed");
    let wt = add_worktree(&r, "feature");

    std::fs::write(wt.join("x.json"), "{ BROKEN\n").expect("write");
    assert!(git_add(&wt, "x.json"));
    std::fs::write(worktree_git_dir(&wt).join("MERGE_HEAD"), "deadbeef\n").expect("write");

    let (code, out) = hook_from_worktree(&r.path(".git/hooks"), &wt);
    assert_eq!(code, 0, "should have paused, not judged the merge:\n{out}");
    assert!(out.contains("paused during a merge"), "{out}");

    let _ = std::fs::remove_dir_all(&wt);
}

/// The exact incident: unstaged work in a linked worktree must survive being
/// parked aside for the duration of the checks and put back afterward, the
/// same guarantee `index_fidelity.rs` proves for the main worktree.
#[test]
fn unstaged_work_in_a_worktree_survives_the_stash_and_restore() {
    let r = Repo::new();
    r.stage("x.json", "{\n  \"a\": 1\n}\n");
    r.commit("chore: seed");
    let wt = add_worktree(&r, "feature");

    std::fs::write(wt.join("x.json"), "{\n  \"a\": 2\n}\n").expect("write");
    assert!(git_add(&wt, "x.json"));
    // Now the working tree disagrees with the index — real unstaged content.
    std::fs::write(wt.join("x.json"), "{ THIS IS NOT JSON\n").expect("write");

    let (code, out) = hook_from_worktree(&r.path(".git/hooks"), &wt);
    assert_eq!(code, 0, "judged the working tree, not the commit:\n{out}");
    assert_eq!(
        std::fs::read_to_string(wt.join("x.json")).expect("read"),
        "{ THIS IS NOT JSON\n",
        "unstaged work in the worktree was not restored"
    );
    assert!(
        !worktree_git_dir(&wt).join("amont-held").exists(),
        "held files left behind in the worktree-private gitdir"
    );
    assert!(
        !r.path(".git/amont-held").exists(),
        "held files stranded in the COMMON gitdir — the exact incident this guards against"
    );

    let _ = std::fs::remove_dir_all(&wt);
}
