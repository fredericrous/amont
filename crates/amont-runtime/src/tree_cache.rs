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
/// How many (directory, extension) pairs the typed-eslint probe asks about
/// before it stops trusting its sample. It runs once per namespace, normally
/// in the background warm-up (application-landscape: 83 pairs, ~1 min there),
/// and a commit only reads the recorded answer.
const MAX_TYPED_SAMPLE: usize = 256;
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
/// without running the gate. Never touches `.venv` or the network on its own
/// account; a wrapper that does (pyright's) is bounded by `deadline` and
/// `cancel`, like any gate run. `None` when it could not be read in time —
/// the caller withholds rather than guess.
pub fn tool_version(
    cwd: &Path,
    gate: &TreeGate,
    deadline: std::time::Instant,
    cancel: &std::sync::atomic::AtomicBool,
) -> Option<String> {
    let words: Vec<&str> = gate.command.split_whitespace().collect();
    // `uvx ruff@0.16.0 …`: the pin is in the command, which is hashed anyway.
    if words.first() == Some(&"uvx") && words.get(1).is_some_and(|w| w.contains('@')) {
        return Some("pinned-in-command".into());
    }
    match gate.tool {
        TreeTool::Eslint | TreeTool::Prettier => {
            let pkg = cwd
                .join("node_modules")
                .join(gate.tool.as_str())
                .join("package.json");
            Some(
                std::fs::read_to_string(pkg)
                    .ok()
                    .and_then(|t| json_string_field(&t, "version"))
                    .unwrap_or_else(|| "absent".into()),
            )
        }
        _ => {
            let probe: Vec<String> = if words.starts_with(&["uv", "run"]) {
                [
                    "uv",
                    "run",
                    "--frozen",
                    "--no-sync",
                    gate.tool.as_str(),
                    "--version",
                ]
                .iter()
                .map(|s| s.to_string())
                .collect()
            } else if gate.tool == TreeTool::Gofmt {
                vec!["go".into(), "version".into()]
            } else {
                vec![gate.tool.as_str().into(), "--version".into()]
            };
            // A tool that is not there, or answers with an error, is a
            // definite answer ("absent"), as it always was. Only one that did
            // not answer in time is unknown, and that withholds.
            match crate::tree_run::run_output(&probe, cwd, deadline, cancel) {
                Ok(out) => Some(
                    Some(out.lines().next().unwrap_or("").trim().to_string())
                        .filter(|v| !v.is_empty())
                        .unwrap_or_else(|| "absent".into()),
                ),
                Err(crate::tree_run::TreeRun::TimedOut | crate::tree_run::TreeRun::Cancelled) => {
                    None
                }
                Err(_) => Some("absent".into()),
            }
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
pub fn namespace(gate: &TreeGate, version: &str) -> Option<String> {
    let staged = crate::git::stdout(&["ls-files", "-s"])?;
    let mut material = format!(
        "amont-tree-ns-v1\ncommand {}\nversion {}\n",
        gate.command, version
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
    lock_in(&gate_dir(gate)?)
}

/// [`try_lock`] on `dir/.lock` — the part that does not ask git where the
/// cache is, so it can be tested without depending on the process's cwd.
fn lock_in(dir: &Path) -> Option<Lock> {
    std::fs::create_dir_all(dir).ok()?;
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

const DURATIONS: &str = ".durations";
const LAST_MS: &str = ".last_ms";

/// The last measured duration of each declared commit check, `<id> <ms>` per
/// line, in `$GIT_DIR/amont-cache/.durations`: how long a commit's own
/// checks run, which is the cover a tree gate can hide behind.
pub fn duration_of(id: &str) -> Option<u64> {
    let path = git_dir()?.join("amont-cache").join(DURATIONS);
    let text = std::fs::read_to_string(path).ok()?;
    text.lines().find_map(|l| {
        let (k, v) = l.split_once(' ')?;
        (k == id).then(|| v.trim().parse().ok()).flatten()
    })
}

/// Merge `measured` into the durations file (temp + rename).
pub fn record_durations(measured: &[(String, u64)]) {
    if measured.is_empty() {
        return;
    }
    let Some(dir) = git_dir().map(|d| d.join("amont-cache")) else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(DURATIONS);
    let mut rows: Vec<(String, u64)> = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(' ')?;
            Some((k.to_string(), v.trim().parse().ok()?))
        })
        .filter(|(k, _)| !measured.iter().any(|(m, _)| m == k))
        .collect();
    rows.extend(measured.iter().cloned());
    let body: String = rows.iter().map(|(k, v)| format!("{k} {v}\n")).collect();
    let tmp = dir.join(format!("{DURATIONS}.tmp.{}", std::process::id()));
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// How long this gate's last commit-time run took, kept in the gate's
/// directory (not a namespace), so the fit test can run FIRST — before the
/// version probe, the skew check and the namespace — and a gate that cannot
/// fit costs the commit nothing. A cancelled run records its elapsed time: a
/// lower bound, which is what matters.
pub fn gate_last_ms(gate: &TreeGate) -> Option<u64> {
    last_ms(&gate_dir(gate)?)
}

pub fn record_gate_last_ms(gate_name: &str, ms: u64) {
    if let Some(dir) = git_dir().map(|d| d.join("amont-cache").join(gate_name)) {
        let _ = std::fs::create_dir_all(&dir);
        record_last_ms(&dir, ms);
    }
}

fn last_ms(ns_dir: &Path) -> Option<u64> {
    std::fs::read_to_string(ns_dir.join(LAST_MS))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn record_last_ms(ns_dir: &Path, ms: u64) {
    let tmp = ns_dir.join(format!("{LAST_MS}.tmp.{}", std::process::id()));
    if std::fs::write(&tmp, ms.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, ns_dir.join(LAST_MS));
    }
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

/// Whether an `eslint --print-config` JSON uses type information
/// (`parserOptions.project` set, or `projectService` true), which makes a
/// per-file cache stale across files.
pub fn config_is_typed(config: &crate::json_read::Value) -> bool {
    let Some(opts) = config
        .get("languageOptions")
        .and_then(|l| l.get("parserOptions"))
        .or_else(|| config.get("parserOptions"))
    else {
        return false;
    };
    let project = opts.get("project").is_some_and(|p| {
        !matches!(
            p,
            crate::json_read::Value::Null | crate::json_read::Value::Bool(false)
        )
    });
    let service = opts.get("projectService").is_some_and(|v| {
        !matches!(
            v,
            crate::json_read::Value::Null | crate::json_read::Value::Bool(false)
        )
    });
    project || service
}

/// Whether eslint here uses type information. FAIL-CLOSED: anything but a
/// definite "untyped" reads as typed, which only costs the cache, where a
/// wrong "untyped" would let a stale per-file cache prove a typed tree.
/// Asked through `tree_run` (bounded by `deadline`, cancellable), over up to
/// ten tracked files until one prints a config (an ignored file prints
/// `undefined`). Remembered in the namespace only when definite, so a
/// transient timeout never disables the cache for good.
pub fn typed_eslint(
    cwd: &Path,
    ns_dir: &Path,
    deadline: std::time::Instant,
    cancel: &std::sync::atomic::AtomicBool,
) -> bool {
    if ns_dir.join(TYPED).is_file() {
        return true;
    }
    if ns_dir.join(UNTYPED).is_file() {
        return false;
    }
    let bin = cwd.join("node_modules").join(".bin").join("eslint");
    if !bin.is_file() {
        return true;
    }
    // Sampled where eslint RUNS (the gate's cwd), and all of them asked: a
    // flat config can add type information to some files only, so one
    // untyped answer proves nothing about the rest.
    let files = crate::git::stdout_in(
        cwd,
        &[
            "ls-files", "*.ts", "*.tsx", "*.js", "*.jsx", "*.mjs", "*.cjs",
        ],
    )
    .unwrap_or_default();
    // One file per (directory, extension): flat configs select files by
    // directory glob and by extension, and `ls-files` is alphabetical, so a
    // plain first-N sample could see only root and `scripts/` files and miss
    // a typed `src/**` block entirely. Past the cap the sample is not the
    // tree: typed, and nothing remembered.
    let mut seen: Vec<(String, String)> = Vec::new();
    let mut sample: Vec<&str> = Vec::new();
    for file in files.lines() {
        let (dir, base) = file.rsplit_once('/').unwrap_or(("", file));
        let ext = base.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
        let key = (dir.to_string(), ext.to_string());
        if !seen.contains(&key) {
            seen.push(key);
            sample.push(file);
        }
    }
    if sample.len() > MAX_TYPED_SAMPLE {
        return true;
    }
    let mut answered = 0;
    for file in sample {
        let argv = vec![
            bin.display().to_string(),
            "--print-config".to_string(),
            file.to_string(),
        ];
        match crate::tree_run::run_output(&argv, cwd, deadline, cancel) {
            Ok(out) => match crate::json_read::parse(out.trim()) {
                Some(config) if config_is_typed(&config) => {
                    let _ = std::fs::write(ns_dir.join(TYPED), b"");
                    return true;
                }
                Some(_) => answered += 1,
                // `undefined`: eslint ignores this file.
                None => {}
            },
            Err(_) => return true,
        }
    }
    if answered == 0 {
        return true;
    }
    let _ = std::fs::write(ns_dir.join(UNTYPED), b"");
    false
}

/// What `{cache}` expands to for `gate` in `ns_dir`, or `""` for a tool
/// without a cache, or typed (or undetermined) eslint.
pub fn cache_flags(
    cwd: &Path,
    gate: &TreeGate,
    ns_dir: &Path,
    deadline: std::time::Instant,
    cancel: &std::sync::atomic::AtomicBool,
) -> String {
    let file = match gate.tool {
        TreeTool::Eslint if !typed_eslint(cwd, ns_dir, deadline, cancel) => ".eslintcache",
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
        let dir = std::env::temp_dir().join(format!("amont-lock-test-{}", std::process::id()));
        let first = lock_in(&dir).expect("first lock");
        assert!(lock_in(&dir).is_none(), "a second lock was granted");
        drop(first);
        // Released EVENTUALLY: a thread elsewhere in this process that forks
        // (to spawn git) holds a copy of the lock's description until its
        // exec closes it. Harmless — a gate reads busy once — but not instant.
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut again = lock_in(&dir);
        while again.is_none() && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(20));
            again = lock_in(&dir);
        }
        assert!(again.is_some(), "the lock was never released");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn tree_cache_typed_configs_are_recognised() {
        use crate::json_read::parse;
        let typed = [
            r#"{"languageOptions":{"parserOptions":{"project":"./tsconfig.json"}}}"#,
            r#"{"languageOptions":{"parserOptions":{"project":true}}}"#,
            r#"{"languageOptions":{"parserOptions":{"projectService":true}}}"#,
            r#"{"languageOptions":{"parserOptions":{"projectService":{"allowDefaultProject":["*.js"]}}}}"#,
            r#"{"parserOptions":{"project":["a.json"]}}"#,
        ];
        for t in typed {
            assert!(config_is_typed(&parse(t).unwrap()), "{t}");
        }
        let untyped = [
            r#"{"languageOptions":{"parserOptions":{"project":null}}}"#,
            r#"{"languageOptions":{"parserOptions":{"projectService":false}}}"#,
            r#"{"languageOptions":{"parserOptions":{}}}"#,
            r#"{"rules":{}}"#,
        ];
        for t in untyped {
            assert!(!config_is_typed(&parse(t).unwrap()), "{t}");
        }
    }

    /// No eslint to ask: typed (no cache), and nothing remembered.
    #[test]
    fn tree_cache_undetermined_eslint_reads_typed() {
        let d = std::env::temp_dir().join(format!("amont-typed-{}", std::process::id()));
        let ns = d.join("ns");
        std::fs::create_dir_all(&ns).unwrap();
        let typed = typed_eslint(
            &d,
            &ns,
            std::time::Instant::now() + std::time::Duration::from_secs(5),
            &std::sync::atomic::AtomicBool::new(false),
        );
        assert!(typed);
        assert!(!ns.join(TYPED).exists() && !ns.join(UNTYPED).exists());
        let _ = std::fs::remove_dir_all(d);
    }

    /// Every sampled file is asked, where eslint runs: one typed file among
    /// untyped ones makes the whole namespace typed.
    #[cfg(unix)]
    #[test]
    fn tree_cache_one_typed_file_makes_eslint_typed() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("amont-typed-mix-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let bin = d.join("node_modules/.bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("a.js"), "").unwrap();
        std::fs::write(d.join("src/b.ts"), "").unwrap();
        for args in [vec!["init", "-q"], vec!["add", "a.js", "src/b.ts"]] {
            assert!(std::process::Command::new("git")
                .args(&args)
                .current_dir(&d)
                .status()
                .unwrap()
                .success());
        }
        let eslint = bin.join("eslint");
        std::fs::write(
            &eslint,
            "#!/bin/sh\ncase \"$2\" in\n  src/*) echo '{\"languageOptions\":{\"parserOptions\":{\"projectService\":true}}}' ;;\n  *) echo '{\"languageOptions\":{\"parserOptions\":{}}}' ;;\nesac\n",
        )
        .unwrap();
        std::fs::set_permissions(&eslint, std::fs::Permissions::from_mode(0o755)).unwrap();
        let ns = d.join("ns");
        std::fs::create_dir_all(&ns).unwrap();
        let typed = typed_eslint(
            &d,
            &ns,
            std::time::Instant::now() + std::time::Duration::from_secs(20),
            &std::sync::atomic::AtomicBool::new(false),
        );
        assert!(typed, "one typed file must make the namespace typed");
        assert!(ns.join(TYPED).exists());
        let _ = std::fs::remove_dir_all(d);
    }

    /// Alphabetical order must not hide a typed block: twelve untyped files
    /// sorting before `src/` and one typed file under it still read typed.
    #[cfg(unix)]
    #[test]
    fn tree_cache_many_untyped_files_before_src_do_not_hide_it() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("amont-typed-many-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let bin = d.join("node_modules/.bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(d.join("scripts")).unwrap();
        std::fs::create_dir_all(d.join("src")).unwrap();
        let mut paths = Vec::new();
        for i in 0..12 {
            let p = format!("a{i:02}.config.js");
            std::fs::write(d.join(&p), "").unwrap();
            paths.push(p);
            let p = format!("scripts/s{i:02}.js");
            std::fs::write(d.join(&p), "").unwrap();
            paths.push(p);
        }
        std::fs::write(d.join("src/z.ts"), "").unwrap();
        paths.push("src/z.ts".into());
        assert!(std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&d)
            .status()
            .unwrap()
            .success());
        assert!(std::process::Command::new("git")
            .arg("add")
            .args(&paths)
            .current_dir(&d)
            .status()
            .unwrap()
            .success());
        let eslint = bin.join("eslint");
        std::fs::write(
            &eslint,
            "#!/bin/sh\ncase \"$2\" in\n  src/*) echo '{\"languageOptions\":{\"parserOptions\":{\"projectService\":true}}}' ;;\n  *) echo '{\"languageOptions\":{\"parserOptions\":{}}}' ;;\nesac\n",
        )
        .unwrap();
        std::fs::set_permissions(&eslint, std::fs::Permissions::from_mode(0o755)).unwrap();
        let ns = d.join("ns");
        std::fs::create_dir_all(&ns).unwrap();
        assert!(typed_eslint(
            &d,
            &ns,
            std::time::Instant::now() + std::time::Duration::from_secs(30),
            &std::sync::atomic::AtomicBool::new(false),
        ));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn tree_cache_reads_a_package_version() {
        let pkg = "{\n  \"name\": \"eslint\",\n  \"version\": \"9.12.0\",\n  \"x\": 1\n}";
        assert_eq!(json_string_field(pkg, "version").as_deref(), Some("9.12.0"));
        assert_eq!(json_string_field("{}", "version"), None);
    }
}
