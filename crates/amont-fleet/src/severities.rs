//! Reading `amont.severity.<check>`, the quieter cousin of `hook.skip`.
//!
//! A skip is at least honest about itself: the check does not run, the
//! dispatcher says so on every commit, and the dashboard counts it. A severity
//! downgrade leaves everything looking normal — the check runs, prints its
//! failure in red, and the commit sails through. Read the terminal and you see
//! a hook doing its job. Read the exit code and it enforced nothing.
//!
//! That gap is the whole reason this module exists. A repo with six downgraded
//! checks and a repo with none are indistinguishable in every column the
//! dashboard had before, and the six-downgrade repo is the one worth knowing
//! about.
//!
//! Read-only, like `skips` was at first. Toggling severity from the UI is a
//! later change; being able to SEE it does not have to wait for that.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use serde::Serialize;

use crate::checks::all_checks;
use crate::skips::{scope_of, Scope};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Warn,
    Block,
    /// Anything else in the config. Git will not complain about it and neither
    /// will the hook — it silently falls back to the declared severity. Worth
    /// showing precisely because it looks like it did something.
    Unrecognised,
}

impl Level {
    /// Read through the RUNTIME's parser, never a copy of it. This type exists
    /// only because the dashboard needs a third state — `Unrecognised` — that
    /// the dispatcher does not: to the hook an unknown value is simply absent,
    /// while here it is a line somebody wrote that does nothing.
    fn of(value: &str) -> Level {
        match amont_runtime::check::Severity::parse(value) {
            Some(amont_runtime::check::Severity::Warn) => Level::Warn,
            Some(amont_runtime::check::Severity::Block) => Level::Block,
            None => Level::Unrecognised,
        }
    }
}

/// A configured line, before anyone has asked which one git applies.
///
/// Carries only what the config file says, so there is no state in which a
/// field is meaningless.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RawEntry {
    check: String,
    value: String,
    scope: Scope,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SeverityOverride {
    /// The key's last component, exactly as configured.
    pub check: String,
    /// The configured value, exactly as written.
    pub value: String,
    pub level: Level,
    pub scope: Scope,
    /// Whether this is the entry git would actually apply.
    ///
    /// `--get-regexp` lists EVERY entry; the dispatcher asks `--get`, which
    /// returns the last one, so a local `block` beats a global `warn`. Listing
    /// both and treating each as authoritative made the dashboard report a
    /// downgrade the dispatcher does not apply — measured, with global `warn`
    /// and local `block` on one check.
    ///
    /// Shadowed entries are still SHOWN. Somebody wrote them, and knowing a
    /// global downgrade is being overridden here is worth more than a tidy list.
    pub effective: bool,
}

impl SeverityOverride {
    /// Every check this key downgrades — the same vocabulary `hook.skip` takes:
    /// a full id, a trigger, or a short name.
    ///
    /// The two surfaces used to disagree. `hook.skip` matched by substring and
    /// this key was an exact name, so a repo that learned `hook.skip clippy`
    /// went on to write `amont.severity.clippy warn` and got silence. One
    /// rule now answers both.
    pub fn covers(&self) -> Vec<&'static str> {
        covers(&self.check)
    }

    /// True when the key names no check this binary knows, or carries a value
    /// git will not read as a severity. Nothing in git says so, which is how a
    /// repo ends up believing a check was downgraded when it was never touched.
    pub fn is_inert(&self) -> bool {
        self.covers().is_empty() || self.level == Level::Unrecognised
    }

    /// The downgrade itself — a check that runs, reports, and does not block.
    ///
    /// Only when this entry is the one git applies: a shadowed `warn` weakens
    /// nothing.
    pub fn weakens(&self) -> bool {
        self.effective && self.level == Level::Warn && !self.covers().is_empty()
    }

    /// Configured, but overridden by a later entry. Not damage — but not
    /// nothing either, which is why it is shown rather than filtered out.
    pub fn shadowed(&self) -> bool {
        !self.effective
    }
}

/// Every `amont.severity.*` entry visible from `repo`, with where it came
/// from.
///
/// `--get-regexp` rather than a key-by-key sweep: the point is to find keys
/// nobody expected, including ones naming checks that do not exist.
pub fn read(repo: &Path) -> Vec<SeverityOverride> {
    let Ok(out) = Command::new("git")
        .args([
            "config",
            "--show-origin",
            "--get-regexp",
            r"^amont\.severity\.",
        ])
        .current_dir(repo)
        .output()
    else {
        return Vec::new();
    };
    // The committed policy's severity lines, ONLY when trusted — untrusted
    // policy does not apply, and a WARN column counting a downgrade the
    // dispatcher never enforces is the exact bug this module's doc records.
    let policy = if amont_runtime::trust::state(repo) == amont_runtime::trust::State::Trusted {
        let lines = amont_runtime::manifest::read_lines(repo);
        amont_runtime::policy::Policy::from_lines(&lines).0
    } else {
        amont_runtime::policy::Policy::default()
    };
    resolve(repo, parse(&String::from_utf8_lossy(&out.stdout)), &policy)
}

/// Turn raw entries into resolved ones by asking which value git applies.
///
/// Two separate questions, both answered by the RUNTIME rather than
/// re-derived here — config precedence is git's (system, then global, then
/// local, then includes), and a reimplementation would be a second opinion
/// about the one thing this column must not get wrong:
///
/// - **Which VALUE wins on this literal key**, when the same key is written
///   more than once (`--get`'s job). A MAP, not a mutation — `parse` used to
///   return `SeverityOverride`s whose `effective` field was `true` for
///   everything until this corrected it, a value that was simply wrong for as
///   long as it existed, and observably so: tests calling `parse` directly
///   got entries claiming to be effective without anyone having asked.
///
/// - **Which KEY wins for a given check**, when two keys of different shapes
///   both name it (full id beats short name beats trigger — `Overrides`'s
///   job, the same one the dispatcher calls). Settling only the first
///   question left two entries on *different* keys — say `pre-commit warn`
///   and `clippy block` — both marked effective, because each was the only
///   line on its own key. `clippy` is more specific and is what the
///   dispatcher actually applies to `pre-commit-clippy`; the WARN column
///   counting `pre-commit warn` as covering that check anyway was reporting a
///   downgrade nothing enforces.
fn resolve(
    repo: &Path,
    raws: Vec<RawEntry>,
    policy: &amont_runtime::policy::Policy,
) -> Vec<SeverityOverride> {
    let mut applied: BTreeMap<&str, Option<String>> = BTreeMap::new();
    for raw in &raws {
        if !applied.contains_key(raw.check.as_str()) {
            let v = amont_runtime::git::stdout_in(
                repo,
                &[
                    "config",
                    "--get",
                    &amont_runtime::registry::severity_key(&raw.check),
                ],
            );
            applied.insert(raw.check.as_str(), v);
        }
    }

    // The runtime's own fold, policy included — the ladder needs scope
    // labels, so this asks `--show-scope` in THAT repository and degrades
    // exactly as the dispatcher degrades (all config beats policy) when the
    // git there is too old to label scopes.
    let overrides = match amont_runtime::git::stdout_in(
        repo,
        &[
            "config",
            "--show-scope",
            "--get-regexp",
            r"^amont\.severity\.",
        ],
    ) {
        Some(scoped) => amont_runtime::registry::Overrides::from_scoped(&scoped, policy),
        None => amont_runtime::registry::Overrides::from_plain_with_policy_below(
            Some(
                raws.iter()
                    .map(|r| format!("amont.severity.{} {}\n", r.check, r.value))
                    .collect(),
            ),
            policy,
        ),
    };

    let policy_rows = policy.severities.iter().map(|(target, severity)| {
        // Effective when the fold's winner for some covered check is THIS
        // key, sourced from policy — the same duplicates-count-as-winners
        // stance the config rows take.
        let effective = covers(target).iter().any(|check| {
            overrides
                .applied_with_source(check)
                .is_some_and(|(k, _, src)| {
                    k == target && src == amont_runtime::registry::Source::Policy
                })
        });
        SeverityOverride {
            effective,
            check: target.clone(),
            level: Level::of(severity.as_str()),
            value: severity.as_str().to_string(),
            scope: crate::skips::Scope::Other {
                origin: "amont.conf".to_string(),
            },
        }
    });

    raws.iter()
        .map(|r| {
            // A duplicate of the winning VALUE is indistinguishable from the
            // winner and is treated as effective too. That errs toward showing
            // a downgrade rather than hiding one, the safe direction here.
            let wins_own_key =
                applied.get(r.check.as_str()).and_then(|v| v.as_deref()) == Some(r.value.as_str());
            let level = Level::of(&r.value);
            // A value the runtime cannot even parse never enters the
            // specificity race at all — `Overrides` drops it rather than
            // letting it win by default, so there is no key here for it to
            // have won. `is_inert` already reports that case on its own
            // terms; this must not additionally call it "overridden".
            let effective = wins_own_key
                && (level == Level::Unrecognised
                    || covers(&r.check).iter().any(|check| {
                        overrides
                            .applied_to(check)
                            .is_some_and(|(winning_key, _)| winning_key == r.check)
                    }));
            SeverityOverride {
                effective,
                check: r.check.clone(),
                level,
                value: r.value.clone(),
                scope: r.scope.clone(),
            }
        })
        .chain(policy_rows)
        .collect()
}

/// Every check `pattern` reaches — the same vocabulary `hook.skip` takes: a
/// full id, a trigger, or a short name.
fn covers(pattern: &str) -> Vec<&'static str> {
    all_checks()
        .into_iter()
        .filter(|c| amont_runtime::names_check(c, pattern).is_some())
        .collect()
}

/// Split out from `read` so the origins can be exercised without arranging for
/// git to produce them. A repo can be made to write a local key easily and a
/// global one only by touching the developer's real `~/.gitconfig`, so a test
/// driving `read` alone can assert `Local` and nothing else — which a `scope`
/// field hard-coded to `Local` would satisfy just as well.
fn parse(stdout: &str) -> Vec<RawEntry> {
    stdout
        .lines()
        .filter_map(|line| {
            // `file:/path/to/config\tamont.severity.<check> <value>`
            let (origin, rest) = line.split_once('\t')?;
            let (key, value) = rest.split_once(' ')?;
            let check = key.strip_prefix("amont.severity.")?.trim();
            let value = value.trim();
            if check.is_empty() {
                return None;
            }
            Some(RawEntry {
                check: check.to_string(),
                value: value.to_string(),
                scope: scope_of(origin),
            })
        })
        .collect()
}

/// Build a local entry from raw parts, for fixtures.
#[cfg(test)]
pub fn for_test(check: &str, value: &str) -> SeverityOverride {
    SeverityOverride {
        check: check.to_string(),
        value: value.to_string(),
        level: Level::of(value),
        scope: Scope::Local,
        effective: true,
    }
}

/// A configured entry that git overrides with a later one.
#[cfg(test)]
pub fn shadowed_for_test(check: &str, value: &str) -> SeverityOverride {
    SeverityOverride {
        effective: false,
        ..for_test(check, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_check_set_to_warn_is_a_weakening() {
        let e = for_test("pre-commit-clippy", "warn");
        assert!(e.weakens());
        assert!(!e.is_inert());
    }

    /// `block` is the default. Writing it changes nothing, so it must not be
    /// reported as damage — but it is still shown, because it is evidence
    /// somebody was here.
    #[test]
    fn an_explicit_block_weakens_nothing() {
        let e = for_test("pre-commit-clippy", "block");
        assert!(!e.weakens());
        assert!(!e.is_inert());
    }

    /// The two ways to configure nothing at all, which look identical to a
    /// human skimming `.git/config`.
    #[test]
    fn a_misspelt_key_or_value_is_inert() {
        let bad_key = for_test("pre-commit-clipy", "warn");
        assert!(bad_key.is_inert());
        assert!(
            !bad_key.weakens(),
            "a key naming no check cannot have weakened one"
        );

        let bad_value = for_test("pre-commit-clippy", "advisory");
        assert!(bad_value.is_inert());
        assert!(!bad_value.weakens());
    }

    /// The bug this vocabulary exists to kill: `hook.skip clippy` reached the
    /// check and `amont.severity.clippy` did not, so the second thing anyone
    /// tried after the first one worked was silence. Whatever a skip resolves
    /// to, a severity key of the same text resolves to as well.
    #[test]
    fn both_surfaces_resolve_a_name_the_same_way() {
        for name in ["pre-commit-clippy", "clippy", "pre-commit"] {
            assert_eq!(
                for_test(name, "warn").covers(),
                crate::skips::suppressed_by(name),
                "`{name}` must reach the same checks on both surfaces"
            );
            assert!(!for_test(name, "warn").is_inert(), "`{name}` names checks");
        }
        // And a trigger key downgrades the whole trigger, which is the point of
        // admitting triggers into the vocabulary at all.
        assert!(for_test("pre-commit", "warn").covers().len() > 1);
    }

    /// A real repo, because the whole module is a parse of `git config
    /// --show-origin --get-regexp` output and a hand-written fixture would only
    /// prove the fixture matches itself. The format — key and value separated
    /// by a space, origin by a tab — is git's, not ours.
    #[test]
    fn reads_what_git_actually_wrote() {
        let d = std::env::temp_dir().join(format!("sev-read-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&d)
                .output()
                .expect("git");
        };
        git(&["init", "-q", "--template=", "."]);
        git(&["config", "amont.severity.pre-commit-clippy", "warn"]);
        git(&["config", "amont.severity.pre-commit-prettier", "block"]);
        // An unrelated key under the same section must not be mistaken for one.
        git(&["config", "amont.other.thing", "warn"]);

        let mut got = read(&d);
        got.sort_by(|a, b| a.check.cmp(&b.check));
        let _ = std::fs::remove_dir_all(&d);

        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0].check, "pre-commit-clippy");
        assert_eq!(got[0].level, Level::Warn);
        assert_eq!(got[0].scope, Scope::Local, "written to .git/config");
        assert_eq!(got[1].check, "pre-commit-prettier");
        assert_eq!(got[1].level, Level::Block);
    }

    /// Where a key LIVES is half the answer: deleting a line from `.git/config`
    /// and finding the check still downgraded is the same trap `hook.skip` set,
    /// and `--show-origin` is the only thing that tells them apart.
    #[test]
    fn the_origin_decides_the_scope() {
        let got = parse(concat!(
            "file:/repo/.git/config\tamont.severity.pre-commit-clippy warn\n",
            "file:/Users/me/.gitconfig\tamont.severity.pre-commit-prettier warn\n",
            "file:/etc/gitconfig\tamont.severity.pre-commit-ban-terms warn\n",
        ));
        assert_eq!(got.len(), 3, "{got:?}");
        assert_eq!(got[0].scope, Scope::Local);
        assert_eq!(got[1].scope, Scope::Global);
        assert!(
            matches!(got[2].scope, Scope::Other { .. }),
            "system config is neither of the two the UI can edit: {:?}",
            got[2].scope
        );
    }

    /// End to end against real git, with the precedence that caused the bug.
    ///
    /// Two entries on one key rather than a global/local pair: `--get` returns
    /// the LAST either way, and this needs no `GIT_CONFIG_GLOBAL` — an env var
    /// a test cannot set without racing every other test in the process.
    #[test]
    fn the_entry_git_applies_is_the_one_marked_effective() {
        let d = std::env::temp_dir().join(format!("sev-eff-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&d)
                .output()
                .expect("git");
        };
        git(&["init", "-q", "--template=", "."]);
        let key = "amont.severity.pre-commit-merge-conflict";
        git(&["config", "--add", key, "warn"]);
        git(&["config", "--add", key, "block"]);

        let got = read(&d);
        let _ = std::fs::remove_dir_all(&d);

        assert_eq!(got.len(), 2, "both entries must still be SHOWN: {got:?}");
        let warn = got.iter().find(|e| e.value == "warn").expect("the warn");
        let block = got.iter().find(|e| e.value == "block").expect("the block");
        assert!(!warn.effective, "git applies block, not warn");
        assert!(block.effective);
        assert!(
            !warn.weakens(),
            "the WARN column would report a downgrade the dispatcher does not apply"
        );
    }

    /// Two DIFFERENT keys naming the same check, not two lines on one key —
    /// the case `the_entry_git_applies_is_the_one_marked_effective` does not
    /// cover. Each is the only line on its own key, so `--get` on either key
    /// alone answers "yes, effective" — the bug this test pins: a full id
    /// must still beat a short name that was configured LOCALLY too, even
    /// though per-key `--get` cannot see the short name losing at all.
    #[test]
    fn a_full_id_beats_a_short_name_on_a_different_key() {
        let d = std::env::temp_dir().join(format!("sev-crosskey-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&d)
                .output()
                .expect("git");
        };
        git(&["init", "-q", "--template=", "."]);
        git(&["config", "amont.severity.clippy", "warn"]);
        git(&["config", "amont.severity.pre-commit-clippy", "block"]);

        let got = read(&d);
        let _ = std::fs::remove_dir_all(&d);

        assert_eq!(got.len(), 2, "{got:?}");
        let short = got
            .iter()
            .find(|e| e.check == "clippy")
            .expect("the short name");
        let full = got
            .iter()
            .find(|e| e.check == "pre-commit-clippy")
            .expect("the full id");
        assert!(
            !short.effective,
            "the full id is more specific and is what the dispatcher applies: {got:?}"
        );
        assert!(full.effective);
        assert!(
            !short.weakens(),
            "the WARN column must not count a downgrade the dispatcher never applies"
        );
    }

    /// The other half of the same rule: a trigger exempted for ONE check is
    /// still effective for every other check it names — `docs/hook-skip-
    /// management.md`'s "the trigger still governs every check it did not
    /// exempt". A trigger-wide `warn` must not be marked shadowed just
    /// because a more specific key wins for one of the many checks it covers.
    #[test]
    fn a_trigger_still_weakens_what_it_was_not_exempted_from() {
        let d = std::env::temp_dir().join(format!("sev-partial-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&d)
                .output()
                .expect("git");
        };
        git(&["init", "-q", "--template=", "."]);
        git(&["config", "amont.severity.pre-commit", "warn"]);
        git(&["config", "amont.severity.clippy", "block"]);

        let got = read(&d);
        let _ = std::fs::remove_dir_all(&d);

        let trigger = got
            .iter()
            .find(|e| e.check == "pre-commit")
            .expect("the trigger");
        assert!(
            trigger.weakens(),
            "pre-commit-prettier and the rest were never exempted: {got:?}"
        );
    }

    /// No keys at all is the normal case, and it must be an empty list rather
    /// than a parse of git's empty output producing one blank entry.
    #[test]
    fn a_repo_with_no_overrides_reads_empty() {
        let d = std::env::temp_dir().join(format!("sev-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::process::Command::new("git")
            .args(["init", "-q", "--template=", "."])
            .current_dir(&d)
            .output()
            .expect("git");
        let got = read(&d);
        let _ = std::fs::remove_dir_all(&d);
        assert!(got.is_empty(), "{got:?}");
    }
}
