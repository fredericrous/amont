//! The rules, and the contract every rule obeys.
//!
//! A rule is one module plus one line in [`RULES`] — the same shape
//! `amont_runtime::registry::CHECKS` uses, and for the same reason: a rule's
//! name, its default stance and its function are declared together, so adding
//! one cannot half-happen.
//!
//! Deliberately NOT registered in `registry::CHECKS`. That table is the set of
//! things that gate a commit, `crates/amont/tests/docs_counts.rs` asserts on
//! its length against counts spelled out in three prose files, and these rules
//! gate nothing in git.
//!
//! ## Two devices carry the precision
//!
//! **[`Rule::examine`] is pure.** No processes, no filesystem, no network. It
//! runs on every Bash call the model makes, so anything it touches is paid for
//! thousands of times a week. Purity is also what makes the backtester honest:
//! a rule that consulted the world during `examine` could not be replayed
//! against a transcript, because the world has moved.
//!
//! **[`Rule::confirm`] runs only after `examine` has already fired.** It is the
//! one place a rule may look at the world, and it exists to turn a heuristic
//! into a fact — "is this repository actually shared across worktrees?", "does
//! this glob actually match nothing?". Fires are rare, so the cost is rare.

use std::ops::Range;

use crate::shell::Parsed;

pub mod bare_stash_pop;
pub mod gh_pr_merge_auto;
pub mod git_add_broad;
pub mod no_verify;
pub mod pipe_to_tail;
pub mod stale_base;

// A `fish-glob` rule was written and removed before the first commit. It caught
// an unquoted glob inside a flag value (`--include=*.py`), which under fish is
// a hard error rather than the literal passthrough bash gives you.
//
// It failed this crate's own admission test. A rule earns a guard when the
// failure it prevents is SILENT — `pipe-to-tail` qualifies because the pipeline
// reports success whatever the mutating command did, so no correcting loop can
// form. A zero-match glob under fish aborts the command loudly and names the
// glob, which is the best feedback a person or a model can get; the measured
// rate was falling on its own accordingly (12.9 per thousand in early July,
// 3.4 by mid-August).
//
// It was also the only rule that needed to know which shell was running, which
// meant either reading the environment inside a pure `examine` or coupling a
// tool published to crates.io, npm and Homebrew to one shell's semantics.
// Neither is worth a rule that should almost never fire.

/// What a rule is allowed to DO when it fires.
///
/// Three states, not two, and the middle one is the point. `Observe` and
/// `Advise` are not interchangeable ways of "not blocking yet": `Advise` puts
/// text into the model's context and therefore changes its behaviour, which
/// contaminates the very rate the observation exists to measure. A rule that
/// talks is intervening.
///
/// So: `Observe` is where every rule ships and where the baseline is measured.
/// `Advise` answers "does it correct itself when told?" — and if the answer is
/// yes, `Deny` is never needed.
// `Advise` and `Deny` are declared here and constructed by nothing yet: the
// build order deliberately ships the backtester and the rules BEFORE the hook
// that can act on them, so that no rule can block until its rate has been
// looked at. The ladder is the design; the rungs above `Observe` come with the
// hook path.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stance {
    Observe,
    Advise,
    Deny,
}

impl Stance {
    pub fn as_str(self) -> &'static str {
        match self {
            Stance::Observe => "observe",
            Stance::Advise => "advise",
            Stance::Deny => "deny",
        }
    }
    /// The inverse of [`Stance::as_str`], for reading a stance back out of
    /// `git config amont.agent.<rule>.stance`. Paired with `as_str` from the
    /// start so the two spellings cannot drift apart later.
    #[allow(dead_code)]
    pub fn parse(s: &str) -> Option<Stance> {
        match s {
            "observe" => Some(Stance::Observe),
            "advise" => Some(Stance::Advise),
            "deny" => Some(Stance::Deny),
            _ => None,
        }
    }
}

/// Why a rule fired, and what to do about it.
#[derive(Debug, Clone)]
pub struct Finding {
    /// One sentence naming the MECHANISM, not the sin. "A pipeline's exit
    /// status is the last command's" tells the reader something they can use;
    /// "this is dangerous" does not.
    pub reason: String,
    /// What to do instead. A rule that says what is wrong without saying what
    /// to do cannot be obeyed, and an unobeyable rule is noise.
    pub remedy: String,
    /// Byte range of what actually matched, within the original command.
    ///
    /// Not cosmetic. 35% of real commands are multi-clause scripts, and a
    /// sample printed from the head of a 2 KB script shows text that has
    /// nothing to do with the match — which makes human review review the
    /// wrong thing. Every excerpt is centred on this.
    pub span: Range<usize>,
}

/// The outcome of the one world-touching step a rule is allowed.
///
/// Read by the hook path, which is the only caller that has a working
/// directory to confirm against; the backtester deliberately never runs
/// `confirm`, because the world has moved since those commands ran.
#[allow(dead_code)]
pub enum Confirmed {
    Yes,
    /// Not confirmed, with the reason. Failing to confirm is always silence.
    No(&'static str),
}

/// How the default stance was chosen, so a graduation shows its evidence.
/// Nothing reads this at run time.
#[derive(Debug, Clone, Copy)]
pub struct Evidence {
    pub per_1000: f32,
    pub measured: &'static str,
    pub trend: Trend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trend {
    /// Not improving on its own over the stated number of weeks.
    Flat(u8),
    /// Already falling without a guard; leave it alone.
    Improving,
    /// Too rare to trend, kept for cost-of-a-miss rather than frequency.
    Rare,
}

pub struct Rule {
    pub id: &'static str,
    pub default_stance: Stance,
    pub evidence: Evidence,
    /// PURE. See the module note.
    pub examine: fn(&Parsed) -> Option<Finding>,
    /// Consumed by the hook path; see [`Confirmed`].
    #[allow(dead_code)]
    pub confirm: Option<fn(&crate::rules::Context, &Finding) -> Confirmed>,
}

/// What a `confirm` is allowed to know.
#[allow(dead_code)]
pub struct Context<'a> {
    pub cwd: &'a std::path::Path,
    pub parsed: &'a Parsed,
}

pub const RULES: &[Rule] = &[
    pipe_to_tail::RULE,
    bare_stash_pop::RULE,
    gh_pr_merge_auto::RULE,
    no_verify::RULE,
    git_add_broad::RULE,
    stale_base::RULE,
];

pub fn by_id(id: &str) -> Option<&'static Rule> {
    RULES.iter().find(|r| r.id == id)
}

/// Run every rule's `examine` over one parsed command.
///
/// A panicking rule is dropped and the others still report, mirroring
/// `dispatch::run_concurrently`. That isolation only exists because the
/// workspace release profile refuses `panic = "abort"` — see the root
/// `Cargo.toml`.
pub fn examine_all(parsed: &Parsed) -> Vec<(&'static Rule, Finding)> {
    let mut out = Vec::new();
    for rule in RULES {
        let found =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (rule.examine)(parsed)));
        match found {
            Ok(Some(f)) => out.push((rule, f)),
            Ok(None) => {}
            Err(_) => eprintln!("amont-agent: rule `{}` panicked; ignoring it", rule.id),
        }
    }
    out
}
