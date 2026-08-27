//! The record that turns "a declaration exists" into "the check ran".
//!
//! Moving a gate entry to commit time (`docs/checks.md`, "Moving a gate entry
//! earlier") makes the push gate skip a script because a `pre-commit`
//! declaration covers it. A declaration is a promise on paper: a commit made
//! with `--no-verify`, from a libgit2 client that runs no hooks, or on a
//! machine without amont was never judged by it — and until this module
//! existed, push time had no way to tell those commits from checked ones.
//!
//! Three hooks share one record:
//!
//! 1. **pre-commit** ([`record`]) writes a one-shot marker into `$GIT_DIR`
//!    naming the gate scripts that actually ran, bound to the tree the commit
//!    is about to write (`git write-tree` — during pre-commit the index IS
//!    the commit's content; `staged_only` parks only the working tree).
//! 2. **post-commit** ([`bind_to_head`]) consumes the marker and, when the
//!    marker's tree matches `HEAD^{tree}`, stamps the commit in a notes ref.
//!    `--no-verify` skips pre-commit but NOT post-commit, so an unchecked
//!    commit arrives here with no marker and gets no stamp — which is the
//!    entire point. The tree comparison makes an aborted commit's leftover
//!    marker harmless, and a retried commit of the SAME tree correctly
//!    stamped: the check really did run on exactly that content.
//! 3. **pre-push** ([`stamps_for`]) reads the stamps back and suppresses a
//!    gate script only for pushes whose relevant commits all carry it.
//!
//! Every failure mode points the same direction: no marker, a mismatched
//! tree, a missing note, a rewritten hash — all mean "no stamp", and no stamp
//! means the push gate RUNS. Nothing here can let an unchecked commit
//! through; it can only cost a redundant gate run.
//!
//! Why a notes ref and not config: notes are keyed by commit, garbage-collect
//! with unreachable commits (an `amont.checked.<hash>` config key would
//! outlive every rebase forever), stay local (notes refs are not pushed by
//! default), and stay out of `git log` (only `refs/notes/commits` displays by
//! default). `amont uninstall` deletes the ref; see `uninstall_repo_hooks`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// First token of the marker file and of every note body. Versioned like
/// `staged_only::FORMAT`: a future amont that changes the shape bumps this,
/// and an old record is ignored rather than misread.
pub const FORMAT: &str = "amont-gate-v1";

/// The notes ref, spelled the way `git notes --ref` wants it.
pub const NOTES_REF: &str = "amont-gate";

/// The same ref, fully qualified — what `git update-ref -d` needs.
pub const NOTES_FULL_REF: &str = "refs/notes/amont-gate";

/// The marker's filename inside `$GIT_DIR`.
const MARKER: &str = "amont-gate";

/// `$GIT_DIR/amont-gate` — the worktree-PRIVATE gitdir, deliberately: the
/// commit this marker waits for happens in this worktree. The stamps the
/// marker becomes live in the common dir (a notes ref) and are shared.
fn marker_path() -> Option<PathBuf> {
    let dir = crate::git::stdout(&["rev-parse", "--git-dir"])?;
    Some(std::path::Path::new(&dir).join(MARKER))
}

/// pre-commit: record that `scripts` ran clean against the tree the commit
/// will carry.
///
/// Called on EVERY pre-commit verdict, with an empty list when nothing
/// qualifying ran (or the commit is about to be blocked) — an aborted or
/// unchecked attempt must not inherit a previous attempt's marker.
///
/// Best-effort throughout: a failure to record costs one redundant gate run
/// at push time, which is the safe direction, and a pre-commit that failed a
/// COMMIT over bookkeeping would be the tail wagging the dog.
pub fn record(scripts: &[&str]) {
    let Some(path) = marker_path() else { return };
    if scripts.is_empty() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    // The index, as the object id `git commit` is about to seal. Inherits
    // `GIT_INDEX_FILE`, so `git commit -a`'s temporary index answers here
    // too. Pure read of the index: writes objects, touches no ref.
    let Some(tree) = crate::git::stdout(&["write-tree"]) else {
        // Not "nothing ran": git could not name the tree, so nothing may be
        // vouched for. Dropping the marker is the fail-safe half (the gate
        // re-runs at push); saying so is the half that was missing.
        crate::hooks::common::warn(
            "git would not name the staged tree — this commit records no gate stamp",
        );
        let _ = std::fs::remove_file(&path);
        return;
    };
    let mut body = format!("{FORMAT}\n{tree}\n");
    for s in scripts {
        body.push_str(s);
        body.push('\n');
    }
    let _ = std::fs::write(&path, body);
}

/// post-commit: consume the marker; stamp HEAD when the tree still matches.
///
/// One-shot by construction — the marker is deleted before anything is
/// judged, so no path through here can leave it to vouch for a later commit.
///
/// Returns the scripts it actually stamped (empty on every no-stamp path,
/// including a note git refused). The caller subtracts this from what the
/// manifest declares to learn what the commit dodged — [`crate::bypass`]
/// keeps that count. Two records, two questions: the stamp gates a check,
/// the ledger only counts.
pub fn bind_to_head() -> Vec<String> {
    let Some(path) = marker_path() else {
        return Vec::new();
    };
    let Ok(body) = std::fs::read_to_string(&path) else {
        return Vec::new(); // no marker: nothing ran at pre-commit, nothing to stamp
    };
    let _ = std::fs::remove_file(&path);
    let mut lines = body.lines();
    if lines.next() != Some(FORMAT) {
        return Vec::new();
    }
    let Some(tree) = lines.next() else {
        return Vec::new();
    };
    let scripts: Vec<&str> = lines.filter(|l| !l.trim().is_empty()).collect();
    if scripts.is_empty() {
        return Vec::new();
    }
    let Some(head_tree) = crate::git::stdout(&["rev-parse", "HEAD^{tree}"]) else {
        crate::hooks::common::warn(
            "git would not name this commit's tree — no gate stamp was written",
        );
        return Vec::new();
    };
    // A different tree means this commit is not the one pre-commit judged —
    // the marker is a dead letter from an aborted attempt.
    if head_tree != tree {
        return Vec::new();
    }
    let note = format!("{FORMAT} {}", scripts.join(" "));
    // The stamp goes on the TREE as well as the commit, and the tree is the
    // one that survives the way work actually reaches `main`.
    //
    // A squash-merge is performed by the forge: it produces a commit nobody
    // here ever saw, carrying no note, so a later `git push` of a tag on
    // that commit re-ran every gate the branch had already proved. Measured
    // on this repository: a branch push took 13 seconds and the tag push
    // that followed took the full suite and died on a reset connection.
    //
    // The tree is identical across that merge whenever the base has not
    // moved — five of five merges in one afternoon here — and identical
    // trees are identical CONTENT, which is the only thing a test suite
    // reads. That is the same argument `attest` makes for signing the tree
    // rather than the commit, and this module's marker has been tree-bound
    // since it was written; this only carries the binding through to the
    // note.
    //
    // Both, not either: the commit note is what a `git log --notes` reader
    // sees, and dropping it would make the stamps invisible in the place
    // people look for them.
    let _ = crate::git::succeeds(&[
        "notes", "--ref", NOTES_REF, "add", "-f", "-m", &note, &head_tree,
    ]);
    if !crate::git::succeeds(&[
        "notes", "--ref", NOTES_REF, "add", "-f", "-m", &note, "HEAD",
    ]) {
        // A note git refused is not a stamp — and the push will re-run these
        // checks, which is right but looks arbitrary unless it is said.
        crate::hooks::common::warn(
            "git refused to write the gate stamp — these checks will run again at push",
        );
        return Vec::new();
    }
    scripts.iter().map(|s| s.to_string()).collect()
}

/// pre-push: which of `commits` carry a stamp, and for which scripts.
///
/// One `notes list` narrows the reads to commits that have a note at all;
/// absent ref, unparseable note, wrong format version — all read as "no
/// stamp", which re-runs the gate.
pub fn stamps_for(commits: &[String]) -> HashMap<String, Vec<String>> {
    let mut out = HashMap::new();
    if commits.is_empty() {
        return out;
    }
    let Some(list) = crate::git::stdout(&["notes", "--ref", NOTES_REF, "list"]) else {
        // NOT the absent-ref case, whatever an older comment here claimed:
        // `notes list` exits 0 with empty output when the ref does not exist,
        // so that arrives as `Some("")` and falls through as "nothing is
        // stamped" — correctly. Reaching HERE means git could not answer at
        // all. Same verdict (the gates re-run: never skip work on a question
        // we could not ask), different sentence, because a transient git
        // failure that reads as "nothing is stamped" is indistinguishable
        // from the real thing — which is exactly how one flaky spawn cost a
        // day of not-diagnosing.
        crate::hooks::common::warn(
            "git would not list the gate stamps — every gated check will run again",
        );
        return out;
    };
    let noted: HashSet<&str> = list
        .lines()
        .filter_map(|l| l.split_whitespace().nth(1))
        .collect();
    // Commit -> tree, in ONE spawn rather than one per commit: this is the
    // push path, and a `rev-parse` each would be a process per pushed
    // commit to answer a question `git log` answers in a batch.
    let mut trees: HashMap<String, String> = HashMap::new();
    {
        let mut args: Vec<&str> = vec!["log", "--no-walk", "--format=%H %T"];
        args.extend(commits.iter().map(String::as_str));
        if let Some(out) = crate::git::stdout(&args) {
            for line in out.lines() {
                let mut it = line.split_whitespace();
                if let (Some(c), Some(t)) = (it.next(), it.next()) {
                    trees.insert(c.to_string(), t.to_string());
                }
            }
        }
        // No mapping is not an error: every commit simply falls back to the
        // commit-keyed lookup below, which is what happened before trees
        // were stamped at all.
    }

    for commit in commits {
        // The commit's own note first, then its TREE's. A squash-merge
        // produces a commit this machine never saw — no note — while the
        // content, and therefore the tree, is the one the gates ran on.
        // Falling through to the tree is what lets a tag push on a merged
        // commit skip work the branch already proved.
        //
        // The direction of failure is unchanged: no note on either, an
        // unparseable one, or a git that would not answer all mean "no
        // stamp", and no stamp runs the gate.
        let key: &str = if noted.contains(commit.as_str()) {
            commit
        } else {
            match trees.get(commit).filter(|t| noted.contains(t.as_str())) {
                Some(tree) => tree,
                None => continue,
            }
        };
        let Some(body) = crate::git::stdout(&["notes", "--ref", NOTES_REF, "show", key]) else {
            continue;
        };
        let Some(first) = body.lines().next() else {
            continue;
        };
        let mut tokens = first.split_whitespace();
        if tokens.next() != Some(FORMAT) {
            continue;
        }
        out.insert(commit.clone(), tokens.map(str::to_string).collect());
    }
    out
}

/// uninstall: forget everything this module ever wrote here.
///
/// The stamps are OUR bookkeeping — unlike `hook.skip` and `amont.severity`,
/// which are the user's statements and are never touched.
pub fn forget() -> bool {
    let marker = marker_path().is_some_and(|path| std::fs::remove_file(&path).is_ok());
    let notes = crate::git::succeeds(&["update-ref", "-d", NOTES_FULL_REF]);
    marker || notes
}

/// The same, for a repository this process is not standing in.
pub fn forget_in(repo: &std::path::Path) -> bool {
    let marker = crate::git::stdout_in(repo, &["rev-parse", "--absolute-git-dir"])
        .is_some_and(|dir| std::fs::remove_file(std::path::Path::new(&dir).join(MARKER)).is_ok());
    let notes = crate::git::succeeds_in(repo, &["update-ref", "-d", NOTES_FULL_REF]);
    marker || notes
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// A real repository, because every function here is a conversation with
    /// git — hand-rolled fixtures would test the conversation we imagined.
    fn repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gate-stamp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "--template=", "."]);
        git(&dir, &["config", "user.email", "t@t.test"]);
        git(&dir, &["config", "user.name", "t"]);
        dir
    }

    /// A fixture git call that FAILS where it fails.
    ///
    /// This used to discard the exit status, and that is how a rare flake
    /// stayed unreadable for a day: if the setup `git commit` did not
    /// happen, the test carried on to an unborn HEAD, and the panic landed
    /// three lines later on a missing gate stamp — a product-shaped
    /// failure for a fixture-shaped cause. Same rule the checks obey:
    /// git failing is not git answering.
    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "fixture: git {args:?} in {} exited {:?}: {}",
            dir.display(),
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// The module talks to the repo at the process cwd; these tests each set
    /// it. Serialised via the crate-wide lock, because cwd is process-global
    /// and `attest`'s tests move it too.
    fn in_repo<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
        let _guard = crate::TEST_CWD.lock().unwrap_or_else(|p| p.into_inner());
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir).unwrap();
        let r = f();
        std::env::set_current_dir(prev).unwrap();
        r
    }

    #[test]
    fn a_recorded_marker_becomes_a_stamp_on_the_matching_commit() {
        let dir = repo("roundtrip");
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        git(&dir, &["add", "a.ts"]);
        in_repo(&dir, || {
            record(&["typecheck", "test"]);
            git(&dir, &["commit", "-qm", "chore: a"]);
            let stamped = bind_to_head();
            assert_eq!(
                stamped,
                vec!["typecheck".to_string(), "test".to_string()],
                "bind_to_head reports the scripts it stamped"
            );
            let head = git(&dir, &["rev-parse", "HEAD"]);
            let stamps = stamps_for(std::slice::from_ref(&head));
            assert_eq!(
                stamps.get(&head).map(Vec::as_slice),
                Some(&["typecheck".to_string(), "test".to_string()][..])
            );
            // One-shot: the marker is gone.
            // One-shot: the marker is gone. The fixture's own path, not
            // `marker_path()` — that helper spawns git, and a transient
            // spawn failure on a loaded runner reads as `None` here while
            // production code correctly treats it as "no marker". Seen once,
            // on Windows, as an unwrap panic in a sibling test.
            assert!(!dir.join(".git").join(MARKER).exists());
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_marker_for_a_different_tree_stamps_nothing() {
        let dir = repo("stale");
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        git(&dir, &["add", "a.ts"]);
        in_repo(&dir, || {
            record(&["typecheck"]);
            // The commit that actually lands carries DIFFERENT content — the
            // aborted-attempt-then-different-retry shape.
            std::fs::write(dir.join("a.ts"), "y").unwrap();
            git(&dir, &["add", "a.ts"]);
            git(&dir, &["commit", "-qm", "chore: different"]);
            assert!(
                bind_to_head().is_empty(),
                "bind_to_head reports nothing when the tree moved"
            );
            let head = git(&dir, &["rev-parse", "HEAD"]);
            assert!(
                stamps_for(&[head]).is_empty(),
                "a stale marker must not vouch"
            );
            assert!(
                !dir.join(".git").join(MARKER).exists(),
                "consumed either way"
            );
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_record_clears_a_previous_marker() {
        let dir = repo("clears");
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        git(&dir, &["add", "a.ts"]);
        in_repo(&dir, || {
            record(&["typecheck"]);
            assert!(dir.join(".git").join(MARKER).exists());
            record(&[]);
            assert!(!dir.join(".git").join(MARKER).exists());
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The version guard's REJECT branch, fed a hand-written marker: an old
    /// (or future) format is ignored rather than misread — the doc's claim,
    /// now pinned. Every other test's markers come from record() itself and
    /// so always carry the current FORMAT.
    #[test]
    fn a_marker_in_an_unknown_format_stamps_nothing() {
        let dir = repo("wrongformat");
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        git(&dir, &["add", "a.ts"]);
        in_repo(&dir, || {
            let tree = git(&dir, &["write-tree"]);
            let marker = dir.join(".git").join(MARKER);
            std::fs::write(&marker, format!("amont-gate-v99\n{tree}\ntypecheck\n")).unwrap();
            git(&dir, &["commit", "-qm", "chore: a"]);
            bind_to_head();
            let head = git(&dir, &["rev-parse", "HEAD"]);
            assert!(
                stamps_for(std::slice::from_ref(&head)).is_empty(),
                "an unknown format was trusted"
            );
            assert!(!marker.exists(), "consumed either way");
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A note somebody else wrote into OUR ref is not a stamp. Absent this,
    /// `git notes --ref=amont-gate add` would be a one-line way to vouch for
    /// an unchecked commit — the parsing trust boundary of the whole chain.
    #[test]
    fn a_foreign_note_is_not_a_stamp() {
        let dir = repo("foreignnote");
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        git(&dir, &["add", "a.ts"]);
        in_repo(&dir, || {
            git(&dir, &["commit", "-qm", "chore: a"]);
            git(
                &dir,
                &[
                    "notes",
                    "--ref",
                    NOTES_REF,
                    "add",
                    "-m",
                    "typecheck test",
                    "HEAD",
                ],
            );
            let head = git(&dir, &["rev-parse", "HEAD"]);
            assert!(
                stamps_for(std::slice::from_ref(&head)).is_empty(),
                "a note without the format token was trusted"
            );
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An absent notes ref is `Some("")`, not `None` — the distinction the
    /// warning on that branch depends on. If git ever starts failing here
    /// instead, this test fails and the warning stops being a lie.
    #[test]
    fn a_repo_with_no_stamps_answers_emptily_rather_than_failing() {
        let dir = repo("no-stamps");
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        git(&dir, &["add", "a.ts"]);
        git(&dir, &["commit", "-qm", "chore: a"]);
        in_repo(&dir, || {
            assert_eq!(
                crate::git::stdout(&["notes", "--ref", NOTES_REF, "list"]).as_deref(),
                Some(""),
                "an absent notes ref must be an ANSWER, not a failure — the \
                 no-stamps path and the git-is-broken path are told apart by it"
            );
            let head = git(&dir, &["rev-parse", "HEAD"]);
            assert!(stamps_for(&[head]).is_empty());
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn forget_removes_the_stamps() {
        let dir = repo("forget");
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        git(&dir, &["add", "a.ts"]);
        in_repo(&dir, || {
            record(&["typecheck"]);
            git(&dir, &["commit", "-qm", "chore: a"]);
            bind_to_head();
            let head = git(&dir, &["rev-parse", "HEAD"]);
            assert!(!stamps_for(std::slice::from_ref(&head)).is_empty());
            forget();
            assert!(stamps_for(&[head]).is_empty());
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A squash-merge produces a commit this machine never saw, and the
    /// stamp has to survive it.
    ///
    /// This is the case that motivated tree-stamping. A branch is verified
    /// locally and stamped; the forge squashes it onto `main` as a NEW
    /// commit with no note; pushing a tag on that commit re-ran every gate
    /// the branch had already proved. Measured on this repository: 13
    /// seconds for the branch push, then the full suite for the tag push,
    /// which died on a reset connection.
    ///
    /// The tree is what the gates actually read, and it is identical across
    /// that merge whenever the base has not moved — five of five merges in
    /// one afternoon here.
    #[test]
    fn a_stamp_survives_a_commit_being_rewritten_with_the_same_tree() {
        let dir = repo("squashed");
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        git(&dir, &["add", "a.ts"]);
        in_repo(&dir, || {
            record(&["test"]);
            git(&dir, &["commit", "-qm", "feat: on a branch"]);
            assert_eq!(bind_to_head(), vec!["test".to_string()]);
            let branch_tip = git(&dir, &["rev-parse", "HEAD"]);

            // Stand in for the forge's squash: a DIFFERENT commit object
            // with the SAME tree. `--amend` with a new message is the
            // cheapest way to get exactly that shape.
            git(
                &dir,
                &[
                    "commit",
                    "-q",
                    "--amend",
                    "-m",
                    "feat: squashed by the forge",
                ],
            );
            let merged = git(&dir, &["rev-parse", "HEAD"]);
            assert_ne!(merged, branch_tip, "the fixture must produce a new commit");
            assert_eq!(
                git(&dir, &["rev-parse", "HEAD^{tree}"]),
                git(&dir, &["rev-parse", &format!("{branch_tip}^{{tree}}")]),
                "…carrying the same tree, which is the whole premise"
            );

            let stamps = stamps_for(std::slice::from_ref(&merged));
            assert_eq!(
                stamps.get(&merged).map(Vec::as_slice),
                Some(&["test".to_string()][..]),
                "the stamp must follow the content, not the commit hash"
            );
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// And it must not follow anything else. A commit whose tree was never
    /// stamped gets nothing, however many other stamps exist — otherwise
    /// this widening would vouch for content nobody checked.
    #[test]
    fn a_different_tree_gets_no_stamp_from_the_fallback() {
        let dir = repo("othertree");
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        git(&dir, &["add", "a.ts"]);
        in_repo(&dir, || {
            record(&["test"]);
            git(&dir, &["commit", "-qm", "chore: stamped"]);
            assert_eq!(bind_to_head(), vec!["test".to_string()]);

            // Different CONTENT, so a different tree, and no marker: this
            // commit was never judged.
            std::fs::write(dir.join("b.ts"), "y").unwrap();
            git(&dir, &["add", "b.ts"]);
            git(&dir, &["commit", "-qm", "chore: unjudged"]);
            let unstamped = git(&dir, &["rev-parse", "HEAD"]);

            assert!(
                stamps_for(std::slice::from_ref(&unstamped)).is_empty(),
                "a tree nobody stamped must not inherit one"
            );
        });
        let _ = std::fs::remove_dir_all(&dir);
    }
}
