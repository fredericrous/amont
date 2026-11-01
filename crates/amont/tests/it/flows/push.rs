//! The push journey: what a person meets between a commit and a branch the
//! remote holds.
//!
//! One repository, installed the way a person installs it (`amont trust`,
//! `amont init`), pushing with real `git push` to a bare remote it owns, so
//! git decides what reaches `pre-push` on stdin. Three declarations over
//! `*.txt`: `check` at both stages (a pair: the commit-time side vouches for
//! the push-time one) and `suite` at push only. Each run logs its name, and
//! `suite` fails on a committed `a.txt` that says `broken`, or when it is
//! told it is a rehearsal (a suite must never learn that). The log lives
//! under `.git/`, at an absolute path: a rehearsal runs the gates in a
//! snapshot elsewhere, and a log in the working tree would make it dirty.
//!
//! The remote can drop one push (its `pre-receive` refuses once): that is
//! how a retry of the same content is made, as it is made for the person
//! when the wire drops after the suite has passed.
//!
//! The journey, in order:
//! 1. the branch checks speak at commit time only when a push would really
//!    be refused, and the push that creates `main` is allowed while the
//!    next one is not;
//! 2. a feature branch: the pair defers to its commit-time stamp, `suite`
//!    stamps the tip, and the retry after a dropped push repeats nothing;
//! 3. stamps are about one tree: new content runs again, an amend that kept
//!    the tree does not, an untracked file costs nothing, a modified tracked
//!    file or `amont.pushStamps false` earns no stamp;
//! 4. rehearsal: `amont rehearse --wait` tests a snapshot of HEAD and the
//!    push finds the stamp; a failed one is reported and the push runs the
//!    gate itself, recording what failed;
//! 5. unstamped commits bring the pair back, or are refused on request;
//! 6. pull-rebase: in sync, diverged after the amend, behind (advised, then
//!    rebased and asked to push again), and an upstream merged away;
//! 7. a branch never pushed: nothing to rehearse for prose, and
//!    `amont run pre-push` stamps what the push then skips;
//! 8. the attestation note reaches the remote; the stamps never do.

use super::{git, git_commit, git_out, head, install, read};
use crate::common::{missing, Repo};
use std::path::{Path, PathBuf};
use std::process::Command;

const SKIP: &str = "passed on this exact tree earlier — not repeating it here";
const STAMPED: &str = "stamped";
const PAIRED: &str = "gated at commit instead";
const UNSTAMPED: &str = "no record of it — running it here";
/// What the remote's `pre-receive` says when it drops a push.
const DROPPED: &str = "the remote dropped this push";

const CONF: &str = "pre-commit  check  *.txt  block  node gate.js check\n\
                    pre-push    check  *.txt  block  node gate.js check\n\
                    pre-push    suite  *.txt  block  node gate.js suite\n";

/// The gate both declarations run: it logs its name first, so the log says
/// what ran even when it then fails.
fn gate_js(r: &Repo) -> String {
    let log = r.path(".git/gate.log").display().to_string();
    format!(
        "const fs = require('fs');\n\
         const who = process.argv[2];\n\
         fs.appendFileSync({log:?}, who + '\\n');\n\
         if (process.env.AMONT_REHEARSAL !== undefined) process.exit(1);\n\
         if (who === 'suite' && fs.readFileSync('a.txt', 'utf8').includes('broken')) process.exit(1);\n"
    )
}

/// How many times `gate` has run, wherever from.
fn runs(r: &Repo, gate: &str) -> usize {
    std::fs::read_to_string(r.path(".git/gate.log"))
        .unwrap_or_default()
        .lines()
        .filter(|l| *l == gate)
        .count()
}

/// The bare remote, under `.git/` so the working tree never sees it, with a
/// `pre-receive` that refuses one push each time [`drop_next`] asks.
fn bare_remote(r: &Repo) -> PathBuf {
    let remote = r.path(".git/origin.git");
    let ok = Command::new("git")
        .args([
            "init",
            "-q",
            "--bare",
            "--template=",
            "--initial-branch=main",
        ])
        .arg(&remote)
        .status()
        .expect("git init --bare")
        .success();
    assert!(ok, "init the bare remote");
    let hooks = remote.join("hooks");
    std::fs::create_dir_all(&hooks).expect("remote hooks dir");
    let hook = hooks.join("pre-receive");
    std::fs::write(
        &hook,
        format!("#!/bin/sh\nif [ -f drop ]; then rm -f drop; echo '{DROPPED}' >&2; exit 1; fi\n"),
    )
    .expect("write pre-receive");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    remote
}

/// The next push reaches the remote and is dropped there, after `pre-push`.
fn drop_next(remote: &Path) {
    std::fs::write(remote.join("drop"), "").expect("arm the drop");
}

/// `git <args>` in the bare remote.
fn on_remote(remote: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new("git");
    cmd.arg("--git-dir").arg(remote).args(args);
    Repo::strip_git_env_impl(&mut cmd);
    String::from_utf8_lossy(&cmd.output().expect("git in the remote").stdout)
        .trim_end()
        .to_string()
}

/// The gate record on `rev`, empty when there is none.
fn note(r: &Repo, rev: &str) -> String {
    git_out(r, &["notes", "--ref", "amont-gate", "show", rev])
}

fn has_note(r: &Repo, rev: &str) -> bool {
    r.git(&["notes", "--ref", "amont-gate", "show", rev])
        .status
        .success()
}

fn tree(r: &Repo) -> String {
    git_out(r, &["rev-parse", "HEAD^{tree}"])
}

/// `amont <args>` with a private global config, and its exit code: a
/// rehearsal that failed (1) is not one that could not start (2).
fn amont_run(r: &Repo, args: &[&str]) -> (i32, String) {
    let run = r.run(args);
    (run.code, run.output())
}

/// The rehearsal's state file.
fn rehearsal_state(r: &Repo) -> String {
    std::fs::read_to_string(r.path(".git/amont-rehearsal")).unwrap_or_default()
}

/// Registered worktrees: the primary plus any snapshot left behind.
fn worktrees(r: &Repo) -> usize {
    git_out(r, &["worktree", "list", "--porcelain"])
        .lines()
        .filter(|l| l.starts_with("worktree "))
        .count()
}

/// Write `a.txt` and make a hooked commit of it.
fn commit_a(r: &Repo, content: &str, msg: &str) -> String {
    r.stage("a.txt", content);
    let (ok, out) = git_commit(r, &["-m", msg]);
    assert!(ok, "the commit `{msg}` failed:\n{out}");
    out
}

/// One repository walks the whole journey.
///
/// Slower than a second: it is a flow, about twenty-five real pushes and
/// twenty commits on one repository, standing in for the thirty-three
/// repositories its steps used to build one by one.
#[test]
fn the_push_journey() {
    let r = Repo::new();
    r.stage("gate.js", &gate_js(&r));
    r.stage("a.txt", "seed\n");
    r.stage("amont.conf", CONF);
    r.commit("chore: seed");
    install(&r);

    // ---- 1. branch checks, at commit and at push ------------------------

    // No remote, no push, nothing for the contract to gate.
    let out = commit_a(&r, "one\n", "feat: one");
    step!("remoteless", !out.contains("refused"), "{out}");

    // A remote, but `main` has never been pushed: nothing is refused until
    // the branch exists there. (`branch-pattern` does speak: `main` is not a
    // name it accepts for a new branch, so the phrase is branch-protect's.)
    let remote = bare_remote(&r);
    r.git(&["remote", "add", "origin", &remote.display().to_string()]);
    let out = commit_a(&r, "two\n", "feat: two");
    step!(
        "never pushed",
        !out.contains("pushing it will be refused"),
        "{out}"
    );

    // The push that creates `main` on the remote: no history to protect, no
    // base to open a pull request against.
    let (ok, out) = git(&r, &["push", "origin", "main"]);
    step!("create main", ok, "{out}");
    step_eq!(
        "create main",
        on_remote(&remote, &["rev-parse", "refs/heads/main"]),
        head(&r),
        "main must now exist on the remote"
    );

    // From here a commit on `main` is warned about, and not blocked.
    let out = commit_a(&r, "three\n", "feat: three");
    step!(
        "commit on main",
        out.contains("pushing it will be refused"),
        "{out}"
    );
    step!("commit on main", out.contains("git switch -c"), "{out}");

    // …and the push it warned about is refused, first and alone.
    let remote_main = on_remote(&remote, &["rev-parse", "refs/heads/main"]);
    let (ok, out) = git(&r, &["push", "origin", "main"]);
    step!(
        "push to main",
        !ok,
        "a direct push to main went through:\n{out}"
    );
    step!("push to main", out.contains("forbidden"), "{out}");
    step!("push to main", out.contains("Pull Request"), "{out}");
    step!(
        "push to main",
        out.contains("pre-push-branch-protect"),
        "{out}"
    );
    step_eq!(
        "push to main",
        on_remote(&remote, &["rev-parse", "refs/heads/main"]),
        remote_main
    );

    // The work moves to a branch, under a name the push would refuse: said
    // at the commit, with the rename, without blocking.
    r.git(&["switch", "-q", "-c", "spike"]);
    let out = commit_a(&r, "four\n", "feat: four");
    step!(
        "off contract",
        out.contains("refused at push time"),
        "{out}"
    );
    step!("off contract", out.contains("git branch -m"), "{out}");

    r.git(&["branch", "-m", "feat/x"]);
    let checks = runs(&r, "check");
    let out = commit_a(&r, "five\n", "feat: five");
    step!("renamed", out.contains("conforms"), "{out}");

    if missing("node") {
        return;
    }

    // ---- 2. the first push of a feature branch, and its retry -------------

    step_eq!(
        "first push",
        runs(&r, "check"),
        checks + 1,
        "the commit-time side ran once"
    );
    let checks = runs(&r, "check");
    let suites = runs(&r, "suite");
    drop_next(&remote);
    let (ok, out) = git(&r, &["push", "-u", "origin", "feat/x"]);
    step!("first push", !ok && out.contains(DROPPED), "{out}");
    step!("first push", out.contains(PAIRED), "the pair defers: {out}");
    step_eq!(
        "first push",
        runs(&r, "check"),
        checks,
        "the pair ran again"
    );
    step_eq!("first push", runs(&r, "suite"), suites + 1, "{out}");
    step!(
        "first push",
        out.contains(STAMPED),
        "the stamp is announced: {out}"
    );
    let stamp = note(&r, "HEAD");
    step!(
        "first push",
        stamp.contains("check"),
        "commit-time token kept: {stamp}"
    );
    step!(
        "first push",
        stamp.contains("pre-push-suite"),
        "push-time token added: {stamp}"
    );

    // The retry: the wire dropped, the content did not change.
    let (ok, out) = git(&r, &["push", "-u", "origin", "feat/x"]);
    step!("retry", ok, "{out}");
    step!(
        "retry",
        out.contains(SKIP),
        "the skip is said out loud: {out}"
    );
    step!("retry", out.contains(PAIRED), "{out}");
    step_eq!(
        "retry",
        runs(&r, "suite"),
        suites + 1,
        "the retry repeated nothing"
    );
    step_eq!("retry", runs(&r, "check"), checks);
    step_eq!(
        "retry",
        on_remote(&remote, &["rev-parse", "refs/heads/feat/x"]),
        head(&r)
    );

    // Nothing left to push: git still runs pre-push, with no refs at all.
    let (ok, out) = git(&r, &["push"]);
    step!("in sync", ok, "{out}");
    step!("in sync", out.contains("in sync"), "{out}");

    // ---- 3. a stamp is about one tree ---------------------------------------

    // A feature branch the remote has is not protected, and says nothing.
    let out = commit_a(&r, "six\n", "feat: six");
    step!("changed tree", !out.contains("refused"), "{out}");
    let (ok, out) = git(&r, &["push"]);
    step!("changed tree", ok, "{out}");
    step!(
        "changed tree",
        !out.contains(SKIP),
        "new content is not vouched for: {out}"
    );
    step_eq!("changed tree", runs(&r, "suite"), suites + 2);

    // Same tree, new commit: its own commit note must not hide the tree's
    // push stamp. A reword stages nothing, so its pre-commit gate judged
    // nothing and is inert: the amend writes no stamp of its own (t07), and
    // any note it does carry must be the commit's own.
    let (ok, out) = git_commit(&r, &["--amend", "-m", "feat: six, reworded"]);
    step!("amend", ok, "{out}");
    step!("amend", has_note(&r, &tree(&r)), "the tree keeps its note");
    step!(
        "amend",
        !note(&r, "HEAD").contains("pre-push-suite"),
        "the commit note must be the commit's own: {}",
        note(&r, "HEAD")
    );
    let (ok, out) = git(&r, &["push", "--force-with-lease"]);
    step!("amend", ok, "divergence never blocks a push:\n{out}");
    step!("amend", out.contains(SKIP), "{out}");
    step_eq!(
        "amend",
        runs(&r, "suite"),
        suites + 2,
        "the push-time stamp on the tree still counts:\n{out}"
    );
    // pull-rebase saw the rewrite, and offers both readings of it.
    step!("amend", out.contains("diverged"), "{out}");
    step!("amend", out.contains("1 ahead, 1 behind"), "{out}");
    step!("amend", out.contains("--force-with-lease"), "{out}");
    step!("amend", out.contains("pull --rebase"), "{out}");

    // An untracked file does not cost a stamp (gates leave artefacts), and
    // pull-rebase stands aside for it.
    r.write("scratch.notes", "a file git has never seen\n");
    commit_a(&r, "seven\n", "feat: seven");
    drop_next(&remote);
    let (ok, out) = git(&r, &["push"]);
    step!("untracked", !ok && out.contains(DROPPED), "{out}");
    step!(
        "untracked",
        out.contains(STAMPED),
        "an untracked file cost a stamp: {out}"
    );
    step!("untracked", out.contains("Uncommitted changes"), "{out}");
    step!(
        "untracked",
        r.path("scratch.notes").exists(),
        "work stashed away"
    );
    let (ok, out) = git(&r, &["push"]);
    step!("untracked", ok, "{out}");
    step!("untracked", out.contains(SKIP), "{out}");
    step_eq!(
        "untracked",
        runs(&r, "suite"),
        suites + 3,
        "the gate ran once"
    );
    std::fs::remove_file(r.path("scratch.notes")).expect("remove scratch");

    // A modified tracked file: what ran is not what is pushed.
    commit_a(&r, "eight\n", "feat: eight");
    r.write("a.txt", "modified, not committed\n");
    drop_next(&remote);
    let (ok, out) = git(&r, &["push"]);
    step!("dirty", !ok && out.contains(DROPPED), "{out}");
    step!(
        "dirty",
        !out.contains(STAMPED),
        "a dirty tree was stamped: {out}"
    );
    let (ok, out) = git(&r, &["push"]);
    step!("dirty", ok, "{out}");
    step!("dirty", !out.contains(SKIP), "{out}");
    step_eq!("dirty", runs(&r, "suite"), suites + 5);
    r.git(&["checkout", "-q", "--", "a.txt"]);

    // `amont.pushStamps false` neither writes nor honours stamps.
    r.git(&["config", "amont.pushStamps", "false"]);
    commit_a(&r, "nine\n", "feat: nine");
    drop_next(&remote);
    let (ok, out) = git(&r, &["push"]);
    step!("stamps off", !ok && out.contains(DROPPED), "{out}");
    step!("stamps off", !out.contains(STAMPED), "{out}");
    let (ok, out) = git(&r, &["push"]);
    step!("stamps off", ok, "{out}");
    step!("stamps off", !out.contains(SKIP), "{out}");
    step_eq!("stamps off", runs(&r, "suite"), suites + 7);
    r.git(&["config", "--unset", "amont.pushStamps"]);

    // ---- 4. rehearsal ---------------------------------------------------------

    // The rehearsal tests the COMMIT: the working tree says something the
    // suite refuses, and a snapshot never sees it.
    commit_a(&r, "ten\n", "feat: ten");
    r.write("a.txt", "broken\n");
    let (code, out) = amont_run(&r, &["rehearse", "--wait"]);
    step_eq!("rehearse", code, 0, "{out}");
    step_eq!(
        "rehearse",
        runs(&r, "suite"),
        suites + 8,
        "the gate ran once: {out}"
    );
    step!("rehearse", out.contains("rehearsal of"), "{out}");
    step!(
        "rehearse",
        note(&r, "HEAD").contains("pre-push-suite"),
        "HEAD is stamped: {out}"
    );
    step_eq!(
        "rehearse",
        read(&r, "a.txt"),
        "broken\n",
        "the working tree was not touched"
    );
    step_eq!("rehearse", worktrees(&r), 1, "the snapshot was removed");
    // A finished rehearsal records its verdict and no step: the step is
    // only for one still running.
    let state = read(&r, ".git/amont-rehearsal");
    step!("rehearse", state.contains("phase=passed"), "{state}");
    step!("rehearse", !state.contains("step="), "{state}");
    step!(
        "rehearse",
        rehearsal_state(&r).contains("phase=passed"),
        "{}",
        rehearsal_state(&r)
    );
    let (ok, out) = git(&r, &["push"]);
    step!("rehearsed push", ok, "{out}");
    step!("rehearsed push", out.contains(SKIP), "{out}");
    step_eq!(
        "rehearsed push",
        runs(&r, "suite"),
        suites + 8,
        "the push repeated nothing"
    );
    r.git(&["checkout", "-q", "--", "a.txt"]);

    // ---- 5. unstamped commits ---------------------------------------------------

    // `--no-verify` dodges the pair, under its own name in the ledger.
    let checks = runs(&r, "check");
    r.stage("a.txt", "eleven\n");
    r.commit("feat: dodge the gate"); // Repo::commit IS --no-verify
    step_eq!("dodged", runs(&r, "check"), checks, "nothing ran at commit");
    let ledger = std::fs::read_to_string(r.path(".git/amont-bypasses"))
        .expect("a dodged gate leaves a ledger");
    step!("dodged", ledger.contains(" check"), "{ledger:?}");

    // `amont.unstampedPush refuse`: turned away with the way out, nothing
    // run while the remote waits.
    r.git(&["config", "amont.unstampedPush", "refuse"]);
    let (ok, out) = git(&r, &["push"]);
    step!("refuse", !ok, "{out}");
    step!("refuse", out.contains("refusing the push"), "{out}");
    step!("refuse", out.contains("amont rehearse --wait"), "{out}");
    step_eq!("refuse", runs(&r, "check"), checks, "nothing ran: {out}");
    // `run` keeps the old behaviour.
    r.git(&["config", "amont.unstampedPush", "run"]);
    let (ok, out) = git(&r, &["push"]);
    step!("refuse, then run", ok, "{out}");
    step!("refuse, then run", out.contains(UNSTAMPED), "{out}");
    step_eq!("refuse, then run", runs(&r, "check"), checks + 1);
    r.git(&["config", "--unset", "amont.unstampedPush"]);

    // By default the pair just comes back, and every gate that ran leaves
    // its run line on the tree's record.
    r.stage("a.txt", "twelve\n");
    r.commit("feat: dodge it again");
    step_eq!(
        "unstamped",
        runs(&r, "check"),
        checks + 1,
        "nothing ran at commit"
    );
    let (ok, out) = git(&r, &["push"]);
    step!("unstamped", ok, "{out}");
    step!(
        "unstamped",
        out.contains(UNSTAMPED),
        "the reason is named: {out}"
    );
    step_eq!(
        "unstamped",
        runs(&r, "check"),
        checks + 2,
        "the pair came back"
    );
    let record = note(&r, &tree(&r));
    step!(
        "unstamped",
        record
            .lines()
            .next()
            .is_some_and(|l| l.starts_with("amont-gate-v1")),
        "line one is still the stamp: {record}"
    );
    for gate in ["check", "suite"] {
        step!(
            "unstamped",
            record.lines().any(|l| l.starts_with("run ")
                && l.contains(&format!("pre-push-{gate} "))
                && l.ends_with(|c: char| c.is_ascii_digit())),
            "no run line for {gate}: {record}"
        );
    }
    step!(
        "unstamped",
        record.lines().filter(|l| l.starts_with("run ")).count() >= 2,
        "{record}"
    );

    // A rehearsal that fails stamps nothing, is reported, and the push runs
    // the gate again in the terminal the person is looking at; the record
    // keeps the failure and what passed before it.
    let suites = runs(&r, "suite");
    r.stage("a.txt", "broken\n");
    r.commit("feat: a change the suite refuses");
    let (code, out) = amont_run(&r, &["rehearse", "--wait"]);
    step_eq!("failed rehearsal", code, 1, "{out}");
    step_eq!("failed rehearsal", runs(&r, "suite"), suites + 1);
    step!(
        "failed rehearsal",
        !note(&r, "HEAD").contains("pre-push-suite"),
        "no stamp"
    );
    let (_, status) = amont_run(&r, &["rehearse", "--status"]);
    step!("failed rehearsal", status.contains("FAILED"), "{status}");
    let state = read(&r, ".git/amont-rehearsal");
    step!(
        "failed rehearsal",
        state.contains("phase=failed"),
        "{state}"
    );
    step!("failed rehearsal", !state.contains("step="), "{state}");
    let (ok, out) = git(&r, &["push"]);
    step!("blocked push", !ok, "{out}");
    step!(
        "blocked push",
        out.contains("rehearsal of this tree failed"),
        "{out}"
    );
    step!(
        "blocked push",
        out.contains("running the gate here"),
        "{out}"
    );
    step_eq!(
        "blocked push",
        runs(&r, "suite"),
        suites + 2,
        "the push ran it again"
    );
    let record = note(&r, &tree(&r));
    step!(
        "blocked push",
        record.contains(" pre-push-suite fail "),
        "the failure is on the record: {record}"
    );
    step!(
        "blocked push",
        record.contains(" pre-push-check pass "),
        "so is what ran before it: {record}"
    );
    commit_a(&r, "fixed\n", "fix: the suite passes again");
    let (ok, out) = git(&r, &["push"]);
    step!("fixed", ok, "{out}");

    // ---- 6. pull-rebase --------------------------------------------------------

    // A colleague pushes to the branch; the fetch says we are behind.
    let colleague = r.path(".git/colleague");
    let mut clone = Command::new("git");
    clone
        .args(["clone", "-q", "--template=", "-b", "feat/x"])
        .arg(&remote)
        .arg(&colleague);
    Repo::strip_git_env_impl(&mut clone);
    step!("colleague", clone.status().expect("clone").success());
    let theirs = Repo {
        dir: colleague.clone(),
    };
    theirs.git(&["config", "user.email", "colleague@example.com"]);
    theirs.git(&["config", "user.name", "colleague"]);
    theirs.git(&["config", "commit.gpgsign", "false"]);
    theirs.stage("theirs.md", "their work\n");
    theirs.commit("docs: their work");
    step!(
        "colleague",
        theirs
            .git(&["push", "-q", "origin", "feat/x"])
            .status
            .success()
    );
    let their_tip = head(&theirs);
    std::mem::forget(theirs); // under `.git/`: removed with the repository
    r.git(&["fetch", "-q", "origin"]);

    // `amont.autoRebase false`: advised, the key named, nothing rebased.
    r.git(&["config", "amont.autoRebase", "false"]);
    let before = head(&r);
    let (ok, out) = git(&r, &["push"]);
    step!("behind, advised", !ok, "{out}");
    step!("behind, advised", out.contains("amont.autoRebase"), "{out}");
    step_eq!(
        "behind, advised",
        head(&r),
        before,
        "advisory mode rebased anyway"
    );
    r.git(&["config", "--unset", "amont.autoRebase"]);

    // By default the branch is synced, and the push stops: the refs git
    // handed it predate the rebase.
    let (ok, out) = git(&r, &["push"]);
    step!(
        "pulled rebase",
        !ok,
        "a push whose refs predate the rebase went on:\n{out}"
    );
    step!("pulled rebase", out.contains("push again"), "{out}");
    step_eq!(
        "pulled rebase",
        head(&r),
        their_tip,
        "the rebase must have happened"
    );

    // The pull request merged and the branch was deleted on the remote: a
    // follow-up push is not blocked by a ref that is gone.
    on_remote(&remote, &["update-ref", "-d", "refs/heads/feat/x"]);
    commit_a(&r, "follow-up\n", "fix: a follow-up");
    let (ok, out) = git(&r, &["push"]);
    step!("merged away", ok, "{out}");
    step!("merged away", out.contains("no longer exists"), "{out}");

    // ---- 7. a branch never pushed ------------------------------------------------

    // `--no-track`: measured against `origin/main`, as a first push is.
    r.git(&["switch", "-q", "--no-track", "-c", "feat/y", "origin/main"]);
    let suites = runs(&r, "suite");
    r.stage("notes.md", "prose\n");
    let (ok, out) = git_commit(&r, &["-m", "docs: prose only"]);
    step!("nothing to rehearse", ok, "{out}");
    let (code, out) = amont_run(&r, &["rehearse", "--wait"]);
    step_eq!("nothing to rehearse", code, 0, "{out}");
    step!(
        "nothing to rehearse",
        out.contains("nothing to rehearse"),
        "{out}"
    );
    step_eq!("nothing to rehearse", runs(&r, "suite"), suites);
    step_eq!("nothing to rehearse", worktrees(&r), 1);

    // `amont run pre-push`: the same gate, no connection open, and a stamp
    // the push then honours.
    r.stage("b.txt", "hello\n");
    let (ok, out) = git_commit(&r, &["-m", "feat: b"]);
    step!("run pre-push", ok, "{out}");
    let (code, out) = amont_run(&r, &["run", "pre-push"]);
    step_eq!("run pre-push", code, 0, "{out}");
    step_eq!(
        "run pre-push",
        runs(&r, "suite"),
        suites + 1,
        "it ran the gate: {out}"
    );
    step!("run pre-push", out.contains(STAMPED), "it stamps: {out}");
    let (ok, out) = git(&r, &["push", "-u", "origin", "feat/y"]);
    step!("run pre-push", ok, "{out}");
    step!("run pre-push", out.contains(SKIP), "{out}");
    step_eq!(
        "run pre-push",
        runs(&r, "suite"),
        suites + 1,
        "the push repeated nothing"
    );

    // ---- 8. what reaches the remote ------------------------------------------------

    if !missing("ssh-keygen") {
        let key = r.path(".git/attest-key");
        let ok = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "t@example.org", "-f"])
            .arg(&key)
            .status()
            .expect("ssh-keygen")
            .success();
        step!("attest", ok, "ssh-keygen");
        r.git(&["config", "amont.attest", "true"]);
        r.git(&["config", "amont.attestKey", &key.display().to_string()]);
        r.stage("b.txt", "attested\n");
        let (ok, out) = git_commit(&r, &["-m", "feat: attested"]);
        step!("attest", ok, "{out}");
        let (ok, out) = git(&r, &["push"]);
        step!("attest", ok, "{out}");
        step!("attest", out.contains("attested"), "{out}");
        let attested = on_remote(
            &remote,
            &["notes", "--ref", "amont-attest", "show", &head(&r)],
        );
        let gates = attested
            .lines()
            .find(|l| l.starts_with("gates "))
            .unwrap_or("");
        step!(
            "attest",
            gates.split_whitespace().any(|g| g == "pre-push-suite"),
            "the remote's note names the gate: {attested}\npush said: {out}"
        );
    }

    // The stamps are the machine's own: an unsigned note is only as honest
    // as whoever can write the ref, so it never travels.
    step_eq!(
        "remote",
        on_remote(&remote, &["for-each-ref", "refs/notes/amont-gate"]),
        "",
        "a gate stamp left the machine"
    );
}
