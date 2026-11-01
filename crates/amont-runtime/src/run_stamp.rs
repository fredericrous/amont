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

/// Each gate's state on `tree`: a valid local token, else a signed
/// attestation for it on this platform (only for the tree checked out here,
/// which is the one the verifier reads), else a recorded deferral, else
/// nothing.
pub fn states(
    settings: &crate::config::Settings,
    manifest: &Manifest,
    tree: &str,
    ids: &[String],
) -> Vec<(String, State)> {
    let tokens = crate::gate_stamp::tree_tokens(tree);
    let deferred = crate::gate_stamp::deferred_on(tree);
    let head_tree = crate::git::stdout(&["rev-parse", "HEAD^{tree}"]);
    let attested: Vec<String> = if head_tree.as_deref() == Some(tree) {
        crate::attest::default_signers()
            .and_then(|s| crate::attest::covered(&s, None, Some(&crate::attest::platform())))
            .map(|g| g.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    ids.iter()
        .filter_map(|id| crate::registry::one_named(id, manifest).map(|c| (id, c)))
        .map(|(id, check)| {
            let own = token_for(settings, manifest, check);
            let twin = twin_of(settings, manifest, check).and_then(|t| {
                crate::registry::one_named(&t, manifest)
                    .and_then(|c| token_for(settings, manifest, c))
            });
            let stamped = own.iter().chain(twin.iter()).any(|t| tokens.contains(t));
            let script = crate::short_name(id).to_string();
            let state = if stamped {
                State::Stamped
            } else if attested.iter().any(|g| g == id) {
                State::Attested
            } else if deferred
                .iter()
                .any(|d| *d == script || *d == check.pairing_name())
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
    json: bool,
    tree: Option<&str>,
    names: &[String],
) -> i32 {
    let rev = tree.unwrap_or("HEAD");
    let Some(tree) = crate::git::stdout(&["rev-parse", &format!("{rev}^{{tree}}")]) else {
        eprintln!("amont: {rev:?} names no tree here");
        return EXIT_REFUSED;
    };
    let ids = match resolve(settings, manifest, names) {
        Ok(ids) => ids,
        Err(e) => {
            eprintln!("amont: {e}");
            return EXIT_REFUSED;
        }
    };
    let states = states(settings, manifest, &tree, &ids);
    if json {
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
