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

impl Context<'_> {
    /// The directory the clause at byte offset `at` actually runs in.
    ///
    /// The payload's `cwd` is the SESSION's directory. A third of real
    /// commands begin `cd /somewhere && …`, and every git question a
    /// `confirm` asks — is this checkout behind, is `refs/stash` shared — is
    /// about the directory the git command runs in, not the one the shell
    /// started in. Asking the wrong repository produced a confident wrong
    /// answer: a branch created in an up-to-date clone was advised as stale
    /// because the session sat in a checkout that was.
    ///
    /// The last `cd` clause before `at` wins, resolved against the session
    /// cwd (`~` against `$HOME`). A `cd` whose target came from a
    /// substitution is unknowable and ends the search: better the session
    /// cwd than a guess. A bare `cd` is `$HOME`; `cd -` is unknowable.
    pub fn cwd_at(&self, at: usize) -> std::path::PathBuf {
        let mut dir = self.cwd.to_path_buf();
        for cmd in self.parsed.clauses() {
            if cmd.at >= at {
                break;
            }
            if cmd.program() != Some("cd") {
                continue;
            }
            // The raw word after `cd`, not `operands()`: that helper drops a
            // leading `-` as a flag and a blanked substitution as nothing,
            // and both are exactly the cases that must read as unknowable.
            let target = cmd.words.iter().skip_while(|w| w.text != "cd").nth(1);
            let Some(target) = target else {
                if let Some(home) = std::env::var_os("HOME") {
                    dir = std::path::PathBuf::from(home);
                }
                continue;
            };
            if target.expanded || target.text.trim().is_empty() || target.text == "-" {
                return self.cwd.to_path_buf();
            }
            let t = target.text.as_str();
            dir = if let Some(rest) = t.strip_prefix("~/") {
                match std::env::var_os("HOME") {
                    Some(home) => std::path::PathBuf::from(home).join(rest),
                    None => return self.cwd.to_path_buf(),
                }
            } else if t == "~" {
                match std::env::var_os("HOME") {
                    Some(home) => std::path::PathBuf::from(home),
                    None => return self.cwd.to_path_buf(),
                }
            } else {
                dir.join(t)
            };
        }
        dir
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::lex;

    fn at_of(parsed: &Parsed, needle: &str) -> usize {
        parsed
            .clauses()
            .iter()
            .find(|c| c.words.iter().any(|w| w.text == needle))
            .map(|c| c.at)
            .expect("clause")
    }

    #[test]
    fn a_leading_cd_moves_the_question() {
        let parsed = lex("cd /tmp/elsewhere && git worktree add ../x -b feat/y");
        let ctx = Context {
            cwd: std::path::Path::new("/session"),
            parsed: &parsed,
        };
        assert_eq!(
            ctx.cwd_at(at_of(&parsed, "worktree")),
            std::path::PathBuf::from("/tmp/elsewhere")
        );
    }

    #[test]
    fn a_relative_cd_resolves_against_the_session_and_the_last_wins() {
        let parsed = lex("cd sub; cd deeper && git stash pop");
        let ctx = Context {
            cwd: std::path::Path::new("/session"),
            parsed: &parsed,
        };
        assert_eq!(
            ctx.cwd_at(at_of(&parsed, "stash")),
            std::path::PathBuf::from("/session/sub/deeper")
        );
    }

    #[test]
    fn a_cd_after_the_clause_does_not_count() {
        let parsed = lex("git stash pop && cd /tmp/after");
        let ctx = Context {
            cwd: std::path::Path::new("/session"),
            parsed: &parsed,
        };
        assert_eq!(
            ctx.cwd_at(at_of(&parsed, "stash")),
            std::path::PathBuf::from("/session")
        );
    }

    /// A target nobody can know without running the shell is not guessed.
    #[test]
    fn an_unknowable_cd_falls_back_to_the_session() {
        for command in ["cd $(mktemp -d) && git stash pop", "cd - && git stash pop"] {
            let parsed = lex(command);
            let ctx = Context {
                cwd: std::path::Path::new("/session"),
                parsed: &parsed,
            };
            assert_eq!(
                ctx.cwd_at(at_of(&parsed, "stash")),
                std::path::PathBuf::from("/session"),
                "{command}"
            );
        }
    }

    #[test]
    fn tilde_is_home() {
        let parsed = lex("cd ~/work/repo && git stash pop");
        let ctx = Context {
            cwd: std::path::Path::new("/session"),
            parsed: &parsed,
        };
        let home = std::path::PathBuf::from(std::env::var_os("HOME").expect("HOME"));
        assert_eq!(ctx.cwd_at(at_of(&parsed, "stash")), home.join("work/repo"));
    }
}
