//! The one-time move of greengate 3.x's local state to greengate 4.0's names
//! (plan "greengate 4.0 renames its data names", a hard rename: 4.0 reads
//! and writes only the new names, so what 3.x left on this machine is moved
//! once instead of being read twice forever).
//!
//! Per repository, the first 4.0 command moves, under a `flock` on
//! `$GIT_COMMON_DIR/greengate-migrate.lock`:
//!
//! - every `amont-*` file and the `amont/` folder in the common dir and in
//!   each linked worktree's private git dir (`$GIT_COMMON_DIR/worktrees/*`),
//!   a ledger's header line rewritten to its `greengate-…` spelling first;
//! - the repository's local `amont.*` config keys to `greengate.*`, each
//!   value copied (`--get-all`, then `--add`), then the old key removed.
//!
//! then writes `$GIT_COMMON_DIR/greengate-migrated`, which every later
//! command only `stat`s. The machine's global `amont.*` keys move the same
//! way, once, under a lock in greengate's config folder.
//!
//! Every step is idempotent and ordered so an interrupted migration resumes:
//! a header is rewritten before its file is renamed, and a key is removed
//! only after all its values were copied. A migration that cannot finish
//! leaves what it has not reached under the old names and refuses the
//! command, naming what to fix.
//!
//! One store is never moved: `amont-held` holds unstaged changes a 3.x
//! check parked, and 3.x records no owner, so a live 3.x may still be about
//! to put them back. The command is refused until `greengate restore` (which
//! reads the old store, [`old_held_store`]) has put them back.
//!
//! The old spellings live HERE only; the names test exempts this file.

use std::path::{Path, PathBuf};

/// greengate 3.x's names, read only to move them.
pub mod old {
    /// The config namespace.
    pub const CONFIG_PREFIX: &str = "amont.";
    /// greenpiste's own keys under that namespace: not greengate's to move.
    pub const FOREIGN_PREFIX: &str = "amont.agent.";
    /// The held store, never moved (see the module doc).
    pub const HELD_STORE: &str = "amont-held";
    /// The held store's index header, which `greengate restore` still reads.
    pub const HELD_FORMAT: &str = "amont-held-v1";
    /// The namespace greengate 3.x's homelab cutover ran in.
    pub const CLUSTER_NAMESPACE: &str = "amont-gates";
    /// The folder of state greengate 3.x kept in the common dir.
    pub const STATE_DIR: &str = "amont";
}

/// One name in a git dir, its 4.0 spelling, and the header line it carries.
struct Move {
    old: &'static str,
    new: &'static str,
    header: Option<(&'static str, &'static str)>,
}

use crate::names as n;

/// Everything 3.x kept in a git dir, in the order it is moved.
const MOVES: &[Move] = &[
    Move {
        old: "amont-bypasses",
        new: n::BYPASS_LEDGER,
        header: Some(("amont-bypass-v1", n::BYPASS_FORMAT)),
    },
    Move {
        old: "amont-downgrades",
        new: n::DOWNGRADE_LEDGER,
        header: Some(("amont-downgrade-v1", n::DOWNGRADE_FORMAT)),
    },
    Move {
        old: "amont-waves",
        new: n::WAVES_LEDGER,
        header: Some(("amont-waves-v1", n::WAVES_FORMAT)),
    },
    Move {
        old: "amont-rehearsal",
        new: n::REHEARSAL_STATE,
        header: Some(("amont-rehearsal-v1", n::REHEARSAL_FORMAT)),
    },
    Move {
        old: "amont-rehearsal.log",
        new: n::REHEARSAL_LOG,
        header: None,
    },
    Move {
        old: "amont-skew",
        new: n::SKEW_MARKER,
        header: Some(("amont-skew-v1", n::SKEW_FORMAT)),
    },
    Move {
        old: "amont-gate",
        new: n::GATE_MARKER,
        header: None,
    },
    Move {
        old: "amont-warm.log",
        new: n::WARM_LOG,
        header: None,
    },
    Move {
        old: "amont-cache",
        new: n::TREE_CACHE,
        header: None,
    },
    Move {
        old: "amont-preserved",
        new: n::PRESERVED_STORE,
        header: None,
    },
    Move {
        old: "amont",
        new: n::STATE_DIR,
        header: None,
    },
];

/// Which way a migration goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// 3.x names to 4.0 names: what every 4.0 command does once.
    Forward,
    /// 4.0 names back to 3.x names: `greengate migrate --reverse`, before a
    /// downgrade to 3.x.
    Reverse,
}

/// The git dirs of one repository: the common dir, then each linked
/// worktree's private one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub common: PathBuf,
}

impl Repo {
    /// The repository `start` stands in, found from the filesystem alone:
    /// `$GIT_DIR` when set, else the nearest `.git` above `start` (a folder,
    /// or a `gitdir:` file), its `commondir` followed. No process is spawned:
    /// this runs before every command.
    pub fn locate(start: &Path) -> Option<Repo> {
        Repo::locate_with(start, std::env::var_os("GIT_DIR"))
    }

    /// As [`Repo::locate`], with `$GIT_DIR` given rather than read.
    pub fn locate_with(start: &Path, git_dir: Option<std::ffi::OsString>) -> Option<Repo> {
        let private = match git_dir.filter(|d| !d.is_empty()) {
            Some(dir) => absolute(start, Path::new(&dir)),
            None => find_git_dir(start)?,
        };
        let common = match std::fs::read_to_string(private.join("commondir")) {
            Ok(text) => absolute(&private, Path::new(text.trim())),
            Err(_) => private.clone(),
        };
        Some(Repo { common })
    }

    /// Each linked worktree's private git dir, listed when asked: the hot
    /// path checks the done-marker first and never lists them.
    pub fn worktrees(&self) -> Vec<PathBuf> {
        match std::fs::read_dir(self.common.join("worktrees")) {
            Ok(entries) => {
                let mut dirs: Vec<PathBuf> = entries
                    .filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect();
                dirs.sort();
                dirs
            }
            Err(_) => Vec::new(),
        }
    }

    /// The common dir, then each linked worktree's private one.
    pub fn git_dirs(&self) -> Vec<PathBuf> {
        std::iter::once(self.common.clone())
            .chain(self.worktrees())
            .collect()
    }

    /// The done-marker: present once this repository was migrated.
    pub fn marker(&self) -> PathBuf {
        self.common.join(n::MIGRATED_MARKER)
    }
}

fn absolute(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

/// The nearest git dir above `start`.
fn find_git_dir(start: &Path) -> Option<PathBuf> {
    let mut here = Some(start);
    while let Some(dir) = here {
        let dotgit = dir.join(".git");
        if dotgit.is_dir() {
            return Some(dotgit);
        }
        if let Ok(text) = std::fs::read_to_string(&dotgit) {
            let pointer = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?;
            return Some(absolute(dir, Path::new(pointer.trim())));
        }
        here = dir.parent();
    }
    None
}

/// The held store greengate 3.x left in this worktree, for `greengate
/// restore`: `$GIT_DIR/amont-held`, when it exists.
pub fn old_held_store(git_dir: &Path) -> Option<PathBuf> {
    let store = git_dir.join(old::HELD_STORE);
    store.is_dir().then_some(store)
}

/// Why a migration stopped. Its `Display` is the line a refused command
/// prints; the variants are what a caller or a test tells apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrateError {
    /// A 3.x check parked unstaged changes here (forward), or a 4.0 one did
    /// (reverse): they are put back first, never moved.
    HeldStore { path: PathBuf },
    /// A detached cluster run 3.x submitted is still pending: its Workflow
    /// carries 3.x's label, which 4.0 cannot collect.
    PendingDetached { path: PathBuf },
    /// 3.x's pending file exists but this version cannot read it: its runs
    /// would be lost all the same, so the move waits.
    PendingUnreadable { path: PathBuf, why: String },
    /// Both spellings of one name exist and are not a log to merge.
    BothExist { source: PathBuf, target: PathBuf },
    /// Another process held the migration lock past its deadline.
    Locked { path: PathBuf },
    /// A file operation failed.
    Io {
        op: &'static str,
        path: PathBuf,
        why: String,
    },
    /// git refused or could not run.
    Git { args: String, why: String },
}

impl std::error::Error for MigrateError {}

impl std::fmt::Display for MigrateError {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MigrateError::HeldStore { path } => write!(
                out,
                "{} holds unstaged changes a check parked; put them back with `{} restore` \
                 in that worktree, then run the command again",
                path.display(),
                n::BIN
            ),
            MigrateError::PendingDetached { path } => write!(
                out,
                "{} lists a detached cluster run greengate 3.x submitted,\n  \
                 which 4.0 cannot collect. Once `kubectl get workflows -n {}`\n  \
                 no longer shows it (or its verdict no longer matters),\n  \
                 remove the file:\n  rm {}\n  then run the command again",
                path.display(),
                old::CLUSTER_NAMESPACE,
                path.display()
            ),
            MigrateError::PendingUnreadable { path, why } => write!(
                out,
                "{} (greengate 3.x's detached cluster runs)\n  cannot be read: \
                 {why}.\n  If no run in it matters any more, remove it:\n  rm {}\n  \
                 then run the command again",
                path.display(),
                path.display()
            ),
            MigrateError::BothExist { source, target } => write!(
                out,
                "both {} and {} exist; remove the one you do not need (`rm -r {}` or \
                 `rm -r {}`), then run `{} migrate`",
                source.display(),
                target.display(),
                source.display(),
                target.display(),
                n::BIN
            ),
            MigrateError::Locked { path } => write!(
                out,
                "another {} is migrating this repository ({} is held); run the command again",
                n::BIN,
                path.display()
            ),
            MigrateError::Io { op, path, why } => {
                write!(out, "could not {op} {}: {why}", path.display())
            }
            MigrateError::Git { args, why } => write!(out, "git {args} failed: {why}"),
        }
    }
}

fn io<'a>(op: &'static str, path: &'a Path) -> impl FnOnce(std::io::Error) -> MigrateError + 'a {
    move |e| MigrateError::Io {
        op,
        path: path.to_path_buf(),
        why: e.to_string(),
    }
}

/// How long a migration waits for another process that holds its lock: a
/// migration takes well under a second, so a longer hold is a stuck peer.
const LOCK_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// What the first-run migration did, for `greengate migrate` to say.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Moved {
    /// Names moved in the git dirs.
    pub state: usize,
    /// Config keys moved.
    pub keys: usize,
}

/// Before any command but `--help`, `--version`, `restore`, `uninstall`,
/// `migrate`, `doctor` and a `--dry-run`: migrate the repository the
/// process stands in, once, then the machine's global keys, once.
pub fn ensure(cwd: &Path, git: &GitFn<'_>, now: u64) -> Result<Moved, MigrateError> {
    let mut moved = ensure_global(git)?;
    let here = ensure_repo(cwd, git, now)?;
    moved.state += here.state;
    moved.keys += here.keys;
    Ok(moved)
}

/// The repository half of [`ensure`]: the one `cwd` stands in, once, unless
/// a reverse asked 4.0 to leave it alone.
pub fn ensure_repo(cwd: &Path, git: &GitFn<'_>, now: u64) -> Result<Moved, MigrateError> {
    let Some(repo) = Repo::locate(cwd) else {
        return Ok(Moved::default());
    };
    if repo.marker().exists() || repo.common.join(n::REVERSED_MARKER).exists() {
        return Ok(Moved::default());
    }
    migrate_repo(&repo, Direction::Forward, git, now)
}

/// Migrate `repo` in `direction`, under its lock. Forward writes the
/// done-marker and clears a reverse; reverse removes the done-marker and
/// leaves the reversed one, so a 4.0 hook does not move it forward again.
pub fn migrate_repo(
    repo: &Repo,
    direction: Direction,
    git: &GitFn<'_>,
    now: u64,
) -> Result<Moved, MigrateError> {
    let _lock = lock(&repo.common.join(n::MIGRATE_LOCK))?;
    let reversed = repo.common.join(n::REVERSED_MARKER);
    // Checked again under the lock: a first run that passed `ensure_repo`'s
    // check before a reverse took the lock must not undo it. A reverse
    // writes its marker before it moves anything, so a reverse that stopped
    // half-way also stands the first run down; `greengate migrate` clears
    // both markers first, so asked by name it moves everything forward.
    if direction == Direction::Forward && (repo.marker().exists() || reversed.exists()) {
        return Ok(Moved::default());
    }
    let dirs = repo.git_dirs();
    for dir in &dirs {
        let held = match direction {
            Direction::Forward => old_held_store(dir),
            Direction::Reverse => Some(dir.join(n::HELD_STORE)).filter(|p| p.is_dir()),
        };
        if let Some(path) = held {
            return Err(MigrateError::HeldStore { path });
        }
    }
    if direction == Direction::Forward {
        let pending = repo
            .common
            .join(old::STATE_DIR)
            .join(crate::cluster::PENDING_FILE);
        // A run that has outlived its Workflow is past collecting; a file
        // this version cannot read is refused too, with its reason.
        match crate::cluster::lists_live(&pending, now) {
            Ok(false) => {}
            Ok(true) => return Err(MigrateError::PendingDetached { path: pending }),
            Err(why) => return Err(MigrateError::PendingUnreadable { path: pending, why }),
        }
    }
    if direction == Direction::Reverse {
        // The markers first: a reverse that stops half-way must not leave
        // a done-marker over state already under 3.x names.
        std::fs::write(&reversed, format!("{}-v1\n", n::REVERSED_MARKER))
            .map_err(io("write", &reversed))?;
        remove_if_present(&repo.marker())?;
    }
    let mut moved = Moved::default();
    for dir in &dirs {
        moved.state += move_state(dir, direction)?;
    }
    // A config file that names no section of the namespace being moved
    // holds no key of it: skip git. This keeps the first run of a
    // repository without 3.x keys free of any extra process.
    let local_files = [
        repo.common.join("config"),
        repo.common.join("config.worktree"),
    ];
    match may_hold(&local_files, direction) {
        Some(true) => moved.keys += move_keys(&Scope::Local(repo.common.clone()), direction, git)?,
        Some(false) => {}
        // A config git cannot read either: going forward, the command that
        // follows says so in its own words, the keys wait, and no marker is
        // written, so the next run tries again. A reverse asked for by name
        // must not report success over keys it never read.
        None if direction == Direction::Forward => return Ok(moved),
        None => {
            return Err(MigrateError::Io {
                op: "read",
                path: repo.common.join("config"),
                why: "the repository's config cannot be read".into(),
            })
        }
    }
    if direction == Direction::Forward {
        let text = format!("{}-v1\n{}\n", n::MIGRATED_MARKER, env!("CARGO_PKG_VERSION"));
        std::fs::write(repo.marker(), text).map_err(io("write", &repo.marker()))?;
        remove_if_present(&reversed)?;
    }
    Ok(moved)
}

fn remove_if_present(path: &Path) -> Result<(), MigrateError> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io("remove", path)(e)),
        _ => Ok(()),
    }
}

/// The migration lock, waited for at most [`LOCK_DEADLINE`], saying so
/// once when another process holds it (a silent wait in a hook reads as a
/// hang).
fn lock(path: &Path) -> Result<std::fs::File, MigrateError> {
    if let Some(file) = greenharness_slots::lock_within(path, std::time::Duration::ZERO)
        .map_err(io("lock", path))?
    {
        return Ok(file);
    }
    eprintln!("{}: waiting for {}", n::BIN, path.display());
    greenharness_slots::lock_within(path, LOCK_DEADLINE)
        .map_err(io("lock", path))?
        .ok_or_else(|| MigrateError::Locked {
            path: path.to_path_buf(),
        })
}

/// Move every name of [`MOVES`] in `dir`, in `direction`; how many moved.
pub fn move_state(dir: &Path, direction: Direction) -> Result<usize, MigrateError> {
    let mut moved = 0;
    for m in MOVES {
        let (from, to, header) = match direction {
            Direction::Forward => (m.old, m.new, m.header),
            Direction::Reverse => (m.new, m.old, m.header.map(|(o, n)| (n, o))),
        };
        let source = dir.join(from);
        let target = dir.join(to);
        if !source.exists() {
            continue;
        }
        moved += 1;
        if source.is_dir() && target.is_dir() {
            merge_dirs(&source, &target)?;
            continue;
        }
        if target.exists() {
            return Err(MigrateError::BothExist { source, target });
        }
        if let Some((old_header, new_header)) = header {
            rewrite_header(&source, old_header, new_header)?;
        }
        std::fs::rename(&source, &target).map_err(io("move", &source))?;
    }
    Ok(moved)
}

/// Move what `source` holds into `target`, which already exists (a tool or
/// a test of the other version wrote there first): a child only in
/// `source` is moved; a `.log` in both becomes `source`'s lines then
/// `target`'s, written whole and renamed into place before `source`'s copy
/// is removed (a crash between the two repeats those lines once); any other
/// clash is refused before anything moves. `source` is removed once empty.
fn merge_dirs(source: &Path, target: &Path) -> Result<(), MigrateError> {
    let read = |dir: &Path| -> Result<Vec<PathBuf>, MigrateError> {
        let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
            .map_err(io("read", dir))?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .collect();
        out.sort();
        Ok(out)
    };
    let children = read(source)?;
    for child in &children {
        let there = target.join(child.file_name().unwrap_or_default());
        let log = child.extension().is_some_and(|e| e == "log");
        let both_dirs = child.is_dir() && there.is_dir();
        if there.exists() && !log && !both_dirs {
            return Err(MigrateError::BothExist {
                source: child.clone(),
                target: there,
            });
        }
    }
    for child in children {
        let there = target.join(child.file_name().unwrap_or_default());
        if child.is_dir() && there.is_dir() {
            merge_dirs(&child, &there)?;
        } else if there.exists() {
            let mut joined = std::fs::read(&child).map_err(io("read", &child))?;
            joined.extend(std::fs::read(&there).map_err(io("read", &there))?);
            let staged = there.with_extension("log.greengate-merge");
            std::fs::write(&staged, joined).map_err(io("write", &staged))?;
            std::fs::rename(&staged, &there).map_err(io("replace", &there))?;
            std::fs::remove_file(&child).map_err(io("remove", &child))?;
        } else {
            std::fs::rename(&child, &there).map_err(io("move", &child))?;
        }
    }
    std::fs::remove_dir(source).map_err(io("remove", source))
}

/// Replace every line of `file` that is exactly `old` with `new`, through a
/// temporary file and a rename, so a crash leaves one whole version.
fn rewrite_header(file: &Path, old: &str, new: &str) -> Result<(), MigrateError> {
    if !file.is_file() {
        return Ok(());
    }
    let text = std::fs::read(file).map_err(io("read", file))?;
    let Ok(text) = String::from_utf8(text) else {
        return Ok(());
    };
    if !text.lines().any(|l| l == old) {
        return Ok(());
    }
    let mut out = String::with_capacity(text.len() + 8);
    for line in text.split_inclusive('\n') {
        let (body, end) = match line.strip_suffix('\n') {
            Some(body) => (body, "\n"),
            None => (line, ""),
        };
        out.push_str(if body == old { new } else { body });
        out.push_str(end);
    }
    let staged = file.with_extension("greengate-migrate.tmp");
    std::fs::write(&staged, out).map_err(io("write", &staged))?;
    std::fs::rename(&staged, file).map_err(io("replace", file))
}

/// Where config keys are moved.
#[derive(Debug, Clone)]
pub enum Scope {
    /// The repository's own config (`--local`), the common dir given.
    Local(PathBuf),
    /// The person's global config (`--global`).
    Global,
}

/// A git command for [`move_keys`]: `(GIT_DIR or None, args)` →
/// `Ok(Some(stdout))` on exit 0, `Ok(None)` on exit 1 (git's "nothing
/// matched"), `Err` on any other end. A seam, so the tests drive a scratch
/// repository and `main` composes the real one.
pub type GitFn<'a> = dyn Fn(Option<&Path>, &[&str]) -> Result<Option<String>, String> + 'a;

fn run_git(
    git: &GitFn<'_>,
    dir: Option<&Path>,
    args: &[&str],
) -> Result<Option<String>, MigrateError> {
    git(dir, args).map_err(|why| MigrateError::Git {
        args: args.join(" "),
        why,
    })
}

/// The values `git config -z --get-all` prints for one key, each ended by
/// a NUL: an empty value (`git config amont.x ""`) is a value too.
fn values(raw: &str) -> Vec<String> {
    match raw.strip_suffix('\0') {
        Some(body) => body.split('\0').map(str::to_string).collect(),
        None => Vec::new(),
    }
}

/// Move the `amont.*` keys of `scope` to `greengate.*` (or back); how many
/// keys moved. Each value the old key holds is added to the new one unless
/// already there (a migration interrupted between two `--add`s resumes
/// without losing one), and the old key goes only once every value is
/// present. A new key that holds a value of its own was set by the person:
/// the new name wins, and the old values are dropped. `amont.agent.*` is
/// greenpiste's, never moved. A `cluster.namespace` of `amont-gates` is
/// dropped on the way forward: 4.0's default namespace is `greengate-gates`.
pub fn move_keys(
    scope: &Scope,
    direction: Direction,
    git: &GitFn<'_>,
) -> Result<usize, MigrateError> {
    let (from, to) = match direction {
        Direction::Forward => (old::CONFIG_PREFIX, n::CONFIG_PREFIX),
        Direction::Reverse => (n::CONFIG_PREFIX, old::CONFIG_PREFIX),
    };
    let (dir, flag) = match scope {
        Scope::Local(common) => (Some(common.as_path()), "--local"),
        Scope::Global => (None, "--global"),
    };
    let pattern = format!("^{}", from.replace('.', r"\."));
    let listed = run_git(
        git,
        dir,
        &["config", flag, "--name-only", "--get-regexp", &pattern],
    )?
    .unwrap_or_default();
    // One line per VALUE, in file order: a key whose values another key
    // separates is listed twice, apart.
    let keys: std::collections::BTreeSet<&str> = listed
        .lines()
        .filter(|k| !k.is_empty() && !k.starts_with(old::FOREIGN_PREFIX))
        .collect();
    let mut moved = 0;
    for key in keys {
        let rest = &key[from.len()..];
        let target = format!("{to}{rest}");
        let old_values = values(
            &run_git(git, dir, &["config", flag, "-z", "--get-all", key])?.unwrap_or_default(),
        );
        let new_values = values(
            &run_git(git, dir, &["config", flag, "-z", "--get-all", &target])?.unwrap_or_default(),
        );
        let owned_by_person = new_values.iter().any(|v| !old_values.contains(v));
        if !owned_by_person {
            for value in &old_values {
                let dropped = direction == Direction::Forward
                    && rest.eq_ignore_ascii_case("cluster.namespace")
                    && value == old::CLUSTER_NAMESPACE;
                if dropped || new_values.contains(value) {
                    continue;
                }
                run_git(git, dir, &["config", flag, "--add", &target, value])?;
            }
        }
        run_git(git, dir, &["config", flag, "--unset-all", key])?;
        moved += 1;
    }
    Ok(moved)
}

/// The machine's global keys, once: a marker in greengate's config folder,
/// written under a lock there. A config that names no `[amont` section
/// needs no git, and then a config folder that cannot be made (a read-only
/// HOME in a container) is no reason to refuse; the files are read again
/// on the next run, which costs no process either. A held
/// `~/.gitconfig.lock` (another git writing) is retried for a few seconds.
fn ensure_global(git: &GitFn<'_>) -> Result<Moved, MigrateError> {
    let Some(dir) = config_dir() else {
        return Ok(Moved::default());
    };
    let marker = dir.join(n::MIGRATED_MARKER);
    if marker.exists() || dir.join(n::REVERSED_MARKER).exists() {
        return Ok(Moved::default());
    }
    let Some(holds) = may_hold(&global_config_files(), Direction::Forward) else {
        // An unreadable global config: git cannot read it either. The keys
        // wait for a run that can.
        return Ok(Moved::default());
    };
    if std::fs::create_dir_all(&dir).is_err() && !holds {
        return Ok(Moved::default());
    }
    std::fs::create_dir_all(&dir).map_err(io("make", &dir))?;
    let _lock = lock(&dir.join(n::MIGRATE_LOCK))?;
    if marker.exists() || dir.join(n::REVERSED_MARKER).exists() {
        return Ok(Moved::default());
    }
    let mut moved = Moved::default();
    if holds {
        // Another git writing `~/.gitconfig` holds its `.lock` for a moment:
        // retried for up to five seconds. Any other failure is reported at
        // once.
        let mut attempts = 0;
        moved.keys = loop {
            match move_keys(&Scope::Global, Direction::Forward, git) {
                Err(MigrateError::Git { why, .. }) if why.contains(".lock") && attempts < 50 => {
                    attempts += 1;
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                done => break done?,
            }
        };
    }
    std::fs::write(&marker, format!("{}-v1\n", n::MIGRATED_MARKER))
        .map_err(io("write", &marker))?;
    Ok(moved)
}

/// Forget that the global keys were moved or reversed, so the next
/// [`ensure`] reads the global config again: `greengate migrate --global`.
pub fn forget_global() -> Result<(), MigrateError> {
    match config_dir() {
        Some(dir) => {
            remove_if_present(&dir.join(n::MIGRATED_MARKER))?;
            remove_if_present(&dir.join(n::REVERSED_MARKER))
        }
        None => Ok(()),
    }
}

/// Whether any of `files` names a section of the namespace `direction`
/// moves from: `[amont]`, `[amont "…"]` (forward) or the greengate ones
/// (reverse), in any case: `None` when a file exists but cannot be read.
/// An `include.path` is read by git and not here: a key reached only
/// through an include is left for `greengate doctor` to name.
fn may_hold(files: &[PathBuf], direction: Direction) -> Option<bool> {
    let section = match direction {
        Direction::Forward => "[amont",
        Direction::Reverse => "[greengate",
    };
    let mut holds = false;
    for file in files {
        let found = match std::fs::read(file) {
            Ok(bytes) => String::from_utf8_lossy(&bytes)
                .to_ascii_lowercase()
                .lines()
                .any(|line| {
                    let line = line.trim_start();
                    line.starts_with(section)
                        && matches!(
                            line[section.len()..].chars().next(),
                            Some(']' | ' ' | '"' | '.')
                        )
                }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(_) => return None,
        };
        holds |= found;
    }
    Some(holds)
}

/// The files `git config --global` reads: `$GIT_CONFIG_GLOBAL` when set,
/// else `$XDG_CONFIG_HOME/git/config` (or `~/.config/git/config`) and
/// `~/.gitconfig`.
fn global_config_files() -> Vec<PathBuf> {
    if let Some(f) = std::env::var_os("GIT_CONFIG_GLOBAL").filter(|f| !f.is_empty()) {
        return vec![PathBuf::from(f)];
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".config")));
    let mut files = Vec::new();
    if let Some(x) = xdg {
        files.push(x.join("git").join("config"));
    }
    if let Some(h) = home {
        files.push(h.join(".gitconfig"));
    }
    files
}

/// Move the machine's global keys back: `greengate migrate --reverse
/// --global`.
pub fn reverse_global(git: &GitFn<'_>) -> Result<Moved, MigrateError> {
    // The reversed marker first, as for a repository: until `migrate
    // --global`, 4.0 leaves the machine's keys under 3.x's names.
    let mut _lock = None;
    if let Some(dir) = config_dir() {
        std::fs::create_dir_all(&dir).map_err(io("make", &dir))?;
        // Under the machine lock, so a first run that passed its check
        // before this started cannot move the keys forward again.
        _lock = Some(lock(&dir.join(n::MIGRATE_LOCK))?);
        let reversed = dir.join(n::REVERSED_MARKER);
        std::fs::write(&reversed, format!("{}-v1\n", n::REVERSED_MARKER))
            .map_err(io("write", &reversed))?;
        remove_if_present(&dir.join(n::MIGRATED_MARKER))?;
    }
    let keys = move_keys(&Scope::Global, Direction::Reverse, git)?;
    Ok(Moved { state: 0, keys })
}

/// greengate's folder of machine state: `$XDG_CONFIG_HOME/greengate`, else
/// `~/.config/greengate`.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(x).join(n::STATE_DIR));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".config").join(n::STATE_DIR))
}

/// What `greengate doctor` reports: the 3.x names still visible from here,
/// one line each, with the command that moves them. `Err` when git could
/// not answer: a failure never reads as "nothing left".
pub fn leftovers(cwd: &Path, git: &GitFn<'_>) -> Result<Vec<String>, MigrateError> {
    let mut out = Vec::new();
    let mut env: Vec<String> = std::env::vars_os()
        .filter_map(|(k, _)| k.into_string().ok())
        .filter(|k| k.starts_with("AMONT_") && !k.starts_with("AMONT_AGENT_"))
        .collect();
    env.sort();
    for k in env {
        let rest = &k["AMONT_".len()..];
        let new = match rest {
            "HOST_SLOT" => greenharness_slots::HELD_ENV.to_string(),
            "SLOT_DIR" => greenharness_slots::DIR_ENV.to_string(),
            _ => format!("{}_{rest}", n::ENV_PREFIX),
        };
        out.push(format!(
            "environment: {k} is ignored by greengate 4.0; set {new} instead"
        ));
    }
    let pattern = r"^amont\.";
    let listed = |dir: Option<&Path>, flag: &str| -> Result<Vec<String>, MigrateError> {
        let names = run_git(
            git,
            dir,
            &["config", flag, "--name-only", "--get-regexp", pattern],
        )?
        .unwrap_or_default();
        Ok(names
            .lines()
            .filter(|k| !k.starts_with(old::FOREIGN_PREFIX))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .map(str::to_string)
            .collect())
    };
    for key in listed(None, "--global")? {
        out.push(format!(
            "global config: {key} is ignored by greengate 4.0; `{} migrate --global` moves it",
            n::BIN
        ));
    }
    if let Some(repo) = Repo::locate(cwd) {
        for key in listed(Some(&repo.common), "--local")? {
            out.push(format!(
                "local config: {key} is ignored by greengate 4.0; `{} migrate` moves it",
                n::BIN
            ));
        }
        for dir in repo.git_dirs() {
            for m in MOVES {
                if dir.join(m.old).exists() {
                    out.push(format!(
                        "state: {} was left by greengate 3.x; `{} migrate` moves it",
                        dir.join(m.old).display(),
                        n::BIN
                    ));
                }
            }
            if let Some(store) = old_held_store(&dir) {
                out.push(format!(
                    "state: {} holds parked changes; `{} restore` puts them back",
                    store.display(),
                    n::BIN
                ));
            }
        }
    }
    Ok(out)
}

/// git, with `GIT_DIR` set to `dir` when one is given, as [`GitFn`] says.
pub fn system_git(dir: Option<&Path>, args: &[&str]) -> Result<Option<String>, String> {
    let mut cmd = std::process::Command::new("git");
    if let Some(dir) = dir {
        cmd.env("GIT_DIR", dir);
    }
    let out = cmd
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    match out.status.code() {
        Some(0) => Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned())),
        Some(1) => Ok(None),
        code => Err(format!(
            "exit {}: {}",
            code.map_or("signal".to_string(), |c| c.to_string()),
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed clock: no test here depends on the date.
    const NOW: u64 = 1_800_000_000;

    fn scratch(name: &str) -> PathBuf {
        let scratch_dir =
            std::env::temp_dir().join(format!("greengate-migrate-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch_dir); // a fresh dir; absent is fine
        std::fs::create_dir_all(&scratch_dir).unwrap();
        scratch_dir
    }

    /// git in `repo`, its global config a file of its own, as [`GitFn`].
    fn git_in(
        repo: &Path,
    ) -> impl Fn(Option<&Path>, &[&str]) -> Result<Option<String>, String> + '_ {
        move |_dir, args| {
            let out = std::process::Command::new("git")
                .current_dir(repo)
                .env("GIT_CONFIG_GLOBAL", repo.join("global-config"))
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .args(args)
                .output()
                .map_err(|e| e.to_string())?;
            match out.status.code() {
                Some(0) => Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned())),
                Some(1) => Ok(None),
                code => Err(format!("{code:?}")),
            }
        }
    }

    /// The values of `key`, newline-joined; empty when unset.
    fn get(git: &GitFn<'_>, key: &str) -> String {
        git(None, &["config", "--get-all", key])
            .unwrap()
            .unwrap_or_default()
    }

    fn init(dir: &Path) {
        let ok = std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(dir)
            .status()
            .unwrap()
            .success();
        assert!(ok);
    }

    #[test]
    fn three_x_state_moves_with_its_headers_and_a_second_run_is_a_no_op() {
        let root = scratch("state");
        init(&root);
        let gd = root.join(".git");
        std::fs::write(gd.join("amont-bypasses"), "amont-bypass-v1\n1 abc gate\n").unwrap();
        std::fs::write(
            gd.join("amont-waves"),
            "amont-waves-v1\n1 2\namont-waves-v1\n3 4\n",
        )
        .unwrap();
        std::fs::create_dir_all(gd.join("amont/logs")).unwrap();
        std::fs::write(gd.join("amont/logs/x.log"), "log").unwrap();
        std::fs::create_dir_all(gd.join("amont-cache/cargo-test")).unwrap();

        let repo = Repo::locate_with(&root, None).unwrap();
        let git = git_in(&root);
        migrate_repo(&repo, Direction::Forward, &git, NOW).unwrap();

        assert_eq!(
            std::fs::read_to_string(gd.join(n::BYPASS_LEDGER)).unwrap(),
            "greengate-bypass-v1\n1 abc gate\n"
        );
        assert_eq!(
            std::fs::read_to_string(gd.join(n::WAVES_LEDGER)).unwrap(),
            "greengate-waves-v1\n1 2\ngreengate-waves-v1\n3 4\n"
        );
        assert!(gd.join("greengate/logs/x.log").is_file());
        assert!(gd.join(n::TREE_CACHE).join("cargo-test").is_dir());
        for old in ["amont-bypasses", "amont-waves", "amont", "amont-cache"] {
            assert!(!gd.join(old).exists(), "{old} left behind");
        }
        assert!(repo.marker().is_file());
        // A second run, marker or not, changes nothing.
        std::fs::remove_file(repo.marker()).unwrap();
        migrate_repo(&repo, Direction::Forward, &git, NOW).unwrap();
        assert!(gd.join(n::BYPASS_LEDGER).is_file());
    }

    #[test]
    fn an_interrupted_move_resumes_and_both_names_are_refused() {
        let root = scratch("resume");
        init(&root);
        let gd = root.join(".git");
        // A crash after the header rewrite, before the rename.
        std::fs::write(
            gd.join("amont-downgrades"),
            "greengate-downgrade-v1\n1 a b c\n",
        )
        .unwrap();
        move_state(&gd, Direction::Forward).unwrap();
        assert_eq!(
            std::fs::read_to_string(gd.join(n::DOWNGRADE_LEDGER)).unwrap(),
            "greengate-downgrade-v1\n1 a b c\n"
        );
        // Both names: refused, nothing moved.
        std::fs::write(gd.join("amont-downgrades"), "amont-downgrade-v1\n").unwrap();
        let err = move_state(&gd, Direction::Forward).unwrap_err();
        assert!(matches!(err, MigrateError::BothExist { .. }), "{err}");
        assert!(gd.join("amont-downgrades").is_file());
    }

    #[test]
    fn linked_worktrees_are_walked_and_a_held_store_refuses() {
        let root = scratch("worktrees");
        init(&root);
        let wt = root.join(".git/worktrees/side");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join("amont-skew"), "amont-skew-v1\n3.0.0\n").unwrap();
        let repo = Repo::locate_with(&root, None).unwrap();
        assert_eq!(repo.worktrees(), vec![wt.clone()]);
        std::fs::create_dir_all(wt.join("amont-held")).unwrap();
        let git = git_in(&root);
        let err = migrate_repo(&repo, Direction::Forward, &git, NOW).unwrap_err();
        assert_eq!(
            err,
            MigrateError::HeldStore {
                path: wt.join("amont-held")
            }
        );
        assert!(err.to_string().contains("restore"), "{err}");
        assert!(
            wt.join("amont-skew").is_file(),
            "nothing moved while refused"
        );
        assert!(!repo.marker().exists());
        std::fs::remove_dir(wt.join("amont-held")).unwrap();
        migrate_repo(&repo, Direction::Forward, &git, NOW).unwrap();
        assert_eq!(
            std::fs::read_to_string(wt.join(n::SKEW_MARKER)).unwrap(),
            "greengate-skew-v1\n3.0.0\n"
        );
    }

    #[test]
    fn local_keys_move_with_every_value_and_the_new_name_wins() {
        let root = scratch("keys");
        init(&root);
        let git = git_in(&root);
        for args in [
            ["config", "--add", "amont.trusted", "aaa"],
            ["config", "--add", "amont.timeout", "600"],
            // Another key between two values: listed twice, apart.
            ["config", "--add", "amont.trusted", "bbb"],
            ["config", "--add", "greengate.timeout", "900"],
            ["config", "--add", "amont.cluster.namespace", "amont-gates"],
            ["config", "--add", "amont.agent.suppressions", "allow"],
        ] {
            git(None, &args).unwrap();
        }
        let repo = Repo::locate_with(&root, None).unwrap();
        let moved = migrate_repo(&repo, Direction::Forward, &git, NOW).unwrap();
        assert_eq!(moved.keys, 3, "trusted, timeout, cluster.namespace");
        assert_eq!(get(&git, "greengate.trusted"), "aaa\nbbb\n");
        assert_eq!(get(&git, "greengate.timeout"), "900\n", "the new name wins");
        assert_eq!(get(&git, "greengate.cluster.namespace"), "");
        assert_eq!(get(&git, "amont.trusted"), "");
        assert_eq!(get(&git, "amont.timeout"), "");
        assert_eq!(get(&git, "amont.agent.suppressions"), "allow\n");

        // And back: the reverse restores the 3.x names, and 4.0 then leaves
        // the repository alone until asked.
        migrate_repo(&repo, Direction::Reverse, &git, NOW).unwrap();
        assert_eq!(get(&git, "amont.trusted"), "aaa\nbbb\n");
        assert_eq!(get(&git, "greengate.trusted"), "");
        assert!(!repo.marker().exists());
        assert!(repo.common.join(n::REVERSED_MARKER).is_file());
        // The repository half only: the global half reads this machine's
        // config home, which a unit test must not touch.
        ensure_repo(&root, &git, NOW).unwrap();
        assert_eq!(
            get(&git, "amont.trusted"),
            "aaa\nbbb\n",
            "a reverse is not undone"
        );
    }

    #[test]
    fn a_copy_cut_off_between_two_values_resumes_without_losing_one() {
        let root = scratch("partial");
        init(&root);
        let git = git_in(&root);
        for args in [
            ["config", "--add", "amont.trusted", "aaa"],
            ["config", "--add", "amont.trusted", "bbb"],
            ["config", "--add", "amont.trusted", "ccc"],
            // The first run stopped after one `--add`.
            ["config", "--add", "greengate.trusted", "aaa"],
        ] {
            git(None, &args).unwrap();
        }
        move_keys(&Scope::Local(root.join(".git")), Direction::Forward, &git).unwrap();
        assert_eq!(get(&git, "greengate.trusted"), "aaa\nbbb\nccc\n");
        assert_eq!(get(&git, "amont.trusted"), "");
    }

    #[test]
    fn a_git_that_fails_stops_the_move_and_keeps_the_old_key() {
        let root = scratch("git-fails");
        init(&root);
        let real = git_in(&root);
        real(None, &["config", "--add", "amont.trusted", "aaa"]).unwrap();
        let failing = |dir: Option<&Path>, args: &[&str]| -> Result<Option<String>, String> {
            if args.contains(&"--add") {
                return Err("exit 255: could not lock config file".into());
            }
            real(dir, args)
        };
        let err = move_keys(
            &Scope::Local(root.join(".git")),
            Direction::Forward,
            &failing,
        )
        .unwrap_err();
        assert!(matches!(err, MigrateError::Git { .. }), "{err}");
        assert_eq!(get(&real, "amont.trusted"), "aaa\n", "the old key stays");
    }

    #[test]
    fn a_3x_pending_file_this_version_cannot_read_refuses_the_move() {
        // The TTL rule itself is tested beside the file's owner
        // (`cluster::detached::collect`, `lists_live`).
        let root = scratch("pending");
        init(&root);
        let gd = root.join(".git");
        std::fs::create_dir_all(gd.join("amont")).unwrap();
        let pending = gd.join("amont/cluster-pending");
        std::fs::write(&pending, "{\"format\":1,\"records\":[{\"name\":\"x\"}]}\n").unwrap();
        let repo = Repo::locate_with(&root, None).unwrap();
        let git = git_in(&root);
        let err = migrate_repo(&repo, Direction::Forward, &git, NOW).unwrap_err();
        assert!(
            matches!(err, MigrateError::PendingUnreadable { .. }),
            "{err}"
        );
        assert!(err.to_string().contains("rm "), "{err}");
        std::fs::write(&pending, "{\"format\":1,\"records\":[]}\n").unwrap();
        migrate_repo(&repo, Direction::Forward, &git, NOW).unwrap();
        assert!(gd.join("greengate/cluster-pending").is_file());
    }

    #[test]
    fn a_live_3x_run_refuses_the_move_and_an_expired_one_lets_it_through() {
        let root = scratch("pending-live");
        init(&root);
        let gd = root.join(".git");
        std::fs::create_dir_all(gd.join("amont")).unwrap();
        let pending = gd.join("amont/cluster-pending");
        std::fs::write(&pending, crate::cluster::sample_pending_file(NOW)).unwrap();
        let repo = Repo::locate_with(&root, None).unwrap();
        let git = git_in(&root);
        assert_eq!(
            migrate_repo(&repo, Direction::Forward, &git, NOW).unwrap_err(),
            MigrateError::PendingDetached {
                path: pending.clone()
            }
        );
        // The same file, a week later: the run is past its Workflow's TTL.
        migrate_repo(&repo, Direction::Forward, &git, NOW + 7 * 86_400).unwrap();
        assert!(gd.join("greengate/cluster-pending").is_file());
    }

    #[test]
    fn a_first_run_stands_down_while_a_reverse_is_marked() {
        let root = scratch("stand-down");
        init(&root);
        let gd = root.join(".git");
        std::fs::write(gd.join("amont-bypasses"), "amont-bypass-v1\n").unwrap();
        let repo = Repo::locate_with(&root, None).unwrap();
        // The state a reverse leaves: its marker, no done-marker, 3.x names.
        std::fs::write(repo.common.join(n::REVERSED_MARKER), "").unwrap();
        let git = git_in(&root);
        migrate_repo(&repo, Direction::Forward, &git, NOW).unwrap();
        assert!(gd.join("amont-bypasses").is_file(), "the reverse holds");
    }

    #[test]
    fn a_reverse_that_stops_half_way_is_moved_forward_again() {
        let root = scratch("half-reverse");
        init(&root);
        let gd = root.join(".git");
        let git = git_in(&root);
        std::fs::write(gd.join("amont-bypasses"), "amont-bypass-v1\n").unwrap();
        git(None, &["config", "amont.timeout", "700"]).unwrap();
        let repo = Repo::locate_with(&root, None).unwrap();
        migrate_repo(&repo, Direction::Forward, &git, NOW).unwrap();
        // A reverse whose key move fails, after the state went back.
        let failing = |dir: Option<&Path>, args: &[&str]| -> Result<Option<String>, String> {
            if args.contains(&"--add") {
                return Err("exit 255: could not lock config file".into());
            }
            git(dir, args)
        };
        assert!(migrate_repo(&repo, Direction::Reverse, &failing, NOW).is_err());
        assert!(gd.join("amont-bypasses").is_file(), "the state went back");
        assert!(!repo.marker().exists(), "no done-marker over 3.x state");
        // Asked forward again: everything is under greengate's names.
        std::fs::remove_file(repo.common.join(n::REVERSED_MARKER)).unwrap();
        migrate_repo(&repo, Direction::Forward, &git, NOW).unwrap();
        assert!(gd.join(n::BYPASS_LEDGER).is_file());
        assert_eq!(get(&git, "greengate.timeout"), "700\n");
    }

    #[test]
    fn a_reverse_refuses_while_4_0_holds_parked_changes() {
        let root = scratch("reverse-held");
        init(&root);
        let gd = root.join(".git");
        std::fs::create_dir_all(gd.join(n::HELD_STORE)).unwrap();
        let repo = Repo::locate_with(&root, None).unwrap();
        let git = git_in(&root);
        assert!(matches!(
            migrate_repo(&repo, Direction::Reverse, &git, NOW).unwrap_err(),
            MigrateError::HeldStore { .. }
        ));
    }

    #[test]
    fn only_a_file_naming_the_namespace_may_hold_its_keys() {
        let root = scratch("may-hold");
        let config_file = root.join("config");
        std::fs::write(&config_file, "[core]\n\tbare = false\n[amontx]\n\ta = 1\n").unwrap();
        assert_eq!(
            may_hold(std::slice::from_ref(&config_file), Direction::Forward),
            Some(false)
        );
        for section in ["[amont]", "[Amont \"severity\"]", "  [amont.cluster]"] {
            std::fs::write(&config_file, format!("[core]\n{section}\n\tx = 1\n")).unwrap();
            assert_eq!(
                may_hold(std::slice::from_ref(&config_file), Direction::Forward),
                Some(true),
                "{section}"
            );
        }
        assert_eq!(
            may_hold(std::slice::from_ref(&config_file), Direction::Reverse),
            Some(false)
        );
        assert_eq!(
            may_hold(&[root.join("absent")], Direction::Forward),
            Some(false)
        );
    }

    #[test]
    fn a_state_folder_both_versions_wrote_is_merged() {
        let root = scratch("merge");
        init(&root);
        let gd = root.join(".git");
        std::fs::create_dir_all(gd.join("amont/land")).unwrap();
        std::fs::create_dir_all(gd.join("greengate/logs")).unwrap();
        std::fs::write(gd.join("amont/cluster.log"), "1 old\n").unwrap();
        std::fs::write(gd.join("amont/land/r.json"), "{}").unwrap();
        std::fs::write(gd.join("greengate/cluster.log"), "2 new\n").unwrap();
        move_state(&gd, Direction::Forward).unwrap();
        assert!(!gd.join("amont").exists());
        assert_eq!(
            std::fs::read_to_string(gd.join("greengate/cluster.log")).unwrap(),
            "1 old\n2 new\n"
        );
        assert!(gd.join("greengate/land/r.json").is_file());
        assert!(gd.join("greengate/logs").is_dir());
        // A clash that is not a log is refused, nothing moved.
        std::fs::create_dir_all(gd.join("amont")).unwrap();
        std::fs::write(gd.join("amont/cluster-pending"), "a").unwrap();
        std::fs::write(gd.join("greengate/cluster-pending"), "b").unwrap();
        assert!(matches!(
            move_state(&gd, Direction::Forward).unwrap_err(),
            MigrateError::BothExist { .. }
        ));
        assert!(gd.join("amont/cluster-pending").is_file());
    }

    #[test]
    fn the_reverse_puts_state_back_under_3x_names() {
        let root = scratch("reverse");
        init(&root);
        let gd = root.join(".git");
        std::fs::write(
            gd.join(n::REHEARSAL_STATE),
            "greengate-rehearsal-v1\npid=1\n",
        )
        .unwrap();
        move_state(&gd, Direction::Reverse).unwrap();
        assert_eq!(
            std::fs::read_to_string(gd.join("amont-rehearsal")).unwrap(),
            "amont-rehearsal-v1\npid=1\n"
        );
    }

    #[test]
    fn a_gitdir_file_is_followed_to_its_common_dir() {
        let root = scratch("pointer");
        init(&root);
        let wt_git = root.join(".git/worktrees/w");
        std::fs::create_dir_all(&wt_git).unwrap();
        std::fs::write(wt_git.join("commondir"), "../..\n").unwrap();
        let checkout = root.join("w");
        std::fs::create_dir_all(checkout.join("src")).unwrap();
        std::fs::write(
            checkout.join(".git"),
            format!("gitdir: {}\n", wt_git.display()),
        )
        .unwrap();
        let repo = Repo::locate_with(&checkout.join("src"), None).unwrap();
        assert_eq!(
            repo.common.canonicalize().unwrap(),
            root.join(".git").canonicalize().unwrap()
        );
    }
}
