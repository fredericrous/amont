//! `gh-pr-merge-auto` — `gh pr merge --auto` where nothing enforces checks.
//!
//! `--auto` means "merge when requirements are met". On a repository with
//! required status checks that is a queue. On one without branch protection
//! there are no requirements to meet, so it merges immediately — before CI has
//! started, let alone passed. The flag reads like a safety feature and behaves
//! like its opposite.
//!
//! ## No `confirm`, on purpose
//!
//! The question this rule would like to ask — does this repository have
//! required checks? — is a network round-trip to the forge. This code runs
//! before every shell command the model issues, and a hook that can hang is
//! worse than a hook that is occasionally imprecise. So the rule states the
//! condition in its reason and lets the reader settle it.

use crate::rules::{Evidence, Finding, Rule, Stance, Trend};
use crate::shell::Parsed;

pub const RULE: Rule = Rule {
    id: "gh-pr-merge-auto",
    default_stance: Stance::Observe,
    evidence: Evidence {
        per_1000: 0.02,
        measured: "2026-08-20",
        trend: Trend::Rare,
    },
    examine,
    confirm: None,
};

fn examine(parsed: &Parsed) -> Option<Finding> {
    for cmd in parsed.clauses() {
        if cmd.program() != Some("gh") || cmd.subcommand() != Some("pr") {
            continue;
        }
        // operands()[0] is `pr`; the action follows.
        if cmd.operands().get(1).map(|w| w.text.as_str()) != Some("merge") {
            continue;
        }
        // `has_flag` skips quoted words, so `gh pr create --body "use --auto"`
        // is not a match. That exemption is why this is an argv test and not a
        // substring one.
        if !cmd.has_flag("--auto") {
            continue;
        }
        return Some(Finding {
            reason: "`--auto` waits only where the repository has required status \
                     checks. Without branch protection there is nothing to wait for, \
                     so the pull request merges immediately — before CI has started."
                .to_string(),
            remedy: "Confirm required checks are configured, or drop `--auto` and \
                     poll until the run concludes successfully before merging."
                .to_string(),
            span: cmd.at..cmd.end,
        });
    }
    None
}
