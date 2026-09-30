//! Tree-gate caches: namespaced by everything that can change a verdict,
//! never trusted across a namespace (ADR-0024).
//!
//! A tool's own cache is not enough. Prettier's cache keys exclude plugin
//! versions and implementations, so a plugin upgrade would reuse stale passes.
//! So every gate's cache lives under a NAMESPACE — a hash of:
//!
//! - the declared command;
//! - the version of the tool the gate actually runs;
//! - the staged blob of every lockfile and every config-like file, matched by
//!   basename anywhere in the tree;
//! - the gate's `inputs=`.
//!
//! A plugin upgrade moves the lockfile, so it moves the namespace, and the old
//! namespace is deleted (under the gate's lock) before anything warms the new
//! one.
//!
//! **Warm** means a completion marker exists in the current namespace: a full
//! run finished there. The marker is only a hint about cost ("starting this
//! gate at commit is cheap"). Proof always comes from the commit-time run on
//! the exact tree.
//!
//! Caches are per worktree (`$GIT_DIR`): eslint's and prettier's key files
//! by absolute path, so a shared cache would give a new worktree no warmth.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::manifest::{TreeGate, TreeTool};

const COMPLETE: &str = ".complete";
const TYPED: &str = ".typed";
const UNTYPED: &str = ".untyped";

/// Lockfiles, by basename: a dependency or plugin upgrade moves one of them.
const LOCKFILES: &[&str] = &[
    "package-lock.json",
    "npm-shrinkwrap.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "bun.lockb",
    "uv.lock",
    "poetry.lock",
    "Pipfile.lock",
    "Cargo.lock",
    "go.sum",
];

/// Whether a path's BASENAME is config-like: `*.json`, `*.toml`, `*.yaml`,
/// `*.yml`, `*config*`, `.*rc*`, `.*ignore`, or a lockfile. Basename only, so
/// `src/config/app.ts` is not config.
pub fn config_like(path: &str) -> bool {
    let base = path.rsplit('/').next().unwrap_or(path);
    LOCKFILES.contains(&base)
        || [".json", ".toml", ".yaml", ".yml"]
            .iter()
            .any(|ext| base.ends_with(ext))
        || base.contains("config")
        || (base.starts_with('.') && (base.contains("rc") || base.ends_with("ignore")))
}

fn git_dir() -> Option<PathBuf> {
    crate::git::stdout(&["rev-parse", "--absolute-git-dir"]).map(PathBuf::from)
}

/// `$GIT_DIR/amont-cache/<gate>`.
pub fn gate_dir(gate: &TreeGate) -> Option<PathBuf> {
    Some(git_dir()?.join("amont-cache").join(&gate.name))
}

/// The version of the tool the gate actually runs, as best it can be read
/// without running the gate. Never touches `.venv` or the network.
pub fn tool_version(cwd: &Path, gate: &TreeGate) -> String {
    let words: Vec<&str> = gate.command.split_whitespace().collect();
    // `uvx ruff@0.16.0 …`: the pin is in the command, which is hashed anyway.
    if words.first() == Some(&"uvx") && words.get(1).is_some_and(|w| w.contains('@')) {
        return "pinned-in-command".into();
    }
    match gate.tool {
        TreeTool::Eslint | TreeTool::Prettier => {
            let pkg = cwd
                .join("node_modules")
                .join(gate.tool.as_str())
                .join("package.json");
            std::fs::read_to_string(pkg)
                .ok()
                .and_then(|t| json_string_field(&t, "version"))
                .unwrap_or_else(|| "absent".into())
        }
        _ => {
            let probe: Vec<&str> = if words.starts_with(&["uv", "run"]) {
                vec![
                    "uv",
                    "run",
                    "--frozen",
                    "--no-sync",
                    gate.tool.as_str(),
                    "--version",
                ]
            } else if gate.tool == TreeTool::Gofmt {
                vec!["go", "version"]
            } else {
                vec![gate.tool.as_str(), "--version"]
            };
            Command::new(probe[0])
                .args(&probe[1..])
                .current_dir(cwd)
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_else(|| "absent".into())
        }
    }
}

/// The first `"<field>": "<value>"` in a JSON text. Enough for a
/// `package.json`'s top-level `version`, which npm writes first-level and
/// unescaped; anything stranger reads as absent.
fn json_string_field(text: &str, field: &str) -> Option<String> {
    let key = format!("\"{field}\"");
    let at = text.find(&key)? + key.len();
    let rest = text[at..].trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// The namespace for `gate`: see the module doc. `None` when git cannot
/// answer, which reads as cold.
pub fn namespace(cwd: &Path, gate: &TreeGate) -> Option<String> {
    let staged = crate::git::stdout(&["ls-files", "-s"])?;
    let mut material = format!(
        "amont-tree-ns-v1\ncommand {}\nversion {}\n",
        gate.command,
        tool_version(cwd, gate)
    );
    for line in staged.lines() {
        // `<mode> <blob> <stage>\t<path>`
        let Some((meta, path)) = line.split_once('\t') else {
            continue;
        };
        if config_like(path)
            || gate
                .inputs
                .iter()
                .any(|i| path == i || path.starts_with(&format!("{i}/")))
        {
            material.push_str(meta);
            material.push(' ');
            material.push_str(path);
            material.push('\n');
        }
    }
    hash(&material)
}

fn hash(material: &str) -> Option<String> {
    use std::io::Write;
    let mut child = Command::new("git")
        .args(["hash-object", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(material.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// A held, non-blocking lock on one gate's cache. Released on drop.
pub struct Lock {
    _file: File,
}

#[cfg(unix)]
extern "C" {
    #[link_name = "flock"]
    fn libc_flock(fd: i32, op: i32) -> i32;
}

#[cfg(unix)]
const LOCK_EX: i32 = 2;
#[cfg(unix)]
const LOCK_NB: i32 = 4;

/// Take `gate`'s lock without waiting. `None` when another run holds it (or
/// the lock file cannot be made) — which the caller reads as cold: a commit
/// never waits on a warm-up.
pub fn try_lock(gate: &TreeGate) -> Option<Lock> {
    let dir = gate_dir(gate)?;
    std::fs::create_dir_all(&dir).ok()?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(".lock"))
        .ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        // SAFETY: a valid fd we own; flock touches no memory.
        if unsafe { libc_flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0 {
            return None;
        }
    }
    Some(Lock { _file: file })
}

/// `gate_dir/<ns>`, created, with every OTHER namespace of the gate deleted.
/// Call only while holding the gate's [`Lock`].
pub fn enter_namespace(gate: &TreeGate, ns: &str) -> Option<PathBuf> {
    let dir = gate_dir(gate)?;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.filter_map(Result::ok) {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name != ns && !name.starts_with('.') && e.path().is_dir() {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    let ns_dir = dir.join(ns);
    std::fs::create_dir_all(&ns_dir).ok()?;
    Some(ns_dir)
}

/// Whether a full run completed in this namespace.
pub fn is_warm(gate: &TreeGate, ns: &str) -> bool {
    gate_dir(gate)
        .map(|d| d.join(ns).join(COMPLETE).is_file())
        .unwrap_or(false)
}

/// Record that a full run completed in `ns_dir`: temp + rename, so a killed
/// writer leaves no marker at all rather than a half one.
pub fn mark_complete(ns_dir: &Path) {
    let tmp = ns_dir.join(format!("{COMPLETE}.tmp.{}", std::process::id()));
    if std::fs::write(&tmp, b"complete\n").is_ok() {
        let _ = std::fs::rename(&tmp, ns_dir.join(COMPLETE));
    }
}

/// Whether eslint here uses type information (`parserOptions.project` or
/// `projectService`): a per-file cache is stale across files then, so the
/// gate runs uncached. Asked once per namespace and remembered in it.
pub fn typed_eslint(cwd: &Path, ns_dir: &Path) -> bool {
    if ns_dir.join(TYPED).is_file() {
        return true;
    }
    if ns_dir.join(UNTYPED).is_file() {
        return false;
    }
    let sample = crate::git::stdout(&["ls-files", "*.ts", "*.tsx", "*.js", "*.mjs"])
        .and_then(|s| s.lines().next().map(str::to_string));
    let bin = cwd.join("node_modules").join(".bin").join("eslint");
    let typed = match sample {
        Some(file) if bin.is_file() => Command::new(&bin)
            .args(["--print-config", &file])
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .map(|o| {
                let t = String::from_utf8_lossy(&o.stdout);
                t.contains("\"projectService\": true")
                    || (t.contains("\"project\"") && !t.contains("\"project\": null"))
            })
            // Could not ask: assume typed, which only costs the cache.
            .unwrap_or(true),
        _ => false,
    };
    let _ = std::fs::write(ns_dir.join(if typed { TYPED } else { UNTYPED }), b"");
    typed
}

/// What `{cache}` expands to for `gate` in `ns_dir`, or `""` for a tool
/// without a cache, or typed eslint.
pub fn cache_flags(cwd: &Path, gate: &TreeGate, ns_dir: &Path) -> String {
    let file = match gate.tool {
        TreeTool::Eslint if !typed_eslint(cwd, ns_dir) => ".eslintcache",
        TreeTool::Prettier => ".prettiercache",
        _ => return String::new(),
    };
    format!(
        "--cache --cache-strategy content --cache-location {}",
        ns_dir.join(file).display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_cache_config_like_by_basename() {
        for p in [
            "package.json",
            "web/tsconfig.base.json",
            "eslint.config.mjs",
            ".prettierrc",
            ".eslintrc.cjs",
            ".gitignore",
            ".prettierignore",
            "pyproject.toml",
            "uv.lock",
            "go.sum",
            "pnpm-lock.yaml",
        ] {
            assert!(config_like(p), "{p}");
        }
        for p in ["src/config/app.ts", "src/main.ts", "README.md", "Makefile"] {
            assert!(!config_like(p), "{p}");
        }
    }

    /// A held lock is not waited for: the second taker reads it as busy.
    #[test]
    fn tree_cache_a_held_lock_is_busy_not_awaited() {
        let name = format!("lock-test-{}", std::process::id());
        let gate =
            crate::manifest::tree_gates(&format!("tree {name} ruff * attest true")).remove(0);
        let first = try_lock(&gate).expect("first lock");
        assert!(try_lock(&gate).is_none(), "a second lock was granted");
        drop(first);
        assert!(try_lock(&gate).is_some(), "the lock was not released");
        if let Some(dir) = gate_dir(&gate) {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn tree_cache_reads_a_package_version() {
        let pkg = "{\n  \"name\": \"eslint\",\n  \"version\": \"9.12.0\",\n  \"x\": 1\n}";
        assert_eq!(json_string_field(pkg, "version").as_deref(), Some("9.12.0"));
        assert_eq!(json_string_field("{}", "version"), None);
    }
}
