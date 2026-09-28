//! Make a fresh snapshot runnable: carry the untracked files a suite needs,
//! then prepare its JavaScript dependencies.
//!
//! A snapshot is a checkout, not a workspace — no `node_modules`, no `.env` —
//! and a gate started there fails having tested nothing. Both steps run in
//! [`crate::pushed_tree::PushedTree::prepare`], before `amont.snapshotPrepare`
//! (the escape hatch), and a failure in either is the snapshot's failure: no
//! suite runs, nothing is stamped.
//!
//! ## What a stamp may rest on
//!
//! A stamp claims the gate passed on the COMMITTED content. So nothing here
//! may put content from the developer's tree where committed content lives:
//! a carried path must be untracked in the snapshot's own index, and the
//! dependencies must be the ones the committed lockfile describes.
//!
//! The default, `install`, is what CI does: `npm ci`, `pnpm install
//! --frozen-lockfile`. `reuse` (pnpm only; see `reuse` for why npm is not
//! reusable) clones the working tree's `node_modules` and keeps the clone
//! only when it has pnpm's isolated layout and a frozen offline install
//! accepts it against the snapshot's manifests and lockfile — which catches
//! a missing or wrong-version package, a stray directory, and a manifest the
//! lockfile does not satisfy, and does NOT catch a package whose files were
//! edited in place without a version change. That limit is why reuse is
//! opt-in.
//!
//! Every question about a manifest goes to the package manager, through exit
//! codes and parseable output: this crate is dependency-free and has no JSON
//! or YAML parser, and a hand-rolled one reading `workspaces` would be a
//! second, divergent answer to a question npm already answers.

use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use crate::ui::{highlight, warning_sign};

const CARRY: &str = "amont.snapshotCarry";
const DEPS: &str = "amont.snapshotDeps";

/// `amont.snapshotDeps`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepsMode {
    /// `npm ci` / `pnpm install --frozen-lockfile` in the snapshot.
    Install,
    /// Clone the working tree's install, keep it only if the manager accepts it.
    Reuse,
    /// Prepare nothing.
    Off,
}

pub fn deps_mode(settings: &crate::config::Settings) -> DepsMode {
    match crate::config::enumerated_or(settings, DEPS, &["install", "reuse", "off"], "install") {
        "reuse" => DepsMode::Reuse,
        "off" => DepsMode::Off,
        _ => DepsMode::Install,
    }
}

/// `amont.snapshotCarry`, split on whitespace and commas.
pub fn carry_list(settings: &crate::config::Settings) -> Vec<String> {
    crate::config::string_value(settings, CARRY)
        .map(|v| {
            v.split(|c: char| c.is_whitespace() || c == ',')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The snapshot's own tracked files, `-z`, with git's environment stripped:
/// a hook's `GIT_DIR`/`GIT_INDEX_FILE` would otherwise answer for the
/// developer's repository, and the pushed tip — not HEAD, not the index the
/// developer is staging into — decides what the snapshot holds.
pub fn snapshot_files(snapshot: &Path) -> Result<Vec<String>, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(snapshot)
        .args(["ls-files", "-z"])
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    crate::hooks::common::strip_git_env(&mut cmd);
    let out = cmd
        .output()
        .map_err(|e| format!("could not list the snapshot's files: {e}"))?;
    if !out.status.success() {
        return Err("could not list the snapshot's files".to_string());
    }
    Ok(crate::git::split_nul_paths(&out.stdout))
}

// ---------------------------------------------------------------------------
// carry
// ---------------------------------------------------------------------------

/// One validated carry entry.
struct Carried {
    entry: String,
    src: PathBuf,
    dst: PathBuf,
}

/// Why one carry entry is refused, or `None` when it is lexically a
/// repo-relative path with nothing to climb out through.
fn lexical_problem(entry: &str) -> Option<&'static str> {
    if entry.is_empty() {
        return Some("empty");
    }
    let bytes = entry.as_bytes();
    if entry.starts_with('/') || entry.starts_with('\\') || (bytes.len() >= 2 && bytes[1] == b':') {
        return Some("not a repo-relative path");
    }
    for part in entry.split(['/', '\\']) {
        if part.is_empty() {
            return Some("has an empty path component");
        }
        if part == "." || part == ".." {
            return Some("has a `.` or `..` component");
        }
        if part.eq_ignore_ascii_case(".git") {
            return Some("is inside .git");
        }
    }
    None
}

/// Is `entry` — or anything inside it, or any directory above it — tracked in
/// the snapshot? Carrying it would put the developer's copy over committed
/// content, and the stamp would vouch for a tree nobody committed.
fn tracked_collision(entry: &str, files: &[String]) -> Option<String> {
    let inside = format!("{entry}/");
    files
        .iter()
        .find(|f| {
            f.as_str() == entry || f.starts_with(&inside) || entry.starts_with(&format!("{f}/"))
        })
        .cloned()
}

fn is_symlink(p: &Path) -> bool {
    std::fs::symlink_metadata(p)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// A symlinked directory between `root` and `rel`'s last component — which
/// would take a read, or a write, somewhere outside the tree it names.
fn symlinked_ancestor(root: &Path, rel: &Path) -> Option<PathBuf> {
    let mut at = root.to_path_buf();
    let parts: Vec<_> = rel.components().collect();
    for part in parts.iter().take(parts.len().saturating_sub(1)) {
        at.push(part.as_os_str());
        if is_symlink(&at) {
            return Some(at);
        }
    }
    None
}

/// A symlink anywhere inside `dir`, not followed.
fn symlink_inside(dir: &Path) -> std::io::Result<Option<PathBuf>> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Ok(Some(entry.path()));
        }
        if kind.is_dir() {
            if let Some(found) = symlink_inside(&entry.path())? {
                return Ok(Some(found));
            }
        }
    }
    Ok(None)
}

fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.is_dir() {
        std::fs::create_dir(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_tree(&entry.path(), &dst.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(src, dst).map(|_| ())
    }
}

/// Copy the untracked files `amont.snapshotCarry` names from `source` into
/// `snapshot`.
///
/// Every entry is validated before anything is copied, and every refusal is
/// reported in ONE message — `config::complain` says a key once, which would
/// hide all but the first bad entry. Any refusal fails preparation: a carry
/// the author got wrong must be loud, never a silently thinner snapshot that
/// still earns a stamp. An entry that is simply absent is skipped (a `.env`
/// exists on a laptop and not in CI); one that is present and cannot be
/// copied fails.
pub fn carry(
    entries: &[String],
    source: &Path,
    snapshot: &Path,
    files: &[String],
) -> Result<(), String> {
    let mut refused: Vec<String> = Vec::new();
    let mut missing: Vec<&str> = Vec::new();
    let mut plan: Vec<Carried> = Vec::new();
    for entry in entries {
        let norm = entry.replace('\\', "/");
        let norm = norm.trim_end_matches('/').to_string();
        if let Some(why) = lexical_problem(&norm) {
            refused.push(format!("{entry}: {why}"));
            continue;
        }
        if let Some(f) = tracked_collision(&norm, files) {
            refused.push(format!(
                "{entry}: tracked content ({f}) — only untracked files can be carried"
            ));
            continue;
        }
        let rel = PathBuf::from(&norm);
        let src = source.join(&rel);
        let dst = snapshot.join(&rel);
        if let Some(link) = symlinked_ancestor(source, &rel) {
            refused.push(format!(
                "{entry}: {} is a symlink in the working tree",
                link.display()
            ));
            continue;
        }
        if let Some(link) = symlinked_ancestor(snapshot, &rel) {
            refused.push(format!(
                "{entry}: {} is a symlink in the snapshot",
                link.display()
            ));
            continue;
        }
        let meta = match std::fs::symlink_metadata(&src) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                missing.push(entry);
                continue;
            }
            Err(e) => {
                refused.push(format!("{entry}: cannot be read: {e}"));
                continue;
            }
        };
        if meta.file_type().is_symlink() {
            refused.push(format!("{entry}: is a symlink"));
            continue;
        }
        if meta.is_dir() {
            match symlink_inside(&src) {
                Ok(Some(link)) => {
                    refused.push(format!("{entry}: holds a symlink ({})", link.display()));
                    continue;
                }
                Ok(None) => {}
                Err(e) => {
                    refused.push(format!("{entry}: cannot be read: {e}"));
                    continue;
                }
            }
        }
        if std::fs::symlink_metadata(&dst).is_ok() {
            refused.push(format!("{entry}: already exists in the snapshot"));
            continue;
        }
        plan.push(Carried {
            entry: entry.clone(),
            src,
            dst,
        });
    }
    if !refused.is_empty() {
        return Err(format!(
            "{CARRY} refused {}: {}",
            if refused.len() == 1 {
                "an entry".to_string()
            } else {
                format!("{} entries", refused.len())
            },
            refused.join("; ")
        ));
    }
    if !missing.is_empty() {
        println!(
            "snapshot: not carrying {} — not in the working tree",
            missing.join(", ")
        );
    }
    for c in &plan {
        if let Some(parent) = c.dst.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{CARRY}: {}: {e}", c.entry))?;
        }
        copy_tree(&c.src, &c.dst)
            .map_err(|e| format!("{CARRY}: {} could not be copied: {e}", c.entry))?;
    }
    if !plan.is_empty() {
        let names: Vec<&str> = plan.iter().map(|c| c.entry.as_str()).collect();
        println!("snapshot: carried {}", names.join(", "));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// dependencies
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Manager {
    Npm,
    Pnpm,
}

impl Manager {
    fn lockfile(self) -> &'static str {
        match self {
            Manager::Npm => "package-lock.json",
            Manager::Pnpm => "pnpm-lock.yaml",
        }
    }
    fn tool(self) -> &'static str {
        match self {
            Manager::Npm => "npm",
            Manager::Pnpm => "pnpm",
        }
    }
}

/// One installation unit: a directory holding a tracked lockfile, and the
/// manager that reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    /// Repo-relative, `""` for the root.
    pub dir: String,
    pub manager: Manager,
    /// pnpm only: a lockfile with no `pnpm-workspace.yaml` beside it is a
    /// standalone project, even inside a workspace (see `audit::pnpm_args`).
    pub standalone: bool,
}

/// The directories of `files` named exactly `name`, repo-relative (`""` for
/// the root), sorted and deduplicated. Pure: the caller decides WHICH index
/// the list comes from — the snapshot's, for a snapshot.
pub fn lockfile_dirs_in(files: &[String], name: &str) -> Vec<String> {
    let mut dirs: Vec<String> = files
        .iter()
        .filter(|p| p.rsplit('/').next().unwrap_or(p) == name)
        .map(|p| {
            p.rsplit_once('/')
                .map(|(d, _)| d.to_string())
                .unwrap_or_default()
        })
        .collect();
    dirs.sort();
    dirs.dedup();
    dirs
}

fn join_rel(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// The `packageManager` field of a package.json, by a targeted scan of that
/// one key — `Some("npm")`/`Some("pnpm")` when it names one of them without
/// ambiguity, `None` otherwise.
pub fn declared_manager(package_json: &str) -> Option<Manager> {
    let key = "\"packageManager\"";
    let mut hits = package_json.match_indices(key);
    let (at, _) = hits.next()?;
    if hits.next().is_some() {
        return None;
    }
    let rest = package_json[at + key.len()..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let value = &rest[..rest.find('"')?];
    if value.starts_with("pnpm@") || value == "pnpm" {
        Some(Manager::Pnpm)
    } else if value.starts_with("npm@") || value == "npm" {
        Some(Manager::Npm)
    } else {
        None
    }
}

/// Every installation unit in `files`. A directory holding BOTH lockfiles is
/// settled by `packageManager`, or refused: guessing would install the wrong
/// tree and test it as if it were the right one.
pub fn units(files: &[String], snapshot: &Path) -> Result<Vec<Unit>, String> {
    let npm = lockfile_dirs_in(files, Manager::Npm.lockfile());
    let pnpm = lockfile_dirs_in(files, Manager::Pnpm.lockfile());
    let mut out = Vec::new();
    let mut dirs: Vec<&String> = npm.iter().chain(pnpm.iter()).collect();
    dirs.sort();
    dirs.dedup();
    for dir in dirs {
        let manager = match (npm.contains(dir), pnpm.contains(dir)) {
            (true, true) => {
                let manifest = snapshot.join(join_rel(dir, "package.json"));
                let body = std::fs::read_to_string(&manifest).unwrap_or_default();
                declared_manager(&body).ok_or_else(|| {
                    format!(
                        "both package-lock.json and pnpm-lock.yaml in {} and no \
                         `packageManager` to choose — set amont.snapshotPrepare",
                        if dir.is_empty() { "the root" } else { dir }
                    )
                })?
            }
            (true, false) => Manager::Npm,
            _ => Manager::Pnpm,
        };
        let standalone =
            manager == Manager::Pnpm && !files.contains(&join_rel(dir, "pnpm-workspace.yaml"));
        out.push(Unit {
            dir: dir.clone(),
            manager,
            standalone,
        });
    }
    Ok(out)
}

fn unit_label(u: &Unit) -> String {
    if u.dir.is_empty() {
        "the root".to_string()
    } else {
        format!("{}/", u.dir)
    }
}

fn manager_cmd(u: &Unit, at: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(crate::hooks::common::program(u.manager.tool()));
    cmd.args(args).current_dir(at).stdin(Stdio::null());
    if u.manager == Manager::Pnpm {
        // A purge prompt cannot be answered from a hook, and aborts.
        cmd.arg("--config.confirmModulesPurge=false");
        if u.standalone {
            cmd.arg("--ignore-workspace");
        }
    }
    crate::hooks::common::strip_git_env(&mut cmd);
    cmd
}

/// Run a manager command quietly; `Ok(output)` on exit 0, `Err(output)`
/// otherwise — the output is what the caller quotes when it says why.
fn quiet(settings: &crate::config::Settings, mut cmd: Command) -> Result<String, String> {
    match crate::hooks::common::capture_within(settings, &mut cmd) {
        Some((crate::hooks::common::Ran::Status(s), text)) if s.success() => Ok(text),
        Some((_, text)) => Err(text),
        None => Err("could not start".to_string()),
    }
}

/// The last meaningful line of a tool's output, for a one-line reason.
fn last_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or("no output")
        .chars()
        .take(200)
        .collect()
}

fn install(settings: &crate::config::Settings, u: &Unit, at: &Path) -> Result<(), String> {
    let args: &[&str] = match u.manager {
        Manager::Npm => &["ci", "--no-audit", "--no-fund", "--prefer-offline"],
        Manager::Pnpm => &["install", "--frozen-lockfile", "--prefer-offline"],
    };
    let mut cmd = manager_cmd(u, at, args);
    let what = format!("{} {}", u.manager.tool(), args[0]);
    if crate::hooks::common::bounded_success(settings, &mut cmd, &what) {
        Ok(())
    } else {
        Err(format!("{what} failed in {}", unit_label(u)))
    }
}

/// Clone a directory by spawning the platform's copy tool by ABSOLUTE path:
/// a GNU `cp` first on PATH has no `-c`. APFS clones with `-c`, btrfs/xfs
/// reflink with `--reflink=auto` (a plain copy elsewhere); symlinks are
/// copied as symlinks either way.
fn clone_dir(src: &Path, dst: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let attempts: &[&[&str]] = &[&["/bin/cp", "-cR"], &["/bin/cp", "-R"]];
    #[cfg(all(unix, not(target_os = "macos")))]
    let attempts: &[&[&str]] = &[
        &["/bin/cp", "-R", "--reflink=auto"],
        &["/usr/bin/cp", "-R", "--reflink=auto"],
    ];
    #[cfg(not(unix))]
    let attempts: &[&[&str]] = &[];
    for argv in attempts {
        if !Path::new(argv[0]).exists() {
            continue;
        }
        let ok = Command::new(argv[0])
            .args(&argv[1..])
            .arg(src)
            .arg(dst)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return Ok(());
        }
        let _ = std::fs::remove_dir_all(dst);
    }
    Err(format!("could not clone {}", src.display()))
}

/// Lexically resolve `p` — `.` and `..` folded, nothing touched on disk.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The first symlink under `dir` that resolves into `source` — a clone
/// still reading the developer's tree through a link is not isolated,
/// whatever else it preserved.
fn link_into(dir: &Path, sources: &[PathBuf]) -> std::io::Result<Option<PathBuf>> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        for entry in std::fs::read_dir(&at)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let path = entry.path();
            if kind.is_symlink() {
                let target = std::fs::read_link(&path)?;
                let resolved = if target.is_absolute() {
                    normalize(&target)
                } else {
                    normalize(&at.join(target))
                };
                // Lexically AND through every link: `/var` is itself a
                // symlink on macOS, so the two spellings of one directory
                // must both be caught.
                let real = path.canonicalize().ok();
                if sources.iter().any(|s| {
                    resolved.starts_with(s) || real.as_ref().is_some_and(|r| r.starts_with(s))
                }) {
                    return Ok(Some(path));
                }
            } else if kind.is_dir() {
                stack.push(path);
            }
        }
    }
    Ok(None)
}

/// The first regular file under `nm` modified after `record`, pnpm's
/// install record (`.modules.yaml`, rewritten as an install finishes).
///
/// Skipped, because an install or the tools legitimately write them after
/// the record: every `.bin/` (pnpm relinks it last), `.cache/` and
/// `.vite*/` (tool caches), and pnpm's own `.pnpm/lock.yaml`,
/// `.pnpm-workspace-state*` and `.modules.yaml`. Symlinks are not followed —
/// a top-level package is a link into `.pnpm`, which is walked itself.
fn newer_than(nm: &Path, record: std::time::SystemTime) -> std::io::Result<Option<PathBuf>> {
    if !nm.is_dir() {
        return Ok(None);
    }
    let mut stack = vec![nm.to_path_buf()];
    while let Some(at) = stack.pop() {
        for entry in std::fs::read_dir(&at)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let kind = entry.file_type()?;
            if kind.is_dir() {
                if name == ".bin" || name == ".cache" || name.starts_with(".vite") {
                    continue;
                }
                stack.push(entry.path());
            } else if kind.is_file() {
                if name == ".modules.yaml"
                    || name.starts_with(".pnpm-workspace-state")
                    || (name == "lock.yaml" && at.file_name().is_some_and(|d| d == ".pnpm"))
                {
                    continue;
                }
                if entry.metadata()?.modified()? > record {
                    return Ok(Some(entry.path()));
                }
            }
        }
    }
    Ok(None)
}

/// Workspace members of a pnpm unit, as pnpm lists them, relative to the
/// unit dir. Asked in the SNAPSHOT: the committed manifests decide.
fn members(settings: &crate::config::Settings, u: &Unit, at: &Path) -> Result<Vec<String>, String> {
    if u.standalone {
        return Ok(Vec::new());
    }
    let text = quiet(
        settings,
        manager_cmd(u, at, &["ls", "-r", "--depth", "-1", "--parseable"]),
    )
    .map_err(|t| format!("pnpm could not list the workspace: {}", last_line(&t)))?;
    let root = normalize(at);
    let canon = at.canonicalize().unwrap_or_else(|_| root.clone());
    Ok(text
        .lines()
        .filter_map(|l| {
            let p = PathBuf::from(l.trim());
            let rel = p
                .strip_prefix(&root)
                .or_else(|_| p.strip_prefix(&canon))
                .ok()?;
            let rel = rel.to_string_lossy().replace('\\', "/");
            (!rel.is_empty()).then_some(rel)
        })
        .collect())
}

/// Is `nm` laid out the way pnpm's default (`isolated`) linker lays it out —
/// every package a link, `@scope` directories holding only links? pnpm's own
/// check does not look at a directory it did not create, and a stray one
/// here is importable by the suite; so anything else is not reused.
fn pnpm_layout_problem(nm: &Path, is_root: bool) -> Option<String> {
    if is_root {
        let modules = std::fs::read_to_string(nm.join(".modules.yaml")).unwrap_or_default();
        let isolated = modules
            .lines()
            .any(|l| l.contains("nodeLinker") && l.contains("isolated"));
        if !isolated {
            return Some("not installed with pnpm's isolated linker".to_string());
        }
    }
    let entries = std::fs::read_dir(nm).ok()?;
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let kind = e.file_type().ok()?;
        if kind.is_symlink() {
            continue;
        }
        if name.starts_with('@') && kind.is_dir() {
            for inner in std::fs::read_dir(e.path()).ok()?.flatten() {
                if !inner.file_type().map(|k| k.is_symlink()).unwrap_or(false) {
                    return Some(format!(
                        "{} is not a link pnpm made",
                        inner.path().display()
                    ));
                }
            }
            continue;
        }
        return Some(format!("{} is not a link pnpm made", e.path().display()));
    }
    None
}

/// Try to reuse the working tree's install for one unit. `Ok(())` when the
/// clone is in place and pnpm accepted it; `Err(reason)` — with the snapshot
/// left exactly as it was — when it cannot be.
///
/// npm is never reused. `npm ls` answers "is this dependency graph valid",
/// not "is this the tree the lockfile describes": a real repository with
/// peer-range conflicts `npm ci` installs without complaint fails `npm ls`
/// on a fresh install (application-landscape: 182 findings), so it can
/// neither accept a correct clone nor be trusted to reject a wrong one.
fn reuse(
    settings: &crate::config::Settings,
    u: &Unit,
    source: &Path,
    snapshot: &Path,
    unit_dirs: &[String],
) -> Result<(), String> {
    if u.manager == Manager::Npm {
        return Err(
            "npm cannot check an installed tree against its lockfile, so npm \
                    snapshots always install"
                .to_string(),
        );
    }
    let src_dir = source.join(&u.dir);
    let snap_dir = snapshot.join(&u.dir);
    let lock = u.manager.lockfile();
    let src_lock = std::fs::read(src_dir.join(lock))
        .map_err(|_| "no lockfile in the working tree".to_string())?;
    let snap_lock = std::fs::read(snap_dir.join(lock)).map_err(|e| format!("{lock}: {e}"))?;
    if src_lock != snap_lock {
        return Err("the working tree's lockfile differs from the commit's".to_string());
    }
    if !src_dir.join("node_modules").is_dir() {
        return Err("nothing installed in the working tree".to_string());
    }
    let mut set = vec![String::new()];
    for m in members(settings, u, &snap_dir)? {
        let repo_rel = join_rel(&u.dir, &m);
        // A member with a lockfile of its own is its own unit.
        if unit_dirs.contains(&repo_rel) {
            continue;
        }
        if src_dir.join(&m).join("node_modules").is_dir() {
            set.push(m);
        }
    }
    // Checked on the SOURCE, before anything is copied: a file changed
    // since pnpm finished installing is an edit pnpm's own check will not
    // see, and a clone would carry it into a stamped snapshot.
    let record = std::fs::metadata(src_dir.join("node_modules/.modules.yaml"))
        .and_then(|m| m.modified())
        .map_err(|_| "no pnpm install record (node_modules/.modules.yaml)".to_string())?;
    for m in &set {
        match newer_than(&src_dir.join(m).join("node_modules"), record) {
            Ok(None) => {}
            Ok(Some(f)) => {
                let shown = f.strip_prefix(source).unwrap_or(&f);
                return Err(format!(
                    "{} changed after the install — an in-place edit pnpm does not check",
                    shown.display()
                ));
            }
            Err(e) => return Err(format!("could not inspect the working tree's install: {e}")),
        }
    }
    let mut cloned: Vec<PathBuf> = Vec::new();
    let rollback = |cloned: &[PathBuf]| {
        for d in cloned {
            let _ = std::fs::remove_dir_all(d);
        }
    };
    let sources = [
        normalize(source),
        source.canonicalize().unwrap_or_else(|_| normalize(source)),
    ];
    for m in &set {
        let from = src_dir.join(m).join("node_modules");
        let to = snap_dir.join(m).join("node_modules");
        if std::fs::symlink_metadata(&to).is_ok() {
            rollback(&cloned);
            return Err(format!("{} already exists in the snapshot", to.display()));
        }
        if let Err(e) = clone_dir(&from, &to) {
            rollback(&cloned);
            return Err(e);
        }
        cloned.push(to.clone());
        let problem = match link_into(&to, &sources) {
            Ok(Some(link)) => Some(format!(
                "{} links back into the working tree",
                link.display()
            )),
            Ok(None) => pnpm_layout_problem(&to, m.is_empty()),
            Err(e) => Some(format!("could not inspect the clone: {e}")),
        };
        if let Some(why) = problem {
            rollback(&cloned);
            return Err(why);
        }
    }
    // pnpm records the absolute paths of the tree it installed; the clone
    // must not carry the source's.
    if let Ok(rd) = std::fs::read_dir(snap_dir.join("node_modules")) {
        for e in rd.flatten() {
            if e.file_name()
                .to_string_lossy()
                .starts_with(".pnpm-workspace-state")
            {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    // A frozen OFFLINE install is both the check and the repair: it fails on
    // a manifest the lockfile does not satisfy, relinks drift from the
    // store, and says "Already up to date" for a faithful clone.
    if let Err(text) = quiet(
        settings,
        manager_cmd(u, &snap_dir, &["install", "--frozen-lockfile", "--offline"]),
    ) {
        rollback(&cloned);
        return Err(format!(
            "pnpm did not accept the clone ({})",
            last_line(&text)
        ));
    }
    Ok(())
}

/// The files `tip` changes relative to where it forked from its upstream —
/// what the push is about. `None` when git cannot say (no upstream, a tip
/// it does not know): the caller then prepares everything.
pub fn changed_since_upstream(source: &Path, tip: &str) -> Option<Vec<String>> {
    let git = |args: &[&str]| -> Option<Vec<u8>> {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(source)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null());
        crate::hooks::common::strip_git_env(&mut cmd);
        let out = cmd.output().ok()?;
        out.status.success().then_some(out.stdout)
    };
    let base = git(&["merge-base", tip, "@{upstream}"])?;
    let base = String::from_utf8_lossy(&base).trim().to_string();
    if base.is_empty() {
        return None;
    }
    let raw = git(&["diff", "--name-only", "-z", &base, tip])?;
    Some(crate::git::split_nul_paths(&raw))
}

/// Which units a snapshot needs: the root one, and every unit that is the
/// NEAREST enclosing unit of a changed file. A unit the push does not touch
/// cannot hold a failure the push introduced — and nothing the gates run for
/// this push reads it — so installing it is pure cost (website-builder: seven
/// spikes, an install each, on every snapshot). `changed` is `None` when the
/// range is unknown, and then every unit is kept.
///
/// Returns `(kept, skipped)`.
pub fn select_units(units: Vec<Unit>, changed: Option<&[String]>) -> (Vec<Unit>, Vec<Unit>) {
    let Some(changed) = changed else {
        return (units, Vec::new());
    };
    let nearest = |f: &str| -> Option<&str> {
        units
            .iter()
            .map(|u| u.dir.as_str())
            .filter(|d| d.is_empty() || f.starts_with(&format!("{d}/")))
            .max_by_key(|d| d.len())
    };
    let touched: Vec<&str> = changed.iter().filter_map(|f| nearest(f)).collect();
    let (kept, skipped): (Vec<Unit>, Vec<Unit>) = units
        .clone()
        .into_iter()
        .partition(|u| u.dir.is_empty() || touched.contains(&u.dir.as_str()));
    (kept, skipped)
}

/// Prepare the dependencies of every unit the push needs in `snapshot`.
pub fn deps(
    settings: &crate::config::Settings,
    mode: DepsMode,
    source: &Path,
    snapshot: &Path,
    files: &[String],
    changed: Option<&[String]>,
) -> Result<(), String> {
    if mode == DepsMode::Off {
        return Ok(());
    }
    let all = units(files, snapshot)?;
    let unit_dirs: Vec<String> = all.iter().map(|u| u.dir.clone()).collect();
    let (units, skipped) = select_units(all, changed);
    if !skipped.is_empty() {
        let names: Vec<String> = skipped.iter().map(unit_label).collect();
        println!(
            "snapshot: not preparing {} — the push changes nothing there",
            names.join(", ")
        );
    }
    for u in &units {
        let at = snapshot.join(&u.dir);
        let label = unit_label(u);
        if mode == DepsMode::Reuse {
            // No clone tool on Windows: `clone_dir` says so and this falls
            // through to the install, like any other refusal.
            match reuse(settings, u, source, snapshot, &unit_dirs) {
                Ok(()) => {
                    println!(
                        "snapshot: node_modules in {label} reused from the working tree — {} \
                         accepted it against the lockfile",
                        u.manager.tool()
                    );
                    continue;
                }
                Err(why) => println!(
                    "snapshot: {} in {label} — not reusing the working tree's install: {why}",
                    highlight(&format!("{} {}", u.manager.tool(), install_verb(u.manager)))
                ),
            }
        } else {
            println!(
                "snapshot: {} in {label}",
                highlight(&format!("{} {}", u.manager.tool(), install_verb(u.manager)))
            );
        }
        install(settings, u, &at)?;
    }
    Ok(())
}

fn install_verb(m: Manager) -> &'static str {
    match m {
        Manager::Npm => "ci",
        Manager::Pnpm => "install --frozen-lockfile",
    }
}

/// Carry → dependencies → nothing else; `snapshotPrepare` runs after this,
/// in `PushedTree::prepare`. Dependencies are skipped when a
/// `snapshotPrepare` command exists: that command owns them.
pub fn run(
    settings: &crate::config::Settings,
    source: &Path,
    snapshot: &Path,
    tip: &str,
    command_owns_deps: bool,
) -> Result<(), String> {
    let entries = carry_list(settings);
    let mode = if command_owns_deps {
        DepsMode::Off
    } else {
        deps_mode(settings)
    };
    if entries.is_empty() && mode == DepsMode::Off {
        return Ok(());
    }
    let files = snapshot_files(snapshot)?;
    if !entries.is_empty() {
        carry(&entries, source, snapshot, &files)?;
    }
    let changed = if mode == DepsMode::Off {
        None
    } else {
        changed_since_upstream(source, tip)
    };
    deps(settings, mode, source, snapshot, &files, changed.as_deref()).map_err(|e| {
        println!("{} {e}", warning_sign());
        e
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn lexical_refusals() {
        for bad in [
            "",
            "/etc/hosts",
            "\\x",
            "C:/x",
            "a//b",
            ".",
            "..",
            "a/../b",
            ".git/config",
            "x/.GIT/y",
        ] {
            assert!(lexical_problem(bad).is_some(), "{bad:?} must be refused");
        }
        for good in [".env", "config/local.json", ".npmrc", "a.b/c"] {
            assert!(lexical_problem(good).is_none(), "{good:?} must pass");
        }
    }

    #[test]
    fn a_tracked_path_inside_above_or_equal_collides() {
        let files = s(&["src/a.ts", "vendor", "README.md"]);
        assert!(tracked_collision("src", &files).is_some());
        assert!(tracked_collision("src/a.ts", &files).is_some());
        assert!(tracked_collision("vendor/x", &files).is_some());
        assert!(tracked_collision(".env", &files).is_none());
        assert!(tracked_collision("sr", &files).is_none());
    }

    #[test]
    fn lockfile_dirs_come_from_the_list_given() {
        let files = s(&[
            "package-lock.json",
            "web/package-lock.json",
            "web/x/pnpm-lock.yaml",
            "notpackage-lock.json",
        ]);
        assert_eq!(
            lockfile_dirs_in(&files, "package-lock.json"),
            s(&["", "web"])
        );
        assert_eq!(lockfile_dirs_in(&files, "pnpm-lock.yaml"), s(&["web/x"]));
    }

    #[test]
    fn package_manager_field_is_read_narrowly() {
        assert_eq!(
            declared_manager(r#"{"packageManager": "pnpm@10.1.0"}"#),
            Some(Manager::Pnpm)
        );
        assert_eq!(
            declared_manager(r#"{"packageManager":"npm@11"}"#),
            Some(Manager::Npm)
        );
        assert_eq!(declared_manager(r#"{"packageManager":"yarn@4"}"#), None);
        assert_eq!(declared_manager(r#"{"name":"x"}"#), None);
        assert_eq!(
            declared_manager(r#"{"packageManager":"npm@1","x":{"packageManager":"pnpm@1"}}"#),
            None,
            "two answers is no answer"
        );
    }

    fn unit(dir: &str) -> Unit {
        Unit {
            dir: dir.to_string(),
            manager: Manager::Pnpm,
            standalone: !dir.is_empty(),
        }
    }

    #[test]
    fn only_the_root_and_the_touched_units_are_kept() {
        let all = vec![unit(""), unit("spikes/a"), unit("spikes/b"), unit("tools")];
        let changed = s(&["src/x.ts", "spikes/b/index.ts", "spikes/bb/y.ts"]);
        let (kept, skipped) = select_units(all.clone(), Some(&changed));
        let dirs = |v: &[Unit]| v.iter().map(|u| u.dir.clone()).collect::<Vec<_>>();
        assert_eq!(
            dirs(&kept),
            s(&["", "spikes/b"]),
            "spikes/bb is not spikes/b"
        );
        assert_eq!(dirs(&skipped), s(&["spikes/a", "tools"]));
        let (kept, skipped) = select_units(all.clone(), None);
        assert_eq!(kept.len(), 4, "an unknown range keeps everything");
        assert!(skipped.is_empty());
    }

    #[test]
    fn a_nested_unit_claims_its_files_from_the_outer_one() {
        let all = vec![unit("web"), unit("web/nested")];
        let changed = s(&["web/nested/a.ts"]);
        let (kept, _) = select_units(all, Some(&changed));
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].dir, "web/nested");
    }

    #[test]
    fn a_file_newer_than_the_install_record_is_found() {
        let d = std::env::temp_dir().join(format!("newer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let pkg = d.join(".pnpm/a@1/node_modules/a");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::create_dir_all(d.join(".bin")).unwrap();
        std::fs::create_dir_all(d.join(".cache")).unwrap();
        std::fs::write(pkg.join("index.js"), "old").unwrap();
        let record = std::time::SystemTime::now();
        std::thread::sleep(std::time::Duration::from_millis(20));
        for skipped in [".bin/tsc", ".cache/x", ".pnpm/lock.yaml", ".modules.yaml"] {
            std::fs::write(d.join(skipped), "new").unwrap();
        }
        assert_eq!(
            newer_than(&d, record).unwrap(),
            None,
            "the install's own writes"
        );
        std::fs::write(pkg.join("index.js"), "edited").unwrap();
        assert_eq!(newer_than(&d, record).unwrap(), Some(pkg.join("index.js")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn normalize_folds_dots_lexically() {
        assert_eq!(
            normalize(Path::new("/a/b/../c/./d")),
            PathBuf::from("/a/c/d")
        );
    }
}
