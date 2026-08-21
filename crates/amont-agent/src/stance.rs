//! Where a rule's stance comes from.
//!
//! Compiled default, overridden by git config, with an environment kill switch
//! on top:
//!
//! ```text
//! rule.default_stance  <  amont.agent.stance  <  amont.agent.<id>.stance
//! ```
//!
//! then clamped to `Observe` if the guard is switched off.
//!
//! ## Why git config and not `amont.conf`
//!
//! `amont.conf` is committed. A rule that a committed file could promote to
//! `Deny` would mean cloning a repository hands it the power to refuse your
//! shell commands — the trust prompt helps, but it is a yes/no over a whole
//! file, which is the wrong granularity for "which of my commands may be
//! blocked". Promotion power stays on the machine, with the person.
//!
//! `amont.conf`'s parser also rejects any stage but `pre-commit`/`pre-push`,
//! and `policy::from_lines` validates targets against `registry::CHECKS`, which
//! will never contain these rules. Teaching it otherwise would be a change to
//! the dependency-free commit path in service of this crate — exactly the
//! coupling the separate crate exists to avoid.
//!
//! ## The kill switch is an environment variable on purpose
//!
//! Reading a git config key costs a process. `$AMONT_AGENT_OFF` costs nothing
//! and is checked first, before any rule has run. `amont.agent.enabled` exists
//! too, but it is consulted only once something has already fired — where its
//! effect is to clamp to silence anyway. That asymmetry looks like an
//! inconsistency and is a deliberate one.

use amont_runtime::config;

use crate::rules::{Rule, Stance};

pub const KEY_ENABLED: &str = "amont.agent.enabled";
pub const KEY_STANCE: &str = "amont.agent.stance";
pub const ENV_OFF: &str = "AMONT_AGENT_OFF";

const ALLOWED: &[&str] = &["observe", "advise", "deny"];

/// Whether the guard may act at all. Checked before any rule runs.
pub fn switched_off() -> bool {
    std::env::var_os(ENV_OFF).is_some_and(|v| !v.is_empty() && v != "0")
}

/// The key naming one rule's stance.
///
/// `amont.agent.<id>.stance` parses as section `amont`, subsection
/// `agent.<id>`, key `stance` — and git treats subsections as case-SENSITIVE
/// while lowercasing sections and keys. Every rule id is lowercase kebab-case,
/// which keeps that from mattering; `rule_ids_are_lowercase_kebab` is what
/// keeps it true.
pub fn key_for(rule: &Rule) -> String {
    format!("amont.agent.{}.stance", rule.id)
}

/// The effective stance for a rule that has already fired.
///
/// Only called on the fired set: each lookup here is a `git config` process,
/// and the no-fire path must not pay for one.
pub fn resolve(rule: &Rule) -> Stance {
    if switched_off() || !config::boolean_or(KEY_ENABLED, true) {
        return Stance::Observe;
    }
    // A floor for every rule, then the per-rule key on top of it. A typo in
    // either is reported by `config`'s own `complain` rather than silently
    // disabling the guard: a stance you believe you set and did not is the
    // failure this whole crate is about.
    let floor = config::enumerated_or(KEY_STANCE, ALLOWED, rule.default_stance.as_str());
    let mine = config::enumerated_or(&key_for(rule), ALLOWED, floor);
    Stance::parse(mine).unwrap_or(rule.default_stance)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::RULES;

    /// The config key is derived from the id, and git subsections are
    /// case-sensitive. An id with a capital in it would produce a key nobody
    /// could set from the shell without knowing that.
    #[test]
    fn rule_ids_are_lowercase_kebab() {
        for r in RULES {
            assert!(
                r.id.bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-'),
                "rule id `{}` is not lowercase kebab-case",
                r.id
            );
            assert!(!r.id.starts_with('-') && !r.id.ends_with('-'), "{}", r.id);
        }
    }

    #[test]
    fn the_key_names_the_rule() {
        let r = &RULES[0];
        assert_eq!(key_for(r), format!("amont.agent.{}.stance", r.id));
    }

    /// A rule id appearing twice would mean two rules sharing one config key,
    /// so setting the stance of one would silently move the other.
    #[test]
    fn rule_ids_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for r in RULES {
            assert!(seen.insert(r.id), "duplicate rule id `{}`", r.id);
        }
    }
}
