//! The two dispatchers.
//!
//! They are NOT the same shape, and both shapes are load-bearing:
//!
//! - `pre-commit` runs its checks CONCURRENTLY and reports EVERY failure.
//!   Serial would be a visible slowdown on each commit; stopping at the first
//!   failure would hide the rest, so you'd fix one lint error, commit, and
//!   immediately meet the next.
//! - `pre-push` runs them SERIALLY and stops at the FIRST failure, naming just
//!   that check. The steps are ordered and expensive (protected branch, then
//!   branch name, then rebase, then the whole test suite) and there is no point
//!   running tests after a rebase conflict.
//!
//! Resist the tempting shared `run_all` helper — collapsing these is the
//! obvious way to silently lose the distinction. `tests/dispatchers.rs` pins
//! both.
//!
//! Checks are FUNCTIONS in this binary, called directly. They used to be files:
//! `.git/hooks/pre-commit-*`, each an identical `sh` shim whose only job was to
//! re-exec this same binary and tell it its own name. One commit therefore cost
//! 27 processes — a shim, the binary, then 13 more shims and 13 more binaries —
//! to do work the binary already had in a table.
//!
//! Deleting that removed the filename glob (order was lexicographic, so a
//! rename could silently reorder a gate), the shebang emulation Windows needed
//! because it cannot execute a `#!` script, and the spawn plumbing under both.
//! Order is now a declared list in `registry`.

use std::sync::Mutex;

use crate::check::{Check, Outcome, Severity, Stage, Verdict};
use crate::configured_skips;
use crate::registry::{all_stage_checks, Ctx, Overrides};
use crate::ui::{highlight, valid_sign, warning_sign};

/// The checks for a stage, minus anything `hook.skip` filters out. Resolution
/// goes through `names_check`, the one rule this and the severity lookup share:
/// `git config hook.skip ruff` skips `pre-commit-ruff` by short name.
fn selected(stage: Stage, manifest: &crate::manifest::Manifest) -> Vec<&dyn Check> {
    selected_during(stage, &[], manifest)
}

/// Do this repository's hooks apply the CONVENTIONS, or only the safety net?
///
/// `git config amont.conventions declared` (usually `--global`, set by
/// `amont enroll`) scopes the house rules to repositories that commit an
/// `amont.conf` — the standing grant of `init.templateDir` then becomes safe
/// to hand a whole team: a clone of somebody else's project gets conflict,
/// secret, size and debug-leftover protection, and none of this team's
/// opinions about commit subjects or branch names. The default,
/// `everywhere`, keeps today's behaviour exactly.
///
/// Presence of the manifest is the declaration; its CONTENT stays
/// trust-gated. Reading presence executes nothing, so no consent is needed.
pub fn conventions_apply(manifest: &crate::manifest::Manifest) -> bool {
    manifest.declared || !declared_mode()
}

/// One config read per process — this sits on the hook path of every commit.
fn declared_mode() -> bool {
    static MODE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| {
        crate::config::enumerated_or(
            "amont.conventions",
            &["everywhere", "declared"],
            "everywhere",
        ) == "declared"
    })
}

/// The checks for a stage, minus `hook.skip` and minus anything that declares
/// it does not run during an operation currently in progress.
fn selected_during<'a>(
    stage: Stage,
    in_progress: &[crate::check::GitState],
    manifest: &'a crate::manifest::Manifest,
) -> Vec<&'a dyn Check> {
    let skips = configured_skips();
    // Externals are included here, so `hook.skip` and the severity override
    // govern a declared command exactly as they govern a built-in. A repository
    // that can add a check it cannot disable would be a worse deal than not
    // being able to add one.
    let (kept, dropped): (Vec<_>, Vec<_>) = all_stage_checks(stage, manifest)
        .into_iter()
        .partition(|c| !skips.iter().any(|s| crate::skip_suppresses(c.name(), s)));
    let names: Vec<&str> = dropped.iter().map(|c| c.name()).collect();
    announce_skips(&names);

    // Announced separately from `hook.skip`, and with the operation named: "not
    // during a rebase" is a property of the moment and will be true again in a
    // minute, which is a different thing to tell a reader than "you disabled
    // this".
    let (kept, paused): (Vec<_>, Vec<_>) = kept.into_iter().partition(|check| {
        !check
            .scope()
            .not_during
            .iter()
            .any(|state| in_progress.contains(state))
    });
    if !paused.is_empty() {
        let what = in_progress
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(" and ");
        println!(
            "{} {} check(s) paused during {what}: {}",
            warning_sign(),
            paused.len(),
            paused
                .iter()
                .map(|c| c.name())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    // The conventions split, last: a held-back check was neither skipped (a
    // choice about THIS repository) nor paused (a property of the moment) —
    // this repository simply never subscribed. One line, count not names:
    // in the clone-of-somebody-else's-project case this prints on every
    // commit, and fifteen names every time is how a safety message becomes
    // scroll-past noise.
    if conventions_apply(manifest) {
        return kept;
    }
    let (kept, held): (Vec<_>, Vec<_>) = kept
        .into_iter()
        .partition(|check| check.reach() == crate::check::Reach::Safety);
    if !held.is_empty() {
        println!(
            "{} {} convention check(s) held back — no amont.conf here and \
             amont.conventions is `declared`; the safety net still runs",
            warning_sign(),
            held.len(),
        );
    }
    kept
}

/// Say out loud which checks did not run.
///
/// A skip is otherwise invisible at exactly the moment it matters. With
/// `hook.skip = merge-conflict` set, a commit printed six green ticks and no
/// hint that a seventh check had been disabled — the developer sees a clean run
/// and concludes they are covered.
///
/// It is worse than it sounds, because one value can silence a whole stage:
/// `hook.skip = pre-commit` suppresses all fifteen. That is now something
/// somebody meant rather than the accident it once was — `e` used to cost
/// twenty by substring reach — but a commit under it still looks exactly like a
/// commit that had nothing to report.
///
/// One line, only when something was actually skipped, so a normal commit is
/// unchanged. This reaches every skip however it was created — hand-edited
/// config included — which no dashboard can claim.
fn announce_skips(dropped: &[&str]) {
    if dropped.is_empty() {
        return;
    }
    // Two lines, not one: "you decided this" (hook.skip on this machine)
    // and "your team decided this" (a skip line in the committed
    // amont.conf) are different things to be told — the same reason paused
    // and held-back get their own sentences. A name both sources suppress
    // is announced as the machine's: the local decision is the nearer one.
    let (machine, _policy) = crate::skips_by_source();
    let (yours, theirs): (Vec<&&str>, Vec<&&str>) = dropped
        .iter()
        .partition(|name| machine.iter().any(|s| crate::skip_suppresses(name, s)));
    let say = |names: &[&&str], via: &str| {
        if names.is_empty() {
            return;
        }
        let plural = if names.len() == 1 { "check" } else { "checks" };
        println!(
            "{} {} {plural} skipped by {}: {}",
            warning_sign(),
            names.len(),
            highlight(via),
            names.iter().map(|n| **n).collect::<Vec<_>>().join(", ")
        );
    };
    say(&yours, "hook.skip");
    say(&theirs, "amont.conf");
}

/// Say, once per stage, what the manifest's policy could not do — withheld
/// behind trust, or aiming at names that exist nowhere. Policy that silently
/// does not apply is a silent behaviour change, which is the one kind this
/// codebase does not allow itself.
fn announce_policy_state(manifest: &crate::manifest::Manifest) {
    if let Some(why) = manifest.policy_withheld {
        println!(
            "{} {} policy not applied: {why}",
            warning_sign(),
            highlight(crate::manifest::MANIFEST),
        );
    }
    for note in &manifest.policy_notes {
        println!("{} {}", warning_sign(), note);
    }
    // The version floor rides the same two call sites: once per stage,
    // beside the other "this repository expects something you lack" lines.
    crate::skew::announce_minimum();
}

/// Run every item concurrently and collect `(name, code)` in the INPUT order.
///
/// Extracted so the concurrency itself can be tested with a rendezvous instead
/// of a stopwatch — an earlier wall-clock test was flaky the moment the machine
/// was busy, and a threshold that trips under load teaches you to ignore it.
fn run_concurrently<T, R, F>(items: &[T], run: F, if_thread_died: R) -> Vec<R>
where
    T: Sync,
    R: Send + Sync + Clone,
    F: Fn(&T) -> R + Sync,
{
    let slots: Vec<Mutex<Option<R>>> = items.iter().map(|_| Mutex::new(None)).collect();
    std::thread::scope(|scope| {
        for (item, slot) in items.iter().zip(&slots) {
            let run = &run;
            let died = &if_thread_died;
            scope.spawn(move || {
                // CAUGHT, not propagated. `thread::scope` re-raises a child
                // panic in the parent, which would abort the whole hook with a
                // backtrace and throw away the other nineteen checks' results —
                // and would make `if_thread_died` unreachable, which is what it
                // was until this test existed to notice.
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(item)))
                    .unwrap_or_else(|_| died.clone());
                *slot.lock().expect("poisoned") = Some(outcome);
            });
        }
    });
    slots
        .into_iter()
        .map(|s| {
            s.into_inner()
                .expect("poisoned")
                .unwrap_or_else(|| if_thread_died.clone())
        })
        .collect()
}

/// Take the index-fidelity hold, or say why the caller must stop.
///
/// Extracted from `pre_commit` so that `amont run` — which its own doc
/// comment calls "a rehearsal of the hook" — can take exactly the same hold
/// rather than judging the working tree while a real commit judges the index.
///
/// Around the WHOLE fan-out, not per check: twenty checks run concurrently and
/// would fight over one working tree.
fn hold_unstaged() -> Result<crate::staged_only::StagedOnly, Verdict> {
    // BEFORE `enter()`, not after: `enter()` is what checks out the tree and
    // parks the unstaged half, and a signal landing in the gap between that
    // and the handler being armed would hit the default disposition — dead
    // process, tree left checked out, nothing restored. The handler no-ops
    // harmlessly on a signal that arrives before there is anything held.
    crate::staged_only::install_signal_handler();
    match crate::staged_only::StagedOnly::enter() {
        Ok(guard) => Ok(guard),
        Err(e) => {
            // Refusing to check the wrong content is the safe direction; a
            // check that read the tree would be answering about a commit
            // nobody is making.
            eprintln!("{e}");
            Err(Verdict::Block)
        }
    }
}

pub fn pre_commit(ctx: &Ctx) -> Verdict {
    // Before anything runs: a pinned tool at the wrong version makes every
    // verdict below it suspect, and the warning costs one --version per pin.
    crate::manifest::verify_tool_pins(&ctx.manifest.pins);
    announce_policy_state(ctx.manifest);
    let in_progress = crate::git_states_in_progress();
    let checks = selected_during(Stage::PreCommit, &in_progress, ctx.manifest);

    let held = match hold_unstaged() {
        Ok(guard) => guard,
        Err(verdict) => return verdict,
    };

    let severities = Overrides::read();
    let (verdict, outcomes) = run_stage_traced(&checks, ctx, &severities);

    // The shadow-mode ledger. Silent, best-effort, and never consulted by any
    // verdict — see `crate::downgrade`.
    crate::downgrade::note(&downgraded_events(&checks, &outcomes, &severities));

    // What post-commit will bind to the commit: the gate-declared checks
    // that RAN clean, recorded while the index still is the commit's tree.
    // Called on every verdict — an empty record clears any leftover marker,
    // so a blocked attempt (or a repo with nothing declared) cannot leave an
    // earlier attempt's marker to vouch for the next commit. `Unavailable`
    // deliberately does not qualify: a check whose tool is missing judged
    // nothing, and stamping it would be the paper promise this exists to
    // replace.
    // EVERY blocking declaration, not only the npm GATE names: a custom
    // `pre-commit check … block …` earns its stamp the same way, and a
    // same-named pre-push declaration defers to it (see `pair_verdict`).
    let ran: Vec<String> = if matches!(verdict, Verdict::Block) {
        Vec::new()
    } else {
        crate::hooks::run_tests::blocking_commit_decls(&ctx.manifest.externals)
            .into_iter()
            .filter(|d| {
                checks
                    .iter()
                    .zip(&outcomes)
                    .any(|(c, o)| c.name() == d.id && matches!(o, Outcome::Passed | Outcome::Fixed))
            })
            .map(|d| d.script)
            .collect()
    };
    let ran: Vec<&str> = ran.iter().map(String::as_str).collect();
    crate::gate_stamp::record(&ran);

    drop(held);
    verdict
}

/// The checks that FAILED without blocking, each with why — the shadow-mode
/// signal [`crate::downgrade`] keeps.
///
/// Derived at the hook entry points and deliberately NOT inside
/// [`run_stage_traced`], which is where `classify` already computes the same
/// set. `run_all` reaches that function too, so recording there would let
/// every `amont run` rehearsal inflate a ledger a lead is going to read as a
/// count of real commits — and `run --all-files` over a dirty tree would add
/// dozens of events for content nobody is committing.
///
/// A check that DECLARES `warn` is counted but is not evidence about a
/// rollout: it was never going to block, so calling it "would have blocked"
/// would inflate the one number the whole feature exists to produce. Only an
/// override of a blocking check earns that.
fn downgraded_events(
    checks: &[&dyn Check],
    outcomes: &[Outcome],
    severities: &Overrides,
) -> Vec<(String, crate::downgrade::Origin)> {
    checks
        .iter()
        .zip(outcomes)
        .filter(|(_, o)| matches!(o, Outcome::Failed))
        .filter(|(c, _)| matches!(severities.of(**c), Severity::Warn))
        .map(|(c, _)| (c.name().to_string(), downgrade_origin(*c, severities)))
        .collect()
}

/// Why this check did not block. Shared, because `pre_push` fail-fasts and
/// never builds an outcomes vector to hand to [`downgraded_events`].
fn downgrade_origin(check: &dyn Check, severities: &Overrides) -> crate::downgrade::Origin {
    use crate::downgrade::Origin;
    use crate::registry::Source;
    if matches!(check.severity(), Severity::Warn) {
        return Origin::Declared;
    }
    match severities.applied_with_source(check.name()) {
        Some((_, _, Source::Config)) => Origin::Config,
        Some((_, _, Source::Policy)) => Origin::Policy,
        // Declared `block`, resolved `warn`, and nothing claims to have
        // overridden it: a contradiction. Count it, but not as evidence — the
        // conservative direction for a number whose only failure mode is
        // being too alarming.
        None => Origin::Declared,
    }
}

/// The pre-commit body, over the checks it is GIVEN.
///
/// A seam, so a test can hand it a check that panics. Without it the value
/// standing in for a dead check was a literal at one call site that no test
/// could reach — the rule was asserted on the runner and merely hoped for here.
fn run_stage(checks: &[&dyn Check], ctx: &Ctx, severities: &Overrides) -> Verdict {
    run_stage_traced(checks, ctx, severities).0
}

/// [`run_stage`], keeping the per-check outcomes — index-aligned with
/// `checks` — alive past the verdict. `pre_commit` needs them to know which
/// gate-declared checks actually ran (`gate_stamp`); `Report` cannot answer
/// that, because `classify` deliberately drops the names of `Passed`.
fn run_stage_traced(
    checks: &[&dyn Check],
    ctx: &Ctx,
    severities: &Overrides,
) -> (Verdict, Vec<Outcome>) {
    if checks.is_empty() {
        return (Verdict::Proceed, Vec::new());
    }
    // One slot per check: everything a check says lands in its own buffer
    // and reaches stdout as ONE block when it finishes — see `live`. Off
    // (`amont.progress false`), no sink is ever installed and every print
    // streams exactly as it always did.
    let stage = crate::live::enabled().then(|| {
        let names: Vec<&str> = checks.iter().map(|c| c.name()).collect();
        crate::live::Stage::begin(&names)
    });
    let items: Vec<(usize, &&dyn Check)> = checks.iter().enumerate().collect();
    let outcomes = run_concurrently(
        &items,
        |(idx, check)| {
            let _sink = stage.as_ref().map(|s| s.enter(*idx));
            // The block is emitted however the check leaves — a panicking
            // check's partial output still reaches the reader, above the
            // dead-check verdict `run_concurrently` fills in.
            let _flush = stage
                .as_ref()
                .map(|s| crate::live::FinishOnDrop::new(s, *idx));
            let sub = Ctx {
                name: check.name(),
                args: ctx.args,
                hooks_dir: ctx.hooks_dir,
                push: ctx.push,
                manifest: ctx.manifest,
            };
            check.run(&sub)
        },
        // A check whose thread died has not passed. Stated here, where the slot
        // is filled, rather than hidden in a `Default` impl that every future
        // `#[derive(Default)]` would silently inherit.
        Outcome::Failed,
    );

    let report = classify(checks, &outcomes, severities);
    announce(&report);
    (report.verdict(), outcomes)
}

/// What a stage concluded, before anything is printed or exited.
///
/// A VALUE, so the classification can be asserted directly. While this was one
/// function that classified, printed and returned an exit code, its tests could
/// only check the code — whether the right thing was SAID went untested.
#[derive(Debug, Default, PartialEq, Eq)]
struct Report<'a> {
    /// Repaired. The commit proceeds, but the author's files changed under
    /// them and that must be said out loud.
    fixed: Vec<&'a str>,
    /// Failed, and the severity that applies blocks.
    blocked: Vec<&'a str>,
    /// Failed, but configured to warn. The check printed an error and meant it,
    /// so somebody has to say it did not block.
    downgraded: Vec<&'a str>,
    /// Could not run. Distinct from "passed", which is the whole point.
    unavailable: Vec<&'a str>,
}

impl Report<'_> {
    fn verdict(&self) -> Verdict {
        Verdict::blocking(!self.blocked.is_empty())
    }
}

/// Pure: outcomes and severities in, a verdict out. No IO.
fn classify<'a>(
    checks: &[&'a dyn Check],
    outcomes: &[Outcome],
    severities: &Overrides,
) -> Report<'a> {
    let mut report = Report::default();
    for (check, outcome) in checks.iter().zip(outcomes) {
        match outcome {
            // `Warned` needs nothing: a check that chose to warn has already
            // said what it wanted to, and a roll-up would only repeat it.
            Outcome::Passed | Outcome::Warned => {}
            Outcome::Fixed => report.fixed.push(check.name()),
            Outcome::Unavailable => report.unavailable.push(check.name()),
            Outcome::Failed => match severities.of(*check) {
                Severity::Block => report.blocked.push(check.name()),
                Severity::Warn => report.downgraded.push(check.name()),
            },
        }
    }
    report
}

/// Says what happened. Prints; decides nothing.
fn announce(report: &Report) {
    if !report.fixed.is_empty() {
        // Louder than a pass, because files on disk are not what the author
        // left them: they asked for the repair, but they did not watch it.
        println!(
            "{} {} check(s) fixed and re-staged: {}",
            valid_sign(),
            report.fixed.len(),
            report.fixed.join(", ")
        );
    }
    if !report.unavailable.is_empty() {
        // Distinct from "passed". Silence here is how a repo looks verified
        // when nothing actually ran — the trailing count is the one line
        // guaranteed to be read, whatever the twenty blocks above said.
        println!(
            "{} {} check(s) could not run: {}",
            warning_sign(),
            report.unavailable.len(),
            report.unavailable.join(", ")
        );
    }
    if !report.downgraded.is_empty() {
        println!(
            "{} {} check(s) reported a problem but are set to warn: {}",
            warning_sign(),
            report.downgraded.len(),
            report.downgraded.join(", ")
        );
    }
    if report.blocked.is_empty() {
        return;
    }
    println!("\n🚨  Error raised by:");
    for name in &report.blocked {
        println!("    - {}", highlight(name));
    }
}

/// Point every check at `git ls-files` instead of the index.
///
/// THE definition, called from both entry points. There used to be two: this
/// one, and a copy in `main.rs` built from a RAW `ls-files` — no `-z` — whose
/// output git QUOTES for any unusual byte, so `é.json` arrived as the nine-byte
/// literal `"\303\251.json"` and was handed to prettier and eslint as a path
/// that does not exist. And because `override_file_set` writes a `OnceLock`,
/// main's quoted list WON: whichever ran first was the one that counted, and
/// main's ran first. `git.rs` documents this exact failure.
pub fn enter_all_files_mode() {
    crate::hooks::common::override_file_set(
        crate::git::stdout_paths(&["ls-files"]).unwrap_or_default(),
    );
}

/// `amont run` — every applicable check, on demand.
///
/// Two questions, and the mode says which it answers:
///
/// - **staged** (default) is "would my commit pass" — the same set a commit
///   would check, so it is a rehearsal of the hook, and it takes the same
///   index-fidelity hold the hook takes.
/// - **`--all-files`** is "does my working tree pass". Deliberately NOT the same
///   question: on a dirty tree it reports on content that is not committed and
///   may never be. That is right for adopting a check into an existing
///   repository, where `git add .` is not an acceptable way to measure the mess,
///   and it is why `--all-files` takes no stash — there is no staged/unstaged
///   distinction to protect when the answer is "all of it".
pub fn run_all(ctx: &Ctx, all_files: bool) -> Verdict {
    // ORDER: the override goes in FIRST. It is what tells `fixing_enabled` and
    // `restage` that the file set is not the index, and both are consulted
    // from inside the checks below.
    if all_files {
        enter_all_files_mode();
        if crate::hooks::common::fixing_requested() {
            println!(
                "{} {} is set, but fixing is off for {}: the input set is the \
                 working tree, not the index",
                warning_sign(),
                highlight("amont.fix"),
                highlight("--all-files")
            );
        }
        // Stash-free, per decision 1 of docs/index-fidelity-and-run-modes.md:
        // there is no staged/unstaged distinction to protect when the input
        // set is `git ls-files`, so a hold would be surprising extra mutation
        // with no correctness upside.
        return run_stage(
            &selected(Stage::PreCommit, ctx.manifest),
            ctx,
            &Overrides::read(),
        );
    }

    // Staged mode IS a rehearsal of the commit, so it takes the same hold the
    // commit does. Without it, `amont run` failed on garbage in the tree
    // that `git commit` — which holds the unstaged half aside — passed, and
    // vice versa: the two modes disagreed about the same repository, which is
    // exactly what this mode exists not to do.
    let held = match hold_unstaged() {
        Ok(guard) => guard,
        Err(verdict) => return verdict,
    };
    let verdict = run_stage(
        &selected(Stage::PreCommit, ctx.manifest),
        ctx,
        &Overrides::read(),
    );
    // AFTER the report has been printed: dropping earlier would put the
    // unstaged content back under a check that is still reading files.
    drop(held);
    verdict
}

/// `amont run <check>` — one check by name. `None` when there is no such
/// check, which the caller turns into a usage error.
///
/// Lives here rather than in `main.rs` so `registry::lookup` stays inside the
/// runtime, and so the hold decision is made once: a named check takes the
/// index-fidelity hold only when it is a `Stage::PreCommit` check running in
/// staged mode. A pre-push or commit-msg check invoked by name must never
/// touch the working tree — nothing about a push is a staging operation.
/// Resolve what `amont run <name>` means, exactly as `hook.skip` resolves a
/// name — the rest of the tool taught `ban-terms`; making `run` demand the
/// full id was a pointless second vocabulary. Ambiguity is an answer, not a
/// guess: the two `branch-pattern` checks are different code at different
/// stages.
///
/// Public because the CALLER needs the answer before anything else happens:
/// main decides whether to synthesize push refs from the resolved name, and
/// an ambiguous name must say so rather than fail on a missing upstream it
/// was never going to use.
pub fn resolve_check_name(name: &str, manifest: &crate::manifest::Manifest) -> Named2 {
    if crate::registry::lookup(name, manifest).is_some() {
        return Named2::Resolved(name.to_string());
    }
    let mut matches: Vec<String> = crate::registry::CHECKS
        .iter()
        .map(|c| c.name.to_string())
        .chain(manifest.externals.iter().map(|e| e.id.clone()))
        .filter(|id| crate::skip_suppresses(id, name))
        .collect();
    matches.dedup();
    match matches.len() {
        0 => Named2::Unknown,
        1 => Named2::Resolved(matches.remove(0)),
        _ => Named2::Ambiguous(matches),
    }
}

/// How a run name resolved.
pub enum Named2 {
    Resolved(String),
    Unknown,
    Ambiguous(Vec<String>),
}

pub fn run_named(ctx: &Ctx, name: &str, all_files: bool) -> Named {
    let full: String = match resolve_check_name(name, ctx.manifest) {
        Named2::Resolved(id) => id,
        Named2::Unknown => return Named::Unknown,
        Named2::Ambiguous(ids) => return Named::Ambiguous(ids),
    };
    let name = full.as_str();
    let Some(run_check) = crate::registry::lookup(name, ctx.manifest) else {
        return Named::Unknown;
    };
    // The Ctx must carry the RESOLVED id: lookup's closure re-resolves
    // through `ctx.name`, and handing it the short name back would panic on
    // the very ambiguity this function just settled.
    let ctx = &Ctx {
        name,
        args: ctx.args,
        hooks_dir: ctx.hooks_dir,
        push: ctx.push,
        manifest: ctx.manifest,
    };
    if all_files {
        enter_all_files_mode();
        return Named::Ran(run_check(ctx));
    }
    let is_pre_commit_check = crate::registry::one_named(name, ctx.manifest)
        .is_some_and(|c| c.stage() == Stage::PreCommit);
    if !is_pre_commit_check {
        return Named::Ran(run_check(ctx));
    }
    let held = match hold_unstaged() {
        Ok(guard) => guard,
        Err(verdict) => return Named::Ran(verdict),
    };
    let verdict = run_check(ctx);
    drop(held);
    Named::Ran(verdict)
}

/// What `run_named` resolved a name to.
pub enum Named {
    Ran(Verdict),
    /// Nothing matches — full id, short name, or entrypoint.
    Unknown,
    /// A short name that reaches more than one check; the caller lists them
    /// so the user can pick a full id.
    Ambiguous(Vec<String>),
}

pub fn pre_push(ctx: &Ctx) -> Verdict {
    // The notes push `attest` makes re-enters this hook; its ref list is only
    // ever the attest ref, so there is nothing to prove — and proving it
    // would recurse.
    if crate::attest::push_guard_active() {
        return Verdict::Proceed;
    }
    crate::manifest::verify_tool_pins(&ctx.manifest.pins);
    announce_policy_state(ctx.manifest);
    // NB: no CHERRY_PICK_HEAD check here — the zsh pre-push had none either.
    let severities = Overrides::read();
    // pre-push had NO state guard at all, with a comment admitting it existed
    // only because the zsh version had none. Now it asks the same question
    // pre-commit does and each check answers for itself.
    let in_progress = crate::git_states_in_progress();
    let pre_push_checks = selected_during(Stage::PrePush, &in_progress, ctx.manifest);
    let stage = crate::live::enabled().then(|| {
        let names: Vec<&str> = pre_push_checks.iter().map(|c| c.name()).collect();
        crate::live::Stage::begin(&names)
    });
    // What actually PASSED, for the attestation at the bottom. `Warned` and
    // `Unavailable` stay out — "could not run" is not "passed" — and a
    // commit-time-gated pair counts, because its stamps say the check ran on
    // every pushed tree.
    let mut passed: Vec<String> = Vec::new();
    // Accumulated rather than written per check: one append at the end costs a
    // single file open, and the loop below can leave early.
    let mut downgraded: Vec<(String, crate::downgrade::Origin)> = Vec::new();
    // PUSH STAMPS. The tips being pushed, and what earlier runs of THIS gate
    // recorded against their trees — an `amont run pre-push` rehearsal, or a
    // push whose gate passed and whose transport then died. A scoped gate
    // (a test suite: its verdict is a function of the tree alone) whose
    // stamp sits on every tip is not run again; the unscoped ones
    // (branch-protect, secrets — questions about the push, not the content)
    // always run. `amont.pushStamps false` turns the reuse off.
    //
    // Reading `ctx.push` here may consume stdin, exactly as `attest` does
    // below; every pre-push check reads it anyway.
    let tips: Vec<String> = {
        let mut t: Vec<String> = ctx
            .push
            .get()
            .iter()
            .filter(|r| !r.local_oid.chars().all(|c| c == '0'))
            .map(|r| r.local_oid.clone())
            .collect();
        t.dedup();
        t
    };
    let reuse_stamps = crate::gate_stamp::push_stamps_enabled();
    // The working tree AS THE SUITE WILL BE HANDED IT, captured before any
    // gate has run.
    //
    // Timing is the point. Asking afterwards means a gate that modifies a
    // tracked file — a formatter, a suite that updates a snapshot fixture —
    // disqualifies its OWN stamp. What a stamp needs to know is what the
    // suite could READ, which is the state it was handed; a change the suite
    // itself made was never an input to it.
    //
    // Tracked modifications only. `stamp_tips` argues that gap, which is a
    // deliberate one.
    let tree_at_start = if reuse_stamps {
        crate::git::stdout(&["status", "--porcelain", "--untracked-files=no"]).unwrap_or_default()
    } else {
        String::new()
    };
    // A background rehearsal of one of these tips may be mid-suite right
    // now. Waiting for it is strictly less work than starting over, and
    // its stamp — read AFTER the wait, below — is the hand-off.
    if reuse_stamps && !tips.is_empty() {
        crate::rehearsal::await_for(&tips);
    }
    let push_stamps = if reuse_stamps && !tips.is_empty() {
        crate::gate_stamp::stamps_for(&tips)
    } else {
        Default::default()
    };
    // Inside a rehearsal snapshot only the content gates make sense: the
    // unscoped checks ask about a PUSH — its branch name, its target, its
    // secrets — and no push is happening. Said once, not per check.
    let rehearsing = crate::rehearsal::in_snapshot();
    if rehearsing {
        crate::say!(
            "rehearsal: running the test gates only — the push-shaped checks run at push time"
        );
    }
    let stamped_on_every_tip = |name: &str| -> bool {
        !tips.is_empty()
            && tips.iter().all(|t| {
                push_stamps
                    .get(t)
                    .is_some_and(|s| s.iter().any(|g| g == name))
            })
    };
    // What actually RAN and passed here (as opposed to being vouched for by
    // a stamp) — the set this run may stamp in turn.
    let mut ran_and_passed: Vec<String> = Vec::new();
    for (idx, check) in pre_push_checks.iter().enumerate() {
        let _sink = stage.as_ref().map(|s| s.enter(idx));
        let _flush = stage
            .as_ref()
            .map(|s| crate::live::FinishOnDrop::new(s, idx));
        // A declared pre-push external whose NAME is also declared at
        // pre-commit (blocking) is a gate pair: the commit-time side earned
        // per-commit stamps, and this side runs only for pushes carrying
        // commits with no record of it — the same contract the npm gate has
        // always had, for vocabularies npm never heard of (`cargo test`,
        // `pytest`, anything). Messages mirror the npm gate's exactly;
        // docs/checks.md quotes them.
        // The push side of a pair is `(name, scope)`, from whichever of the
        // two kinds of check this is. A declared pre-push external supplies
        // its own; anything else is a BUILT-IN, and `pairing_name` plus the
        // registry's scope say the same two things about it.
        //
        // The external is tried FIRST and its inputs are unchanged, so the
        // declared path behaves exactly as before — that is what the
        // untouched declared-pair tests prove.
        if rehearsing && check.scope().is_unscoped() {
            continue;
        }
        if !check.scope().is_unscoped() && stamped_on_every_tip(check.name()) {
            crate::say!(
                "{} {} passed on this exact tree earlier — not repeating it here",
                valid_sign(),
                highlight(check.name()),
            );
            passed.push(check.name().to_string());
            continue;
        }
        let declared = ctx
            .manifest
            .externals
            .iter()
            .find(|e| e.stage == Stage::PrePush && e.id == check.name());
        let builtin_scope = check.scope();
        let pairing: Option<(&str, &crate::check::Scope)> = match declared {
            Some(ext) => match &ext.kind {
                crate::manifest::Kind::Runnable { scope, .. } => {
                    Some((ext.short_name.as_str(), scope))
                }
                // A declared pre-push entry that runs nothing has no scope to
                // judge with, and is not a built-in either. Nothing to pair.
                _ => None,
            },
            None => Some((check.pairing_name(), &builtin_scope)),
        };
        if let Some((name, push_scope)) = pairing {
            match crate::hooks::run_tests::pair_verdict(name, push_scope, ctx.manifest, ctx.push) {
                crate::hooks::run_tests::PairVerdict::Gated => {
                    crate::say!(
                        "{} {} gated at commit instead — not repeating it here",
                        valid_sign(),
                        highlight(name),
                    );
                    passed.push(check.name().to_string());
                    continue;
                }
                crate::hooks::run_tests::PairVerdict::Unstamped(n) => {
                    crate::say!(
                        "{} {} is declared at commit time, but {n} pushed \
                         commit{} carr{} no record of it — running it here",
                        warning_sign(),
                        name,
                        if n == 1 { "" } else { "s" },
                        if n == 1 { "ies" } else { "y" },
                    );
                }
                crate::hooks::run_tests::PairVerdict::NotPaired => {}
            }
        }
        let sub = Ctx {
            name: check.name(),
            args: ctx.args,
            hooks_dir: ctx.hooks_dir,
            push: ctx.push,
            manifest: ctx.manifest,
        };
        match check.run(&sub) {
            Outcome::Passed => {
                passed.push(check.name().to_string());
                if !check.scope().is_unscoped() {
                    ran_and_passed.push(check.name().to_string());
                }
            }
            // Announced, never fatal: a check that could not run has not
            // invalidated anything, and neither has a warning.
            Outcome::Unavailable => {
                println!(
                    "{} {} could not run",
                    warning_sign(),
                    highlight(check.name())
                )
            }
            Outcome::Warned => {}
            // Cannot occur: `Fix::Rewrite` is refused on a pre-push
            // declaration, so nothing here can repair anything.
            Outcome::Fixed => {}
            Outcome::Failed => match severities.of(*check) {
                Severity::Warn => {
                    downgraded.push((
                        check.name().to_string(),
                        downgrade_origin(*check, &severities),
                    ));
                    println!(
                        "{} {} reported a problem (severity warn)",
                        warning_sign(),
                        highlight(check.name())
                    );
                }
                // Fail-fast applies ONLY to Block: the later steps are
                // expensive and their preconditions are gone.
                Severity::Block => {
                    // Record what already warned before leaving. Those checks
                    // ran and reported; a later check blocking does not unmake
                    // them, and dropping them here would make the ledger quietly
                    // under-count every push that ended badly.
                    crate::downgrade::note(&downgraded);
                    println!("\n🚨  Error raised by hook {}", highlight(check.name()));
                    return Verdict::Block;
                }
            },
        }
    }
    // Every block gate passed — stamp the tips with the scoped gates that
    // RAN here, so the next push of this content (a retry after a dropped
    // connection, or the real push after an `amont run pre-push` rehearsal)
    // skips them. Only when what ran is what is being pushed: with
    // `amont.testPushedTree` the suite ran on the tip itself — unless the
    // snapshot could not be made, which `stamp_tips` asks about rather than
    // assuming; otherwise it ran on the working tree, which vouches for the
    // tip only when the two are the same content — HEAD, with nothing
    // modified and nothing untracked.
    // The same proxy `attestable` uses, for the same reason: a scoped gate
    // whose files the push never touched returns `Passed` having run
    // nothing, and a stamp for THAT would let a later push that does touch
    // them skip a suite nobody ran.
    if reuse_stamps && !ran_and_passed.is_empty() {
        let changed = crate::pushrefs::changed_files(ctx.push.get());
        let really_ran: Vec<String> = pre_push_checks
            .iter()
            .filter(|c| ran_and_passed.iter().any(|p| p == c.name()))
            .filter(|c| c.scope().touches(&changed))
            .map(|c| c.name().to_string())
            .collect();
        stamp_tips(&tips, &really_ran, &tree_at_start);
    }
    // …and say so to CI, if this repository opted in.
    // Gated behind `enabled()` HERE, not just inside `attest_push`: reading
    // `ctx.push` may consume stdin, and a disabled repo should leave stdin
    // exactly as it found it.
    if !passed.is_empty() && crate::attest::enabled() {
        let remote = ctx
            .args
            .first()
            .map(|a| a.to_string_lossy().into_owned())
            .unwrap_or_default();
        let changed = crate::pushrefs::changed_files(ctx.push.get());
        let vouched = attestable(&pre_push_checks, &passed, &changed);
        crate::attest::attest_push(&remote, ctx.push.get(), &vouched);
    }
    crate::downgrade::note(&downgraded);
    Verdict::Proceed
}

/// The scoped pre-push gates — test suites — that have work to do for a
/// push that changed `changed`: what a rehearsal would run, and therefore
/// what its stamp would have to name before a push may skip anything.
///
/// The same two filters `pre_push` applies before stamping (selected here,
/// and `scope().touches` the change), so the rehearsal's idea of "nothing to
/// do" is the push's idea of "nothing to stamp".
pub fn scoped_push_gates(manifest: &crate::manifest::Manifest, changed: &[String]) -> Vec<String> {
    let in_progress = crate::git_states_in_progress();
    selected_during(Stage::PrePush, &in_progress, manifest)
        .into_iter()
        .filter(|c| !c.scope().is_unscoped() && c.scope().touches(changed))
        .map(|c| c.name().to_string())
        .collect()
}

/// Push-stamp every tip whose content is what the gates actually tested.
///
/// See the comment at the call site for the two cases. Silent when nothing
/// qualifies — a dirty working tree is the ordinary state of a machine
/// mid-work, and a note on every push would teach people to ignore it.
fn stamp_tips(tips: &[String], gates: &[String], tree_at_start: &str) {
    let pushed_tree_mode = crate::pushed_tree::enabled();
    let head = crate::git::stdout(&["rev-parse", "HEAD"]);
    // TRACKED modifications only — a KNOWN gap, kept deliberately, and
    // spelled out because the comment that used to sit here argued it
    // backwards ("untracked files are not in any tree the suite could have
    // been asked about"). That is not the reason. The danger is not that the
    // tree lacks them, it is that the RUN had them: a new test file, a
    // fixture, a local `.env`, present while the suite ran and absent from
    // the tree the stamp vouches for.
    //
    // Counting them was tried, and is worse than the gap. Gates leave
    // artefacts — a log, a coverage directory, whatever a declared
    // `amont.conf` command writes — and nothing cleans them up, so a
    // repository using declared gates would stop earning stamps permanently
    // after its first commit. That does not merely lose an optimisation: it
    // puts the suite back INSIDE the push, which is the failure the whole
    // stamping mechanism exists to prevent. amont's own
    // `a_push_stamp_merges_with_a_commit_time_stamp` fixture is exactly that
    // shape, and CI is where it surfaced — a `*.log` line in one developer's
    // global gitignore had hidden it on the machine that wrote the change.
    //
    // `amont.testPushedTree true` closes the gap properly for anyone who
    // wants it closed: it runs the suite in a checkout of the commit, where
    // no untracked file exists to be read.
    //
    // `tree_at_start` — not a fresh `git status`. The gates have run by now
    // and may have written into the tree; what a stamp needs to know is what
    // the suite could READ, which is the state it was handed. See the
    // capture in `pre_push`.
    let worktree_clean = tree_at_start.trim().is_empty();
    let in_snapshot = crate::rehearsal::in_snapshot();
    let mut stamped: Vec<&str> = Vec::new();
    for tip in tips {
        // `pushed_tree_mode` is the CONFIG, not what happened. A snapshot
        // that could not be created falls back to the working tree and says
        // so, and stamping on the strength of the flag vouched for content
        // the suite never saw. `pushed_tree::fell_back` is what actually
        // happened.
        let snapshot_ran = pushed_tree_mode && !crate::pushed_tree::fell_back(tip);
        // Inside a rehearsal the working tree IS a checkout git made of this
        // commit, so it is the tip's content by construction — and whatever
        // `amont.snapshotPrepare` had to add to make it runnable (a
        // `node_modules`, a virtualenv) is not a reason to distrust it. That
        // is the same argument `snapshot_ran` makes for the pushed-tree mode.
        let is_head = head.as_deref() == Some(tip.as_str());
        let vouchable = snapshot_ran || (is_head && (in_snapshot || worktree_clean));
        if !vouchable {
            continue;
        }
        let spec = format!("{tip}^{{tree}}");
        let Some(tree) = crate::git::stdout(&["rev-parse", &spec]) else {
            continue;
        };
        if crate::gate_stamp::stamp_push(tip, &tree, gates) {
            stamped.push(tip);
        }
    }
    if !stamped.is_empty() {
        crate::say!(
            "{} stamped {} for this tree — the next push of it skips them ({})",
            valid_sign(),
            highlight(&gates.join(" ")),
            crate::gate_stamp::NOTES_REF,
        );
    }
}

/// Of the checks that passed, the ones an attestation may actually VOUCH for.
///
/// A language gate whose scope the push never touched returns `Passed` having
/// run nothing — `cargo_test` walks its refs, finds no crate root, and falls
/// out of the loop green. That is right for a push gate (there was nothing to
/// object to) and wrong for an attestation: a JS-only push was minting
/// `gates … pre-push-cargo-test pre-push-go-test pre-push-pytest`, and in a
/// MIXED repository CI would then skip a suite that nobody ran on that tree.
///
/// The declared `scope` is the honest filter, and the same data `amont list`
/// already reports. Unscoped checks (`Scope::ALWAYS` — branch-protect,
/// secrets) match everything and are vouched for, which is accurate: they
/// really did run. An empty `changed` vouches for nothing scoped, which is
/// the safe direction — CI runs the suite.
///
/// [`Scope::touches`], NOT `Scope::matches`. `matches` also asks whether the
/// repository has opted in — whether a `Cargo.toml` exists — and asking that
/// of a push DIFF can only answer no unless the push happened to touch the
/// marker. So `pre-push-cargo-test` was vouched for by a push that edited
/// `Cargo.toml` and never by one that edited only `.rs` files, which is the
/// ordinary case and the one worth skipping CI for. The feature attested
/// almost nothing, silently, and looked like it worked.
///
/// WHAT THIS IS STILL A PROXY FOR, stated plainly because the trust path
/// deserves it: the honest question is "did this gate actually run", and no
/// gate reports that. `rust_tools::test` returns `Passed` whether it ran a
/// suite or found no crate root and fell out of its loop — the very thing
/// the first paragraph describes. Scope is the closest available stand-in.
/// It is now a good one: a gate is vouched for only if the push changed a
/// file its extensions cover.
///
/// The gap that remains is narrow and one-directional: a `.rs` file outside
/// every crate would be covered here while `cargo test` had nothing to say
/// about it. Closing it properly means an outcome that distinguishes "ran
/// and passed" from "found nothing to do", which is a change to every gate
/// and to `Outcome` itself — worth doing, not worth smuggling into this.
fn attestable(checks: &[&dyn Check], passed: &[String], changed: &[String]) -> Vec<String> {
    checks
        .iter()
        .filter(|c| passed.iter().any(|p| p == c.name()))
        .filter(|c| c.scope().touches(changed))
        .map(|c| c.name().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::{Builtin, Scope};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A check whose only job is to carry a name and a severity into `report`.
    /// Its `run` is never called — `report` is fed outcomes directly, which is
    /// what makes `Unavailable` testable at all: the real thing needs a missing
    /// binary, and a test that uninstalls the developer's toolchain is worse
    /// than no test.
    const fn stub(name: &'static str, severity: Severity) -> Builtin {
        Builtin {
            name,
            stage: Stage::PreCommit,
            scope: Scope::ALWAYS,
            severity,
            run: |_| Outcome::Passed,
            fix: crate::check::Fix::None,
            reach: crate::check::Reach::Convention,
        }
    }

    /// A pre-push gate with an OPT-IN file, as the real Rust and Python
    /// gates have: `.rs` files, but only where a `Cargo.toml` exists.
    const fn opt_in(
        name: &'static str,
        exts: &'static [&'static str],
        names: &'static [&'static str],
    ) -> Builtin {
        Builtin {
            name,
            stage: Stage::PrePush,
            scope: Scope::new(exts, names),
            severity: Severity::Block,
            run: |_| Outcome::Passed,
            fix: crate::check::Fix::None,
            reach: crate::check::Reach::Convention,
        }
    }

    /// A pre-push gate scoped to one language's files.
    const fn scoped(name: &'static str, exts: &'static [&'static str]) -> Builtin {
        Builtin {
            name,
            stage: Stage::PrePush,
            scope: Scope::new(exts, &[]),
            severity: Severity::Block,
            run: |_| Outcome::Passed,
            fix: crate::check::Fix::None,
            reach: crate::check::Reach::Convention,
        }
    }

    /// The over-claim this filter exists to stop, caught in the wild: a
    /// JS-only push minted `gates … pre-push-cargo-test pre-push-go-test
    /// pre-push-pytest`, because each of those gates finds nothing of its
    /// language to do and returns `Passed` having run NOTHING. Harmless in a
    /// single-language repo, unsound in a mixed one — CI would skip a suite
    /// nobody ran on that tree.
    #[test]
    fn a_gate_whose_language_the_push_never_touched_is_not_vouched_for() {
        let js = scoped("pre-push-run-tests-js", &[".ts", ".js"]);
        let rust = scoped("pre-push-cargo-test", &[".rs"]);
        let py = scoped("pre-push-pytest", &[".py"]);
        let always = stub("pre-push-secrets", Severity::Block);
        let checks: Vec<&dyn Check> = vec![&js, &rust, &py, &always];
        let passed: Vec<String> = checks.iter().map(|c| c.name().to_string()).collect();

        let changed = vec!["app/routes/home.ts".to_string()];
        let vouched = attestable(&checks, &passed, &changed);
        assert_eq!(
            vouched,
            vec![
                "pre-push-run-tests-js".to_string(),
                "pre-push-secrets".to_string()
            ],
            "only the gate that had work, plus the unscoped one that always runs"
        );

        // Nothing computed about the push vouches for nothing scoped — the
        // safe direction, since CI then runs the suite.
        assert_eq!(
            attestable(&checks, &passed, &[]),
            vec!["pre-push-secrets".to_string()]
        );

        // A check that did NOT pass is never vouched for, whatever its scope.
        let only_rust_passed = vec!["pre-push-cargo-test".to_string()];
        assert!(attestable(&checks, &only_rust_passed, &changed).is_empty());
    }

    /// No overrides configured. `report` takes them as a VALUE now, so its
    /// tests need no repository and no git at all.
    fn none() -> Overrides {
        Overrides::default()
    }

    static BLOCKER: Builtin = stub("stub-blocker", Severity::Block);
    static WARNER: Builtin = stub("stub-warner", Severity::Warn);

    /// The unit tests hold `&dyn Check` for the same reason the dispatcher
    /// does: `report` must not be able to tell a built-in from an external.
    const fn as_checks(cs: [&'static Builtin; 3]) -> [&'static dyn Check; 3] {
        [cs[0], cs[1], cs[2]]
    }

    /// The classification itself, which used to be unreachable: while one
    /// function classified AND printed AND returned a code, a test could assert
    /// the code and nothing else.
    #[test]
    fn every_outcome_lands_in_the_right_bucket() {
        let checks: [&dyn Check; 4] = [&BLOCKER, &BLOCKER, &WARNER, &BLOCKER];
        let got = classify(
            &checks,
            &[
                Outcome::Passed,
                Outcome::Unavailable,
                Outcome::Failed,
                Outcome::Failed,
            ],
            &none(),
        );
        assert_eq!(got.blocked, ["stub-blocker"], "{got:?}");
        assert_eq!(got.downgraded, ["stub-warner"], "{got:?}");
        assert_eq!(got.unavailable, ["stub-blocker"], "{got:?}");
    }

    /// A clean stage concludes nothing at all — not an empty message, no
    /// message. Twenty checks that passed should print no roll-ups.
    #[test]
    fn a_clean_stage_has_nothing_to_report() {
        let checks: [&dyn Check; 2] = [&BLOCKER, &WARNER];
        let got = classify(&checks, &[Outcome::Passed, Outcome::Warned], &none());
        assert_eq!(got, Report::default());
        assert_eq!(got.verdict(), Verdict::Proceed);
    }

    #[test]
    fn a_blocking_failure_is_the_only_thing_that_fails_the_commit() {
        let b: &dyn Check = &BLOCKER;
        let w: &dyn Check = &WARNER;
        assert_eq!(
            classify(&[b], &[Outcome::Failed], &none()).verdict(),
            Verdict::Block
        );
        assert_eq!(
            classify(&[b], &[Outcome::Passed], &none()).verdict(),
            Verdict::Proceed
        );
        // Every non-blocking shape, one at a time, so a regression cannot hide
        // behind a passing sibling.
        assert_eq!(
            classify(&[b], &[Outcome::Warned], &none()).verdict(),
            Verdict::Proceed
        );
        assert_eq!(
            classify(&[b], &[Outcome::Unavailable], &none()).verdict(),
            Verdict::Proceed
        );
        assert_eq!(
            classify(&[w], &[Outcome::Failed], &none()).verdict(),
            Verdict::Proceed
        );
    }

    #[test]
    fn one_blocking_failure_among_many_still_fails() {
        let checks = as_checks([&BLOCKER, &WARNER, &BLOCKER]);
        assert_eq!(
            classify(
                &checks,
                &[Outcome::Unavailable, Outcome::Failed, Outcome::Failed],
                &none()
            )
            .verdict(),
            Verdict::Block
        );
        // Same shape, with the only *blocking* failure removed.
        assert_eq!(
            classify(
                &checks,
                &[Outcome::Unavailable, Outcome::Failed, Outcome::Passed],
                &none()
            )
            .verdict(),
            Verdict::Proceed
        );
    }

    /// The slot of a check whose thread died. Reading that as a pass is how a
    /// crash becomes a green commit.
    ///
    /// Asserted through the RUNNER, not through a `Default` impl: the rule
    /// belongs to this call site, and a test on `Outcome::default()` proved
    /// only that a trait impl existed, not that the runner used it.
    /// A check that PANICS must fail the commit, not pass it — and must not
    /// take the other checks down with it.
    ///
    /// Driven through the stage body rather than the runner, because the value
    /// that stands in for a dead check is chosen at the call site and the
    /// runner's own test cannot see that choice.
    #[test]
    fn a_panicking_check_blocks_the_commit() {
        static DIES: Builtin = Builtin {
            name: "stub-dies",
            stage: Stage::PreCommit,
            scope: Scope::ALWAYS,
            severity: Severity::Block,
            run: |_| panic!("this check died"),
            fix: crate::check::Fix::None,
            reach: crate::check::Reach::Convention,
        };
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let push = crate::pushrefs::PushRefs::default();
        let manifest = crate::manifest::Manifest::default();
        let ctx = Ctx {
            name: "pre-commit",
            args: &[],
            hooks_dir: std::path::Path::new("."),
            push: &push,
            manifest: &manifest,
        };
        let verdict = run_stage(&[&DIES], &ctx, &none());
        std::panic::set_hook(hook);
        assert_eq!(
            verdict,
            Verdict::Block,
            "a check that died must not let the commit through"
        );
    }

    #[test]
    fn a_thread_that_dies_leaves_a_failure_behind() {
        // The default hook would print a backtrace for the deliberate panic and
        // make a passing run look broken.
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let items = ["a", "b", "c"];
        let out = run_concurrently(
            &items,
            |n: &&str| {
                if *n == "b" {
                    panic!("this check died");
                }
                Outcome::Passed
            },
            Outcome::Failed,
        );
        std::panic::set_hook(hook);
        assert_eq!(
            out,
            vec![Outcome::Passed, Outcome::Failed, Outcome::Passed],
            "a dead check must not read as one that passed, \
             and must not take the other checks down with it"
        );
    }
    use std::time::{Duration, Instant};

    /// Concurrency proved by RENDEZVOUS, not by a stopwatch: every task must
    /// observe all the others arrive. Were the runner serial, the first task
    /// would wait alone, time out, and return non-zero — a failure, not a hang.
    #[test]
    fn run_concurrently_actually_overlaps() {
        static ARRIVED: AtomicUsize = AtomicUsize::new(0);
        ARRIVED.store(0, Ordering::SeqCst);
        let names: Vec<&'static str> = vec!["a", "b", "c", "d"];
        let n = names.len();

        let out = run_concurrently(
            &names,
            move |_: &&str| {
                ARRIVED.fetch_add(1, Ordering::SeqCst);
                let deadline = Instant::now() + Duration::from_secs(10);
                while ARRIVED.load(Ordering::SeqCst) < n {
                    if Instant::now() > deadline {
                        return 1; // never met the others — execution was serial
                    }
                    std::thread::yield_now();
                }
                0
            },
            1,
        );
        assert!(
            out.iter().all(|c| *c == 0),
            "tasks did not overlap: {out:?}"
        );
    }

    #[test]
    fn results_come_back_in_input_order() {
        let names: Vec<&'static str> = vec!["first", "second", "third"];
        let out = run_concurrently(&names, |n| if *n == "second" { 7 } else { 0 }, -1);
        assert_eq!(out, vec![0, 7, 0], "results keep the input order");
    }

    /// The filter calls the shared resolver rather than restating it. This test
    /// used to inline `n.contains(s)` — its own copy of the rule — and so went
    /// on passing after the rule changed underneath it.
    #[test]
    fn skips_are_filtered_by_the_shared_resolver() {
        let all = ["pre-commit-ruff", "pre-commit-prettier"];
        let skips = ["ruff".to_string()];
        let kept: Vec<_> = all
            .iter()
            .copied()
            .filter(|n| !skips.iter().any(|s| crate::skip_suppresses(n, s)))
            .collect();
        assert_eq!(kept, vec!["pre-commit-prettier"]);
    }

    /// The ordinary Rust push: source changed, `Cargo.toml` untouched.
    ///
    /// This test used to assert the OPPOSITE, and said so — it pinned
    /// `attestable` filtering on `Scope::matches`, which also asks whether
    /// the repository has opted in. Asking that of a push diff meant a
    /// `.rs`-only push, in a repository with a `Cargo.toml` sitting right
    /// there, attested nothing. Since that is what nearly every Rust push
    /// looks like, the feature was attesting almost nothing at all — quietly,
    /// while appearing to work.
    #[test]
    fn an_ordinary_source_push_is_vouched_for() {
        let rust = opt_in("pre-push-cargo-test", &[".rs"], &["Cargo.toml"]);
        let checks: Vec<&dyn Check> = vec![&rust];
        let passed = vec!["pre-push-cargo-test".to_string()];

        let changed = vec!["crates/amont/src/main.rs".to_string()];
        assert_eq!(
            attestable(&checks, &passed, &changed),
            vec!["pre-push-cargo-test".to_string()],
            "a push that changed Rust must vouch for the Rust gate"
        );

        // And still when the marker IS in the diff — the opt-in file is not
        // required, but it is not disqualifying either.
        let changed = vec![
            "crates/amont/src/main.rs".to_string(),
            "Cargo.toml".to_string(),
        ];
        assert_eq!(
            attestable(&checks, &passed, &changed),
            vec!["pre-push-cargo-test".to_string()]
        );
    }

    /// The over-claim the filter exists to stop, with an opt-in gate: a
    /// JS-only push must not vouch for the Rust suite, whatever files the
    /// repository contains.
    ///
    /// This is the half of the contract the fix above must not have broken,
    /// and it is the reason `touches` asks about EXTENSIONS rather than
    /// dropping the scope test altogether.
    #[test]
    fn an_opt_in_gate_is_not_vouched_for_by_a_push_in_another_language() {
        let rust = opt_in("pre-push-cargo-test", &[".rs"], &["Cargo.toml"]);
        let checks: Vec<&dyn Check> = vec![&rust];
        let passed = vec!["pre-push-cargo-test".to_string()];

        for changed in [
            vec!["app/routes/home.ts".to_string()],
            // Even the marker alone: editing Cargo.toml changes no Rust
            // source, so the suite has nothing new to have verified.
            vec!["Cargo.toml".to_string()],
            vec![],
        ] {
            assert!(
                attestable(&checks, &passed, &changed).is_empty(),
                "must not vouch for the Rust gate on {changed:?}"
            );
        }
    }
}
