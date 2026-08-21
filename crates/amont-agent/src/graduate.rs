//! Moving a rule up the ladder, and back down it.
//!
//! Promotion is gated on evidence and recorded. Demotion is one word, no
//! questions, no gate — and that asymmetry is deliberate. A guard that is hard
//! to back out of is a guard people uninstall instead of demoting, and
//! uninstalling switches off every rule at once. Retreat has to be cheaper than
//! removal or the whole ladder is theatre.
//!
//! ## The gate is the corpus, not the rate
//!
//! The obvious gate would be a fire-rate ceiling: refuse `deny` above N per
//! thousand. It is the wrong instrument. `pipe-to-tail` fires on roughly one
//! Bash call in twenty-two and is still the rule with the strongest case for
//! blocking, because every one of those firings is correct. A rate is a measure
//! of COST; it says nothing about whether the rule is right.
//!
//! What says whether a rule is right is a person having looked at its matches.
//! So promotion asks for reviewed judgements — including expected-negatives,
//! since a corpus of forty positives and no negatives has an unmeasured
//! precision rather than a perfect one — and it asks that the rule still agrees
//! with every one of them today.
//!
//! The rate is still reported, because a correct rule can be too loud to live
//! with, and that is a decision for a person rather than a threshold.

use crate::corpus;
use crate::rules::{Rule, Stance};

/// Enough labelled cases that agreement means something. Not a large number,
/// because the point is that somebody looked — a corpus of twelve real
/// commands a human read beats a thousand nobody did.
const MIN_REVIEWED: usize = 12;
/// Of those, this many must be commands the rule must NOT fire on.
const MIN_NEGATIVES: usize = 4;

pub struct Verdict {
    pub allowed: bool,
    pub lines: Vec<(bool, String)>,
}

pub fn assess(rule: &Rule, to: Stance) -> Verdict {
    assess_score(rule, to, &corpus::score(rule))
}

/// The judgement itself, separated from where the cases came from so the tests
/// exercise this exact code rather than a second copy of it that can drift.
pub fn assess_score(rule: &Rule, to: Stance, score: &corpus::Score) -> Verdict {
    let mut lines = Vec::new();
    let mut allowed = true;

    // Demoting toward Observe is always allowed; only promotion is gated.
    if to <= rule.default_stance && to == Stance::Observe {
        return Verdict {
            allowed: true,
            lines,
        };
    }

    let enough = score.reviewed >= MIN_REVIEWED;
    lines.push((
        enough,
        format!("{} reviewed cases (need {MIN_REVIEWED})", score.reviewed),
    ));
    allowed &= enough;

    let negatives = score.negatives >= MIN_NEGATIVES;
    lines.push((
        negatives,
        format!(
            "{} of them are expected-negatives (need {MIN_NEGATIVES})",
            score.negatives
        ),
    ));
    allowed &= negatives;

    let agrees = score.agrees();
    lines.push((
        agrees,
        if agrees {
            "the rule agrees with every judgement".to_string()
        } else {
            format!(
                "{} judgement(s) the rule now disagrees with — run `amont-agent corpus check`",
                score.disagreements.len()
            )
        },
    ));
    allowed &= agrees;

    // Reported, never gated. See the module note.
    lines.push((
        true,
        match score.precision() {
            Some(p) => format!(
                "precision {:.0}% over the reviewed set; measured {:.1} firings per 1000 calls",
                p * 100.0,
                rule.evidence.per_1000
            ),
            None => format!(
                "precision unmeasured; measured {:.1} firings per 1000 calls",
                rule.evidence.per_1000
            ),
        },
    ));

    Verdict { allowed, lines }
}

/// Record the new stance. Global, because the guard is a property of this
/// machine's agent rather than of whichever repository happens to be open.
pub fn set(rule: &Rule, to: Stance) -> Result<(), String> {
    let key = crate::stance::key_for(rule);
    let out = std::process::Command::new("git")
        .args(["config", "--global", &key, to.as_str()])
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::Verdict as CaseVerdict;

    fn score_of(rule: &Rule, text: &str) -> corpus::Score {
        corpus::score_cases(rule, &corpus::parse(text))
    }

    fn cases(matches: usize, negatives: usize) -> String {
        let mut s = String::new();
        for _ in 0..matches {
            s.push_str(&corpus::line_for(
                CaseVerdict::Match,
                "git push origin main | tail -1",
            ));
        }
        for _ in 0..negatives {
            s.push_str(&corpus::line_for(
                CaseVerdict::NoMatch,
                "git status --short",
            ));
        }
        s
    }

    /// The point of the gate. A rule nobody has reviewed cannot be promoted,
    /// however good its numbers look.
    #[test]
    fn an_unreviewed_rule_cannot_be_promoted() {
        let rule = crate::rules::by_id("bare-stash-pop").unwrap();
        let score = corpus::score_cases(rule, &corpus::parse(""));
        assert_eq!(score.reviewed, 0);
        let v = assess_score(rule, Stance::Deny, &score);
        assert!(!v.allowed);
    }

    /// Positives alone are not evidence: they measure recall, not precision.
    #[test]
    fn positives_without_negatives_are_not_enough() {
        let rule = crate::rules::by_id("pipe-to-tail").unwrap();
        let v = assess_score(rule, Stance::Deny, &score_of(rule, &cases(20, 0)));
        assert!(!v.allowed, "twenty positives and no negatives passed");
        assert!(v.lines.iter().any(|(ok, t)| !ok && t.contains("negatives")));
    }

    #[test]
    fn a_reviewed_rule_that_still_agrees_may_be_promoted() {
        let rule = crate::rules::by_id("pipe-to-tail").unwrap();
        let v = assess_score(rule, Stance::Deny, &score_of(rule, &cases(12, 4)));
        assert!(
            v.allowed,
            "{:?}",
            v.lines.iter().map(|l| &l.1).collect::<Vec<_>>()
        );
    }

    /// A judgement the rule no longer honours blocks promotion even when the
    /// counts are satisfied — that is the regression the corpus exists to stop.
    #[test]
    fn a_disagreement_blocks_promotion() {
        let rule = crate::rules::by_id("pipe-to-tail").unwrap();
        let mut text = cases(12, 4);
        text.push_str(&corpus::line_for(
            CaseVerdict::NoMatch,
            "git push origin main 2>&1 | tail -5",
        ));
        let v = assess_score(rule, Stance::Deny, &score_of(rule, &text));
        assert!(!v.allowed);
        assert!(v.lines.iter().any(|(ok, t)| !ok && t.contains("disagrees")));
    }

    /// Demotion is never gated. See the module note.
    #[test]
    fn demotion_needs_no_evidence() {
        let rule = crate::rules::by_id("pipe-to-tail").unwrap();
        let v = assess_score(rule, Stance::Observe, &score_of(rule, ""));
        assert!(v.allowed);
    }
}
