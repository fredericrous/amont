//! Make the checks see what is being committed.
//!
//! `staged_files()` asks the index for the PATH LIST, which is right, and then
//! hands those paths to tools that open them from the WORKING TREE. Eleven of
//! the fifteen pre-commit checks do this. So `git add -p` half a file, commit,
//! and prettier reads the whole working-tree file: it fails on lines you did not
//! stage, or passes on lines you did.
//!
//! `pre-commit` fixes this for everyone by stashing the unstaged changes for the
//! duration of the run, and their wording names both directions:
//!
//! > Running hooks on unstaged changes can lead to both false-positives and
//! > false-negatives during committing.
//!
//! ## This is the most dangerous code in the repository
//!
//! A stash taken and not restored loses uncommitted work. That is worse than
//! either failure that overwrote tracked files, because there is nothing on disk
//! to recover from. Hence, in order of how likely each is to bite:
//!
//! 1. **Nothing to stash → nothing happens.** The common case never touches the
//!    tree.
//! 2. **Restore on a SIGNAL.** `Drop` runs on unwind and on early return; it
//!    does NOT run when Ctrl-C kills the process, and interrupting a slow
//!    pre-commit is the most probable route to an orphaned stash, not the least.
//! 3. **Restore in `Drop`** for panics and early returns.
//! 4. **Never mid-operation.** A merge or rebase in progress means the tree is
//!    already holding somebody else's work — `GitState` (PR 2) answers this.
//! 5. **Restore failure is loud and fatal**, and prints the store's path, so
//!    the work is findable on disk rather than silently gone. (Not `git stash
//!    list`: nothing here is a stash ref — see the `impl` comment below for
//!    why byte-exact copies beat both `stash --keep-index` and a patch.)
//! 6. **`amont restore`** for when even the handler was interrupted.
//!
//! The store describes itself in an `index` file, and every repo-controlled
//! path lives under `files/`. Both because the store used to encode its
//! metadata in the payload FILENAMES, which a repository is allowed to collide
//! with — and did, deleting a tracked file and planting a symlink outside the
//! worktree. See [`Held`].

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Mutex;

use crate::ui::{error_sign, warning_sign};

/// Whether this process is holding a patch. Global because a signal handler
/// cannot be handed a reference, and because there is at most one per process.
static HELD: AtomicBool = AtomicBool::new(false);

/// Guards `checkout` through `HELD.store(true, ..)` in `enter()` against a
/// `restore()` racing in concurrently from the signal-watcher thread.
///
/// Without this, a signal landing mid-`checkout` lets `restore()` read `HELD`
/// as still `false`, no-op, and kill the process — while `checkout`'s child
/// keeps running, ORPHANED, and eventually finishes writing the tree anyway.
/// The parked files are never put back and nothing said so. This is not
/// hypothetical: it is what running the checkout-then-signal race a dozen
/// times over actually produced, once `restore()` moved off the interrupted
/// call stack and onto a thread of its own — see `install_signal_handler`.
/// `restore()` blocking here until `enter()` finishes is exactly the fix: by
/// the time it can read `HELD`, `checkout` has definitely either succeeded
/// (and it restores) or never called (and it no-ops), never "maybe".
static ENTER_LOCK: Mutex<()> = Mutex::new(());

/// Where the unstaged changes are parked. Inside `$GIT_DIR` so they are never
/// committed, never seen by a check, and findable by hand.
pub const STORE: &str = "amont-held";

/// Our metadata, beside the payloads rather than encoded into their names.
const INDEX: &str = "index";

/// Everything repo-controlled lives under here. See [`Held`].
const FILES: &str = "files";

/// The index's first field. Present so an older or newer store is recognised
/// as such instead of being half-understood.
const FORMAT: &str = "amont-held-v1";

/// What each held path's ON-DISK content looked like right after `enter()`'s
/// checkout — i.e. the index content the checks were meant to see. `rel NUL
/// hex NUL` pairs. Absent (an older store, a crash before the write) means no
/// mid-check-edit protection, exactly as before the file existed.
const EXPECTED: &str = "expected";

/// Where a file the user edited MID-CHECK parks the held copy instead of
/// clobbering the edit. Beside the store, not inside it: the store is deleted
/// on a clean restore and this must survive to be read.
const PRESERVED: &str = "amont-preserved";

/// FNV-1a over the content. Not a security boundary — the question is "did
/// an editor save land here while the checks ran", and an accidental
/// collision needs an accidental 64-bit fixed point.
fn content_hash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// One parked path, and what it was.
///
/// This used to be encoded in the FILENAME — `<name>.amont-absent` and
/// `<name>.amont-symlink` beside the payload copies — and a repository is
/// allowed to contain files with those names. It cost exactly what you would
/// expect: a repo tracking `notes` and `notes.amont-absent`, with the second
/// modified, had `notes` DELETED from the working tree on restore, because the
/// suffix strip turned one file's payload into a statement about another. The
/// symlink form was worse: the repo chose both the link name and an arbitrary
/// ABSOLUTE target, so committing in it planted a symlink pointing anywhere on
/// the machine, and anything that later wrote that path wrote through it.
///
/// Marker-by-name cannot be made safe by escaping, because the store is also
/// read by `amont restore` from a later process that has only the filenames
/// to go on. So the metadata moved out of band, and every repo-controlled path
/// moved under `files/`, where it cannot collide with `index` no matter what it
/// is called.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Held {
    /// Content differs from the index. The bytes are at `files/<rel>`.
    ///
    /// `mode` is the working tree's, captured before `checkout` resets it.
    /// `git diff --name-only` lists a pure `chmod +x` with IDENTICAL content,
    /// so without this a `chmod +x deploy.sh` you had not staged came back
    /// non-executable and nothing said so — invisible to every content-based
    /// assertion, which is why it survived so long.
    Modified { rel: String, mode: Option<u32> },
    /// Deleted in the tree but not staged. Restore deletes it again.
    Absent { rel: String },
    /// A symlink, and where it pointed. No payload file is written at all.
    Symlink { rel: String, target: String },
}

/// A repo-relative path that cannot escape the tree it came from.
///
/// Applied on the way OUT of the store as well as in: the index is a file on
/// disk that a later `amont restore` trusts, so `..`, an absolute path or a
/// drive prefix must not survive being written into it by any route.
fn safe_rel(rel: &str) -> Option<std::path::PathBuf> {
    use std::path::Component;
    if rel.is_empty() {
        return None;
    }
    let mut out = std::path::PathBuf::new();
    for c in Path::new(rel).components() {
        match c {
            Component::Normal(part) => out.push(part),
            // RootDir, Prefix, ParentDir, CurDir: all of them mean this path
            // is not a plain location inside the worktree.
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

fn push_field(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
    out.push(0);
}

/// NUL-delimited, for the reason `git.rs` gives for `-z`: a path may contain a
/// newline, a tab, a quote or a backslash, and may not contain a NUL. There is
/// no escaping here to get wrong, which is the point.
fn encode_index(entries: &[Held]) -> Vec<u8> {
    let mut out = Vec::new();
    push_field(&mut out, FORMAT);
    for e in entries {
        match e {
            Held::Modified { rel, mode } => {
                push_field(&mut out, "file");
                push_field(&mut out, rel);
                push_field(&mut out, &mode.map(|m| m.to_string()).unwrap_or_default());
            }
            Held::Absent { rel } => {
                push_field(&mut out, "absent");
                push_field(&mut out, rel);
            }
            Held::Symlink { rel, target } => {
                push_field(&mut out, "symlink");
                push_field(&mut out, rel);
                push_field(&mut out, target);
            }
        }
    }
    out
}

/// Parse, or say why not. A store we cannot read in full is never partially
/// applied: the caller keeps it and tells the user where it is.
fn parse_index(raw: &[u8]) -> Result<Vec<Held>, String> {
    let mut fields: Vec<&[u8]> = raw.split(|b| *b == 0).collect();
    // Every field is NUL-TERMINATED, so a well-formed store leaves exactly one
    // trailing empty slice. Any other empty field is a truncation.
    if fields.last().is_some_and(|f| f.is_empty()) {
        fields.pop();
    }
    let text = |f: &[u8]| -> Result<String, String> {
        std::str::from_utf8(f)
            .map(|s| s.to_string())
            .map_err(|_| "held store index is not valid UTF-8".to_string())
    };

    let header = fields
        .first()
        .ok_or_else(|| "held store index is empty".to_string())?;
    if *header != FORMAT.as_bytes() {
        return Err(format!(
            "held store index is not {FORMAT} — refusing to guess at its shape"
        ));
    }

    let mut out = Vec::new();
    let mut i = 1;
    let take = |i: &mut usize, what: &str| -> Result<String, String> {
        let f = fields
            .get(*i)
            .ok_or_else(|| format!("held store index ends mid-record, expected {what}"))?;
        *i += 1;
        text(f)
    };
    while i < fields.len() {
        let kind = take(&mut i, "a record kind")?;
        match kind.as_str() {
            "file" => {
                let rel = take(&mut i, "a path")?;
                let mode = take(&mut i, "a mode")?;
                let mode = if mode.is_empty() {
                    None
                } else {
                    Some(
                        mode.parse::<u32>()
                            .map_err(|_| format!("held store index has a bad mode: {mode:?}"))?,
                    )
                };
                out.push(Held::Modified { rel, mode });
            }
            "absent" => out.push(Held::Absent {
                rel: take(&mut i, "a path")?,
            }),
            "symlink" => {
                let rel = take(&mut i, "a path")?;
                let target = take(&mut i, "a link target")?;
                out.push(Held::Symlink { rel, target });
            }
            other => return Err(format!("held store index has an unknown record: {other:?}")),
        }
    }
    Ok(out)
}

#[cfg(unix)]
fn mode_of(meta: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(meta.permissions().mode())
}

#[cfg(not(unix))]
fn mode_of(_meta: &std::fs::Metadata) -> Option<u32> {
    None // No execute bit to lose.
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

/// Unstaged changes, set aside for the duration of a stage.
pub struct StagedOnly {
    held: bool,
}

/// Why COPIES and not `git stash --keep-index`, and not a patch either.
///
/// Saving is the easy half; restoring is the whole problem.
///
/// `stash pop` MERGES into a tree that already holds the staged content, so it
/// writes conflict markers into the user's file. Measured on the first attempt.
///
/// `git diff` + `git apply` is deterministic on Unix and is what `pre-commit`
/// does — but it applies PATCH semantics to text, and Git for Windows converts
/// line endings by default. Measured on the second attempt: every restore test
/// failed on Windows and passed everywhere else, which is the worst possible
/// shape for the one routine in this codebase that can lose somebody's work.
///
/// So: byte-exact copies. Read the file, put it back. No patch to apply, no
/// newline policy to agree about, and binary files need no special case. It
/// costs a temporary copy of only the files that have unstaged changes.
impl StagedOnly {
    /// The three early exits, and THE ORDER IS THE DESIGN.
    ///
    /// Both of the first two used to return "nothing held" with NO OUTPUT,
    /// which meant the checks silently judged the working tree — the exact
    /// failure this module exists to prevent, announced as a clean run.
    /// `docs/index-fidelity-and-run-modes.md` §1 says conflicted paths ABORT
    /// the stage; they did not.
    ///
    /// 1. **Conflicts first**, and they are an `Err`: a conflicted path has no
    ///    staged and unstaged halves to separate, so there is nothing this can
    ///    honestly do. `pre_commit` turns the `Err` into a printed message and
    ///    `Verdict::Block`. Safe by construction: git itself refuses a commit
    ///    with unmerged entries, so nothing that would have succeeded now
    ///    fails.
    /// 2. **Nothing unstaged** ⇒ nothing held, SILENTLY. The common case must
    ///    stay free, and it is not a degraded run: there is no unstaged content
    ///    for a check to be confused by.
    /// 3. **Mid-operation LAST**, and only when there IS unstaged content, with
    ///    one printed line. Checked after the conflict test rather than before
    ///    it, deliberately: the other order made the ordinary conflicted-merge
    ///    case take the mid-operation branch and warn instead of aborting.
    ///    Checked after the emptiness test because with a clean tree fidelity
    ///    is not actually off, and `registry.rs:64-67` calls out on purpose
    ///    that a resolution commit still runs its checks.
    pub fn enter() -> Result<StagedOnly, String> {
        // Conflicted paths cannot be split into staged and unstaged halves.
        let conflicted: Vec<String> =
            crate::git::stdout_paths(&["diff", "--name-only", "--diff-filter=U"])
                .unwrap_or_default();
        if !conflicted.is_empty() {
            return Err(format!(
                "{} unmerged paths — resolve and stage them first:\n    {}",
                error_sign(),
                conflicted.join("\n    ")
            ));
        }
        // Tracked files only: an untracked file is not part of this commit and
        // moving it would surprise everyone.
        let changed: Vec<String> =
            crate::git::stdout_paths(&["diff", "--name-only"]).unwrap_or_default();
        if changed.is_empty() {
            return Ok(StagedOnly { held: false });
        }
        // A tree mid-merge is already holding work that is not the author's,
        // so taking a copy of it would be the wrong instrument entirely — but
        // the checks are then reading the tree, and that has to be said.
        let in_progress = crate::git_states_in_progress();
        if !in_progress.is_empty() {
            println!(
                "{} {} in progress — checks see the working tree, not just the index",
                warning_sign(),
                in_progress
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(" and ")
            );
            return Ok(StagedOnly { held: false });
        }

        let Some(store) = store_dir() else {
            return Ok(StagedOnly { held: false });
        };
        // A stash left behind by an interrupted restore still holds work
        // nobody has recovered — point 5 in the module doc. Clearing it to
        // make room for a new one would be exactly the loss this module
        // exists to prevent, so refuse instead of silently deleting it.
        if has_contents(&store) {
            return Err(format!(
                "{} a previous stash was left behind at {} — recover it with \
                 `amont restore`, then retry",
                error_sign(),
                store.display()
            ));
        }
        let root = crate::hooks::common::repo_root();
        let root = Path::new(&root);

        // Explicitly, because a run whose every entry is a deletion or a
        // symlink writes no payload file and so would otherwise never create
        // the directory the index goes in.
        if std::fs::create_dir_all(&store).is_err() {
            return Err(held_nothing(&store));
        }

        let mut entries: Vec<Held> = Vec::with_capacity(changed.len());
        for rel in &changed {
            // git gave us this path, but it is repo-controlled and it is about
            // to be joined onto a root, so it is checked like anything else.
            let Some(relp) = safe_rel(rel) else {
                return Err(held_nothing(&store));
            };
            let from = root.join(&relp);

            // A symlink must be read as a link, not opened: `fs::read` follows
            // it, which copies the TARGET's bytes instead of the link, and
            // silently mistakes a dangling link (a normal mid-edit state) for
            // a deleted file — restore would then delete the link rather than
            // put it back.
            match std::fs::symlink_metadata(&from) {
                Ok(meta) if meta.file_type().is_symlink() => match std::fs::read_link(&from) {
                    Ok(target) => entries.push(Held::Symlink {
                        rel: rel.clone(),
                        target: target.to_string_lossy().into_owned(),
                    }),
                    Err(_) => return Err(held_nothing(&store)),
                },
                Ok(meta) => {
                    // It exists, so we must be able to reproduce it. Failing to
                    // read it now means failing to restore it later, and the
                    // one thing this module may not do is park work it cannot
                    // put back.
                    let Ok(bytes) = std::fs::read(&from) else {
                        return Err(held_nothing(&store));
                    };
                    let to = store.join(FILES).join(&relp);
                    if let Some(parent) = to.parent() {
                        if std::fs::create_dir_all(parent).is_err() {
                            return Err(held_nothing(&store));
                        }
                    }
                    if std::fs::write(&to, bytes).is_err() {
                        return Err(held_nothing(&store));
                    }
                    entries.push(Held::Modified {
                        rel: rel.clone(),
                        mode: mode_of(&meta),
                    });
                }
                // Deleted in the tree but not staged: record the absence, so
                // the restore deletes it again rather than resurrecting it.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    entries.push(Held::Absent { rel: rel.clone() })
                }
                // Something else is wrong with the path. Do not guess.
                Err(_) => return Err(held_nothing(&store)),
            }
        }

        // The index lands BEFORE the tree is touched, so a store can never be
        // half-described while the working tree has already moved.
        if std::fs::write(store.join(INDEX), encode_index(&entries)).is_err() {
            return Err(held_nothing(&store));
        }

        // Tree := index, for exactly the paths being parked. Scoped, not
        // `checkout -- .`: the repo-wide form makes git traverse and lstat
        // the entire working tree to reset the handful of files just
        // enumerated — seconds on a large tree, on every dirty commit — and
        // the pathspec list is BY CONSTRUCTION the complete set of tracked
        // paths that differ. `:(literal)` per path, because a path is data
        // here and `*`/`:` are pathspec syntax. Locked against a concurrent
        // `restore()` — see `ENTER_LOCK`.
        {
            let _guard = ENTER_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            let mut spec = String::new();
            for p in &changed {
                spec.push_str(":(literal)");
                spec.push_str(p);
                spec.push('\0');
            }
            if crate::git::stdout_piped_raw(
                &["checkout", "--pathspec-from-file=-", "--pathspec-file-nul"],
                &spec,
            )
            .is_none()
            {
                let _ = std::fs::remove_dir_all(&store);
                return Err(format!(
                    "{} could not set the unstaged changes aside; nothing was changed",
                    error_sign()
                ));
            }
            // What the checks will see, recorded so `put_back` can tell an
            // editor save made MID-CHECK apart from its own checkout — the
            // clobber this module used to commit silently. Best-effort: a
            // failure to record only means the old unconditional restore.
            let mut expected = String::new();
            for e in &entries {
                if let Held::Modified { rel, .. } = e {
                    if let Some(relp) = safe_rel(rel) {
                        if let Ok(bytes) = std::fs::read(root.join(&relp)) {
                            expected.push_str(rel);
                            expected.push('\0');
                            expected.push_str(&format!("{:016x}", content_hash(&bytes)));
                            expected.push('\0');
                        }
                    }
                }
            }
            let _ = std::fs::write(store.join(EXPECTED), expected);
            HELD.store(true, Ordering::SeqCst);
        }
        Ok(StagedOnly { held: true })
    }

    /// Put them back. Idempotent, and safe to call from the watcher thread
    /// `install_signal_handler` starts — never from the signal handler itself.
    pub fn restore() {
        // Blocks until `enter()` has definitely finished its own checkout —
        // see `ENTER_LOCK`. The common case (no `enter()` active) is
        // uncontended.
        let _guard = ENTER_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        if !HELD.swap(false, Ordering::SeqCst) {
            return;
        }
        let Some(store) = store_dir() else {
            return;
        };
        if !store.is_dir() {
            return;
        }
        let root = crate::hooks::common::repo_root();
        match put_back(&store, Path::new(&root)) {
            Ok(()) => {
                let _ = std::fs::remove_dir_all(&store);
            }
            Err(_) => {
                // The one message in this codebase that must never be swallowed.
                eprintln!(
                    "{} YOUR UNSTAGED CHANGES COULD NOT BE PUT BACK AUTOMATICALLY.",
                    error_sign()
                );
                eprintln!("    They are safe, in: {}", store.display());
                eprintln!("    Recover them with: amont restore");
            }
        }
    }
}

/// Whether `dir` exists and holds at least one entry.
fn has_contents(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
}

fn held_nothing(store: &Path) -> String {
    let _ = std::fs::remove_dir_all(store);
    format!(
        "{} could not hold the unstaged changes aside; nothing was changed",
        error_sign()
    )
}

/// Put every held path back over the tree.
///
/// Dispatches on the index: a store written by this version describes itself,
/// and one left behind by an older binary mid-upgrade is still recoverable
/// through [`put_back_legacy`]. Nobody's work is stranded by the format change.
fn put_back(store: &Path, root: &Path) -> std::io::Result<()> {
    match std::fs::read(store.join(INDEX)) {
        Ok(raw) => {
            let entries = parse_index(&raw).map_err(std::io::Error::other)?;
            put_back_v1(store, root, &entries)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => put_back_legacy(store, root),
        Err(e) => Err(e),
    }
}

fn put_back_v1(store: &Path, root: &Path, entries: &[Held]) -> std::io::Result<()> {
    let expected = read_expected(store);
    // The guard stands down when fixing is ON: a repo-wide fixer (`prettier
    // --write .`) legitimately rewrites held files mid-run, and telling its
    // writes apart from an editor save is not possible from here. `amont.fix`
    // is an explicit opt-in whose documented contract has always been "the
    // tree returns to your unstaged version"; the guard protects the default
    // population, which is everyone else.
    let guard = !crate::hooks::common::fixing_requested();
    let mut preserved: Vec<(String, std::path::PathBuf)> = Vec::new();
    let escapes = |rel: &str| {
        std::io::Error::other(format!(
            "held store names a path outside the worktree: {rel:?}"
        ))
    };
    for e in entries {
        match e {
            Held::Absent { rel } => {
                let relp = safe_rel(rel).ok_or_else(|| escapes(rel))?;
                // It was deleted in the tree; `checkout` brought it back.
                let _ = std::fs::remove_file(root.join(relp));
            }
            Held::Symlink { rel, target } => {
                let relp = safe_rel(rel).ok_or_else(|| escapes(rel))?;
                let link = root.join(relp);
                // `checkout` put the staged symlink there; replace it, don't
                // merge with it.
                let _ = std::fs::remove_file(&link);
                create_symlink(target, &link)?;
            }
            Held::Modified { rel, mode } => {
                let relp = safe_rel(rel).ok_or_else(|| escapes(rel))?;
                let target = root.join(&relp);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let held = std::fs::read(store.join(FILES).join(&relp))?;
                // The clobber guard. If the file on disk no longer holds what
                // `enter()`'s checkout put there, somebody wrote it while the
                // checks ran — an editor save is WORK, and this write used to
                // destroy it silently. Keep the newer file; park the held
                // copy where the warning says.
                if guard
                    && expected.get(rel.as_str()).is_some_and(|want| {
                        std::fs::read(&target)
                            .is_ok_and(|now| content_hash(&now) != *want && now != held)
                    })
                {
                    let kept = preserve(store, &relp, &held)?;
                    preserved.push((rel.clone(), kept));
                    continue;
                }
                std::fs::write(&target, held)?;
                // AFTER the write: a recorded mode without `u+w` applied first
                // would make writing the content fail.
                if let Some(m) = mode {
                    set_mode(&target, *m)?;
                }
            }
        }
    }
    if !preserved.is_empty() {
        eprintln!(
            "{} {} file(s) changed while the checks ran — the newer content was KEPT.",
            crate::ui::warning_sign(),
            preserved.len()
        );
        eprintln!("    The unstaged version each held before the commit is parked at:");
        for (rel, kept) in &preserved {
            eprintln!("      {} -> {}", crate::ui::sanitize(rel), kept.display());
        }
        eprintln!("    Compare and merge by hand; the parked copies are yours to delete.");
    }
    Ok(())
}

/// Park `held` beside the store, never inside it — the store is deleted on a
/// clean restore and this must survive to be read. A previous incident's copy
/// is not overwritten; the name grows a counter instead.
fn preserve(store: &Path, relp: &Path, held: &[u8]) -> std::io::Result<std::path::PathBuf> {
    let base = store
        .parent()
        .map(|p| p.join(PRESERVED))
        .ok_or_else(|| std::io::Error::other("store has no parent"))?;
    let mut to = base.join(relp);
    let mut n = 0;
    while to.exists() {
        n += 1;
        to = base.join(relp).with_extension(format!("kept-{n}"));
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&to, held)?;
    Ok(to)
}

/// The `EXPECTED` record, or empty when it is absent or unreadable — which
/// simply disables the clobber guard, the pre-existing behaviour.
fn read_expected(store: &Path) -> std::collections::HashMap<String, u64> {
    let Ok(raw) = std::fs::read_to_string(store.join(EXPECTED)) else {
        return Default::default();
    };
    let mut map = std::collections::HashMap::new();
    let mut it = raw.split('\0');
    while let (Some(rel), Some(hex)) = (it.next(), it.next()) {
        if rel.is_empty() {
            break;
        }
        if let Ok(h) = u64::from_str_radix(hex, 16) {
            map.insert(rel.to_string(), h);
        }
    }
    map
}

/// A store from before the index existed, where the kind of each entry was
/// encoded in its file NAME.
///
/// Kept only so that upgrading mid-hold cannot strand somebody's work. The
/// ambiguity that made this format unsafe — a repository may contain files
/// called `x.amont-absent` — is not fixable at recovery time: by the time we
/// are reading it, the statement and the payload are already indistinguishable.
/// This is a best-effort read of a format we no longer write.
fn put_back_legacy(store: &Path, root: &Path) -> std::io::Result<()> {
    for entry in walk(store)? {
        let rel = entry.strip_prefix(store).unwrap_or(&entry).to_path_buf();
        let rel_str = rel.to_string_lossy().to_string();
        let escapes = || {
            std::io::Error::other(format!(
                "held store names a path outside the worktree: {rel_str:?}"
            ))
        };
        if let Some(original) = rel_str.strip_suffix(".amont-absent") {
            let relp = safe_rel(original).ok_or_else(escapes)?;
            let _ = std::fs::remove_file(root.join(relp));
            continue;
        }
        if let Some(original) = rel_str.strip_suffix(".amont-symlink") {
            let link_target = std::fs::read_to_string(&entry)?;
            let relp = safe_rel(original).ok_or_else(escapes)?;
            let link_path = root.join(relp);
            let _ = std::fs::remove_file(&link_path);
            create_symlink(&link_target, &link_path)?;
            continue;
        }
        let relp = safe_rel(&rel_str).ok_or_else(escapes)?;
        let target = root.join(&relp);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, std::fs::read(&entry)?)?;
    }
    Ok(())
}

#[cfg(unix)]
fn create_symlink(target: &str, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// Windows distinguishes file and directory symlinks at creation time. The
/// target usually still exists (it was the staged content `checkout` left
/// behind, untouched by this whole dance), so ask it; a dangling link falls
/// back to `symlink_file`, the more common case.
#[cfg(windows)]
fn create_symlink(target: &str, link: &Path) -> std::io::Result<()> {
    let resolved = link
        .parent()
        .map(|parent| parent.join(target))
        .unwrap_or_else(|| Path::new(target).to_path_buf());
    if resolved.is_dir() {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}

#[cfg(not(any(unix, windows)))]
fn create_symlink(target: &str, link: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other(format!(
        "no symlink support on this platform: {} -> {target}",
        link.display()
    )))
}

fn walk(dir: &Path) -> std::io::Result<Vec<std::path::PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            out.extend(walk(&path)?);
        } else {
            out.push(path);
        }
    }
    Ok(out)
}

/// Where the store lives, agreeing with [`StagedOnly::restore`] and
/// [`restore_command`] BY CONSTRUCTION — all three call this one function
/// rather than each asking git their own way. That used to be
/// `hooks_dir.parent()` in `enter()` against `git rev-parse --git-dir`
/// everywhere else: correct for the main worktree, where `.git/hooks`'s
/// parent IS `$GIT_DIR`, but wrong for a LINKED worktree, where hooks
/// dispatch from the COMMON directory's shared `hooks/` while `--git-dir`
/// names the worktree's own PRIVATE gitdir. The mismatch parked files in one
/// directory and looked for them in the other — silently, since a missing
/// store reads as "nothing to do" — which is how a real commit in a real
/// worktree lost real unstaged content. Sharing one function instead of one
/// convention makes that class of drift impossible rather than merely fixed.
fn store_dir() -> Option<std::path::PathBuf> {
    let dir = crate::git::stdout(&["rev-parse", "--git-dir"])?;
    Some(Path::new(&dir).join(STORE))
}

impl Drop for StagedOnly {
    fn drop(&mut self) {
        if self.held {
            StagedOnly::restore();
        }
    }
}

/// Put back files this tool parked, from a later invocation.
///
/// For when even the signal handler was interrupted.
pub fn restore_command() -> Result<(), String> {
    let store = store_dir().ok_or_else(|| "not inside a git repository".to_string())?;
    if !store.is_dir() {
        println!("{} nothing of ours to restore", warning_sign());
        return Ok(());
    }
    // `store_dir()` already established we are in a repository, so this cannot
    // fail here — asked the checked way anyway, because `repo_root()`'s "."
    // would make `put_back` write held files relative to the current directory
    // rather than the work tree, and a restore that lands in the wrong place is
    // the failure this whole module exists to prevent.
    let root = crate::hooks::common::repo_root_checked()?;
    put_back(&store, Path::new(&root))
        .map_err(|e| format!("could not put {} back: {e}", store.display()))?;
    let _ = std::fs::remove_dir_all(&store);
    println!("restored your unstaged changes");
    Ok(())
}

/// Restore before dying on a signal.
///
/// `Drop` does not run when the process is killed, and Ctrl-C during a slow
/// pre-commit is the most likely way to reach an orphaned stash. Installed only
/// when a stash is actually held.
///
/// The handler itself does almost nothing. `restore()` runs `git`, walks the
/// filesystem, writes files and prints — none of that is async-signal-safe,
/// and running it IN the handler risks a deadlock: if the thread the signal
/// interrupted already held a lock the handler's own code would then wait on
/// forever (the allocator's, or stdio's), the process hangs instead of
/// exiting, which is worse than either failure `restore` exists to prevent.
///
/// So the handler only records which signal arrived and writes one byte down
/// a pipe — both on POSIX's async-signal-safe list — and a plain background
/// thread, blocked reading that pipe, does the actual restore once it wakes,
/// in ordinary thread context where none of those restrictions apply. This is
/// the standard "self-pipe" pattern for getting work out of a signal handler.
#[cfg(unix)]
pub fn install_signal_handler() {
    let mut fds = [-1i32; 2];
    if unsafe { libc_pipe(fds.as_mut_ptr()) } != 0 {
        // No pipe, no watcher, no handler: Ctrl-C falls back to the default
        // action. Losing the safety net is better than building it on a
        // primitive that just failed us.
        return;
    }
    let (read_fd, write_fd) = (fds[0], fds[1]);
    SIGNAL_PIPE_WRITE.store(write_fd, Ordering::SeqCst);

    std::thread::spawn(move || loop {
        let mut byte = 0u8;
        let n = unsafe { libc_read(read_fd, &mut byte as *mut u8, 1) };
        if n <= 0 {
            return; // pipe closed, or a real error: nothing left to watch for
        }
        StagedOnly::restore();
        // Re-raise with the default handler so the exit status is honest
        // about having been killed.
        let sig = PENDING_SIGNAL.load(Ordering::SeqCst);
        if sig != 0 {
            unsafe {
                libc_signal(sig, 0); // SIG_DFL
                libc_raise(sig);
            }
        }
    });

    extern "C" fn on_signal(sig: i32) {
        PENDING_SIGNAL.store(sig, Ordering::SeqCst);
        let fd = SIGNAL_PIPE_WRITE.load(Ordering::SeqCst);
        if fd >= 0 {
            let byte = 1u8;
            unsafe {
                libc_write(fd, &byte as *const u8, 1);
            }
        }
    }
    // Numeric literals rather than a `libc` import: this module deliberately
    // ships dependency-free (see the `extern` block below and
    // `scripts/check-no-deps.sh`), and unlike `sigset_t`-based APIs these four
    // numbers are fixed by POSIX on every platform this runs on.
    //
    // SIGHUP was missing and is at least as likely as Ctrl-C: it is the
    // terminal-closed and SSH-connection-dropped case, and a pre-commit
    // interrupted that way orphaned the held store with nothing said. SIGQUIT
    // is the Ctrl-\ sibling of SIGINT.
    const SIGHUP: i32 = 1;
    const SIGINT: i32 = 2;
    const SIGQUIT: i32 = 3;
    const SIGTERM: i32 = 15;
    unsafe {
        for sig in [SIGHUP, SIGINT, SIGQUIT, SIGTERM] {
            libc_signal(sig, on_signal as *const () as usize);
        }
    }
}

/// The Windows twin: `SetConsoleCtrlHandler` instead of `signal(2)`. The
/// system already runs the handler on a fresh thread, so there is no
/// self-pipe dance — `restore()` is called directly (its `ENTER_LOCK`
/// synchronisation against a mid-`enter()` signal is the same on both
/// platforms), and returning FALSE hands the event to the default handler,
/// which terminates the process: the exit stays honest about being killed.
/// Before this, Ctrl-C mid-check on Windows orphaned the held patch and the
/// recovery was "your next commit is blocked, run `amont restore`" — the
/// platform with the fewest git-hooks veterans got the scariest failure.
///
/// Raw FFI rather than a crate for the same reason as the unix externs
/// below: this binary ships dependency-free, and this is one kernel32 call
/// with a stable signature. Covers Ctrl-C, Ctrl-Break, and console-closed.
#[cfg(windows)]
pub fn install_signal_handler() {
    extern "system" fn on_ctrl(_event: u32) -> i32 {
        StagedOnly::restore();
        0 // FALSE: pass to the default handler, which terminates
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn SetConsoleCtrlHandler(handler: extern "system" fn(u32) -> i32, add: i32) -> i32;
    }
    unsafe {
        SetConsoleCtrlHandler(on_ctrl, 1);
    }
}

#[cfg(not(any(unix, windows)))]
pub fn install_signal_handler() {}

/// The write end of the self-pipe a signal handler wakes the watcher thread
/// through. `-1` until `install_signal_handler` has run.
#[cfg(unix)]
static SIGNAL_PIPE_WRITE: AtomicI32 = AtomicI32::new(-1);

/// Which signal woke the watcher, so it can re-raise the right one.
#[cfg(unix)]
static PENDING_SIGNAL: AtomicI32 = AtomicI32::new(0);

// Externs rather than a dependency: `scripts/check-no-deps.sh` keeps this
// binary crate-free, and these are five libc calls with stable signatures —
// `pipe`/`read`/`write` need nothing beyond plain integers and byte pointers,
// so unlike `sigset_t`-based APIs there is no opaque, platform-varying struct
// layout to get wrong by hand.
#[cfg(unix)]
extern "C" {
    #[link_name = "signal"]
    fn libc_signal_raw(sig: i32, handler: usize) -> usize;
    #[link_name = "raise"]
    fn libc_raise_raw(sig: i32) -> i32;
    #[link_name = "pipe"]
    fn libc_pipe_raw(fds: *mut i32) -> i32;
    #[link_name = "read"]
    fn libc_read_raw(fd: i32, buf: *mut u8, count: usize) -> isize;
    #[link_name = "write"]
    fn libc_write_raw(fd: i32, buf: *const u8, count: usize) -> isize;
}

#[cfg(unix)]
unsafe fn libc_signal(sig: i32, handler: usize) {
    unsafe {
        libc_signal_raw(sig, handler);
    }
}

#[cfg(unix)]
unsafe fn libc_raise(sig: i32) {
    unsafe {
        libc_raise_raw(sig);
    }
}

#[cfg(unix)]
unsafe fn libc_pipe(fds: *mut i32) -> i32 {
    unsafe { libc_pipe_raw(fds) }
}

#[cfg(unix)]
unsafe fn libc_read(fd: i32, buf: *mut u8, count: usize) -> isize {
    unsafe { libc_read_raw(fd, buf, count) }
}

#[cfg(unix)]
unsafe fn libc_write(fd: i32, buf: *const u8, count: usize) -> isize {
    unsafe { libc_write_raw(fd, buf, count) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modified(rel: &str) -> Held {
        Held::Modified {
            rel: rel.to_string(),
            mode: Some(0o100_644),
        }
    }

    /// The store has to survive names a repository is allowed to choose, and
    /// those include every character a filename may hold except NUL — which is
    /// exactly why the index is NUL-delimited rather than line-based.
    #[test]
    fn the_index_round_trips_hostile_names() {
        let entries = vec![
            modified("a\nb.txt"),
            modified("a\tb"),
            modified("a\\b"),
            modified("é.json"),
            modified("quote\"and'apostrophe"),
            // The names that used to BE the metadata.
            modified("notes.amont-absent"),
            Held::Absent {
                rel: "gone.amont-symlink".to_string(),
            },
            Held::Symlink {
                rel: "link\nname".to_string(),
                target: "target\nwith\nnewlines".to_string(),
            },
        ];
        let raw = encode_index(&entries);
        assert_eq!(parse_index(&raw).expect("round trip"), entries);
    }

    #[test]
    fn an_empty_index_round_trips() {
        let raw = encode_index(&[]);
        assert_eq!(parse_index(&raw).expect("round trip"), Vec::<Held>::new());
    }

    /// A store we do not recognise is never half-understood.
    #[test]
    fn parse_index_rejects_a_foreign_header() {
        let err = parse_index(b"amont-held-v99\0file\0a\0\0").expect_err("must refuse");
        assert!(err.contains("amont-held-v1"), "{err}");
    }

    #[test]
    fn parse_index_rejects_a_truncated_record() {
        // A `file` record promises a path and a mode.
        let raw = b"amont-held-v1\0file\0a.txt\0";
        let err = parse_index(raw).expect_err("must refuse");
        assert!(err.contains("ends mid-record"), "{err}");
    }

    #[test]
    fn parse_index_rejects_an_unknown_record_kind() {
        let raw = b"amont-held-v1\0execute\0rm -rf\0";
        assert!(parse_index(raw).is_err());
    }

    /// The guard that makes the store unable to name anything outside the
    /// worktree, applied on the way out as well as in.
    #[test]
    fn safe_rel_refuses_anything_that_leaves_the_tree() {
        for bad in [
            "",
            "..",
            "../x",
            "a/../../b",
            "/etc/passwd",
            "/",
            ".",
            "./a",
        ] {
            assert!(safe_rel(bad).is_none(), "{bad:?} must be refused");
        }
        for good in ["a", "a/b.txt", "é.json", "a\nb", "notes.amont-absent"] {
            assert!(safe_rel(good).is_some(), "{good:?} should be allowed");
        }
    }

    /// Windows drive-qualified paths are absolute even when they do not start
    /// with a separator, and `Path::join` honours them.
    #[cfg(windows)]
    #[test]
    fn safe_rel_refuses_a_drive_prefix() {
        assert!(safe_rel("C:\\Windows\\System32").is_none());
        assert!(safe_rel("C:x").is_none());
    }
}
