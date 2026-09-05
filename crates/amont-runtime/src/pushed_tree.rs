//! Run a pre-push check against what is being PUSHED.
//!
//! `rust-test` and `run-tests-js` take their file set from the pushed refs —
//! correct — and then run the suite with `current_dir` set to the developer's
//! working tree. So the suite can pass on an uncommitted fix, or fail on an
//! uncommitted experiment, and in neither case has it tested the commits being
//! pushed. Same gap as the pre-commit one, from the other end.
//!
//! ## Why not the stash
//!
//! Holding unstaged changes aside is the pre-commit answer, and it is the wrong
//! instrument here. A push is not a staging operation: the difference that
//! matters is not tree-versus-index but tree-versus-the-commit-you-are-sending,
//! and that includes everything staged-but-uncommitted too. Setting all of it
//! aside for the length of a test suite would leave the developer looking at a
//! tree that is not theirs for minutes at a time.
//!
//! ## What it costs, which is the whole question
//!
//! `git worktree add --detach <tip>` materialises the pushed commit somewhere
//! else and the suite runs there. The tree is untouched, and an interrupted run
//! leaves a worktree rather than a mangled checkout.
//!
//! The cost is real: a second checkout, and a build that cannot reuse the
//! primary tree's `target/` cache, so the first push after this lands is a cold
//! build. That is why it is opt-in — `git config amont.testPushedTree true`
//! — rather than the default. The default keeps today's behaviour and now SAYS
//! what it is testing, which was the actual bug: not that it used the tree, but
//! that nobody knew it did.

use std::path::{Path, PathBuf};

use crate::ui::warning_sign;

/// Whether the user asked for the accurate-but-slower answer.
///
/// Read through [`crate::config`], so `on`, `yes` and every capitalisation work
/// exactly as git-config(1) says they do — and a value git cannot parse takes
/// the default while SAYING so, rather than reading as a quiet "no".
pub fn enabled() -> bool {
    crate::config::boolean_or("amont.testPushedTree", false)
}

/// A command to run inside a fresh snapshot before any suite does.
///
/// A checkout is not a workspace: a pnpm monorepo has no `node_modules` in
/// a worktree git just created, and a suite started there fails on
/// `Cannot find module` having tested nothing. What makes it a workspace is
/// per-repository knowledge — `pnpm install --offline --frozen-lockfile`,
/// `npm ci`, nothing at all for a Rust crate — so it is a git-config value,
/// set once per clone by the person who knows:
///
/// ```sh
/// git config amont.snapshotPrepare "pnpm install --offline --frozen-lockfile"
/// ```
///
/// Runs through the shell, in the snapshot, before the gate, and a failure
/// is the snapshot's failure: no suite runs on a tree that was never made
/// runnable. Used by both snapshot consumers — `amont.testPushedTree` at
/// push time and the background rehearsal — so the two cannot drift.
const PREPARE: &str = "amont.snapshotPrepare";

pub fn prepare_command() -> Option<String> {
    crate::config::string_value(PREPARE).filter(|s| !s.trim().is_empty())
}

/// A checkout of the pushed commit, removed when it goes out of scope.
pub struct PushedTree {
    path: PathBuf,
    repo: PathBuf,
}

impl PushedTree {
    /// Materialise `tip`, or `None` when that is not possible — in which case
    /// the caller falls back to the working tree and says so.
    /// Takes the repository explicitly rather than relying on the working
    /// directory: `set_current_dir` is process-global, so a test that changed
    /// it would race every other test in the binary.
    pub fn create(repo: &Path, tip: &str) -> Option<PushedTree> {
        let base = std::env::temp_dir().join(unique_name("amont-push"));
        Self::create_at(base, repo, tip)
    }

    /// The actual work, over an explicit path — split out so a test can hand
    /// it a path it controls, since `create`'s own path is unpredictable BY
    /// DESIGN and cannot be aimed at a fixture.
    ///
    /// `create_dir`, not a `remove_dir_all` before creating: that deleted
    /// whatever was already at this path BEFORE this process had established
    /// it owned it — an unpredictable name makes landing on an existing path
    /// unlikely, not impossible, and "unlikely" is not the bar for a delete.
    /// `create_dir` is exclusive: it fails loudly on anything already there
    /// instead of removing it, and git's own `worktree add` is content to
    /// receive a directory that already exists as long as it is empty —
    /// which this one, having just been created, provably is.
    fn create_at(base: PathBuf, repo: &Path, tip: &str) -> Option<PushedTree> {
        std::fs::create_dir(&base).ok()?;
        let ok = crate::git::succeeds(&[
            "-C",
            repo.to_str()?,
            "worktree",
            "add",
            "--detach",
            "--quiet",
            base.to_str()?,
            tip,
        ]);
        if !ok {
            // Ours to clean up: we created it moments ago, so nothing else
            // could have raced in ahead of us to make this delete unsafe.
            let _ = std::fs::remove_dir_all(&base);
            return None;
        }
        let tree = PushedTree {
            path: base,
            repo: repo.to_path_buf(),
        };
        if !tree.prepare() {
            // `Drop` removes the worktree; the caller hears `None` and says
            // what it is doing instead.
            return None;
        }
        Some(tree)
    }

    /// Run `amont.snapshotPrepare`, if set, inside the checkout. True when
    /// there was nothing to run or it exited 0.
    fn prepare(&self) -> bool {
        let Some(script) = prepare_command() else {
            return true;
        };
        println!("preparing the snapshot: {script}");
        #[cfg(unix)]
        let mut cmd = {
            let mut c = std::process::Command::new("sh");
            c.arg("-c").arg(&script);
            c
        };
        #[cfg(not(unix))]
        let mut cmd = {
            let mut c = std::process::Command::new("cmd");
            c.arg("/C").arg(&script);
            c
        };
        cmd.current_dir(&self.path)
            .stdin(std::process::Stdio::null());
        crate::hooks::common::strip_git_env(&mut cmd);
        let ok = crate::hooks::common::bounded_success(&mut cmd, PREPARE);
        if !ok {
            println!(
                "{} {PREPARE} failed in the snapshot — nothing can be tested there",
                warning_sign()
            );
        }
        ok
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// `<prefix>-<pid>-<hash>`: the pid stays for a human correlating a leftover
/// directory with a hung process, but the pid ALONE made the name guessable —
/// `ps` hands it to anyone on the box — and a pre-planted directory or
/// symlink at a predictable path in a world-writable `/tmp` is somebody
/// else's decision about where our checkout goes.
///
/// It is no longer the difference between safe and destructive: `create_at`
/// takes the path with an exclusive `create_dir` and refuses anything already
/// there, rather than the `remove_dir_all`-first shape this comment used to
/// describe. Unpredictability is now defence in depth — it means the refusal
/// is not something an attacker can arrange on demand.
///
/// No dependency for real randomness here (this crate ships dependency-free),
/// so the tail is a hash of the wall clock and the pid through
/// `RandomState`'s own OS-seeded key — enough that guessing it in advance is
/// impractical, not a cryptographic promise.
fn unique_name(prefix: &str) -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hash, Hasher};

    let pid = std::process::id();
    let mut hasher = RandomState::new().build_hasher();
    pid.hash(&mut hasher);
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .hash(&mut hasher);
    format!("{prefix}-{pid}-{:016x}", hasher.finish())
}

impl Drop for PushedTree {
    fn drop(&mut self) {
        // `--force`: the suite may have written into it, and a build artefact
        // must not be a reason to leave a worktree registered forever.
        let _ = crate::git::succeeds(&[
            "-C",
            self.repo.to_str().unwrap_or_default(),
            "worktree",
            "remove",
            "--force",
            self.path.to_str().unwrap_or_default(),
        ]);
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Tips whose snapshot was ASKED FOR and could not be made, so their suite
/// ran against the working tree after all.
///
/// Read by `dispatch::stamp_tips`, which otherwise decides a tip is vouchable
/// from `enabled()` — the CONFIG — and would stamp a tip whose content was
/// never tested. See [`fell_back`].
///
/// A `Mutex` because pre-push checks run concurrently and each loops over its
/// own refs; a poisoned lock is treated as "it fell back", which costs a
/// redundant gate run and never a false stamp.
static FELL_BACK: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::OnceLock::new();

fn fallbacks() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    FELL_BACK.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

fn note_fallback(tip: &str) {
    if let Ok(mut set) = fallbacks().lock() {
        set.insert(tip.to_string());
    }
}

/// Did the snapshot for `tip` fail, leaving its suite on the working tree?
///
/// `true` also when the lock is poisoned: a stamp is a claim that the gate
/// ran on exactly this content, and a claim we cannot check is one we do not
/// make.
pub fn fell_back(tip: &str) -> bool {
    match fallbacks().lock() {
        Ok(set) => set.contains(tip),
        Err(_) => true,
    }
}

/// Where a pre-push suite should run, and whether that is the honest answer.
///
/// Takes ONE commit, not the whole ref list: a push can carry several refs
/// (`git push origin a b`), each with its own tip, and a caller that checked
/// out only the first one and ran every ref's tests against it would test
/// the second ref's code against the first ref's tree. Callers loop over
/// their own refs and pass each one's tip in turn.
///
/// Returns the directory plus the guard that owns it — dropping the guard
/// removes the worktree, so the caller must hold it for the length of the run.
pub fn where_to_run(tip: &str, fallback: &str) -> (PathBuf, Option<PushedTree>) {
    // Inside a rehearsal the working tree IS a snapshot of the tip: a
    // second checkout would test the same content twice, and the warning
    // below would be false.
    if crate::rehearsal::in_snapshot() {
        return (PathBuf::from(fallback), None);
    }
    if !enabled() {
        // Today's behaviour, but no longer silent about it. NOT a fallback:
        // nobody asked for a snapshot, and `stamp_tips` already knows to
        // vouch for the working tree only when it IS the tip.
        println!(
            "{} testing the WORKING TREE, not the pushed commits \
             (`git config amont.testPushedTree true` to test what you are pushing)",
            warning_sign()
        );
        return (PathBuf::from(fallback), None);
    }
    match PushedTree::create(Path::new(fallback), tip) {
        Some(tree) => {
            let path = tree.path().to_path_buf();
            (path, Some(tree))
        }
        None => {
            // Recorded, not just printed. `stamp_tips` used to read
            // `enabled()` and conclude "the suite ran on the tip itself",
            // which is exactly what did NOT happen here: the gate passed on
            // a possibly-dirty working tree and the tip was stamped anyway,
            // so the next push of that tree — a retry after a dropped
            // connection, or a tag on the merged commit — skipped a gate
            // nobody had run on its content. `gate_stamp`'s own promise is
            // that no path here can let an unchecked commit through.
            note_fallback(tip);
            println!(
                "{} could not check out {tip} to test it; testing the working tree instead \
                 (nothing will be stamped for it)",
                warning_sign()
            );
            (PathBuf::from(fallback), None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point: two calls must not name the same path, or a
    /// predictable name is right back to being predictable.
    #[test]
    fn unique_name_does_not_repeat() {
        let a = unique_name("amont-push");
        let b = unique_name("amont-push");
        assert_ne!(a, b);
        assert!(a.starts_with("amont-push-"));
    }

    /// The point of the whole fix: something already at the target path is
    /// left ALONE, not deleted to make way. `create_at` fails closed
    /// (`None`) instead of the old `remove_dir_all`-first shape, which would
    /// have destroyed `sentinel.txt` here to clear the path for a worktree
    /// that was never going to use it anyway (the repo below is fake).
    #[test]
    fn an_existing_path_is_left_alone_not_cleared() {
        let base = std::env::temp_dir().join(format!("pushed-collision-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("sentinel.txt"), "do not delete me").unwrap();

        let got = PushedTree::create_at(base.clone(), Path::new("/does/not/matter"), "HEAD");
        assert!(
            got.is_none(),
            "must refuse rather than reuse a path it did not create"
        );
        assert_eq!(
            std::fs::read_to_string(base.join("sentinel.txt")).unwrap(),
            "do not delete me",
            "an existing path must never be cleared to make room"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    fn repo(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pushed-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for args in [
            vec!["init", "-q", "--template=", "."],
            vec!["config", "user.email", "t@t.test"],
            vec!["config", "user.name", "t"],
            // Git for Windows rewrites line endings on checkout; a byte
            // comparison would otherwise assert git's newline policy.
            vec!["config", "core.autocrlf", "false"],
        ] {
            std::process::Command::new("git")
                .args(&args)
                .current_dir(&d)
                .output()
                .expect("git");
        }
        d
    }

    /// The point: the checkout holds the COMMIT, not whatever the developer
    /// has open.
    #[test]
    fn the_worktree_holds_the_committed_content() {
        let d = repo("tree");
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&d)
                .output()
                .expect("git")
        };
        std::fs::write(d.join("a.txt"), "committed\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "seed"]);
        let head = String::from_utf8_lossy(&git(&["rev-parse", "HEAD"]).stdout)
            .trim()
            .to_string();
        // Uncommitted, and it must not travel.
        std::fs::write(d.join("a.txt"), "dirty, not pushed\n").unwrap();

        let tree = PushedTree::create(&d, &head).expect("worktree");
        let seen = std::fs::read_to_string(tree.path().join("a.txt")).unwrap();
        let at = tree.path().to_path_buf();
        drop(tree);

        assert_eq!(seen, "committed\n", "the worktree saw the dirty tree");
        assert!(!at.exists(), "the worktree outlived its guard");
        let _ = std::fs::remove_dir_all(&d);
    }
}
