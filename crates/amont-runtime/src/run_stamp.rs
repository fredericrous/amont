//! `amont run --stamp` and `amont stamps` (ADR-0015): a runner other than the
//! commit hook earns amont's verdict for a tree, and anyone can ask which
//! gates a tree carries a verdict for.
//!
//! The commit hook stamps the tree it is about to seal and the push hook the
//! tips it pushes. A tool that verifies a tree of its own (relais, in a clean
//! candidate worktree) had no way to leave the same record, so the person's
//! later commit and push of that exact tree ran the suite again. `run --stamp`
//! runs the named gates on HEAD of a clean worktree and writes the same tokens
//! the hooks write, through the same writers:
//!
//! - a commit-time declaration that blocks is stamped on the TREE note, under
//!   its script name ([`crate::gate_stamp::stamp_tree`]), which is what the
//!   pre-commit hook ([`crate::gate_stamp::vouched_for_staged_tree`]) and the
//!   push twin's pairing read. Not the one-shot marker: that lives in the
//!   worktree-private gitdir and dies with a throwaway worktree;
//! - a push gate is stamped on the tree and the commit
//!   ([`crate::gate_stamp::stamp_push`]), as a rehearsal does;
//! - a built-in pre-commit check gets its built-in token on the tree note. The
//!   commit hook reruns built-ins whatever the note says (they read the index);
//!   the token is evidence for `amont stamps` and the tools that read it.
//!
//! Nothing here signs. Signing stays at push time, with the key the pre-push
//! hook already uses, over the stamps it finds (decisions programme "one
//! verdict per tree", t02).
//!
//! Every failure points the usual way: a gate that did not pass, was deferred,
//! could not run or had nothing to judge writes no token, and no token means
//! the hooks run it.

use crate::check::{Check, Outcome, Severity, Stage};
use crate::manifest::Manifest;
use crate::pushrefs::PushRefs;
use crate::registry::Ctx;
use crate::ui::{highlight, valid_sign, warning_sign};

/// Exit codes of `amont run --stamp`.
pub const EXIT_STAMPED: i32 = 0;
pub const EXIT_FAILED: i32 = 1;
pub const EXIT_REFUSED: i32 = 2;
/// A gate was deferred, could not run, or had nothing to judge: no token was
/// written for it, and nothing failed.
pub const EXIT_UNSTAMPED: i32 = 4;

/// What became of one gate in a `run --stamp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    Stamped,
    /// A push gate whose commit-time twin passed in this run: its pairing at
    /// push time reads the twin's tree token, so it is not run twice.
    CoveredBy(String),
    Failed,
    /// Deferred by the busy policy (ADR-0013): never a token.
    Deferred,
    /// Could not run, warned, repaired files, not opted in, or judged nothing.
    NotStamped(&'static str),
}

/// How `amont stamps` prints its answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    Json,
    Text,
}

/// One gate's state on a tree, as `amont stamps` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Stamped,
    Attested,
    Deferred,
    Absent,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Stamped => "stamped",
            State::Attested => "attested",
            State::Deferred => "deferred",
            State::Absent => "absent",
        }
    }
}

/// Resolve `names` to check ids. A short name that reaches more than one
/// check (`cargo-test`: the commit-time declaration and the push built-in)
/// selects all of them: the push one is then covered by its twin, not run
/// twice. Empty `names`: the default set.
fn resolve(
    settings: &crate::config::Settings,
    manifest: &Manifest,
    names: &[String],
) -> Result<Vec<String>, String> {
    if names.is_empty() {
        return Ok(default_gates(settings, manifest));
    }
    let mut out: Vec<String> = Vec::new();
    for n in names {
        let ids = match crate::dispatch::resolve_check_name(n, manifest) {
            crate::dispatch::Named2::Resolved(id) => vec![id],
            crate::dispatch::Named2::Ambiguous(ids) => ids,
            crate::dispatch::Named2::Unknown => {
                return Err(format!("unknown gate {n:?} — try `amont list`"))
            }
        };
        for id in ids {
            if crate::registry::one_named(&id, manifest).is_none() {
                return Err(format!("{n:?} is not a check `--stamp` can run"));
            }
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    Ok(out)
}

/// Every gate the hooks honour a stamp for: the blocking commit-time
/// declarations, and the scoped push gates this repository opted into.
/// `hook.skip` removes a gate here as it does from a stage.
pub fn default_gates(settings: &crate::config::Settings, manifest: &Manifest) -> Vec<String> {
    let skips = crate::configured_skips(settings);
    let skipped = |id: &str| skips.iter().any(|s| crate::skip_suppresses(id, s));
    let tracked = crate::tracked_paths();
    let mut out: Vec<String> =
        crate::hooks::run_tests::blocking_commit_decls(settings, &manifest.externals)
            .into_iter()
            .map(|d| d.id)
            .filter(|id| !skipped(id))
            .collect();
    for c in crate::registry::all_stage_checks(Stage::PrePush, manifest) {
        let scope = c.scope();
        if !scope.is_unscoped() && scope.opted_in(&tracked) && !skipped(c.name()) {
            let id = c.name().to_string();
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

/// The token a gate's verdict is stamped under, and whether it is a
/// commit-time one (tree note) or a push one (tree and commit).
fn token_for(
    settings: &crate::config::Settings,
    manifest: &Manifest,
    check: &dyn Check,
) -> Option<String> {
    if check.stage() == Stage::PreCommit {
        if let Some(d) =
            crate::hooks::run_tests::blocking_commit_decls(settings, &manifest.externals)
                .into_iter()
                .find(|d| d.id == check.name())
        {
            return manifest
                .externals
                .iter()
                .find(|e| e.id == d.id)
                .and_then(|e| crate::gate_identity::external_token(e, &d.script));
        }
    }
    crate::gate_identity::token(&manifest.externals, check.name())
}

/// The commit-time declaration a push gate pairs with, if any.
fn twin_of(
    settings: &crate::config::Settings,
    manifest: &Manifest,
    check: &dyn Check,
) -> Option<String> {
    if check.stage() != Stage::PrePush {
        return None;
    }
    let short = check.pairing_name();
    crate::hooks::run_tests::blocking_commit_decls(settings, &manifest.externals)
        .into_iter()
        .find(|d| d.script == short)
        .map(|d| d.id)
}

/// Run one check by itself, keeping its outcome (a deferral wins, as in a
/// stage).
fn run_one(ctx: &Ctx, check: &dyn Check) -> Outcome {
    let slot = crate::host_slots::enter_check(check.name(), check.weight());
    let outcome = check.run(ctx);
    if slot.deferred().is_some() {
        Outcome::Deferred
    } else {
        outcome
    }
}

/// `amont run --stamp [<gate>…]`.
pub fn stamp(settings: &crate::config::Settings, manifest: &Manifest, names: &[String]) -> i32 {
    // Captured before anything runs: what the gates are handed is what the
    // stamp vouches for. Tracked changes only, as `stamp_tips` argues; the
    // untracked artefacts a gate leaves behind do not count.
    let dirty = crate::git::stdout(&["status", "--porcelain", "--untracked-files=no"]);
    match dirty.as_deref() {
        None => {
            eprintln!("amont: git would not say whether the worktree is clean — nothing stamped");
            return EXIT_REFUSED;
        }
        Some(s) if !s.trim().is_empty() => {
            eprintln!(
                "amont: run --stamp needs a clean worktree: tracked files differ from HEAD, so \
                 the gates would judge content no tree holds — commit or stash them first"
            );
            return EXIT_REFUSED;
        }
        Some(_) => {}
    }
    let (Some(head), Some(tree)) = (
        crate::git::stdout(&["rev-parse", "HEAD"]),
        crate::git::stdout(&["rev-parse", "HEAD^{tree}"]),
    ) else {
        eprintln!("amont: run --stamp needs a commit at HEAD to stamp");
        return EXIT_REFUSED;
    };
    let ids = match resolve(settings, manifest, names) {
        Ok(ids) => ids,
        Err(e) => {
            eprintln!("amont: {e}");
            return EXIT_REFUSED;
        }
    };
    if ids.is_empty() {
        println!("{} no gate to stamp here", warning_sign());
        return EXIT_STAMPED;
    }
    let checks: Vec<&dyn Check> = ids
        .iter()
        .filter_map(|id| crate::registry::one_named(id, manifest))
        .collect();
    let tracked = crate::tracked_paths();
    // Commit-time gates judge the whole tree, not an index nobody staged.
    if checks.iter().any(|c| c.stage() == Stage::PreCommit) {
        crate::dispatch::enter_all_files_mode();
    }
    let hooks_dir = std::path::PathBuf::from(".git/hooks");
    let empty = PushRefs::preloaded(Vec::new());
    // A push gate judges a push; standalone there is none, so the one ref a
    // rehearsal would push is synthesised, as `amont run pre-push` does —
    // only when a push gate actually has to run (one covered by its twin
    // needs no ref, and a repository with no remote then says nothing).
    let push: std::cell::OnceCell<Option<(PushRefs, Vec<String>)>> = std::cell::OnceCell::new();
    let push_for = || {
        push.get_or_init(|| match crate::pushrefs::synthetic_from_upstream() {
            Ok(r) => {
                let refs = PushRefs::preloaded(vec![r]);
                let changed = crate::pushrefs::changed_files(refs.get());
                Some((refs, changed))
            }
            Err(e) => {
                eprintln!("amont: push gates cannot run here: {e}");
                None
            }
        })
        .as_ref()
    };
    let mut ended: Vec<(String, Ended)> = Vec::new();
    // Commit-time first, so a push gate can be covered by its twin.
    let mut ordered = checks.clone();
    ordered.sort_by_key(|c| u8::from(c.stage() != Stage::PreCommit));
    for check in ordered {
        let name = check.name().to_string();
        let scope = check.scope();
        if !scope.opted_in(&tracked) {
            ended.push((name, Ended::NotStamped("not opted in here")));
            continue;
        }
        let end = match check.stage() {
            Stage::PreCommit => {
                if !scope.is_unscoped() && !scope.touches(&tracked) {
                    Ended::NotStamped("no tracked file in its scope")
                } else {
                    let ctx = Ctx {
                        name: check.name(),
                        args: &[],
                        hooks_dir: &hooks_dir,
                        push: &empty,
                        manifest,
                        settings,
                    };
                    finish(settings, manifest, check, run_one(&ctx, check), |tok| {
                        crate::gate_stamp::stamp_tree(&tree, &[tok])
                    })
                }
            }
            Stage::PrePush => {
                let twin = twin_of(settings, manifest, check);
                if let Some(t) =
                    twin.filter(|t| ended.iter().any(|(n, e)| n == t && *e == Ended::Stamped))
                {
                    Ended::CoveredBy(t)
                } else {
                    match push_for() {
                        None => Ended::NotStamped("no push ref to judge"),
                        Some((_, changed)) if !scope.is_unscoped() && !scope.touches(changed) => {
                            Ended::NotStamped("nothing it judges changed since the base")
                        }
                        Some((refs, _)) => {
                            let ctx = Ctx {
                                name: check.name(),
                                args: &[],
                                hooks_dir: &hooks_dir,
                                push: refs,
                                manifest,
                                settings,
                            };
                            finish(settings, manifest, check, run_one(&ctx, check), |tok| {
                                crate::gate_stamp::stamp_push(&head, &tree, &[tok])
                            })
                        }
                    }
                }
            }
        };
        ended.push((name, end));
    }
    report(&tree, &ended)
}

/// Turn one outcome into what is recorded, writing the token on a pass.
fn finish(
    settings: &crate::config::Settings,
    manifest: &Manifest,
    check: &dyn Check,
    outcome: Outcome,
    write: impl FnOnce(String) -> bool,
) -> Ended {
    match outcome {
        Outcome::Passed => match token_for(settings, manifest, check) {
            Some(tok) => {
                if write(tok) {
                    Ended::Stamped
                } else {
                    Ended::NotStamped("git refused the note")
                }
            }
            None => Ended::NotStamped("its identity cannot be worked out"),
        },
        Outcome::Failed => match crate::registry::severity_of(settings, check) {
            Severity::Block => Ended::Failed,
            Severity::Warn => Ended::NotStamped("failed, set to warn"),
        },
        Outcome::Deferred => Ended::Deferred,
        Outcome::Fixed => Ended::NotStamped("repaired files"),
        Outcome::Warned => Ended::NotStamped("warned"),
        Outcome::Unavailable => Ended::NotStamped("could not run"),
        Outcome::Inert => Ended::NotStamped("judged nothing"),
    }
}

fn report(tree: &str, ended: &[(String, Ended)]) -> i32 {
    let short = tree.get(..12).unwrap_or(tree);
    let mut code = EXIT_STAMPED;
    for (name, end) in ended {
        match end {
            Ended::Stamped => println!(
                "{} stamped {} on tree {short}",
                valid_sign(),
                highlight(name)
            ),
            Ended::CoveredBy(t) => println!(
                "{} {} covered by its commit-time twin {}",
                valid_sign(),
                highlight(name),
                highlight(t)
            ),
            Ended::Failed => {
                println!(
                    "{} {} failed — not stamped",
                    warning_sign(),
                    highlight(name)
                );
                code = EXIT_FAILED;
            }
            Ended::Deferred => {
                println!(
                    "{} {} deferred, no host slot freed — not stamped",
                    warning_sign(),
                    highlight(name)
                );
                if code == EXIT_STAMPED {
                    code = EXIT_UNSTAMPED;
                }
            }
            Ended::NotStamped(why) => {
                println!("{} {} not stamped: {why}", warning_sign(), highlight(name));
                if code == EXIT_STAMPED {
                    code = EXIT_UNSTAMPED;
                }
            }
        }
    }
    code
}

/// The ids that carry a gate's verdict: itself, and its twin at the other
/// stage. A commit-time declaration and the push gate it pairs with run the
/// same command on the same tree, so a token or a signed attestation for
/// either is the verdict of both (the push hook skips on the first, CI on
/// the second).
fn verdict_ids(
    settings: &crate::config::Settings,
    manifest: &Manifest,
    check: &dyn Check,
) -> Vec<String> {
    let mut ids = vec![check.name().to_string()];
    match check.stage() {
        Stage::PrePush => ids.extend(twin_of(settings, manifest, check)),
        Stage::PreCommit => {
            let declared =
                crate::hooks::run_tests::blocking_commit_decls(settings, &manifest.externals)
                    .into_iter()
                    .any(|d| d.id == check.name());
            if declared {
                ids.extend(
                    crate::registry::all_stage_checks(Stage::PrePush, manifest)
                        .into_iter()
                        .filter(|c| c.pairing_name() == check.pairing_name())
                        .map(|c| c.name().to_string()),
                );
            }
        }
    }
    ids
}

/// A detached checkout of a commit with no files, removed on drop: the
/// verifier judges the tree checked out where it is pointed.
struct Detached {
    dir: std::path::PathBuf,
}

impl Detached {
    fn at(commit: &str) -> Option<Detached> {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("amont-stamps-{}-{n}", std::process::id()));
        let path = dir.to_str()?;
        crate::git::succeeds(&[
            "worktree",
            "add",
            "--detach",
            "--no-checkout",
            "-q",
            path,
            commit,
        ])
        .then_some(Detached { dir })
    }
}

impl Drop for Detached {
    fn drop(&mut self) {
        if let Some(path) = self.dir.to_str() {
            let _ = crate::git::succeeds(&["worktree", "remove", "--force", path]);
            // best-effort: scratch, swept below
        }
        let _ = std::fs::remove_dir_all(&self.dir); // best-effort: scratch left by a failed remove
    }
}

/// A commit holding `tree`, for a caller that was given a bare tree id. Fixed
/// identity and date, so the same tree is the same commit; nothing refers to
/// it and gc takes it.
fn commit_of_tree(tree: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["commit-tree", tree, "-m", "amont stamps"])
        .env("GIT_AUTHOR_NAME", "amont")
        .env("GIT_AUTHOR_EMAIL", "amont@localhost")
        .env("GIT_COMMITTER_NAME", "amont")
        .env("GIT_COMMITTER_EMAIL", "amont@localhost")
        .env("GIT_AUTHOR_DATE", "@0 +0000")
        .env("GIT_COMMITTER_DATE", "@0 +0000")
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|c| !c.is_empty())
}

/// The gate ids a signed attestation covers for `tree` on this platform.
///
/// The verifier syncs the notes from origin before it judges (up to 15 s, 45
/// if origin is dead), so [`states`] asks only when some gate is not already
/// stamped. For a tree other than the one checked out, a throwaway checkout of
/// `commit` (or of a commit made for the tree) is what it is pointed at.
fn attested_for(tree: &str, commit: Option<&str>) -> Vec<String> {
    let Some(signers) = crate::attest::default_signers() else {
        return Vec::new();
    };
    let platform = crate::attest::platform();
    let checked_out = crate::git::stdout(&["rev-parse", "HEAD^{tree}"]);
    let gates = if checked_out.as_deref() == Some(tree) {
        crate::attest::covered(&signers, None, Some(&platform))
    } else {
        let Some(commit) = commit.map(str::to_string).or_else(|| commit_of_tree(tree)) else {
            return Vec::new();
        };
        let Some(checkout) = Detached::at(&commit) else {
            return Vec::new();
        };
        crate::attest::covered_in(&checkout.dir, &signers, None, Some(&platform))
    };
    gates
        .map(|g| g.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default()
}

/// Each gate's state on `tree`: a valid local token (its own or its twin's),
/// else a signed attestation for it on this platform, else a recorded
/// deferral, else nothing. `commit` is the commit `tree` was asked for by, if
/// it was asked for by one.
pub fn states(
    settings: &crate::config::Settings,
    manifest: &Manifest,
    tree: &str,
    commit: Option<&str>,
    ids: &[String],
) -> Vec<(String, State)> {
    let tokens = crate::gate_stamp::tree_tokens(tree);
    let deferred = crate::gate_stamp::deferred_on(tree);
    let attested: std::cell::OnceCell<Vec<String>> = std::cell::OnceCell::new();
    ids.iter()
        .filter_map(|id| crate::registry::one_named(id, manifest).map(|c| (id, c)))
        .map(|(id, check)| {
            let verdict = verdict_ids(settings, manifest, check);
            let stamped = verdict
                .iter()
                .filter_map(|v| crate::registry::one_named(v, manifest))
                .filter_map(|c| token_for(settings, manifest, c))
                .any(|t| tokens.contains(&t));
            let state = if stamped {
                State::Stamped
            } else if attested
                .get_or_init(|| attested_for(tree, commit))
                .iter()
                .any(|g| verdict.contains(g))
            {
                State::Attested
            } else if deferred
                .iter()
                .any(|d| *d == crate::short_name(id) || *d == check.pairing_name())
            {
                State::Deferred
            } else {
                State::Absent
            };
            (id.clone(), state)
        })
        .collect()
}

/// `amont stamps [--json] [--tree <rev>] [<gate>…]`. Exit 0 with an answer,
/// 2 on usage (an unknown gate, a rev that names no tree).
pub fn stamps_command(
    settings: &crate::config::Settings,
    manifest: &Manifest,
    output: Output,
    tree: Option<&str>,
    names: &[String],
) -> i32 {
    let rev = tree.unwrap_or("HEAD");
    let Some(tree) = crate::git::stdout(&["rev-parse", &format!("{rev}^{{tree}}")]) else {
        eprintln!("amont: {rev:?} names no tree here");
        return EXIT_REFUSED;
    };
    // The commit the tree was asked for by, when it was asked for by one.
    let commit = crate::git::stdout(&[
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("{rev}^{{commit}}"),
    ]);
    let ids = match resolve(settings, manifest, names) {
        Ok(ids) => ids,
        Err(e) => {
            eprintln!("amont: {e}");
            return EXIT_REFUSED;
        }
    };
    let states = states(settings, manifest, &tree, commit.as_deref(), &ids);
    if output == Output::Json {
        let gates: Vec<String> = states
            .iter()
            .map(|(id, s)| crate::json::string_field(id, s.as_str()))
            .collect();
        println!(
            "{}",
            crate::json::object(&[
                crate::json::string_field("tree", &tree),
                crate::json::string_field("platform", &crate::attest::platform()),
                format!("\"gates\":{}", crate::json::object(&gates)),
            ])
        );
    } else {
        println!("tree {tree} ({})", crate::attest::platform());
        for (id, s) in &states {
            println!("  {:<9} {id}", s.as_str());
        }
    }
    0
}
