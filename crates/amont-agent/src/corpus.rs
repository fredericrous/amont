//! Reviewed judgements, kept as a test.
//!
//! A rule's fire rate says how *often* it speaks. It says nothing about whether
//! it is right, and being right is the whole product: recall is recoverable —
//! a rule that misses something can be widened next week and the backtester
//! will price it — while one false positive refuses work the author knew was
//! correct, and the response to that is to delete the hook from
//! `settings.json`, which switches off every rule at once.
//!
//! So precision needs evidence, and evidence needs a human. The loop is:
//!
//! ```text
//! amont-agent explain <rule> --format cases >> tests/corpus/<rule>.cases
//! $EDITOR tests/corpus/<rule>.cases      # turn each `?` into match / nomatch
//! amont-agent corpus check               # and it is now a test
//! ```
//!
//! **The review output IS the corpus format.** There is no separate "mark as
//! reviewed" tool, because a review workflow with two file formats is one
//! nobody completes.
//!
//! ## Why a file and not a metric
//!
//! Tracking precision over time would chart the regression. A checked-in file
//! of labelled judgements *prevents* it: `corpus check` runs in the test suite,
//! so widening a rule in a way that breaks a judgement somebody already made is
//! a red build, not a number that drifts while nobody is looking.
//!
//! ## One line per case
//!
//! 35% of real commands span several lines, so newlines and tabs are escaped on
//! the way in and restored on the way out. The escaping is deliberately the
//! smallest thing that round-trips, because a corpus nobody can read by eye is
//! a corpus nobody will label.

use std::path::{Path, PathBuf};

pub const HEADER: &str = "# amont-agent-cases-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The rule should fire here.
    Match,
    /// The rule must stay silent here. These are the ones that matter — a
    /// corpus of forty positives and no negatives has an unmeasured precision,
    /// not a perfect one.
    NoMatch,
    /// Emitted by `explain`, meaning nobody has looked yet. Never counts as
    /// evidence.
    Unreviewed,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Match => "match",
            Verdict::NoMatch => "nomatch",
            Verdict::Unreviewed => "?",
        }
    }
    fn parse(s: &str) -> Option<Verdict> {
        match s {
            "match" => Some(Verdict::Match),
            "nomatch" => Some(Verdict::NoMatch),
            "?" => Some(Verdict::Unreviewed),
            _ => None,
        }
    }
}

pub struct Case {
    pub verdict: Verdict,
    pub command: String,
    pub line: usize,
}

/// Where a rule's reviewed cases live.
pub fn path_for(rule: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("corpus")
        .join(format!("{rule}.cases"))
}

pub fn escape(command: &str) -> String {
    let mut out = String::with_capacity(command.len());
    for c in command.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

pub fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            // An escape we do not know is kept verbatim rather than eaten, so
            // a hand-edited file cannot silently lose a character.
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

pub fn line_for(verdict: Verdict, command: &str) -> String {
    format!("{}\t{}\n", verdict.as_str(), escape(command))
}

pub fn parse(text: &str) -> Vec<Case> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((verdict, command)) = line.split_once('\t') else {
            continue;
        };
        let Some(verdict) = Verdict::parse(verdict.trim()) else {
            continue;
        };
        out.push(Case {
            verdict,
            command: unescape(command),
            line: i + 1,
        });
    }
    out
}

pub fn read(rule: &str) -> Vec<Case> {
    read_at(&path_for(rule))
}

pub fn read_at(path: &Path) -> Vec<Case> {
    std::fs::read_to_string(path)
        .map(|t| parse(&t))
        .unwrap_or_default()
}

/// One rule's agreement with the judgements already made about it.
pub struct Score {
    pub reviewed: usize,
    pub negatives: usize,
    pub unreviewed: usize,
    /// Cases where the engine disagrees with a human. Each is either a rule
    /// that regressed or a judgement that needs revisiting; both need a person.
    pub disagreements: Vec<Disagreement>,
}

pub struct Disagreement {
    pub line: usize,
    pub expected: Verdict,
    pub command: String,
}

impl Score {
    pub fn agrees(&self) -> bool {
        self.disagreements.is_empty()
    }
    /// True positives over everything the rule claimed. `None` when nothing
    /// was claimed — an unmeasured precision, which is not the same as 1.0.
    pub fn precision(&self) -> Option<f64> {
        let claimed = self
            .disagreements
            .iter()
            .filter(|d| d.expected == Verdict::NoMatch)
            .count();
        let matched = self.reviewed - self.negatives;
        let total = matched + claimed;
        if total == 0 {
            None
        } else {
            Some(matched as f64 / total as f64)
        }
    }
}

/// Run one rule's cases through the engine as it stands today.
pub fn score(rule: &crate::rules::Rule) -> Score {
    score_cases(rule, &read(rule.id))
}

pub fn score_cases(rule: &crate::rules::Rule, cases: &[Case]) -> Score {
    let mut score = Score {
        reviewed: 0,
        negatives: 0,
        unreviewed: 0,
        disagreements: Vec::new(),
    };
    for case in cases {
        if case.verdict == Verdict::Unreviewed {
            score.unreviewed += 1;
            continue;
        }
        score.reviewed += 1;
        if case.verdict == Verdict::NoMatch {
            score.negatives += 1;
        }
        let parsed = crate::shell::lex(&case.command);
        let fired = (rule.examine)(&parsed).is_some();
        let expected = case.verdict == Verdict::Match;
        if fired != expected {
            score.disagreements.push(Disagreement {
                line: case.line,
                expected: case.verdict,
                command: case.command.clone(),
            });
        }
    }
    score
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A multi-line command must survive the round trip, because 35% of real
    /// commands are multi-clause scripts and a corpus that cannot hold them
    /// can only ever describe the easy half.
    #[test]
    fn a_command_survives_the_round_trip() {
        for command in [
            "git push | tail -1",
            "git commit -F- <<'MSG' 2>&1 | tail -8\nsubject\n\nbody\nMSG\n",
            "echo 'a\tb' && git push",
            "a\\nb literal backslash-n",
            "trailing backslash \\",
        ] {
            let line = line_for(Verdict::Match, command);
            assert_eq!(line.matches('\n').count(), 1, "one line per case");
            let back = parse(&line);
            assert_eq!(back.len(), 1);
            assert_eq!(back[0].command, command, "round trip changed {command:?}");
        }
    }

    #[test]
    fn comments_and_blank_lines_are_not_cases() {
        let text = format!("{HEADER}\n\n# a note\nmatch\tgit push | tail -1\n");
        let cases = parse(&text);
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].verdict, Verdict::Match);
    }

    /// An unlabelled case is not evidence. Counting `?` as agreement would let
    /// a dump from `explain` masquerade as a review nobody did.
    #[test]
    fn an_unreviewed_case_counts_as_no_evidence() {
        let rule = crate::rules::by_id("pipe-to-tail").unwrap();
        let cases = parse("?\tgit push | tail -1\n?\tgit status\n");
        let s = score_cases(rule, &cases);
        assert_eq!(s.reviewed, 0);
        assert_eq!(s.unreviewed, 2);
        assert!(s.agrees(), "nothing was claimed, so nothing can disagree");
        assert_eq!(s.precision(), None, "unmeasured, not perfect");
    }

    #[test]
    fn a_disagreement_names_the_line_and_the_command() {
        let rule = crate::rules::by_id("pipe-to-tail").unwrap();
        // A human says this must stay silent; the rule fires. That is the
        // shape of every false positive worth catching.
        let cases = parse("nomatch\tgit push origin main | tail -1\n");
        let s = score_cases(rule, &cases);
        assert!(!s.agrees());
        assert_eq!(s.disagreements[0].line, 1);
        assert_eq!(s.disagreements[0].expected, Verdict::NoMatch);
    }
}
