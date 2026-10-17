//! The single owner of every "is this file ours, and may we touch it?" answer.
//!
//! Three separate places used to answer that question, each with its own
//! one-liner, and every one of them failed OPEN — an error, an odd file type or
//! a git that would not answer all collapsed into `false`, and `false` meant
//! "not foreign", which meant "go ahead and overwrite it". The three:
//!
//! ```text
//! install::foreign_hooks   read_to_string(..).map(|t| !is_our_shim(&t)).unwrap_or(false)
//! fleet::scan::is_ours     read_to_string(..).map(|s| is_our_shim(&s)).unwrap_or(false)
//! fleet::fix::plan         read_to_string(..).map(|t| !is_our_shim(&t)).unwrap_or(false)
//! ```
//!
//! `read_to_string` fails on any file that is not valid UTF-8. A compiled hook
//! — somebody's Go binary at `.git/hooks/pre-commit`, which is a perfectly
//! ordinary thing to have — reads back `Err(InvalidData)`, and `unwrap_or(false)`
//! turned that into "not foreign". `amont install` then wrote a shim straight
//! over it, with no `--force`, no refusal, and no message. That is the same
//! class of failure as the two incidents that overwrote tracked source files,
//! and it had no guard at all.
//!
//! So everything here fails CLOSED. When this module cannot establish that a
//! path is ours and safe, it says so with a reason, and the caller refuses.
//! Refusing is cheap; the alternative has destroyed somebody's work three times.
//!
//! ## Why the LINK is the thing, never its target
//!
//! `std::fs::write` and `std::fs::OpenOptions` FOLLOW symlinks: opening
//! `.git/hooks/pre-commit` when that is a link to `../../devhooks/pre-commit`
//! truncates and rewrites `devhooks/pre-commit`, a tracked file in the working
//! tree. That is the verified bug this module exists to end: the guard checked
//! the link path — untracked, in `.git`, nothing alarming about it — and the
//! write landed somewhere else entirely.
//!
//! `std::fs::rename` does NOT follow symlinks; it replaces the link. So every
//! write in this module is staged to a sibling temporary file and renamed into
//! place. `--force` on a symlinked hook therefore means "replace the link", and
//! the file it pointed at is never opened at all. There is no code path here
//! that writes through a link, which is a stronger statement than "we check
//! first" — a check can be raced, and this cannot.
//!
//! ## Windows
//!
//! - A junction and a directory symlink both report `is_symlink()` from
//!   `symlink_metadata`, so the symlink refusal covers them. Ordinary users
//!   cannot create file symlinks without Developer Mode, which is why the
//!   symlink tests are `#[cfg(unix)]` — the CODE is not.
//! - `fs::rename` over a file another process has open fails on Windows where
//!   it would succeed on unix. That failure is REPORTED (`SwapFailure` names
//!   the path and the io error) rather than swallowed, because a hook that was
//!   not written is exactly what the caller must be told about.
//! - `nlink` is unix-only. The multiply-linked refusal below is `#[cfg(unix)]`;
//!   Windows hard links exist but std exposes no count, so that particular
//!   check simply is not made there. Every other guard applies on both.

use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

/// A line every shim carries and nothing else does.
///
/// `uninstall` needs to answer "is this file ours to delete?" and the answer
/// must not be "it is named pre-commit". A colleague's own `pre-commit` lives at
/// the same path and deleting it would be the third time this project destroyed
/// somebody's file. Marker-based on purpose rather than byte-comparing against
/// `bake(SHIM, path)`: a shim somebody hand-edited is still ours, and uninstall
/// should still take it.
pub const SHIM_MARKER: &str = "git-templates hook shim";

/// How far into a file the marker may appear. Every shim this project has ever
/// baked carries it on line 2 — verified against every revision of every file
/// under `templates/hooks/` in this repository's history, all 36 of them, from
/// the first `sh` shim to the current one. Ten lines is that fact with room to
/// spare, not a guess.
const MARKER_WINDOW: usize = 10;

/// Whether a file in `.git/hooks` is one of ours.
///
/// ANCHORED, and that is the whole point. The predicate used to be
/// `text.contains(SHIM_MARKER)` over the entire file, which claimed as ours any
/// hook that so much as mentioned the phrase — a colleague's `pre-commit`
/// carrying `# replaces the git-templates hook shim` in its header was "ours",
/// and `install --force` overwrote it while `uninstall` deleted it outright.
/// Prose about this project is not a claim of ownership over the file it is
/// written in.
///
/// Three conditions, all necessary:
///
/// - the line is a `#` COMMENT, so a heredoc, a quoted string or a `grep`
///   pattern mentioning the phrase does not count;
/// - the marker OPENS that comment — `# git-templates hook shim…` — rather than
///   appearing somewhere inside a sentence. This is the condition that actually
///   separates a shim from prose, and it is the reason the rule is stated in
///   terms of position rather than presence: `# I removed the git-templates
///   hook shim on purpose` is a note ABOUT us, and a note about us is not a
///   signature;
/// - within the first [`MARKER_WINDOW`] lines, so it is a header rather than a
///   remark somewhere in a 200-line script.
///
/// Backward compatibility is a hard requirement here and not a nicety: an
/// `uninstall` that stops recognising a shim baked two years ago leaves it
/// installed, running, and now unremovable by the tool that put it there. Every
/// one of the 36 shim revisions in this repository's history opens line 2 with
/// exactly `# git-templates hook shim`, so all three conditions hold for all of
/// them; `every_shim_form_this_project_has_ever_baked_is_still_ours` keeps that
/// true rather than assuming it.
pub fn is_our_shim(text: &str) -> bool {
    text.lines().take(MARKER_WINDOW).any(|line| {
        line.trim_start()
            .strip_prefix('#')
            .is_some_and(|body| body.trim_start().starts_with(SHIM_MARKER))
    })
}

/// Why a file at a hook path is not ours to write.
///
/// Every variant is a REASON, carried so the refusal can name it. "not ours"
/// alone sends somebody to look at a file for a difference they cannot see;
/// "not valid UTF-8" tells them it is a compiled hook in one word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForeignWhy {
    /// Readable text without our marker. Somebody wrote this on purpose.
    HandWritten,
    /// Not valid UTF-8 — a compiled binary, or text in another encoding. The
    /// case the old `unwrap_or(false)` silently overwrote.
    NotUtf8,
    /// It exists and we could not read it. Permissions, a bad mount, a
    /// disappearing network share. Never "therefore ours".
    Unreadable { why: String },
    /// A hard link with other names pointing at the same inode. Truncating it
    /// would rewrite whatever else refers to it, and we cannot see from here
    /// what that is.
    #[cfg(unix)]
    MultiplyLinked { links: u64 },
}

impl ForeignWhy {
    /// A fragment for the middle of a sentence: "… is {}".
    pub fn describe(&self) -> String {
        match self {
            ForeignWhy::HandWritten => "not one of our shims".to_string(),
            ForeignWhy::NotUtf8 => "not valid UTF-8 — a compiled hook, probably".to_string(),
            ForeignWhy::Unreadable { why } => format!("unreadable ({why})"),
            #[cfg(unix)]
            ForeignWhy::MultiplyLinked { links } => {
                format!("a hard link with {links} names — rewriting it rewrites the others")
            }
        }
    }
}

/// What is at a hook path, decided once, by [`classify`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookFile {
    /// Nothing there. The only state in which writing is unambiguously fine.
    Absent,
    /// A regular file carrying our marker.
    Ours,
    Foreign(ForeignWhy),
    /// A symlink, of any kind, pointing anywhere — including at one of our own
    /// shims. `target` is `None` when the link itself could not be read.
    Symlink {
        target: Option<PathBuf>,
    },
    /// A directory, a fifo, a socket, a device. Not something to `write` to.
    NotARegularFile,
    /// `symlink_metadata` failed with something other than "not found". We do
    /// not know what is there, which is not the same as nothing being there.
    Unknown {
        why: String,
    },
}

impl HookFile {
    /// A fragment for the middle of a sentence: "… is {}".
    pub fn describe(&self) -> String {
        match self {
            HookFile::Absent => "absent".to_string(),
            HookFile::Ours => "one of our shims".to_string(),
            HookFile::Foreign(why) => why.describe(),
            HookFile::Symlink { target: Some(t) } => format!("a symlink to {}", t.display()),
            HookFile::Symlink { target: None } => "a symlink we could not read".to_string(),
            HookFile::NotARegularFile => "not a regular file".to_string(),
            HookFile::Unknown { why } => format!("unstattable ({why})"),
        }
    }
}

/// What is at `path`. Never mutates, never follows a link, never guesses.
///
/// The order of the tests is load-bearing:
///
/// 1. `symlink_metadata` — NOT `metadata`, which follows links and would report
///    a symlink-to-a-shim as an ordinary file of ours. `NotFound` is the one
///    error that means "nothing is there"; every other error means we could not
///    look, which is [`HookFile::Unknown`] and refuses.
/// 2. symlink before file type, because a link to a regular file passes
///    `is_file()` on a `metadata` call and would sail through.
/// 3. link count before reading, because a hard-linked file is foreign whatever
///    its contents say.
/// 4. read, then UTF-8, then the marker — each failure its own named reason
///    rather than a shared `false`.
pub fn classify(path: &Path) -> HookFile {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return HookFile::Absent,
        Err(e) => return HookFile::Unknown { why: e.to_string() },
    };
    if meta.file_type().is_symlink() {
        return HookFile::Symlink {
            target: std::fs::read_link(path).ok(),
        };
    }
    if !meta.is_file() {
        return HookFile::NotARegularFile;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() > 1 {
            return HookFile::Foreign(ForeignWhy::MultiplyLinked {
                links: meta.nlink(),
            });
        }
    }
    // `read`, not `read_to_string`: the bytes decide, and a decode failure is a
    // classification (`NotUtf8`) rather than an error to swallow.
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            return HookFile::Foreign(ForeignWhy::Unreadable { why: e.to_string() });
        }
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return HookFile::Foreign(ForeignWhy::NotUtf8);
    };
    if is_our_shim(text) {
        HookFile::Ours
    } else {
        HookFile::Foreign(ForeignWhy::HandWritten)
    }
}

/// Whether git tracks a path — with "could not ask" kept distinct from "no".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tracked {
    Yes,
    No,
    /// git could not answer. NOT "no": this is the state in which every caller
    /// must refuse, because "we could not tell whether this file is somebody's
    /// tracked source" is the exact ignorance the two overwrite incidents were
    /// made of.
    Unknown {
        why: String,
    },
}

/// Ask git whether it tracks `path`.
///
/// Asked as `git -C <parent> ls-files --error-unmatch -- <file-name>`, and the
/// two halves of that are both deliberate.
///
/// **The BASENAME, not the absolute path.** git normalises a pathspec
/// LEXICALLY, but resolves its working directory PHYSICALLY. When `.git/hooks`
/// is a symlink into the working tree — the setup at the centre of both
/// incidents — handing git the absolute path `<repo>/.git/hooks/pre-commit`
/// gets `exit 1, pathspec did not match`, because no such path exists in the
/// index under that spelling. Handing it `pre-commit` from `-C
/// <repo>/.git/hooks` resolves through the link to `<repo>/devhooks/pre-commit`
/// and gets `exit 0`. Same file, same question, opposite answers: the
/// absolute-path form silently reports every hook reached through a symlinked
/// directory as untracked. (`amont-fleet`'s own `fix::is_tracked` still asks
/// the absolute-path way, and has this hole.)
///
/// **The exit code, not `status.success()`.** git distinguishes them and the
/// distinction is the whole point:
///
/// - `0` — tracked.
/// - `1` — the pathspec matched nothing in the index. Untracked. This is also
///   what a hook under a plain `.git/hooks` returns, since `.git` is outside
///   the working tree.
/// - `128` + "not a git repository" — no repository at all. Treated as
///   untracked: the test fixtures throughout this suite build hook directories
///   in bare temp dirs, and refusing those would make every one of them fail
///   for a reason unrelated to what it tests.
/// - anything else — spawn failure, `detected dubious ownership`, a broken
///   `.git`, a permissions error. [`Tracked::Unknown`], which refuses.
///
/// The `dubious ownership` case is the one worth naming out loud, because it is
/// common (a repo owned by another uid, which is every repo inside a container
/// bind mount) and because it produces a `fatal:` that a `success()` check reads
/// as a clean "no". Under the old predicate, that meant `install --force` was
/// free to write over tracked files in exactly the environments where the user
/// cannot see what happened. The refusal message names
/// `git config --global --add safe.directory <path>` because that is the fix,
/// and a refusal without one is just an obstacle.
pub fn tracked(path: &Path) -> Tracked {
    tracked_with(Path::new("git"), path)
}

/// [`tracked`], with the `git` to run named explicitly.
///
/// Split out for the tests, and for one specific reason: the branch that
/// matters most here is the one where git answers with a `fatal:` — and there
/// is no way to provoke that from a test except by owning a repository as
/// another uid or by putting a fake `git` first on `$PATH`. `std::env::set_var`
/// in a `#[test]` changes the environment of the whole PROCESS, and rust's test
/// harness runs every test in that one process, in parallel. The first attempt
/// at this test did exactly that and took eleven unrelated tests down with it,
/// all of them ones that shell out to git. Passing the program in costs one
/// parameter and cannot race anything.
fn tracked_with(git: &Path, path: &Path) -> Tracked {
    let Some(name) = path.file_name() else {
        return Tracked::Unknown {
            why: format!("{} has no file name", path.display()),
        };
    };
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let out = match Command::new(git)
        .arg("-C")
        .arg(&dir)
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(name)
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            return Tracked::Unknown {
                why: format!("could not run git: {e}"),
            };
        }
    };
    let stderr = String::from_utf8_lossy(&out.stderr);
    match out.status.code() {
        Some(0) => Tracked::Yes,
        Some(1) => Tracked::No,
        Some(128) if stderr.to_lowercase().contains("not a git repository") => Tracked::No,
        Some(code) => Tracked::Unknown {
            why: format!("git ls-files exited {code}: {}", first_line(&stderr)),
        },
        // Killed by a signal. No exit code at all, and certainly no answer.
        None => Tracked::Unknown {
            why: "git ls-files was killed before it answered".to_string(),
        },
    }
}

fn first_line(s: &str) -> String {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("no output")
        .trim()
        .to_string()
}

/// A refusal to touch a path, with the reason attached.
///
/// Carries the path rather than a name, because "commit-msg" is not enough to
/// go and look at when four directories could hold one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refuse {
    Foreign {
        path: PathBuf,
        why: ForeignWhy,
    },
    Symlink {
        path: PathBuf,
        target: Option<PathBuf>,
    },
    NotARegularFile {
        path: PathBuf,
    },
    Tracked {
        path: PathBuf,
    },
    TrackedUnknown {
        path: PathBuf,
        why: String,
    },
    Unstattable {
        path: PathBuf,
        why: String,
    },
}

impl Refuse {
    pub fn path(&self) -> &Path {
        match self {
            Refuse::Foreign { path, .. }
            | Refuse::Symlink { path, .. }
            | Refuse::NotARegularFile { path }
            | Refuse::Tracked { path }
            | Refuse::TrackedUnknown { path, .. }
            | Refuse::Unstattable { path, .. } => path,
        }
    }

    /// One line, naming the path, the reason, and — where there is one — the
    /// command that resolves it. A refusal that does not say what to do next is
    /// indistinguishable from a bug.
    pub fn explain(&self) -> String {
        match self {
            Refuse::Foreign { path, why } => {
                format!(
                    "{} is {} — leaving it alone",
                    path.display(),
                    why.describe()
                )
            }
            Refuse::Symlink {
                path,
                target: Some(t),
            } => format!(
                "{} is a symlink to {} — writing through it would rewrite that file",
                path.display(),
                t.display()
            ),
            Refuse::Symlink { path, target: None } => format!(
                "{} is a symlink we could not read — refusing to write through it",
                path.display()
            ),
            Refuse::NotARegularFile { path } => format!(
                "{} is not a regular file (a directory, a fifo, a device) — refusing",
                path.display()
            ),
            Refuse::Tracked { path } => format!(
                "{} is TRACKED by git — that is somebody's source, not our hook",
                path.display()
            ),
            Refuse::TrackedUnknown { path, why } => format!(
                "cannot tell whether {} is tracked ({why})\n    \
                 If this is a repository you own: \
                 git config --global --add safe.directory {}",
                path.display(),
                path.parent().unwrap_or(path).display()
            ),
            Refuse::Unstattable { path, why } => {
                format!("cannot look at {} ({why}) — refusing", path.display())
            }
        }
    }
}

/// May we write a hook at `path`, and what is there now?
///
/// `force` is the user saying "I looked at it". It relaxes exactly two
/// refusals — a foreign file, and a symlink — and it relaxes them into
/// REPLACING THE LINK, never into writing through it (see the module doc).
///
/// `force` does NOT relax the tracked guard, and this is the one rule in the
/// module with no exception. `--force` is a statement about `.git/hooks`, which
/// is machine-local scratch space; it is not consent to rewrite a file in the
/// working tree that git is watching. The user who typed it was thinking about
/// a hook they wrote last month, not about `devhooks/pre-commit` being on the
/// other end of a link they forgot they made. Nor does it relax
/// `TrackedUnknown`: an override that fires when we could not even ask the
/// question is an override of the guard rather than of the finding.
///
/// The tracked check runs FIRST so its refusal survives everything else — a
/// tracked symlink refuses as `Tracked`, not as a forceable `Symlink`.
pub fn guard_write(path: &Path, force: bool) -> Result<HookFile, Refuse> {
    guard_write_with(Path::new("git"), path, force)
}

/// [`guard_write`], with the `git` to ask named — see [`tracked_with`] for why
/// the seam exists at all.
fn guard_write_with(git: &Path, path: &Path, force: bool) -> Result<HookFile, Refuse> {
    match tracked_with(git, path) {
        Tracked::No => {}
        Tracked::Yes => {
            return Err(Refuse::Tracked {
                path: path.to_path_buf(),
            });
        }
        Tracked::Unknown { why } => {
            return Err(Refuse::TrackedUnknown {
                path: path.to_path_buf(),
                why,
            });
        }
    }
    let what = classify(path);
    match &what {
        HookFile::Absent | HookFile::Ours => Ok(what),
        HookFile::Foreign(why) => {
            if force {
                Ok(what)
            } else {
                Err(Refuse::Foreign {
                    path: path.to_path_buf(),
                    why: why.clone(),
                })
            }
        }
        HookFile::Symlink { target } => {
            if force {
                Ok(what)
            } else {
                Err(Refuse::Symlink {
                    path: path.to_path_buf(),
                    target: target.clone(),
                })
            }
        }
        // Neither of these is forceable, deliberately. `--force` means "replace
        // the hook that is there"; a directory or a device at a hook path is
        // not a hook that is there, it is a sign that something else is going
        // on, and renaming over it either fails or destroys something we cannot
        // describe. Refusing costs one `rm`.
        HookFile::NotARegularFile => Err(Refuse::NotARegularFile {
            path: path.to_path_buf(),
        }),
        HookFile::Unknown { why } => Err(Refuse::Unstattable {
            path: path.to_path_buf(),
            why: why.clone(),
        }),
    }
}

/// May we remove the hook at `path`?
///
/// `expect_ours` is what separates `uninstall` (which removes only files
/// carrying our marker) from a caller that has already established ownership
/// some other way. With it set, anything that is not [`HookFile::Ours`] is
/// refused with its reason — including a symlink that points AT one of our
/// shims, because deleting the link and deleting the shim are different acts
/// and only one of them was asked for.
///
/// [`HookFile::Absent`] is `Ok`: there is nothing to remove and nothing to
/// refuse, and `uninstall` run twice must not be an error.
///
/// The tracked guard applies here exactly as it does to writes, and for a
/// sharper reason: `remove_file` on a tracked path deletes work.
pub fn guard_remove(path: &Path, expect_ours: bool) -> Result<(), Refuse> {
    let what = classify(path);
    if what == HookFile::Absent {
        return Ok(());
    }
    match tracked(path) {
        Tracked::No => {}
        Tracked::Yes => {
            return Err(Refuse::Tracked {
                path: path.to_path_buf(),
            });
        }
        Tracked::Unknown { why } => {
            return Err(Refuse::TrackedUnknown {
                path: path.to_path_buf(),
                why,
            });
        }
    }
    if !expect_ours {
        return Ok(());
    }
    match what {
        HookFile::Ours | HookFile::Absent => Ok(()),
        HookFile::Foreign(why) => Err(Refuse::Foreign {
            path: path.to_path_buf(),
            why,
        }),
        HookFile::Symlink { target } => Err(Refuse::Symlink {
            path: path.to_path_buf(),
            target,
        }),
        HookFile::NotARegularFile => Err(Refuse::NotARegularFile {
            path: path.to_path_buf(),
        }),
        HookFile::Unknown { why } => Err(Refuse::Unstattable {
            path: path.to_path_buf(),
            why,
        }),
    }
}

/// A body written to a sibling temporary file, not yet in place.
///
/// The two-phase shape exists so a multi-file install is all-or-nothing at the
/// only point where that is achievable. Guarding four paths and then writing
/// four files leaves a window in which the third write fails and the repository
/// holds two new dispatchers and two old ones — the state `bake_repo_hooks`'s
/// own comment claims to prevent and did not. Staging all four first moves
/// every failure that can be anticipated (no space, no permission, a read-only
/// directory) BEFORE the first destination is touched.
///
/// It is not a transaction and does not pretend to be: `commit_all` can still
/// fail on its third rename. What it gives is that the failure is reported with
/// the exact list of what landed and what did not, instead of a count.
#[derive(Debug)]
pub struct Staged {
    dest: PathBuf,
    tmp: PathBuf,
    committed: bool,
}

impl Staged {
    pub fn dest(&self) -> &Path {
        &self.dest
    }
    pub fn tmp(&self) -> &Path {
        &self.tmp
    }
}

/// A staged file that never landed is litter in somebody's `.git/hooks`, where
/// it is both confusing and — being executable and named like a hook — worth
/// not leaving. `Drop` runs on the early return from `commit_all` and on any
/// `?` between `stage` and `commit_all`.
impl Drop for Staged {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.tmp);
        }
    }
}

/// Write `body` to a sibling of `dest`, ready to be renamed into place.
///
/// A SIBLING, in the destination's own directory, which is not a detail:
/// `fs::rename` across filesystems fails with `EXDEV`, and `.git` is a mount
/// point often enough (a bind mount, a container volume, a separate partition
/// for a large repo) that staging in `/tmp` would turn a rename that cannot
/// fail into one that fails on exactly the machines hardest to reproduce.
///
/// The temporary name carries the pid so two installs racing in the same
/// repository do not stage over each other, and starts with a dot so it does
/// not look like a hook to anyone reading the directory.
pub fn stage(dest: &Path, body: &str, exec: bool) -> io::Result<Staged> {
    let dir = match dest.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "hook".to_string());
    let tmp = dir.join(format!(".amont-tmp-{}-{name}", std::process::id()));
    // Remove first: a leftover from a killed run could be a symlink, and
    // `fs::write` would follow it.
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, body)?;
    set_mode(&tmp, exec)?;
    Ok(Staged {
        dest: dest.to_path_buf(),
        tmp,
        committed: false,
    })
}

/// Everything that did and did not happen when a swap went wrong.
///
/// `landed` and `not_written` are both present because either alone is a lie by
/// omission. "3 of 4 written" tells somebody they have a problem; naming which
/// three tells them what their repository currently does on commit.
#[derive(Debug)]
pub struct SwapFailure {
    pub landed: Vec<PathBuf>,
    pub not_written: Vec<PathBuf>,
    pub at: PathBuf,
    pub error: io::Error,
}

impl std::fmt::Display for SwapFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "cannot put {} in place: {}",
            self.at.display(),
            self.error
        )?;
        writeln!(f, "    written:     {}", show(&self.landed))?;
        write!(f, "    NOT written: {}", show(&self.not_written))
    }
}

fn show(paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return "(none)".to_string();
    }
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Rename every staged file into place, stopping at the first failure.
///
/// `fs::rename` REPLACES a symlink at the destination rather than following it,
/// which is the property the whole staging dance is for. Same directory, so
/// `EXDEV` cannot arise.
pub fn commit_all(mut staged: Vec<Staged>) -> Result<Vec<PathBuf>, SwapFailure> {
    let mut landed: Vec<PathBuf> = Vec::new();
    for i in 0..staged.len() {
        match std::fs::rename(&staged[i].tmp, &staged[i].dest) {
            Ok(()) => {
                staged[i].committed = true;
                landed.push(staged[i].dest.clone());
            }
            Err(error) => {
                return Err(SwapFailure {
                    landed,
                    not_written: staged[i..].iter().map(|s| s.dest.clone()).collect(),
                    at: staged[i].dest.clone(),
                    error,
                });
            }
        }
    }
    Ok(landed)
}

/// Remove a file, treating "already gone" as success.
///
/// `remove_file` on unix unlinks a symlink rather than its target, which is the
/// behaviour `uninstall --force` needs and the reason this is not `remove_dir_all`
/// or anything cleverer.
pub fn remove_regular(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(unix)]
fn set_mode(p: &Path, exec: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if exec { 0o755 } else { 0o644 };
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_p: &Path, _exec: bool) -> io::Result<()> {
    Ok(()) // Windows has no execute bit; git runs the shim through sh regardless.
}

/// `.` and `..` resolved textually, without asking the filesystem.
///
/// Deliberately NOT `canonicalize`: that requires every component to exist, it
/// resolves symlinks (so a containment test built on it would answer about the
/// TARGET, which is the opposite of what a link refusal wants to know), and on
/// Windows it returns an extended-length `\\?\C:\…` path that compares equal to
/// nothing anybody wrote down.
pub fn resolve_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                // The parent of a root is the root. Anything else (an empty
                // buffer, a leading `..`) keeps the `..`, because dropping it
                // would silently change which directory the path names.
                Some(Component::RootDir) | Some(Component::Prefix(_)) => {}
                _ => out.push(Component::ParentDir),
            },
            other => out.push(other),
        }
    }
    out
}

/// Whether `child` is `parent` or lives under it, lexically.
///
/// Used for containment questions about paths that may not exist yet, where
/// `canonicalize` cannot be asked.
pub fn is_within(child: &Path, parent: &Path) -> bool {
    resolve_lexical(child).starts_with(resolve_lexical(parent))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gh-hookfile-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("mkdir");
        d
    }

    /// root ignores permission bits, so a test built on them proves nothing
    /// there. Same guard, and the same reason, as `tests/it/install.rs`.
    #[cfg(unix)]
    fn running_as_root() -> bool {
        extern "C" {
            fn geteuid() -> u32;
        }
        unsafe { geteuid() == 0 }
    }

    /// The bug, exactly: a compiled hook is not valid UTF-8, `read_to_string`
    /// returned `Err`, `unwrap_or(false)` said "not foreign", and the installer
    /// wrote over somebody's binary without a word.
    #[test]
    fn a_non_utf8_hook_is_foreign_not_ours() {
        let d = tmpdir("notutf8");
        let p = d.join("pre-commit");
        // A Mach-O/ELF-ish header: bytes no UTF-8 decoder accepts.
        std::fs::write(&p, [0x7f, b'E', b'L', b'F', 0x02, 0x01, 0xff, 0xfe]).expect("write");
        assert_eq!(classify(&p), HookFile::Foreign(ForeignWhy::NotUtf8));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Unreadable is FOREIGN, never ours. The old predicate turned every io
    /// error into "not foreign", which is the failing-open half of the same
    /// line.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_hook_is_foreign_not_ours() {
        use std::os::unix::fs::PermissionsExt;
        if running_as_root() {
            return;
        }
        let d = tmpdir("unreadable");
        let p = d.join("pre-commit");
        std::fs::write(&p, "#!/bin/sh\n# git-templates hook shim.\n").expect("write");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).expect("chmod");

        let got = classify(&p);
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644));
        assert!(
            matches!(got, HookFile::Foreign(ForeignWhy::Unreadable { .. })),
            "an unreadable hook classified as {got:?}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A link to one of OUR OWN shims is still a link. The content is ours; the
    /// path is not, and `fs::write` here rewrites whatever is on the other end.
    #[cfg(unix)]
    #[test]
    fn a_symlink_to_our_own_shim_is_still_a_symlink() {
        let d = tmpdir("symlink-ours");
        let real = d.join("real-shim");
        std::fs::write(&real, "#!/bin/sh\n# git-templates hook shim.\nexec x\n").expect("write");
        let link = d.join("pre-commit");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        match classify(&link) {
            HookFile::Symlink { target } => {
                assert_eq!(target.as_deref(), Some(real.as_path()));
            }
            other => panic!("a symlink classified as {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Mentioning this project is not a claim of ownership. The unanchored
    /// `text.contains(SHIM_MARKER)` said otherwise, and `--force` then
    /// overwrote a hook whose author had written a courteous note about why
    /// they replaced ours.
    #[test]
    fn a_hook_that_merely_mentions_the_marker_is_not_ours() {
        let prose = "#!/bin/sh\n\
             # My own commit-msg. I removed the git-templates hook shim on purpose:\n\
             # it disagreed with our house rules. Do not put it back.\n\
             exec my-linter \"$@\"\n";
        assert!(!is_our_shim(prose), "prose about the shim claimed the file");

        // Deep in the body counts for nothing either — this is the `grep`-in-a
        // -script shape.
        let deep = format!("#!/bin/sh\n{}\n# {SHIM_MARKER}\n", "echo x\n".repeat(20));
        assert!(!is_our_shim(&deep), "a line 20 deep claimed the file");

        // And it must be a COMMENT, not a string a script happens to carry.
        let quoted = "#!/bin/sh\ngrep -q \"git-templates hook shim\" \"$0\" && exit 0\n";
        assert!(!is_our_shim(quoted), "a quoted mention claimed the file");
    }

    /// Backward compatibility, checked against the actual history rather than
    /// asserted. Every revision of every file under `templates/hooks/` that has
    /// ever carried the marker put it on line 2, as a `#` comment — the `sh`
    /// shims of the Phase 0 port, the `.zsh`/`.js` suffixed generation, and the
    /// current one. An `uninstall` that stops recognising any of them leaves a
    /// shim installed and running with no tool willing to remove it.
    #[test]
    fn every_shim_form_this_project_has_ever_baked_is_still_ours() {
        let historical = [
            // The current template, and the one `include_str!` embeds.
            crate::install::SHIM,
            // The Phase 0 shim, and the shape both fleet test suites still
            // build fixtures from.
            "#!/bin/sh\n# git-templates hook shim.\nexec \"$BIN\" --hooks-dir \"$(dirname \"$0\")\" pre-commit \"$@\"\n",
            // The generation that dispatched by suffix.
            "#!/bin/sh\n# git-templates hook shim → the amont binary.\nexec x --hooks-dir y pre-commit-ruff\n",
            // Hand-edited, which uninstall must still take: the marker is the
            // claim, not byte equality with the template.
            "#!/bin/sh\n# git-templates hook shim → the amont binary.\n# edited by me\nexec /usr/local/bin/amont \"$@\"\n",
        ];
        for shim in historical {
            assert!(
                is_our_shim(shim),
                "a shim this project baked is no longer recognised:\n{}",
                shim.lines().take(3).collect::<Vec<_>>().join("\n")
            );
        }
    }

    /// The verified bug, at the level of one function: a write must land on the
    /// LINK and leave the file it pointed at exactly as it was.
    #[cfg(unix)]
    #[test]
    fn a_staged_write_replaces_the_link_and_never_its_target() {
        let d = tmpdir("write-through");
        let target = d.join("tracked-source");
        std::fs::write(&target, "PRECIOUS\n").expect("write");
        let link = d.join("pre-commit");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        let s = stage(&link, "#!/bin/sh\n# git-templates hook shim.\n", true).expect("stage");
        commit_all(vec![s]).expect("commit");

        assert_eq!(
            std::fs::read_to_string(&target).expect("read"),
            "PRECIOUS\n",
            "THE WRITE WENT THROUGH THE LINK"
        );
        assert!(
            !std::fs::symlink_metadata(&link)
                .expect("stat")
                .file_type()
                .is_symlink(),
            "the link survived the write"
        );
        assert!(matches!(classify(&link), HookFile::Ours));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A git that cannot answer must not read as "not tracked". Shimmed with a
    /// `git` that exits 128 saying something other than "not a git repository"
    /// — the shape of `detected dubious ownership`, which is what a repository
    /// owned by another uid produces on every call, in every container that
    /// bind-mounts a checkout.
    #[cfg(unix)]
    #[test]
    fn a_git_that_cannot_answer_is_unknown_not_untracked() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmpdir("git-shim");
        let fake = d.join("fake-git");
        std::fs::write(
            &fake,
            "#!/bin/sh\necho \"fatal: detected dubious ownership in repository\" >&2\nexit 128\n",
        )
        .expect("write");
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let hook = d.join("pre-commit");
        std::fs::write(&hook, "#!/bin/sh\n").expect("write");

        let got = tracked_with(&fake, &hook);
        assert!(
            matches!(got, Tracked::Unknown { .. }),
            "a fatal git reported as {got:?}"
        );
        let err = guard_write_with(&fake, &hook, true)
            .expect_err("--force must not override an unanswerable tracked check");
        assert!(
            matches!(err, Refuse::TrackedUnknown { .. }),
            "refused as {err:?}"
        );
        assert!(
            err.explain().contains("safe.directory"),
            "the refusal must name the fix:\n{}",
            err.explain()
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Staging is the all-or-nothing point. If any body cannot be written, no
    /// destination has been touched — there is nothing to undo, because nothing
    /// was done.
    #[cfg(unix)]
    #[test]
    fn a_failed_stage_leaves_every_destination_untouched() {
        use std::os::unix::fs::PermissionsExt;
        if running_as_root() {
            return;
        }
        let d = tmpdir("stage-fail");
        let existing = d.join("pre-commit");
        std::fs::write(&existing, "OLD\n").expect("write");
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o555)).expect("chmod");

        let err = stage(&existing, "NEW\n", true).expect_err("a read-only dir must fail");
        let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755));

        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(
            std::fs::read_to_string(&existing).expect("read"),
            "OLD\n",
            "a failed stage changed the destination"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// An uncommitted stage must not leave an executable file named like a hook
    /// lying in `.git/hooks`.
    #[test]
    fn dropping_an_uncommitted_stage_removes_its_temporary() {
        let d = tmpdir("drop");
        let dest = d.join("pre-commit");
        let tmp = {
            let s = stage(&dest, "body\n", true).expect("stage");
            let t = s.tmp().to_path_buf();
            assert!(t.is_file(), "stage wrote nothing");
            t
        };
        assert!(!tmp.exists(), "an uncommitted temporary survived");
        assert!(!dest.exists(), "staging touched the destination");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Absent is the only unambiguous "yes", and it has to be one — an install
    /// into an empty hooks dir is the common case.
    #[test]
    fn an_absent_hook_is_absent_and_writable() {
        let d = tmpdir("absent");
        let p = d.join("pre-commit");
        assert_eq!(classify(&p), HookFile::Absent);
        assert_eq!(guard_write(&p, false), Ok(HookFile::Absent));
        assert_eq!(guard_remove(&p, true), Ok(()));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A directory named `pre-commit` is not a hook to replace, and `--force`
    /// does not make it one.
    #[test]
    fn a_directory_at_a_hook_path_is_refused_even_with_force() {
        let d = tmpdir("notfile");
        let p = d.join("pre-commit");
        std::fs::create_dir_all(&p).expect("mkdir");
        assert_eq!(classify(&p), HookFile::NotARegularFile);
        for force in [false, true] {
            assert!(matches!(
                guard_write(&p, force),
                Err(Refuse::NotARegularFile { .. })
            ));
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// `--force` is a statement about a hook, and its exact reach is worth
    /// pinning: a foreign file and a symlink yield, and nothing else does.
    #[cfg(unix)]
    #[test]
    fn force_reaches_a_foreign_file_and_a_symlink_and_stops_there() {
        let d = tmpdir("force-reach");
        let foreign = d.join("commit-msg");
        std::fs::write(&foreign, "#!/bin/sh\necho mine\n").expect("write");
        assert!(matches!(
            guard_write(&foreign, false),
            Err(Refuse::Foreign { .. })
        ));
        assert!(guard_write(&foreign, true).is_ok());

        let link = d.join("pre-commit");
        std::os::unix::fs::symlink(&foreign, &link).expect("symlink");
        assert!(matches!(
            guard_write(&link, false),
            Err(Refuse::Symlink { .. })
        ));
        assert!(guard_write(&link, true).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A symlink pointing at one of our shims is refused by `uninstall` too:
    /// deleting the link and deleting the shim are different acts.
    #[cfg(unix)]
    #[test]
    fn uninstall_refuses_a_symlink_even_when_it_points_at_our_shim() {
        let d = tmpdir("rm-symlink");
        let real = d.join("real");
        std::fs::write(&real, "#!/bin/sh\n# git-templates hook shim.\n").expect("write");
        let link = d.join("pre-commit");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        assert!(matches!(
            guard_remove(&link, true),
            Err(Refuse::Symlink { .. })
        ));
        assert!(real.is_file(), "the target was removed");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// git tracks it ⇒ nobody writes it, whatever `--force` says.
    #[test]
    fn a_tracked_hook_is_refused_and_force_does_not_reach_it() {
        let d = tmpdir("tracked");
        let run = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(&d)
                .args(args)
                .output()
                .expect("git");
        };
        run(&["init", "-q", "--template=", "."]);
        run(&["config", "user.email", "t@t.test"]);
        run(&["config", "user.name", "t"]);
        let p = d.join("pre-commit");
        std::fs::write(&p, "tracked source\n").expect("write");
        run(&["add", "-A"]);
        run(&["commit", "-qm", "seed"]);

        assert_eq!(tracked(&p), Tracked::Yes);
        for force in [false, true] {
            assert_eq!(
                guard_write(&p, force),
                Err(Refuse::Tracked { path: p.clone() }),
                "force={force} reached a tracked file"
            );
        }
        assert_eq!(
            guard_remove(&p, false),
            Err(Refuse::Tracked { path: p.clone() })
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The fixtures throughout this suite build hook directories in bare temp
    /// dirs with no repository anywhere above them. "No repository" is a clean
    /// "not tracked", not an unanswerable question — refuse it and every
    /// synthetic fixture in the workspace starts failing for a reason unrelated
    /// to what it tests.
    ///
    /// Relies on `std::env::temp_dir()` not sitting inside a checkout, the same
    /// assumption `install::tests::an_ordinary_directory_is_safe` already
    /// makes. Not asserted with `GIT_CEILING_DIRECTORIES`, because setting it
    /// would mean `set_var` — process-wide, and this harness runs every test in
    /// one process in parallel.
    #[test]
    fn no_repository_at_all_is_untracked_rather_than_unknown() {
        let d = tmpdir("norepo");
        let p = d.join("pre-commit");
        std::fs::write(&p, "x\n").expect("write");
        assert_eq!(tracked(&p), Tracked::No);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn lexical_resolution_never_touches_the_filesystem() {
        assert_eq!(
            resolve_lexical(Path::new("/a/b/../c/./d")),
            PathBuf::from("/a/c/d")
        );
        assert_eq!(resolve_lexical(Path::new("a/../..")), PathBuf::from(".."));
        // The parent of the root is the root, not `/..`.
        assert_eq!(resolve_lexical(Path::new("/..")), PathBuf::from("/"));
        assert!(is_within(Path::new("/a/b/c"), Path::new("/a/b")));
        assert!(is_within(Path::new("/a/b"), Path::new("/a/b")));
        assert!(!is_within(Path::new("/a/bc"), Path::new("/a/b")));
        assert!(!is_within(Path::new("/a/b/../../x"), Path::new("/a/b")));
    }
}
