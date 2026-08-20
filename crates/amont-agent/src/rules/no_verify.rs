//! `no-verify` — turning the commit-time gate off wholesale.
//!
//! `--no-verify` does not skip one slow check; it skips `pre-commit` and
//! `commit-msg` entirely. When a gate is slow the pressure to reach for it is
//! real, which is exactly why amont records a ledger of dodged gates rather
//! than discarding the signal.
//!
//! Ships observing and probably stays there: the measured rate fell roughly
//! five-fold over the weeks amont was adopted, from 25.9 to 5.4 per thousand.
//! A habit already correcting itself does not need a guard pointed at it — and
//! the crate's own build order says so.
//!
//! ## The short form is per-subcommand
//!
//! `-n` means `--no-verify` on `git commit`, and `--dry-run` on `git push`.
//! Matching `-n` against a flat flag list would report a dry-run push as a gate
//! bypass, which is the opposite of what it is.

use crate::rules::{Evidence, Finding, Rule, Stance, Trend};
use crate::shell::Parsed;

pub const RULE: Rule = Rule {
    id: "no-verify",
    default_stance: Stance::Observe,
    evidence: Evidence {
        per_1000: 5.4,
        measured: "2026-08-20",
        trend: Trend::Improving,
    },
    examine,
    confirm: None,
};

fn examine(parsed: &Parsed) -> Option<Finding> {
    for cmd in parsed.clauses() {
        // git only. `--no-verify` belongs to other programs too and means
        // other things; this rule is about the commit gate.
        if cmd.program() != Some("git") {
            continue;
        }
        let sub = cmd.subcommand();
        let long = cmd.has_flag("--no-verify");
        let short = sub == Some("commit") && cmd.has_short('n');
        if !long && !short {
            continue;
        }
        return Some(Finding {
            reason: "`--no-verify` does not skip one check — it skips the whole \
                     pre-commit and commit-msg gate, including the ones that were \
                     not in the way."
                .to_string(),
            remedy: "Downgrade the specific check instead: \
                     `git config amont.severity.<check-id> warn`. \
                     `amont list --json` names the check that is blocking."
                .to_string(),
            span: cmd.at..cmd.end,
        });
    }
    None
}
