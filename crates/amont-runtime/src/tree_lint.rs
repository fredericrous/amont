//! Tree gates at commit time: run beside the commit's checks, never decide it,
//! stamp the tree when they prove it (ADR-0024).
//!
//! The commit's verdict is untouched: it comes from the checks it always came
//! from. A tree gate adds at most `amont.treeLintSlack` after those finish, and
//! a pass is recorded as the token `tree:<name>` in the commit's gate stamp
//! (`gate_stamp::record`), which post-commit binds to both the commit and its
//! tree. The push reads the tree's note, so a reword keeps the proof.
//!
//! No stamp, and the gates do not even start, when the tree they would lint is
//! not exactly the commit's tree:
//!
//! - tracked files had unstaged edits (captured BEFORE the staged-only hold
//!   parks them — after, the working tree always looks clean);
//! - untracked, non-ignored files exist;
//! - ignored files exist outside the allow-list (`snapshotPrepareOutputs`,
//!   plus tool caches by default) — a gitignored generated module can make a
//!   linter pass here and fail in CI;
//! - a merge, rebase, cherry-pick or revert is in progress.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::hooks::common::{hl, ok, say};
use crate::manifest::{TreeGate, CACHE_PLACEHOLDER};
use crate::tree_run::TreeRun;

pub const TOGGLE: &str = "amont.treeLint";
pub const SLACK: &str = "amont.treeLintSlack";
pub const OUTPUTS: &str = "amont.snapshotPrepareOutputs";

/// Seconds tree gates may run after the commit's own checks have finished.
const DEFAULT_SLACK: i64 = 2;

/// The ceiling for one tree gate at commit, whatever the slack: a gate is
/// cancelled at `slack` after the checks anyway, this only bounds a commit
/// whose checks themselves run long.
const CEILING: Duration = Duration::from_secs(600);

/// Ignored paths that are tool caches or dependency trees CI recreates, and
/// never a module a linter could import in place of a committed one.
pub const DEFAULT_OUTPUTS: &[&str] = &[
    "node_modules/",
    ".venv/",
    "venv/",
    "__pycache__/",
    ".ruff_cache/",
    ".pytest_cache/",
    ".mypy_cache/",
    ".hypothesis/",
    ".eslintcache",
    ".prettiercache",
    ".DS_Store",
];

/// Whether tree gates run here. On by default where attestation is on:
/// a stamp nobody signs is paid for and never used.
pub fn enabled(settings: &crate::config::Settings) -> bool {
    crate::config::boolean_or(settings, TOGGLE, crate::attest::enabled(settings))
}

fn slack(settings: &crate::config::Settings) -> Duration {
    let secs = crate::config::integer_or(settings, SLACK, DEFAULT_SLACK, 0..=600);
    Duration::from_secs(u64::try_from(secs).unwrap_or(0))
}

/// The allow-list: the defaults plus `snapshotPrepareOutputs`.
pub fn outputs(settings: &crate::config::Settings) -> Vec<String> {
    let mut out: Vec<String> = DEFAULT_OUTPUTS.iter().map(|s| s.to_string()).collect();
    if let Some(v) = crate::config::string_value(settings, OUTPUTS) {
        out.extend(v.split_whitespace().map(str::to_string));
    }
    out
}

/// Whether an ignored entry (as `git ls-files -o -i --directory` prints it:
/// directories end in `/`) is covered by an allow-list pattern.
///
/// A pattern ending in `/` names a directory, anywhere in the tree
/// (`__pycache__/` matches `pkg/__pycache__/`). Any other pattern names a
/// file by basename, with `*` as the only wildcard (`*.log`).
pub fn allowed(entry: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| {
        if let Some(dir) = p.strip_suffix('/') {
            let dir = dir.trim_start_matches("./");
            entry == format!("{dir}/")
                || entry.starts_with(&format!("{dir}/"))
                || entry.contains(&format!("/{dir}/"))
        } else {
            let base = entry
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or(entry);
            glob_basename(p, base)
        }
    })
}

/// `*` matches any run of characters; nothing else is special.
fn glob_basename(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let mut rest = name;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            let Some(r) = rest.strip_prefix(part) else {
                return false;
            };
            rest = r;
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else {
            let Some(at) = rest.find(part) else {
                return false;
            };
            rest = &rest[at + part.len()..];
        }
    }
    true
}

/// Why this commit's tree cannot be proven, or `None` when it can. Runs
/// BEFORE the staged-only hold: only then can unstaged edits be seen.
pub fn guard(settings: &crate::config::Settings) -> Option<String> {
    if !crate::git_states_in_progress().is_empty() {
        return Some("a merge, rebase, cherry-pick or revert is in progress".into());
    }
    if !crate::git::succeeds(&["diff", "--quiet"]) {
        return Some("tracked files have unstaged edits (the tree is not the commit's)".into());
    }
    let untracked =
        crate::git::stdout(&["ls-files", "--others", "--exclude-standard", "--directory"])
            .unwrap_or_default();
    if let Some(first) = untracked.lines().find(|l| !l.trim().is_empty()) {
        return Some(format!("an untracked file is present ({first})"));
    }
    let ignored = crate::git::stdout(&[
        "ls-files",
        "--others",
        "--ignored",
        "--exclude-standard",
        "--directory",
    ])
    .unwrap_or_default();
    let allow = outputs(settings);
    if let Some(first) = ignored
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !allowed(l, &allow))
    {
        return Some(format!(
            "an ignored file outside {} is present ({first})",
            hl("snapshotPrepareOutputs")
        ));
    }
    None
}

/// The argv a gate runs: its command through `sh -c`, as a CI `run:` line
/// runs through a shell, with `{cache}` expanded to `cache_flags`.
pub fn argv(gate: &TreeGate, cache_flags: &str) -> Vec<String> {
    if cache_flags.trim().is_empty() {
        // Exactly what CI runs: no dangling `--`.
        return vec!["sh".into(), "-c".into(), gate.normalized()];
    }
    let command: Vec<&str> = gate
        .command
        .split_whitespace()
        .flat_map(|w| {
            if w == CACHE_PLACEHOLDER {
                cache_flags.split_whitespace().collect::<Vec<_>>()
            } else {
                vec![w]
            }
        })
        .collect();
    vec!["sh".into(), "-c".into(), command.join(" ")]
}

/// Tree gates in flight.
pub struct SideCar {
    cancel: Arc<AtomicBool>,
    running: Vec<(String, JoinHandle<TreeRun>)>,
}

fn cwd_of(root: &Path, gate: &TreeGate) -> PathBuf {
    match &gate.cwd {
        Some(dir) => root.join(dir),
        None => root.to_path_buf(),
    }
}

/// Start the WARM `gates` against the working tree, which the staged-only
/// hold has made the commit's tree; send the cold ones to a background
/// warm-up. A gate whose cache another run holds is skipped, never waited
/// for. Returns `None` when nothing starts.
pub fn start(
    settings: &crate::config::Settings,
    root: &Path,
    gates: &[TreeGate],
) -> Option<SideCar> {
    if gates.is_empty() {
        return None;
    }
    if cfg!(not(unix)) {
        say("  tree gates need a Unix shell and do not run here — CI will lint");
        return None;
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + CEILING;
    let mut running = Vec::new();
    let mut cold = Vec::new();
    let mut busy = Vec::new();
    for g in gates {
        let cwd = cwd_of(root, g);
        let Some(ns) =
            crate::tree_cache::namespace(&cwd, g).filter(|ns| crate::tree_cache::is_warm(g, ns))
        else {
            cold.push(g.name.clone());
            continue;
        };
        let Some(lock) = crate::tree_cache::try_lock(g) else {
            busy.push(g.name.clone());
            continue;
        };
        let Some(ns_dir) = crate::tree_cache::gate_dir(g).map(|d| d.join(&ns)) else {
            cold.push(g.name.clone());
            continue;
        };
        let argv = argv(g, &crate::tree_cache::cache_flags(&cwd, g, &ns_dir));
        let flag = Arc::clone(&cancel);
        let handle = std::thread::spawn(move || {
            let run = crate::tree_run::run(&argv, &cwd, deadline, &flag);
            // Only now: the runner killed the whole group before returning,
            // so nothing that could still write the cache is alive.
            drop(lock);
            run
        });
        running.push((g.name.clone(), handle));
    }
    if !busy.is_empty() {
        say(&format!(
            "  tree lint not proven: {} — a warm-up holds the cache; CI will lint",
            busy.join(" ")
        ));
    }
    if !cold.is_empty() {
        warm_later(settings, &cold);
    }
    (!running.is_empty()).then_some(SideCar { cancel, running })
}

/// Where the background warm-up writes: its own log, never the rehearsal's.
fn warm_log() -> Option<PathBuf> {
    crate::git::stdout(&["rev-parse", "--absolute-git-dir"])
        .map(|d| PathBuf::from(d).join("amont-warm.log"))
}

/// Start the background warm-up for the cold gates, and say so once.
fn warm_later(settings: &crate::config::Settings, cold: &[String]) {
    #[cfg(unix)]
    {
        let Some(log) = warm_log() else { return };
        let started = crate::rehearsal::spawn_amont(&["warm", "--worker"], &log);
        if crate::live::quiet(settings) {
            return;
        }
        match started {
            Ok(_) => say(&format!(
                "  tree lint cold: {} — warming in background (log: {})",
                cold.join(" "),
                log.display()
            )),
            Err(e) => say(&format!(
                "  tree lint cold: {} — could not start the warm-up ({e}); run {}",
                cold.join(" "),
                hl("amont warm")
            )),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (settings, cold);
    }
}

/// `amont warm` — fill every cold tree gate's cache in its current
/// namespace: a full run with `{cache}`, then the completion marker. Old
/// namespaces are deleted first, under the gate's lock. A gate whose lock is
/// held is left to whoever holds it. Exit 0 whatever the lint found: a
/// warm-up proves nothing, it only makes the next commit's run cheap.
pub fn warm(root: &Path) -> i32 {
    let manifest = crate::manifest::load(root);
    if manifest.tree.is_empty() {
        println!("amont warm: no trusted tree gate here");
        return 0;
    }
    for g in &manifest.tree {
        let cwd = cwd_of(root, g);
        let Some(ns) = crate::tree_cache::namespace(&cwd, g) else {
            println!("{}: git could not name the namespace — skipped", g.name);
            continue;
        };
        if crate::tree_cache::is_warm(g, &ns) {
            println!("{}: already warm", g.name);
            continue;
        }
        let Some(_lock) = crate::tree_cache::try_lock(g) else {
            println!("{}: another run holds the cache — skipped", g.name);
            continue;
        };
        let Some(ns_dir) = crate::tree_cache::enter_namespace(g, &ns) else {
            println!("{}: cannot create the cache directory — skipped", g.name);
            continue;
        };
        let argv = argv(g, &crate::tree_cache::cache_flags(&cwd, g, &ns_dir));
        let started = Instant::now();
        let run = crate::tree_run::run(&argv, &cwd, started + CEILING, &AtomicBool::new(false));
        let secs = started.elapsed().as_secs_f32();
        match run {
            TreeRun::Passed | TreeRun::Failed(_) => {
                crate::tree_cache::mark_complete(&ns_dir);
                println!("{}: warm ({secs:.1}s)", g.name);
            }
            other => println!("{}: not warmed — {other:?}", g.name),
        }
    }
    0
}

/// Wait at most `amont.treeLintSlack` for the gates still running, cancel the
/// rest, and return the stamp tokens of the gates that proved the tree.
/// `stampable` is false when the commit is blocked or a check rewrote files:
/// then nothing is waited for and nothing is stamped.
pub fn finish(settings: &crate::config::Settings, car: SideCar, stampable: bool) -> Vec<String> {
    let until = Instant::now()
        + if stampable {
            slack(settings)
        } else {
            Duration::ZERO
        };
    while Instant::now() < until && car.running.iter().any(|(_, h)| !h.is_finished()) {
        std::thread::sleep(Duration::from_millis(20));
    }
    car.cancel.store(true, Ordering::SeqCst);
    let mut proven = Vec::new();
    let mut unproven = Vec::new();
    for (name, handle) in car.running {
        match handle
            .join()
            .unwrap_or(TreeRun::Spawn("thread died".into()))
        {
            TreeRun::Passed if stampable => proven.push(name),
            TreeRun::Passed => {}
            TreeRun::Failed(summary) => unproven.push(format!("{name} — {summary}")),
            TreeRun::TimedOut | TreeRun::Cancelled => {
                unproven.push(format!("{name} — still running when the commit was ready"))
            }
            TreeRun::Spawn(e) => unproven.push(format!("{name} — could not start ({e})")),
        }
    }
    if !proven.is_empty() {
        ok(settings, &format!("tree lint proven: {}", proven.join(" ")));
    }
    if stampable {
        for u in &unproven {
            say(&format!(
                "  tree lint not proven: {} — CI will lint",
                crate::ui::sanitize(u)
            ));
        }
    }
    proven.iter().map(|n| format!("tree:{n}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pats(extra: &[&str]) -> Vec<String> {
        DEFAULT_OUTPUTS
            .iter()
            .chain(extra)
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn tree_lint_allows_tool_caches_anywhere() {
        let p = pats(&[]);
        assert!(allowed("node_modules/", &p));
        assert!(allowed("packages/api/__pycache__/", &p));
        assert!(allowed(".venv/", &p));
        assert!(allowed("web/.DS_Store", &p));
    }

    #[test]
    fn tree_lint_refuses_an_ignored_module() {
        let p = pats(&[]);
        assert!(!allowed("src/generated/types.ts", &p));
        assert!(!allowed(".react-router/", &p));
        assert!(!allowed("build/", &p));
    }

    #[test]
    fn tree_lint_repo_patterns_extend_the_list() {
        let p = pats(&["build/", ".react-router/", "*.db", "*.db-wal"]);
        assert!(allowed("build/", &p));
        assert!(allowed(".react-router/", &p));
        assert!(allowed("landscape.db", &p));
        assert!(allowed("landscape.db-wal", &p));
        assert!(!allowed("landscape.sqlite", &p));
    }

    #[test]
    fn tree_lint_argv_runs_through_sh_with_cache_expanded() {
        let g = crate::manifest::tree_gates("tree eslint eslint * attest npm run lint -- {cache}")
            .remove(0);
        assert_eq!(
            argv(&g, "--cache --cache-location /x"),
            vec!["sh", "-c", "npm run lint -- --cache --cache-location /x"]
        );
        assert_eq!(argv(&g, ""), vec!["sh", "-c", "npm run lint"]);
    }
}
