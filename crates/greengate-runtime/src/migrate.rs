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
    pub worktrees: Vec<PathBuf>,
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
        let worktrees = match std::fs::read_dir(common.join("worktrees")) {
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
        };
        Some(Repo { common, worktrees })
    }

    fn git_dirs(&self) -> impl Iterator<Item = &Path> {
        std::iter::once(self.common.as_path()).chain(self.worktrees.iter().map(PathBuf::as_path))
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

/// Before any command but `--help`, `--version` and `restore`: migrate the
/// repository the process stands in, once, then the machine's global keys,
/// once. `Err` is the message the command is refused with.
pub fn ensure(cwd: &Path) -> Result<(), String> {
    ensure_global()?;
    let Some(repo) = Repo::locate(cwd) else {
        return Ok(());
    };
    if repo.marker().exists() {
        return Ok(());
    }
    migrate_repo(&repo, Direction::Forward, &system_git)
}

/// Migrate `repo` in `direction`, under its lock, and (forward) write the
/// done-marker; reverse removes it. `git` runs a git command with the
/// repository's common dir as `GIT_DIR`, and answers its stdout.
pub fn migrate_repo(repo: &Repo, direction: Direction, git: &GitFn<'_>) -> Result<(), String> {
    let _lock = lock(&repo.common.join(n::MIGRATE_LOCK))?;
    if direction == Direction::Forward && repo.marker().exists() {
        return Ok(());
    }
    if direction == Direction::Forward {
        for dir in repo.git_dirs() {
            if let Some(store) = old_held_store(dir) {
                return Err(format!(
                    "{} holds unstaged changes a greengate 3.x check parked; put them back with \
                     `{} restore` in that worktree, then run the command again",
                    store.display(),
                    n::BIN
                ));
            }
        }
    }
    for dir in repo.git_dirs() {
        move_state(dir, direction)?;
    }
    // A config file that names no section of the namespace being moved
    // holds no key of it: skip git. This keeps the first run of a
    // repository without 3.x keys free of any extra process.
    let local_files = [
        repo.common.join("config"),
        repo.common.join("config.worktree"),
    ];
    if may_hold(&local_files, direction) {
        move_keys(&Scope::Local(repo.common.clone()), direction, git)?;
    }
    match direction {
        Direction::Forward => std::fs::write(
            repo.marker(),
            format!("{}-v1\n{}\n", n::MIGRATED_MARKER, env!("CARGO_PKG_VERSION")),
        )
        .map_err(|e| format!("could not write {}: {e}", repo.marker().display())),
        Direction::Reverse => match std::fs::remove_file(repo.marker()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("could not remove {}: {e}", repo.marker().display()))
            }
            _ => Ok(()),
        },
    }
}

/// Move every name of [`MOVES`] in `dir`, in `direction`.
pub fn move_state(dir: &Path, direction: Direction) -> Result<(), String> {
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
        if source.is_dir() && target.is_dir() {
            merge_dirs(&source, &target)?;
            continue;
        }
        if target.exists() {
            return Err(format!(
                "both {} and {} exist; keep one (the newer is {}), then run the command again",
                source.display(),
                target.display(),
                target.display()
            ));
        }
        if let Some((old_header, new_header)) = header {
            rewrite_header(&source, old_header, new_header)?;
        }
        std::fs::rename(&source, &target).map_err(|e| {
            format!(
                "could not move {} to {}: {e}",
                source.display(),
                target.display()
            )
        })?;
    }
    Ok(())
}

/// Move what `source` holds into `target`, which already exists (a tool or
/// a test of the other version wrote there first): a child only in
/// `source` is moved; a `.log` in both is appended, `source`'s lines first;
/// any other clash is refused before anything moves. `source` is removed
/// once empty.
fn merge_dirs(source: &Path, target: &Path) -> Result<(), String> {
    let read = |dir: &Path| -> Result<Vec<PathBuf>, String> {
        let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
            .map_err(|e| format!("could not read {}: {e}", dir.display()))?
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
            return Err(format!(
                "both {} and {} exist; keep one, then run the command again",
                child.display(),
                there.display()
            ));
        }
    }
    for child in children {
        let there = target.join(child.file_name().unwrap_or_default());
        if child.is_dir() && there.is_dir() {
            merge_dirs(&child, &there)?;
        } else if there.exists() {
            let mut joined = std::fs::read(&child)
                .map_err(|e| format!("could not read {}: {e}", child.display()))?;
            joined.extend(
                std::fs::read(&there)
                    .map_err(|e| format!("could not read {}: {e}", there.display()))?,
            );
            std::fs::write(&there, joined)
                .map_err(|e| format!("could not write {}: {e}", there.display()))?;
            std::fs::remove_file(&child)
                .map_err(|e| format!("could not remove {}: {e}", child.display()))?;
        } else {
            std::fs::rename(&child, &there).map_err(|e| {
                format!(
                    "could not move {} to {}: {e}",
                    child.display(),
                    there.display()
                )
            })?;
        }
    }
    std::fs::remove_dir(source).map_err(|e| format!("could not remove {}: {e}", source.display()))
}

/// Replace every line of `file` that is exactly `old` with `new`, through a
/// temporary file and a rename, so a crash leaves one whole version.
fn rewrite_header(file: &Path, old: &str, new: &str) -> Result<(), String> {
    if !file.is_file() {
        return Ok(());
    }
    let text =
        std::fs::read(file).map_err(|e| format!("could not read {}: {e}", file.display()))?;
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
    let tmp = file.with_extension("greengate-migrate.tmp");
    std::fs::write(&tmp, out).map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, file).map_err(|e| format!("could not replace {}: {e}", file.display()))
}

/// Where config keys are moved.
#[derive(Debug, Clone)]
pub enum Scope {
    /// The repository's own config (`--local`), the common dir given.
    Local(PathBuf),
    /// The person's global config (`--global`).
    Global,
}

/// A git command for [`move_keys`]: `(GIT_DIR or None, args)` → stdout, or
/// `None` when git failed. A seam, so the tests drive a scratch repository.
pub type GitFn<'a> = dyn Fn(Option<&Path>, &[&str]) -> Option<String> + 'a;

/// Move the `amont.*` keys of `scope` to `greengate.*` (or back). A key the
/// target already holds keeps its own values: the old ones are dropped, the
/// new name wins. `amont.agent.*` is greenpiste's, never moved. A
/// `cluster.namespace` of `amont-gates` is dropped on the way forward: 4.0's
/// default namespace is `greengate-gates`.
pub fn move_keys(scope: &Scope, direction: Direction, git: &GitFn<'_>) -> Result<(), String> {
    let (from, to) = match direction {
        Direction::Forward => (old::CONFIG_PREFIX, n::CONFIG_PREFIX),
        Direction::Reverse => (n::CONFIG_PREFIX, old::CONFIG_PREFIX),
    };
    let (dir, flag) = match scope {
        Scope::Local(common) => (Some(common.as_path()), "--local"),
        Scope::Global => (None, "--global"),
    };
    let pattern = format!("^{}", from.replace('.', r"\."));
    let listed = git(
        dir,
        &["config", flag, "--name-only", "--get-regexp", &pattern],
    )
    .unwrap_or_default();
    let mut keys: Vec<&str> = listed.lines().filter(|k| !k.is_empty()).collect();
    keys.dedup();
    for key in keys {
        if key.starts_with(old::FOREIGN_PREFIX) {
            continue;
        }
        let rest = &key[from.len()..];
        let target = format!("{to}{rest}");
        let target_set = git(dir, &["config", flag, "--get-all", &target]).is_some();
        if !target_set {
            let values = git(dir, &["config", flag, "--get-all", key]).unwrap_or_default();
            for value in values.lines() {
                let dropped = direction == Direction::Forward
                    && rest.eq_ignore_ascii_case("cluster.namespace")
                    && value == old::CLUSTER_NAMESPACE;
                if dropped {
                    continue;
                }
                git(dir, &["config", flag, "--add", &target, value]).ok_or_else(|| {
                    format!("could not set {target} in the {} config", &flag[2..])
                })?;
            }
        }
        git(dir, &["config", flag, "--unset-all", key])
            .ok_or_else(|| format!("could not remove {key} from the {} config", &flag[2..]))?;
    }
    Ok(())
}

/// The machine's global keys, once: a marker in greengate's config folder,
/// written under a lock there. A held `~/.gitconfig.lock` (another git
/// writing) is waited for, a few seconds at most.
fn ensure_global() -> Result<(), String> {
    let Some(dir) = config_dir() else {
        return Ok(());
    };
    let marker = dir.join(n::MIGRATED_MARKER);
    if marker.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not make {}: {e}", dir.display()))?;
    let _lock = lock(&dir.join(n::MIGRATE_LOCK))?;
    if marker.exists() {
        return Ok(());
    }
    if !may_hold(&global_config_files(), Direction::Forward) {
        return std::fs::write(&marker, format!("{}-v1\n", n::MIGRATED_MARKER))
            .map_err(|e| format!("could not write {}: {e}", marker.display()));
    }
    let mut last = Ok(());
    for _ in 0..50 {
        last = move_keys(&Scope::Global, Direction::Forward, &system_git);
        if last.is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    last?;
    std::fs::write(&marker, format!("{}-v1\n", n::MIGRATED_MARKER))
        .map_err(|e| format!("could not write {}: {e}", marker.display()))
}

/// Whether any of `files` names a section of the namespace `direction`
/// moves from: `[amont]`, `[amont "…"]` (forward) or the greengate ones
/// (reverse), in any case. A file that cannot be read may hold anything.
/// An `include.path` is read by git and not here: a key reached only
/// through an include is left for `greengate doctor` to name.
fn may_hold(files: &[PathBuf], direction: Direction) -> bool {
    let section = match direction {
        Direction::Forward => "[amont",
        Direction::Reverse => "[greengate",
    };
    files.iter().any(|f| match std::fs::read(f) {
        Ok(bytes) => String::from_utf8_lossy(&bytes)
            .to_ascii_lowercase()
            .lines()
            .any(|l| {
                let l = l.trim_start();
                l.starts_with(section)
                    && matches!(
                        l[section.len()..].chars().next(),
                        Some(']' | ' ' | '"' | '.')
                    )
            }),
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    })
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
pub fn reverse_global() -> Result<(), String> {
    move_keys(&Scope::Global, Direction::Reverse, &system_git)?;
    if let Some(dir) = config_dir() {
        let _ = std::fs::remove_file(dir.join(n::MIGRATED_MARKER));
    }
    Ok(())
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
/// one line each, with the command that moves them.
pub fn leftovers(cwd: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut env: Vec<String> = std::env::vars_os()
        .filter_map(|(k, _)| k.into_string().ok())
        .filter(|k| k.starts_with("AMONT_") && !k.starts_with("AMONT_AGENT_"))
        .collect();
    env.sort();
    for k in env {
        let rest = &k["AMONT_".len()..];
        let new = match rest {
            "HOST_SLOT" | "SLOT_DIR" => format!("GREENHARNESS_{rest}"),
            _ => format!("{}_{rest}", n::ENV_PREFIX),
        };
        out.push(format!(
            "environment: {k} is ignored by greengate 4.0; set {new} instead"
        ));
    }
    let pattern = r"^amont\.";
    if let Some(listed) = system_git(
        None,
        &["config", "--global", "--name-only", "--get-regexp", pattern],
    ) {
        for key in listed
            .lines()
            .filter(|k| !k.starts_with(old::FOREIGN_PREFIX))
        {
            out.push(format!(
                "global config: {key} is ignored by greengate 4.0; `{} migrate` moves it",
                n::BIN
            ));
        }
    }
    if let Some(repo) = Repo::locate(cwd) {
        if let Some(listed) = system_git(
            Some(&repo.common),
            &["config", "--local", "--name-only", "--get-regexp", pattern],
        ) {
            for key in listed
                .lines()
                .filter(|k| !k.starts_with(old::FOREIGN_PREFIX))
            {
                out.push(format!(
                    "local config: {key} is ignored by greengate 4.0; `{} migrate` moves it",
                    n::BIN
                ));
            }
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
            if let Some(store) = old_held_store(dir) {
                out.push(format!(
                    "state: {} holds parked changes; `{} restore` puts them back",
                    store.display(),
                    n::BIN
                ));
            }
        }
    }
    out
}

/// git, with `GIT_DIR` set to `dir` when one is given: stdout on success,
/// `None` on a failure or when git cannot run.
pub fn system_git(dir: Option<&Path>, args: &[&str]) -> Option<String> {
    let mut cmd = std::process::Command::new("git");
    if let Some(dir) = dir {
        cmd.env("GIT_DIR", dir);
    }
    let out = cmd
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// An exclusive lock on `path`, waited for (a migration takes well under a
/// second); released when the guard drops, and by the kernel if the process
/// dies, so a crash never leaves the repository locked.
#[cfg(unix)]
fn lock(path: &Path) -> Result<std::fs::File, String> {
    use std::os::unix::io::AsRawFd;
    extern "C" {
        #[link_name = "flock"]
        fn libc_flock(fd: i32, op: i32) -> i32;
    }
    const LOCK_EX: i32 = 2;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|e| format!("could not open {}: {e}", path.display()))?;
    // SAFETY: a valid fd we own; flock touches no memory.
    if unsafe { libc_flock(file.as_raw_fd(), LOCK_EX) } != 0 {
        return Err(format!("could not lock {}", path.display()));
    }
    Ok(file)
}

/// No `flock` here: the migration runs unlocked, as every other writer of
/// this state does on Windows.
#[cfg(not(unix))]
fn lock(path: &Path) -> Result<std::fs::File, String> {
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|e| format!("could not open {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("greengate-migrate-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn git_in(repo: &Path) -> impl Fn(Option<&Path>, &[&str]) -> Option<String> + '_ {
        move |_dir, args| {
            let out = std::process::Command::new("git")
                .current_dir(repo)
                .env("GIT_CONFIG_GLOBAL", repo.join("global-config"))
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .args(args)
                .output()
                .ok()?;
            out.status
                .success()
                .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
        }
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
        migrate_repo(&repo, Direction::Forward, &git).unwrap();

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
        migrate_repo(&repo, Direction::Forward, &git).unwrap();
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
        assert!(err.contains("both"), "{err}");
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
        assert_eq!(repo.worktrees, vec![wt.clone()]);
        std::fs::create_dir_all(wt.join("amont-held")).unwrap();
        let git = git_in(&root);
        let err = migrate_repo(&repo, Direction::Forward, &git).unwrap_err();
        assert!(err.contains("restore"), "{err}");
        assert!(
            wt.join("amont-skew").is_file(),
            "nothing moved while refused"
        );
        assert!(!repo.marker().exists());
        std::fs::remove_dir(wt.join("amont-held")).unwrap();
        migrate_repo(&repo, Direction::Forward, &git).unwrap();
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
            ["config", "--add", "amont.trusted", "bbb"],
            ["config", "--add", "amont.timeout", "600"],
            ["config", "--add", "greengate.timeout", "900"],
            ["config", "--add", "amont.cluster.namespace", "amont-gates"],
            ["config", "--add", "amont.agent.suppressions", "allow"],
        ] {
            git(None, &args).unwrap();
        }
        let repo = Repo::locate_with(&root, None).unwrap();
        migrate_repo(&repo, Direction::Forward, &git).unwrap();
        assert_eq!(
            git(None, &["config", "--get-all", "greengate.trusted"]).unwrap(),
            "aaa\nbbb\n"
        );
        assert_eq!(
            git(None, &["config", "--get", "greengate.timeout"]).unwrap(),
            "900\n"
        );
        assert_eq!(
            git(
                None,
                &["config", "--get-all", "greengate.cluster.namespace"]
            ),
            None
        );
        assert_eq!(git(None, &["config", "--get", "amont.trusted"]), None);
        assert_eq!(git(None, &["config", "--get", "amont.timeout"]), None);
        assert_eq!(
            git(None, &["config", "--get", "amont.agent.suppressions"]).unwrap(),
            "allow\n"
        );

        // And back: the reverse restores the 3.x names.
        migrate_repo(&repo, Direction::Reverse, &git).unwrap();
        assert_eq!(
            git(None, &["config", "--get-all", "amont.trusted"]).unwrap(),
            "aaa\nbbb\n"
        );
        assert_eq!(git(None, &["config", "--get", "greengate.trusted"]), None);
        assert!(!repo.marker().exists());
    }

    #[test]
    fn only_a_file_naming_the_namespace_may_hold_its_keys() {
        let root = scratch("may-hold");
        let f = root.join("config");
        std::fs::write(&f, "[core]\n\tbare = false\n[amontx]\n\ta = 1\n").unwrap();
        assert!(!may_hold(std::slice::from_ref(&f), Direction::Forward));
        for section in ["[amont]", "[Amont \"severity\"]", "  [amont.cluster]"] {
            std::fs::write(&f, format!("[core]\n{section}\n\tx = 1\n")).unwrap();
            assert!(
                may_hold(std::slice::from_ref(&f), Direction::Forward),
                "{section}"
            );
        }
        assert!(!may_hold(std::slice::from_ref(&f), Direction::Reverse));
        assert!(!may_hold(&[root.join("absent")], Direction::Forward));
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
        assert!(move_state(&gd, Direction::Forward)
            .unwrap_err()
            .contains("both"));
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
