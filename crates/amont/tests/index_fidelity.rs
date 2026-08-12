//! The checks must judge what is being committed, not what happens to be on
//! disk.
//!
//! The dangerous half is the restore. A patch taken and not put back loses
//! uncommitted work, which is worse than either failure that overwrote tracked
//! files, because there is nothing on disk to recover from. Most of these tests
//! are about that.

mod common;
use common::Repo;
use std::process::Command;

const VALID: &str = "{\n  \"a\": 2\n}\n";
const BROKEN: &str = "{ THIS IS NOT JSON\n";

fn seed(r: &Repo) {
    r.stage("x.json", "{\n  \"a\": 1\n}\n");
    r.commit("chore: seed");
}

fn tree(r: &Repo, path: &str) -> String {
    std::fs::read_to_string(r.path(path)).expect("read")
}

/// The false POSITIVE: staged content is valid, the tree is not, and the hook
/// blocked a commit that was about to be correct.
#[test]
fn a_check_judges_the_staged_content_not_the_tree() {
    let r = Repo::new();
    seed(&r);
    r.stage("x.json", VALID);
    r.write("x.json", BROKEN);

    let run = r.hook("pre-commit", &[]);
    assert!(
        run.passed(),
        "judged the working tree, not the commit:\n{}",
        run.output()
    );
    // …and the tree is exactly as the author left it.
    assert_eq!(tree(&r, "x.json"), BROKEN, "unstaged work was not restored");
}

/// The false NEGATIVE, which is the worse direction: the tree is fine and the
/// staged content is not, so a broken commit sails through.
#[test]
fn a_broken_staged_change_is_still_caught() {
    let r = Repo::new();
    seed(&r);
    r.stage("x.json", BROKEN);
    r.write("x.json", VALID);

    let run = r.hook("pre-commit", &[]);
    assert!(
        !run.passed(),
        "a broken staged change passed because the tree was fine:\n{}",
        run.output()
    );
    assert_eq!(tree(&r, "x.json"), VALID, "unstaged work was not restored");
}

/// Git QUOTES non-ASCII bytes in `--name-only` output by default — `é.json`
/// prints as the literal text `"\303\251.json"`. A line-oriented reader that
/// treated that as a real path would never find the actual file, record a
/// false "absent" marker for it, and restore nothing where the real unstaged
/// bytes belong — silent data loss on any filename outside ASCII.
#[test]
fn a_non_ascii_path_survives_the_stash_and_restore() {
    let r = Repo::new();
    r.stage("é.json", "{\n  \"a\": 1\n}\n");
    r.commit("chore: seed");
    r.stage("é.json", VALID);
    r.write("é.json", BROKEN);

    let run = r.hook("pre-commit", &[]);
    assert!(
        run.passed(),
        "judged the working tree, not the commit:\n{}",
        run.output()
    );
    assert_eq!(
        tree(&r, "é.json"),
        BROKEN,
        "unstaged work on a non-ASCII path was not restored"
    );
}

/// The common case, and it must cost nothing: a clean tree is never touched.
#[test]
fn nothing_unstaged_means_nothing_is_moved() {
    let r = Repo::new();
    seed(&r);
    r.stage("x.json", VALID);

    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.output());
    assert_eq!(tree(&r, "x.json"), VALID);
    assert!(
        !r.path(".git/amont-held").exists(),
        "held something with nothing to hold"
    );
}

/// A failing check must not cost the author their unstaged work.
#[test]
fn a_blocked_commit_still_restores() {
    let r = Repo::new();
    seed(&r);
    r.stage("x.json", BROKEN);
    r.write("bad.txt", "unstaged and precious\n");
    // Make the working tree differ so a patch is actually taken.
    r.write("x.json", "{\n  \"a\": 9\n}\n");

    let run = r.hook("pre-commit", &[]);
    assert!(!run.passed(), "guard: the staged JSON is broken");
    assert_eq!(
        tree(&r, "x.json"),
        "{\n  \"a\": 9\n}\n",
        "a blocked commit lost the unstaged change"
    );
    assert!(
        !r.path(".git/amont-held").exists(),
        "held files left behind after a block"
    );
}

/// Untracked files are not part of this commit and must not be moved — losing
/// somebody's new file would be the same failure by another route.
#[test]
fn untracked_files_are_left_alone() {
    let r = Repo::new();
    seed(&r);
    r.stage("x.json", VALID);
    r.write("x.json", BROKEN);
    std::fs::write(r.path("scratch.txt"), "not staged, not tracked\n").expect("write");

    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.output());
    assert_eq!(
        std::fs::read_to_string(r.path("scratch.txt")).expect("read"),
        "not staged, not tracked\n",
        "an untracked file was moved"
    );
}

/// Mid-merge the tree already holds somebody else's work, so taking a copy of
/// it would be the wrong instrument entirely.
///
/// CHANGE OF MIND, recorded: this case used only to assert that the tree was
/// untouched. That is still required and still asserted — but skipping the hold
/// means every check judges the WORKING TREE rather than the index, and doing
/// that in silence is the exact failure this module exists to prevent, dressed
/// up as an ordinary clean run. So it must also SAY SO.
#[test]
fn nothing_is_stashed_mid_merge() {
    let r = Repo::new();
    seed(&r);
    r.stage("x.json", VALID);
    r.write("x.json", BROKEN);
    std::fs::write(r.path(".git/MERGE_HEAD"), "deadbeef\n").expect("write");

    let run = r.hook("pre-commit", &[]);
    assert_eq!(tree(&r, "x.json"), BROKEN, "the tree was touched mid-merge");
    assert!(!r.path(".git/amont-held").exists());
    assert!(
        run.says("in progress"),
        "index fidelity was off and nothing said so:\n{}",
        run.output()
    );
}

/// …but ONLY when there is unstaged content. With a clean tree, fidelity is
/// not actually off — the tree IS the index — so the notice would be noise on
/// every commit of a merge resolution, which `registry.rs` deliberately keeps
/// running its checks.
#[test]
fn mid_merge_with_a_clean_tree_says_nothing() {
    let r = Repo::new();
    seed(&r);
    r.stage("x.json", VALID);
    std::fs::write(r.path(".git/MERGE_HEAD"), "deadbeef\n").expect("write");

    let run = r.hook("pre-commit", &[]);
    assert!(
        !run.says("in progress"),
        "nothing is degraded here, so nothing should be announced:\n{}",
        run.output()
    );
}

/// A conflicted path has no staged and unstaged halves to separate, so there
/// is nothing this can honestly do — and it used to return "nothing held" with
/// NO OUTPUT, leaving every check silently judging the working tree.
/// `docs/index-fidelity-and-run-modes.md` §1 says conflicts ABORT the stage.
///
/// Safe by construction: git itself refuses a commit with unmerged entries, so
/// nothing that would have succeeded is now blocked.
#[test]
fn a_conflicted_path_aborts_the_stage() {
    let r = Repo::new();
    r.stage("x.json", "{\n  \"a\": 1\n}\n");
    r.commit("chore: seed");
    r.git(&["checkout", "-q", "-b", "other"]);
    r.stage("x.json", "{\n  \"a\": 2\n}\n");
    r.commit("feat: theirs");
    r.git(&["checkout", "-q", "main"]);
    r.stage("x.json", "{\n  \"a\": 3\n}\n");
    r.commit("feat: ours");
    let merged = r.git(&["merge", "--no-commit", "other"]);
    assert!(
        !merged.status.success(),
        "fixture: the merge was supposed to conflict"
    );

    let run = r.hook("pre-commit", &[]);
    assert!(
        !run.passed(),
        "a conflicted tree cannot be split, and this proceeded anyway:\n{}",
        run.output()
    );
    assert!(
        run.says("unmerged"),
        "the refusal must name what it found:\n{}",
        run.output()
    );
    assert!(!r.path(".git/amont-held").exists());
}

/// Trust a declared manifest non-interactively, the way `external.rs` does —
/// `amont trust` performs the decision outright when run directly, unlike
/// `install`'s offer, which needs a confirming tty.
#[cfg(unix)]
fn trust(r: &Repo) {
    let out = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("trust")
        .current_dir(&r.dir)
        .output()
        .expect("amont trust");
    assert!(out.status.success(), "could not trust the manifest");
}

/// Ctrl-C DURING pre-commit, not after: this is the exact window the signal
/// handler exists for, and the one a handler that itself runs `git`,
/// allocates and prints risks deadlocking in — if the interrupted thread
/// already held the allocator's or stdio's lock, non-async-signal-safe code
/// running IN the handler would wait on it forever.
///
/// A declared `sleep 2` check is the deterministic way to guarantee the child
/// is still mid-run when the signal arrives: it opens a wide, fixed window
/// (send SIGINT any time in the first ~1.8s) rather than racing a real check's
/// unpredictable, sub-millisecond duration.
#[cfg(unix)]
#[test]
fn ctrl_c_mid_run_still_restores_and_dies_by_the_signal() {
    use std::os::unix::process::ExitStatusExt;

    let r = Repo::new();
    r.stage("amont.conf", "pre-commit  slowpoke  *  warn  sleep 2\n");
    trust(&r);
    r.stage("x.json", VALID);
    r.commit("chore: seed");
    r.stage("x.json", VALID);
    r.write("x.json", BROKEN);

    let mut child = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-commit")
        .current_dir(&r.dir)
        .spawn()
        .expect("spawn amont pre-commit");

    // Comfortably inside the 2s window the declared check guarantees, and
    // comfortably past the moment `StagedOnly::enter` has already run —
    // parking the unstaged bytes and installing the handler happens before
    // any check, declared or built-in, starts.
    std::thread::sleep(std::time::Duration::from_millis(300));

    let status_kill = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("send SIGINT");
    assert!(status_kill.success(), "could not signal the child");

    let status = child.wait().expect("wait on the signalled child");
    assert_eq!(
        status.signal(),
        Some(2),
        "must die BY SIGINT (re-raised with the default handler), not exit some other way: {status:?}"
    );
    assert_eq!(
        tree(&r, "x.json"),
        BROKEN,
        "unstaged work was not restored after Ctrl-C"
    );
    assert!(
        !r.path(".git/amont-held").exists(),
        "held files left behind after the signal-handler restore"
    );
}

/// `amont restore` is the recovery path for when even the signal handler was
/// interrupted.
#[test]
fn restore_puts_back_held_files() {
    let r = Repo::new();
    seed(&r);
    r.stage("x.json", VALID);
    r.write("x.json", BROKEN);

    // Park the file the way the guard does, then abandon it.
    std::fs::create_dir_all(r.path(".git/amont-held")).expect("mkdir");
    std::fs::write(r.path(".git/amont-held/x.json"), BROKEN).expect("write");
    Command::new("git")
        .arg("-C")
        .arg(&r.dir)
        .args(["checkout", "--", "."])
        .output()
        .expect("git checkout");
    assert_eq!(
        tree(&r, "x.json"),
        VALID,
        "fixture: tree reset to the index"
    );

    let out = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("restore")
        .current_dir(&r.dir)
        .output()
        .expect("amont restore");
    assert!(out.status.success(), "{:?}", out);
    assert_eq!(tree(&r, "x.json"), BROKEN, "restore did not put it back");
    assert!(!r.path(".git/amont-held").exists());
}

/// Nothing to restore is not an error — it is how somebody checks.
#[test]
fn restore_with_nothing_parked_is_fine() {
    let r = Repo::new();
    seed(&r);
    let out = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("restore")
        .current_dir(&r.dir)
        .output()
        .expect("amont restore");
    assert!(out.status.success());
}

/// A signal mid-check must still restore, for EVERY signal that is handled.
///
/// The handler itself only writes a byte to a pipe — a plain thread outside
/// signal context does the actual restore — so this is the one test proving
/// that indirection still ends where the old, simpler, signal-unsafe handler
/// did: the tree back the way the author left it, and the process dead by the
/// signal that hit it.
///
/// Parameterised over the signals a hook realistically meets. SIGHUP is the
/// terminal-closed / SSH-dropped case and is at least as likely as Ctrl-C; it
/// was NOT handled, so it orphaned the held store with nothing said. SIGQUIT
/// (3) is handled too but deliberately left out of the table: it dumps core on
/// some runners, which would make a passing suite look like a crash.
///
/// `sleep 5` is the declared check: slow enough that the signal is guaranteed
/// to land while it is still the thing running, cheap enough not to make a
/// hung path (a regression back to restoring inline) drag the suite out
/// waiting on it — `recv_timeout` below bounds that instead.
#[cfg(unix)]
#[test]
fn a_signal_mid_check_still_restores() {
    for signal in ["-INT", "-HUP", "-TERM"] {
        signal_mid_check_restores(signal);
    }
}

#[cfg(unix)]
fn signal_mid_check_restores(signal: &str) {
    let r = Repo::new();
    seed(&r);
    r.stage("x.json", VALID);
    r.write("x.json", BROKEN);
    r.stage("amont.conf", "pre-commit  slow  *  block  sleep  5\n");
    let trusted = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("trust")
        .current_dir(&r.dir)
        .output()
        .expect("amont trust");
    assert!(trusted.status.success(), "could not trust the manifest");

    let mut child = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-commit")
        .current_dir(&r.dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn amont pre-commit");

    // Wait for parking to be COMPLETE, not merely started.
    //
    // The store directory appearing is the wrong signal: `enter()` creates it
    // first and copies into it, and only afterwards runs the `git checkout`
    // that resets the tree and sets the flag `restore()` reads. A signal
    // arriving in that window is handled correctly — `restore()` sees nothing
    // held, puts nothing back, and the tree was never touched, so no work is
    // lost — but it leaves the store on disk for `amont restore`, which is
    // not the behaviour this test is about. Waiting on the store's existence
    // made that window look like a failure, and it did, once, under load.
    //
    // The tree matching the INDEX is the honest proxy: that is what `checkout`
    // does, immediately before the flag is set.
    let store = r.path(".git/amont-held");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    // A NON-panicking read: `checkout` replaces the file, so there is a moment
    // where it does not exist, and `tree()` would panic on it rather than
    // simply not-match-yet. That is the poll racing the thing it polls for.
    let staged_content_is_on_disk =
        || std::fs::read_to_string(r.path("x.json")).ok().as_deref() == Some(VALID);
    while !staged_content_is_on_disk() {
        assert!(
            std::time::Instant::now() < deadline,
            "fixture: the unstaged change was never parked"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(store.exists(), "fixture: parked without a store");

    let signalled = Command::new("kill")
        .arg(signal)
        .arg(child.id().to_string())
        .status()
        .expect("run kill");
    assert!(signalled.success(), "fixture: could not signal the child");

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait());
    });
    let status = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap_or_else(|_| {
            panic!(
                "amont did not exit within 10s of {signal} — \
                 the pipe/thread handoff deadlocked"
            )
        })
        .expect("wait on the child");
    assert!(
        !status.success(),
        "{signal} must not look like a clean pass"
    );

    assert_eq!(
        tree(&r, "x.json"),
        BROKEN,
        "unstaged work was not restored after {signal}"
    );
    assert!(!store.exists(), "held files left behind after {signal}");
}

/// A symlink is a link, not a file with those bytes in it. `fs::read` follows
/// it — copying the TARGET's content as the "backup" — so a naive park-and-
/// restore quietly turns the link into a plain file. `link` must still be a
/// link afterward, pointing where the author unstaged-left it.
#[test]
fn an_unstaged_symlink_retarget_survives_the_hook() {
    let r = Repo::new();
    seed(&r);
    if !try_symlink("x.json", &r.path("link")) {
        println!("  ! symlinks unavailable — skipping");
        return;
    }
    r.git(&["add", "link"]);
    r.commit("chore: add a symlink");

    // Unstaged: repoint the tracked link elsewhere.
    std::fs::remove_file(r.path("link")).expect("remove the old link");
    assert!(
        try_symlink("unstaged-target", &r.path("link")),
        "fixture: retarget failed after the capability check passed"
    );

    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.output());

    let meta = std::fs::symlink_metadata(r.path("link")).expect("link must still exist");
    assert!(
        meta.file_type().is_symlink(),
        "the symlink was replaced with a regular file"
    );
    assert_eq!(
        std::fs::read_link(r.path("link")).expect("read link"),
        std::path::Path::new("unstaged-target"),
        "the unstaged retarget was not restored"
    );
}

/// A symlink mid-retarget commonly points at nothing for a moment. `fs::read`
/// on a dangling link fails exactly like it does on a deleted file, so this
/// must not be mistaken for one — that reads the working tree, decides the
/// link was deleted, and deletes it again on restore.
#[test]
fn a_dangling_unstaged_symlink_is_not_deleted() {
    let r = Repo::new();
    seed(&r);
    if !try_symlink("x.json", &r.path("link")) {
        println!("  ! symlinks unavailable — skipping");
        return;
    }
    r.git(&["add", "link"]);
    r.commit("chore: add a symlink");

    std::fs::remove_file(r.path("link")).expect("remove the old link");
    assert!(
        try_symlink("does-not-exist", &r.path("link")),
        "fixture: retarget failed after the capability check passed"
    );

    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.output());

    let meta = std::fs::symlink_metadata(r.path("link"))
        .expect("a dangling symlink was deleted instead of restored");
    assert!(meta.file_type().is_symlink());
    assert_eq!(
        std::fs::read_link(r.path("link")).expect("read link"),
        std::path::Path::new("does-not-exist"),
    );
}

/// A regression the first two symlink tests could not catch: they both use
/// `link`, which has no extension, so `Path::with_extension` (the original,
/// wrong implementation) happened to behave like appending a suffix for it.
/// A tracked symlink that HAS one, `link.txt`, is where `with_extension`
/// actually replaces `.txt` with the marker — parking it as
/// `link.amont-symlink` and, on restore, recreating bare `link` while the
/// real `link.txt` (whatever `checkout` left there) is never touched again.
#[test]
fn a_retargeted_symlink_with_an_extension_keeps_its_name() {
    let r = Repo::new();
    seed(&r);
    if !try_symlink("x.json", &r.path("link.txt")) {
        println!("  ! symlinks unavailable — skipping");
        return;
    }
    r.git(&["add", "link.txt"]);
    r.commit("chore: add a symlink");

    std::fs::remove_file(r.path("link.txt")).expect("remove the old link");
    assert!(
        try_symlink("unstaged-target", &r.path("link.txt")),
        "fixture: retarget failed after the capability check passed"
    );

    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.output());

    let meta = std::fs::symlink_metadata(r.path("link.txt"))
        .expect("link.txt must still exist, under its full name");
    assert!(
        meta.file_type().is_symlink(),
        "the symlink was replaced with a regular file"
    );
    assert_eq!(
        std::fs::read_link(r.path("link.txt")).expect("read link"),
        std::path::Path::new("unstaged-target"),
        "the unstaged retarget was not restored"
    );
    assert!(
        !r.path("link").exists(),
        "restore invented a bare `link` — with_extension truncated the marker's name"
    );
}

/// The same `with_extension` mistake, on the OTHER marker: a tracked file
/// deleted in the working tree but not staged. `x.json` already has an
/// extension, so this is the case `nothing_unstaged_means_nothing_is_moved`
/// and friends never exercised — they all pass `x.json` through the
/// MODIFIED branch, which never went through `with_extension` at all.
#[test]
fn an_unstaged_delete_of_an_extensioned_file_restores_under_its_full_name() {
    let r = Repo::new();
    seed(&r);
    r.stage("x.json", VALID);
    std::fs::remove_file(r.path("x.json")).expect("delete unstaged");

    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.output());

    // The wrong marker name ("x.amont-absent") does not match "x.json",
    // so `remove_file` on it silently no-ops and `checkout`'s resurrected
    // x.json is left behind — the delete the author made is never restored.
    assert!(
        !r.path("x.json").exists(),
        "the unstaged delete was not restored — with_extension named the \
         marker for `x`, not `x.json`, so restore's remove_file missed it"
    );
}

/// A stash an earlier, interrupted run failed to restore still holds work
/// nobody has recovered. `enter()` used to `remove_dir_all` it unconditionally
/// before parking a new one — the next commit with any unstaged change would
/// silently destroy that unrecovered backup. It must refuse instead.
#[test]
fn a_previous_unrecovered_stash_is_not_clobbered() {
    let r = Repo::new();
    seed(&r);
    r.stage("x.json", VALID);
    r.write("x.json", BROKEN);

    // Simulate a run that parked something and never got to restore it.
    std::fs::create_dir_all(r.path(".git/amont-held")).expect("mkdir");
    std::fs::write(
        r.path(".git/amont-held/precious.txt"),
        "not recovered yet\n",
    )
    .expect("write");

    let run = r.hook("pre-commit", &[]);
    assert!(
        !run.passed(),
        "must refuse rather than silently clobber an unrecovered stash:\n{}",
        run.output()
    );
    assert_eq!(
        std::fs::read_to_string(r.path(".git/amont-held/precious.txt")).expect("read"),
        "not recovered yet\n",
        "the unrecovered stash must survive the refusal"
    );
    assert_eq!(
        tree(&r, "x.json"),
        BROKEN,
        "the tree must be untouched when the guard refuses"
    );
}

/// A linked worktree's `.git` is a FILE pointing at a private admin
/// directory (`<main>/.git/worktrees/<name>`) — not the common `.git` the
/// hooks share and the stash lives in. `enter()` gets this right because it
/// is handed `--hooks-dir` directly; `restore()` used to re-derive the
/// location via `git rev-parse --git-dir`, which names the PRIVATE directory
/// from inside a worktree and so never finds what `enter()` parked. That
/// looked exactly like "nothing was held" and silently lost the author's
/// unstaged work.
#[test]
fn a_commit_in_a_linked_worktree_still_gets_its_unstaged_work_back() {
    let r = Repo::new();
    seed(&r);
    let wt = r.worktree("wt");
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(&wt)
            .args(args)
            .output()
            .expect("git")
    };

    std::fs::write(wt.join("x.json"), VALID).expect("write");
    git(&["add", "x.json"]);
    std::fs::write(wt.join("x.json"), BROKEN).expect("write");

    let run = r.hook_at(&wt, "pre-commit", &[]);
    assert!(
        run.passed(),
        "judged the working tree, not the commit:\n{}",
        run.output()
    );
    assert_eq!(
        std::fs::read_to_string(wt.join("x.json")).expect("read"),
        BROKEN,
        "unstaged work in a linked worktree was not restored — \
         restore() looked in the wrong .git"
    );
    // `enter()` and `restore()` share ONE `store_dir()` — wherever that
    // resolves to, nothing is left behind once restore() has succeeded.
    let git_dir = String::from_utf8_lossy(&git(&["rev-parse", "--git-dir"]).stdout)
        .trim()
        .to_string();
    assert!(
        !std::path::Path::new(&git_dir).join("amont-held").exists(),
        "held files left behind after a successful restore"
    );
}

/// Same worktree distinction, through the manual recovery path: `amont
/// restore` run from inside a linked worktree must find the stash the guard
/// left — wherever `store_dir()` actually parks it — not report "nothing of
/// ours to restore".
#[test]
fn restore_command_finds_the_stash_from_a_linked_worktree() {
    let r = Repo::new();
    seed(&r);
    let wt = r.worktree("wt");
    let git_in = |dir: &std::path::Path, args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git")
    };

    // Park a file the way the guard does. `enter()` and `restore()` share ONE
    // `store_dir()`, so wherever `git rev-parse --git-dir` resolves FROM THE
    // WORKTREE is where a real stash would have landed — then abandon it, as
    // if the process died before its own restore ran.
    let git_dir = String::from_utf8_lossy(&git_in(&wt, &["rev-parse", "--git-dir"]).stdout)
        .trim()
        .to_string();
    let store = std::path::Path::new(&git_dir).join("amont-held");
    std::fs::create_dir_all(&store).expect("mkdir");
    std::fs::write(store.join("x.json"), BROKEN).expect("write");
    git_in(&wt, &["checkout", "--", "."]);

    let out = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("restore")
        .current_dir(&wt)
        .output()
        .expect("amont restore");
    assert!(out.status.success(), "{:?}", out);
    assert_eq!(
        std::fs::read_to_string(wt.join("x.json")).expect("read"),
        BROKEN,
        "restore did not find the stash from the worktree"
    );
    assert!(!store.exists());
}

/// `None` on a platform/runner that cannot create the link at all (Windows
/// without `SeCreateSymbolicLinkPrivilege`), so the two tests above skip
/// instead of failing on a capability the environment never had.
fn try_symlink(target: &str, link: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, link).is_ok()
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, link);
        false
    }
}

/// A repository may contain a file whose name ends in our old marker suffix,
/// and one did.
///
/// The store used to encode "this path was deleted" as a sibling file named
/// `<name>.amont-absent`. So a repo tracking both `notes` and
/// `notes.amont-absent` handed the restore an ambiguity it could not see:
/// it read the payload of the second as a STATEMENT about the first, deleted
/// `notes` from the working tree, and never put the real bytes back. Both
/// halves are asserted here, because both halves were losses.
#[test]
fn a_tracked_file_named_like_the_absent_marker_does_not_delete_its_neighbour() {
    let r = Repo::new();
    seed(&r);
    r.stage("notes", "the neighbour\n");
    r.stage("notes.amont-absent", "not a marker, a file\n");
    r.commit("chore: two files, one awkward name");

    // Something staged, so the hook has work to do…
    r.stage("x.json", VALID);
    // …and an unstaged change to the awkwardly named file, so it is held.
    r.write("notes.amont-absent", "edited, and not staged\n");

    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.output());

    assert!(
        r.path("notes").exists(),
        "the neighbouring file was deleted by a suffix match"
    );
    assert_eq!(tree(&r, "notes"), "the neighbour\n");
    assert_eq!(
        tree(&r, "notes.amont-absent"),
        "edited, and not staged\n",
        "the unstaged edit was never put back"
    );
}

/// The same collision in its more serious form: the repository chose both the
/// link name and its target.
///
/// `<name>.amont-symlink` used to mean "restore `<name>` as a symlink whose
/// target is this file's contents" — so a tracked `payload.amont-symlink`
/// containing an ABSOLUTE path made committing in that repository plant a
/// symlink to it. The repo picked where it pointed; anything that wrote that
/// path afterwards wrote through it, outside the repository entirely.
#[test]
fn a_tracked_file_named_like_the_symlink_marker_does_not_plant_a_symlink() {
    let r = Repo::new();
    seed(&r);

    let outside = std::env::temp_dir().join(format!("amont-outside-{}", std::process::id()));
    std::fs::write(&outside, "a file the repository does not own\n").expect("write outside");

    r.stage("payload.amont-symlink", &format!("{}\n", outside.display()));
    r.commit("chore: a file whose name looks like metadata");

    r.stage("x.json", VALID);
    r.write("payload.amont-symlink", &outside.display().to_string());

    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.output());

    assert!(
        std::fs::symlink_metadata(r.path("payload")).is_err(),
        "the repository planted a symlink through the held store"
    );
    assert_eq!(
        std::fs::read_to_string(&outside).expect("outside file"),
        "a file the repository does not own\n",
        "the file outside the repository was touched"
    );
    let _ = std::fs::remove_file(&outside);
}

/// A store written by an older binary still has to be recoverable, or an
/// upgrade mid-hold would strand exactly the work this module exists to keep.
/// `restore_puts_back_held_files` above hand-builds one of these too.
#[test]
fn a_legacy_suffix_store_is_still_recovered() {
    let r = Repo::new();
    seed(&r);
    r.stage("gone.txt", "will be deleted\n");
    r.commit("chore: seed the legacy shapes");

    let store = r.path(".git/amont-held");
    std::fs::create_dir_all(&store).expect("mkdir");
    // The old format: payload by plain name, metadata by suffix. No index.
    std::fs::write(store.join("x.json"), BROKEN).expect("write payload");
    std::fs::write(store.join("gone.txt.amont-absent"), b"").expect("write absent marker");

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("restore")
        .current_dir(&r.dir)
        .output()
        .expect("amont restore");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(tree(&r, "x.json"), BROKEN, "legacy payload not put back");
    assert!(
        !r.path("gone.txt").exists(),
        "legacy absent marker not honoured"
    );
}

/// The executable bit is uncommitted work too.
///
/// `git diff --name-only` lists a mode-only change, so the file was held and
/// restored — but only its BYTES were recorded, and `git checkout` had reset
/// the mode in between. `fs::write` preserves whatever mode the file already
/// has, so the restored file came back with the index's mode and the `chmod`
/// was silently gone. Every content-based assertion in this file passed
/// throughout, which is why it took an audit to find.
#[cfg(unix)]
#[test]
fn an_unstaged_chmod_survives_the_hook() {
    use std::os::unix::fs::PermissionsExt;
    let r = Repo::new();
    seed(&r);
    r.stage("deploy.sh", "#!/bin/sh\necho hi\n");
    r.commit("chore: a script, not yet executable");

    r.stage("x.json", VALID);
    // Unstaged: both a content edit and the bit.
    r.write("deploy.sh", "#!/bin/sh\necho edited\n");
    std::fs::set_permissions(r.path("deploy.sh"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");

    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.output());

    let mode = std::fs::metadata(r.path("deploy.sh"))
        .expect("stat")
        .permissions()
        .mode();
    assert!(
        mode & 0o111 != 0,
        "the unstaged executable bit was lost (mode {mode:o})"
    );
    assert_eq!(tree(&r, "deploy.sh"), "#!/bin/sh\necho edited\n");
}

/// The pure-mode case, with IDENTICAL content — the one every byte-comparing
/// assertion is blind to.
#[cfg(unix)]
#[test]
fn an_unstaged_chmod_with_no_content_change_survives() {
    use std::os::unix::fs::PermissionsExt;
    let r = Repo::new();
    seed(&r);
    r.stage("tool.sh", "#!/bin/sh\n");
    r.commit("chore: seed");

    r.stage("x.json", VALID);
    std::fs::set_permissions(r.path("tool.sh"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");

    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.output());

    let mode = std::fs::metadata(r.path("tool.sh"))
        .expect("stat")
        .permissions()
        .mode();
    assert!(mode & 0o111 != 0, "mode-only change lost (mode {mode:o})");
}

/// The clobber window, closed. A file with unstaged edits is held; a check
/// (standing in for an editor save) writes it while the stage runs; the
/// restore must KEEP the newer content and park the held copy, saying where —
/// this write used to destroy the save silently.
#[cfg(unix)]
#[test]
fn an_editor_save_mid_check_is_kept_not_clobbered() {
    use std::os::unix::fs::PermissionsExt;
    let r = Repo::new();
    r.stage("a.txt", "committed\n");
    r.commit("chore: seed");
    r.write("a.txt", "my unstaged edit\n"); // held when the stage begins

    // The "editor": a declared check that writes a.txt mid-run. At that
    // moment a.txt holds the INDEX content ("committed"), so the write is
    // exactly what a background save looks like to the restore.
    r.stage(
        "editor.sh",
        "#!/bin/sh\nprintf 'editor save\\n' > a.txt\nexit 0\n",
    );
    std::fs::set_permissions(r.path("editor.sh"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
    r.git(&["add", "editor.sh"]);
    r.stage("amont.conf", "pre-commit  editor  *  warn  ./editor.sh\n");
    trust(&r);

    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.output());
    assert_eq!(
        tree(&r, "a.txt"),
        "editor save\n",
        "the mid-check save was clobbered:\n{}",
        run.output()
    );
    assert!(
        run.says("changed while the checks ran"),
        "the preservation must be announced:\n{}",
        run.output()
    );
    let parked = r.path(".git/amont-preserved/a.txt");
    assert_eq!(
        std::fs::read_to_string(&parked).expect("parked copy"),
        "my unstaged edit\n",
        "the held copy must be parked, not lost"
    );
}

/// With `amont.fix` on the guard stands down: a repo-wide fixer legitimately
/// rewrites held files mid-run, and the documented contract there has always
/// been "the tree returns to your unstaged version".
#[cfg(unix)]
#[test]
fn a_fixing_repo_keeps_the_old_restore_contract() {
    use std::os::unix::fs::PermissionsExt;
    let r = Repo::new();
    r.stage("a.txt", "committed\n");
    r.commit("chore: seed");
    r.git(&["config", "amont.fix", "true"]);
    r.write("a.txt", "my unstaged edit\n");

    r.stage("fixer.sh", "#!/bin/sh\nprintf 'fixed\\n' > a.txt\nexit 0\n");
    std::fs::set_permissions(r.path("fixer.sh"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
    r.git(&["add", "fixer.sh"]);
    r.stage("amont.conf", "pre-commit  fixit  *  warn  ./fixer.sh\n");
    trust(&r);

    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.output());
    assert_eq!(
        tree(&r, "a.txt"),
        "my unstaged edit\n",
        "with fixing on, the unstaged version comes back — the designed flow:\n{}",
        run.output()
    );
    assert!(
        !run.says("changed while the checks ran"),
        "the guard must stand down under amont.fix:\n{}",
        run.output()
    );
}

/// The scoped checkout must reset exactly the held paths — including names
/// that are pathspec syntax if taken literally. `*.rs` as a FILENAME is a
/// glob to git unless the pathspec says literal.
#[test]
fn a_held_path_that_looks_like_a_glob_survives_the_hold() {
    let r = Repo::new();
    r.stage("*.rs", "committed star\n");
    r.stage("é.txt", "committed accent\n");
    r.commit("chore: hostile names");
    r.write("*.rs", "unstaged star\n");
    r.write("é.txt", "unstaged accent\n");
    r.stage("b.txt", "staged\n");

    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.output());
    assert_eq!(tree(&r, "*.rs"), "unstaged star\n", "glob-named file lost");
    assert_eq!(tree(&r, "é.txt"), "unstaged accent\n", "accented file lost");
}

/// git FAILING is not git answering "empty": the stage still proceeds (its
/// plumbing must never block a commit), but it says out loud that it judged
/// nothing — the third member of the repo_hooks / push-gates bug family.
#[cfg(unix)]
#[test]
fn a_git_failure_is_announced_not_read_as_nothing_staged() {
    use std::os::unix::fs::PermissionsExt;
    let r = Repo::new();
    seed(&r);
    let config = r.path(".git/config");
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    if std::fs::read(&config).is_ok() {
        return; // running as root; the fixture cannot fail
    }
    let run = r.hook("pre-commit", &[]);
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o644)).expect("chmod back");
    assert!(
        run.says("would not list the staged files"),
        "a git failure must be announced, not judged as an empty index:\n{}",
        run.output()
    );
}
