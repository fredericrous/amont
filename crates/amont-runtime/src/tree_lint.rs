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

fn slack_ms(settings: &crate::config::Settings) -> u64 {
    u64::try_from(slack(settings).as_millis()).unwrap_or(u64::MAX)
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
    content_guard(Path::new(&crate::hooks::common::repo_root()), settings)
}

/// Untracked files, and ignored ones outside the allow-list, under `dir`.
fn content_guard(dir: &Path, settings: &crate::config::Settings) -> Option<String> {
    let untracked = crate::git::stdout_in(
        dir,
        &["ls-files", "--others", "--exclude-standard", "--directory"],
    )
    .unwrap_or_default();
    if let Some(first) = untracked.lines().find(|l| !l.trim().is_empty()) {
        return Some(format!("an untracked file is present ({first})"));
    }
    let ignored = crate::git::stdout_in(
        dir,
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
        ],
    )
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

/// What one gate's thread decided, and did.
enum Decision {
    /// It ran: how it ended, and how long it took.
    Ran(TreeRun, u64),
    /// The tool it would run is not the one CI resolves.
    Skew(String),
    /// Its version could not be read before the deadline or the cancel.
    NoVersion,
    /// No completed run in its current namespace.
    Cold,
    /// Another run held its cache lock.
    Busy,
    /// Its last run (ms) would not fit this commit's cover plus the slack.
    Slow(u64),
}

type InFlight = (String, JoinHandle<Decision>);

/// Tree gates in flight.
pub struct SideCar {
    cancel: Arc<AtomicBool>,
    running: Vec<InFlight>,
}

/// This hook run's tree-gate outcomes, for `gate_evidence`: proven, failed,
/// cold, busy, skew, withheld, cancelled — the hit rate, and why it missed.
static EVIDENCE: std::sync::Mutex<Vec<crate::gate_stamp::Run>> = std::sync::Mutex::new(Vec::new());

fn note(name: &str, outcome: crate::gate_stamp::RunOutcome, ms: u64) {
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut e) = EVIDENCE.lock() {
        e.push(crate::gate_stamp::Run {
            at,
            gate: format!("tree-{name}"),
            outcome,
            ms,
        });
    }
}

/// Every gate withheld for the same reason (the guard fired).
pub fn note_withheld(gates: &[TreeGate]) {
    for g in gates {
        note(&g.name, crate::gate_stamp::RunOutcome::Withheld, 0);
    }
}

/// Write this run's tree-gate evidence against `tree`'s gate note, and
/// clear it. Best-effort, like every evidence writer.
pub fn flush_evidence(tree: &str) {
    let runs = EVIDENCE
        .lock()
        .map(|mut e| std::mem::take(&mut *e))
        .unwrap_or_default();
    if !runs.is_empty() {
        crate::gate_stamp::record_runs(tree, &runs);
    }
}

fn cwd_of(root: &Path, gate: &TreeGate) -> PathBuf {
    match &gate.cwd {
        Some(dir) => root.join(dir),
        None => root.to_path_buf(),
    }
}

/// Start one thread per gate against the working tree, which the staged-only
/// hold has made the commit's tree. EVERYTHING a gate needs to decide runs in
/// its own thread, so none of it delays the commit's checks: the version
/// probe (bounded and cancellable — a wrapper may reach the network), the
/// skew check, the namespace, the warm and fit tests, the lock, the run.
/// `cover_ms` is how long this commit's own declared checks are expected to
/// run. Returns `None` when nothing starts.
pub fn start(
    settings: &crate::config::Settings,
    root: &Path,
    gates: &[TreeGate],
    pins: &[crate::manifest::ToolPin],
    cover_ms: u64,
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
    let budget_ms = cover_ms.saturating_add(slack_ms(settings));
    let running = gates
        .iter()
        .map(|g| {
            let gate = g.clone();
            let pins = pins.to_vec();
            let cwd = cwd_of(root, g);
            let flag = Arc::clone(&cancel);
            let handle = std::thread::spawn(move || {
                decide_and_run(&gate, &cwd, &pins, budget_ms, deadline, &flag)
            });
            (g.name.clone(), handle)
        })
        .collect();
    Some(SideCar { cancel, running })
}

fn decide_and_run(
    g: &TreeGate,
    cwd: &Path,
    pins: &[crate::manifest::ToolPin],
    budget_ms: u64,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Decision {
    // Would it fit? Its last run against this commit's cover plus the slack,
    // asked FIRST so a gate that cannot fit costs nothing. Unknown runs once,
    // to learn.
    if let Some(ms) = crate::tree_cache::gate_last_ms(g).filter(|ms| *ms > budget_ms) {
        return Decision::Slow(ms);
    }
    let Some(version) = crate::tree_cache::tool_version(cwd, g, deadline, cancel) else {
        return Decision::NoVersion;
    };
    if let Some(why) = crate::tree_skew::skew(cwd, g, pins, &version) {
        return Decision::Skew(why);
    }
    let Some(ns) =
        crate::tree_cache::namespace(g, &version).filter(|ns| crate::tree_cache::is_warm(g, ns))
    else {
        return Decision::Cold;
    };
    let Some(ns_dir) = crate::tree_cache::gate_dir(g).map(|d| d.join(&ns)) else {
        return Decision::Cold;
    };
    let Some(lock) = crate::tree_cache::try_lock(g) else {
        return Decision::Busy;
    };
    let argv = argv(g, &crate::tree_cache::cache_flags(cwd, g, &ns_dir));
    let began = Instant::now();
    let run = crate::tree_run::run(&argv, cwd, deadline, cancel);
    let ms = u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX);
    // Only now: the runner killed the whole group before returning, so
    // nothing that could still write the cache is alive.
    drop(lock);
    Decision::Ran(run, ms)
}

/// Which declared tree gates every pushed tip's TREE proves, as note gate
/// ids (`tree-<name>`), and which it does not, by name. A tip whose tree git
/// cannot name proves nothing.
pub fn tree_verdict(gates: &[TreeGate], tips: &[String]) -> (Vec<String>, Vec<String>) {
    let trees: Vec<Option<String>> = tips
        .iter()
        .map(|t| crate::git::stdout(&["rev-parse", &format!("{t}^{{tree}}")]))
        .collect();
    let tokens: Vec<Vec<String>> = trees
        .iter()
        .map(|t| {
            t.as_deref()
                .map(crate::gate_stamp::tree_tokens)
                .unwrap_or_default()
        })
        .collect();
    let mut proven = Vec::new();
    let mut unproven = Vec::new();
    for g in gates {
        let key = g.stamp_key();
        if !tips.is_empty() && tokens.iter().all(|toks| toks.iter().any(|t| t == &key)) {
            proven.push(g.id());
        } else {
            unproven.push(g.name.clone());
        }
    }
    (proven, unproven)
}

pub const REHEARSAL_TIMEOUT: &str = "amont.treeLintRehearsalTimeout";
pub const WAIT: &str = "amont.treeLintWait";

/// Whether a snapshot is still exactly its tree once `snapshotPrepare` (and
/// `snapshotCarry`) ran: tracked content unchanged, nothing untracked, and
/// nothing ignored outside the allow-list. Prepare runs arbitrary commands,
/// so this is what keeps it from forging the tree a stamp names.
pub fn prepare_guard(snapshot: &Path, settings: &crate::config::Settings) -> Option<String> {
    if !crate::git::succeeds_in(snapshot, &["diff", "--quiet", "HEAD"]) {
        return Some("snapshotPrepare changed tracked content".into());
    }
    content_guard(snapshot, settings)
}

/// The rehearsal's half (ADR-0024): prove `gates` on the snapshot of `tree`,
/// uncached (every snapshot sits at a new path) and bounded by
/// `amont.treeLintRehearsalTimeout`, and stamp the TREE. Runs before the
/// rehearsal's test gates, so a push waiting on lint does not wait on tests.
pub fn rehearse(
    settings: &crate::config::Settings,
    snapshot: &Path,
    tree: &str,
    gates: &[TreeGate],
) -> Vec<String> {
    if gates.is_empty() || cfg!(not(unix)) {
        return Vec::new();
    }
    if let Some(why) = prepare_guard(snapshot, settings) {
        println!("tree lint not proven in the snapshot: {why}");
        note_withheld(gates);
        flush_evidence(tree);
        return Vec::new();
    }
    let budget = crate::config::integer_or(settings, REHEARSAL_TIMEOUT, 120, 1..=3600);
    let budget = Duration::from_secs(u64::try_from(budget).unwrap_or(120));
    let mut tokens = Vec::new();
    let pins = crate::manifest::load(snapshot).pins;
    for g in gates {
        let cwd = cwd_of(snapshot, g);
        let Some(version) = crate::tree_cache::tool_version(
            &cwd,
            g,
            Instant::now() + budget,
            &AtomicBool::new(false),
        ) else {
            note(&g.name, crate::gate_stamp::RunOutcome::Skew, 0);
            println!(
                "tree lint not proven: {} — its tool's version could not be read",
                g.name
            );
            continue;
        };
        if let Some(why) = crate::tree_skew::skew(&cwd, g, &pins, &version) {
            note(&g.name, crate::gate_stamp::RunOutcome::Skew, 0);
            println!("tree lint not proven: {} — {why}", g.name);
            continue;
        }
        let started = Instant::now();
        let run = crate::tree_run::run(
            &argv(g, ""),
            &cwd,
            started + budget,
            &AtomicBool::new(false),
        );
        let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        match run {
            TreeRun::Passed => {
                note(&g.name, crate::gate_stamp::RunOutcome::Passed, ms);
                println!(
                    "tree lint proven: {} ({:.1}s)",
                    g.name,
                    started.elapsed().as_secs_f32()
                );
                tokens.push(g.stamp_key());
            }
            other => {
                let outcome = match other {
                    TreeRun::Failed(_) => crate::gate_stamp::RunOutcome::Failed,
                    TreeRun::TimedOut | TreeRun::Cancelled => {
                        crate::gate_stamp::RunOutcome::Cancelled
                    }
                    _ => crate::gate_stamp::RunOutcome::Unavailable,
                };
                note(&g.name, outcome, ms);
                println!("tree lint not proven: {} — {other:?}", g.name);
            }
        }
    }
    flush_evidence(tree);
    if !tokens.is_empty() && !crate::gate_stamp::stamp_tree(tree, &tokens) {
        println!("git refused the tree note — nothing stamped");
        return Vec::new();
    }
    tokens
}

/// [`tree_verdict`], waiting — at most `amont.treeLintWait` counted from
/// `started` (the start of pre-push), as ONE deadline for every gate — for a
/// rehearsal running on the tree of one of `tips` to stamp its tree gates.
/// Independent of `amont.rehearsalWait`: it neither uses nor extends it.
pub fn await_verdict(
    settings: &crate::config::Settings,
    gates: &[TreeGate],
    tips: &[String],
    started: Instant,
) -> (Vec<String>, Vec<String>) {
    let secs = crate::config::integer_or(settings, WAIT, 30, 0..=3600);
    let until = started + Duration::from_secs(u64::try_from(secs).unwrap_or(30));
    let trees: Vec<String> = tips
        .iter()
        .filter_map(|t| crate::git::stdout(&["rev-parse", &format!("{t}^{{tree}}")]))
        .collect();
    loop {
        let verdict = tree_verdict(gates, tips);
        let rehearsing =
            crate::rehearsal::read().is_some_and(|s| s.alive() && trees.contains(&s.tree));
        if verdict.1.is_empty() || !rehearsing || Instant::now() >= until {
            return verdict;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
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
        let Some(version) = crate::tree_cache::tool_version(
            &cwd,
            g,
            Instant::now() + Duration::from_secs(60),
            &AtomicBool::new(false),
        ) else {
            println!("{}: its tool's version could not be read — skipped", g.name);
            continue;
        };
        let Some(ns) = crate::tree_cache::namespace(g, &version) else {
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
/// then nothing is waited for and nothing is stamped. Cold gates go to the
/// background warm-up.
pub fn finish(settings: &crate::config::Settings, car: SideCar, stampable: bool) -> Vec<String> {
    use crate::gate_stamp::RunOutcome;
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
    let mut cold = Vec::new();
    let mut busy = Vec::new();
    let mut slow = Vec::new();
    for (name, handle) in car.running {
        let decision = handle
            .join()
            .unwrap_or(Decision::Ran(TreeRun::Spawn("thread died".into()), 0));
        match decision {
            Decision::Ran(run, ms) => {
                // A completed run's time, or a cancelled one's lower bound:
                // what the next commit's fit test reads.
                if !matches!(run, TreeRun::Spawn(_)) {
                    crate::tree_cache::record_gate_last_ms(&name, ms);
                }
                match run {
                    TreeRun::Passed if stampable => {
                        note(&name, RunOutcome::Passed, ms);
                        proven.push(name);
                    }
                    TreeRun::Passed => note(&name, RunOutcome::Withheld, ms),
                    TreeRun::Failed(summary) => {
                        note(&name, RunOutcome::Failed, ms);
                        unproven.push(format!("{name} — {summary}"));
                    }
                    TreeRun::TimedOut | TreeRun::Cancelled => {
                        note(&name, RunOutcome::Cancelled, ms);
                        unproven.push(format!("{name} — still running when the commit was ready"));
                    }
                    TreeRun::Spawn(e) => {
                        note(&name, RunOutcome::Unavailable, ms);
                        unproven.push(format!("{name} — could not start ({e})"));
                    }
                }
            }
            Decision::Skew(why) => {
                note(&name, RunOutcome::Skew, 0);
                unproven.push(format!("{name} — {why}"));
            }
            Decision::NoVersion => {
                note(&name, RunOutcome::Skew, 0);
                unproven.push(format!(
                    "{name} — its tool's version could not be read in time"
                ));
            }
            Decision::Cold => {
                note(&name, RunOutcome::Cold, 0);
                cold.push(name);
            }
            Decision::Busy => {
                note(&name, RunOutcome::Busy, 0);
                busy.push(name);
            }
            Decision::Slow(ms) => {
                note(&name, RunOutcome::Slow, 0);
                slow.push(format!("{name} (~{:.1}s)", ms as f64 / 1000.0));
            }
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
        if !busy.is_empty() {
            say(&format!(
                "  tree lint not proven: {} — a warm-up holds the cache; CI will lint",
                busy.join(" ")
            ));
        }
        if !slow.is_empty() && !crate::live::quiet(settings) {
            say(&format!(
                "  tree lint skipped: {} — longer than this commit's checks leave; CI will lint",
                slow.join(" ")
            ));
        }
    }
    if !cold.is_empty() {
        warm_later(settings, &cold);
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
