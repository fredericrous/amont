//! Committed repo policy — the team's decisions, shipped with the repository.
//!
//! `amont.conf` could always ADD checks; it could never say anything about
//! the built-ins, so "clippy is warn-only here" meant every teammate running
//! the same `git config` incantation, unverified. `severity` and `skip`
//! lines close that: committed, reviewed like code, and trust-gated exactly
//! like declared checks — a repository you cloned to read cannot weaken your
//! safety net until you consent.
//!
//! Precedence is a specificity ladder decided per KEY: built-in default <
//! system config < global config < POLICY < local config < worktree <
//! command-line. Between different keys naming the same check, specificity
//! (full id > short name > trigger) decides, whatever the source — a local
//! `amont.severity.pre-commit warn` must never be unbeatable by policy, and
//! a policy full-id beats a local trigger. Skips are a UNION of all sources:
//! there is no unskip mechanism anywhere, so ordering has nothing to decide.
//!
//! The store is a process-global `OnceLock`, installed by each entrypoint
//! immediately after `manifest::load` — one process means one repository,
//! structurally (see the counter-precedent note at `manifest::Manifest`).
//! The FLEET never touches it: a scanner walks many repositories and reads
//! `manifest::read_lines` per repo instead. Every RULE here is a pure
//! function over `&Policy`, so the rules are unit-testable without the
//! global; no amont-runtime unit test may call [`install`].

use crate::check::Severity;
use crate::manifest::{Line, PolicyLine};

/// What a trusted manifest's policy lines add up to.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Policy {
    /// `(target, severity)` in file order — later lines overwrite earlier
    /// ones at fold time, matching git config's own precedence rule.
    pub severities: Vec<(String, Severity)>,
    /// Skip targets, resolved by the same three-way naming `hook.skip` uses.
    pub skips: Vec<String>,
}

impl Policy {
    pub fn is_empty(&self) -> bool {
        self.severities.is_empty() && self.skips.is_empty()
    }

    /// Collect the policy from parsed lines, and say which targets name
    /// nothing — validated here, over the WHOLE file, because `parse_line`
    /// sees only earlier lines and a `severity smoke warn` written above its
    /// own `pre-commit smoke …` declaration would be wrongly refused.
    ///
    /// The naming universe is built-ins plus the file's CHECK lines only
    /// (`Line::is_check`) — a `tool ruff …` pin must not make
    /// `severity ruff warn` look valid, and `Broken` lines count because a
    /// broken line still produces a check id that `hook.skip` can reach.
    ///
    /// An unmatched target is a NOTE, not a `Line::Broken` — a broken line
    /// manufactures a check named after itself, and a phantom
    /// `pre-commit-clipy` helps nobody.
    pub fn from_lines(lines: &[Line]) -> (Policy, Vec<String>) {
        let mut policy = Policy::default();
        let mut notes = Vec::new();
        let names_something = |target: &str| {
            crate::registry::CHECKS
                .iter()
                .any(|c| crate::names_check(c.name, target).is_some())
                || lines
                    .iter()
                    .filter(|l| l.is_check())
                    .any(|l| crate::names_check(&l.id(), target).is_some())
        };
        for line in lines {
            let Line::Policy { what, lineno } = line else {
                continue;
            };
            let target = match what {
                PolicyLine::Severity { target, .. } | PolicyLine::Skip { target } => target,
            };
            if !names_something(target) {
                let kind = match what {
                    PolicyLine::Severity { .. } => "severity",
                    PolicyLine::Skip { .. } => "skip",
                };
                notes.push(format!(
                    "{}:{lineno}: {kind} {target:?} names no check here",
                    crate::manifest::MANIFEST
                ));
                continue;
            }
            match what {
                PolicyLine::Severity { target, severity } => {
                    policy.severities.push((target.clone(), *severity));
                }
                PolicyLine::Skip { target } => policy.skips.push(target.clone()),
            }
        }
        (policy, notes)
    }
}

static POLICY: std::sync::OnceLock<Policy> = std::sync::OnceLock::new();

/// Install the loaded repository's policy for this process. Idempotent —
/// first install wins, the `override_file_set` precedent — and called by
/// every entrypoint immediately after `manifest::load`, BEFORE any config
/// read. That ordering is the whole contract: `check_timeout` and friends
/// cache on first read.
pub fn install(policy: Policy) {
    let _ = POLICY.set(policy);
}

/// The installed policy, or an empty one — a process that never loaded a
/// manifest has no policy, which resolves every rule to today's behaviour.
pub fn current() -> &'static Policy {
    static EMPTY: Policy = Policy {
        severities: Vec::new(),
        skips: Vec::new(),
    };
    POLICY.get().unwrap_or(&EMPTY)
}

/// The union `hook.skip` resolution sees: machine skips plus policy skips.
/// A union and not a ladder — nothing anywhere can UN-skip, so there is no
/// conflict for ordering to settle.
pub fn union_skips(config_skips: Vec<String>, policy: &Policy) -> Vec<String> {
    let mut all = config_skips;
    for s in &policy.skips {
        if !all.contains(s) {
            all.push(s.clone());
        }
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::parse_lines;

    #[test]
    fn collects_severities_and_skips_in_file_order() {
        let lines = parse_lines(
            "severity clippy warn\nskip yamllint\nseverity pre-push-cargo-test block\n",
        );
        let (p, notes) = Policy::from_lines(&lines);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(
            p.severities,
            vec![
                ("clippy".to_string(), Severity::Warn),
                ("pre-push-cargo-test".to_string(), Severity::Block),
            ]
        );
        assert_eq!(p.skips, vec!["yamllint".to_string()]);
    }

    /// A target can be a declared check — including one written BELOW the
    /// policy line, which is why validation is a whole-file pass.
    #[test]
    fn a_declared_check_below_the_policy_line_still_validates() {
        let lines =
            parse_lines("severity smoke warn\npre-commit    smoke   *   block   ./smoke.sh\n");
        let (p, notes) = Policy::from_lines(&lines);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(p.severities.len(), 1);
    }

    /// A tool pin must not lend its name to the validation universe.
    #[test]
    fn a_tool_pin_does_not_validate_a_policy_target() {
        let lines = parse_lines("tool ruffian 0.4\nseverity ruffian warn\n");
        let (p, notes) = Policy::from_lines(&lines);
        assert!(p.severities.is_empty());
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("names no check here"), "{notes:?}");
        assert!(notes[0].contains("amont.conf:2"), "{notes:?}");
    }

    /// A typo is a note with a position, never a phantom check.
    #[test]
    fn an_unmatched_target_is_a_note_not_a_check() {
        let lines = parse_lines("severity clipy warn\n");
        let (p, notes) = Policy::from_lines(&lines);
        assert!(p.is_empty());
        assert_eq!(
            notes,
            vec!["amont.conf:1: severity \"clipy\" names no check here"]
        );
    }

    /// Triggers and short names resolve exactly as `hook.skip` resolves them.
    #[test]
    fn triggers_and_short_names_are_valid_targets() {
        let lines = parse_lines("skip pre-commit\nseverity ban-terms warn\n");
        let (p, notes) = Policy::from_lines(&lines);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(p.skips, vec!["pre-commit".to_string()]);
        assert_eq!(p.severities.len(), 1);
    }

    #[test]
    fn union_adds_policy_skips_without_duplicating() {
        let p = Policy {
            severities: Vec::new(),
            skips: vec!["yamllint".into(), "clippy".into()],
        };
        let got = union_skips(vec!["clippy".into()], &p);
        assert_eq!(got, vec!["clippy".to_string(), "yamllint".to_string()]);
    }
}
