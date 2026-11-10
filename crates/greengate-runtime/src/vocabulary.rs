//! The project's vocabulary: commit types and branch prefixes, in one place.
//!
//! These were two hand-maintained lists in two hooks, and they drifted badly —
//! 8 of 12 commit types were REJECTED as branch prefixes, so `docs/…` and
//! `refactor/…` could not be pushed even though `docs:` and `refactor:` were
//! valid commit types. It cost two branch renames before anyone looked.
//!
//! Syncing them once would not hold: the next type added drifts again. So the
//! lists live here, each derived view reads from them, and the reconciliation
//! test below FAILS unless every name is either shared or explicitly declared
//! as an exception WITH A REASON. Adding a type now forces the decision rather
//! than deferring it to whoever next hits a rejected push.

/// A conventional-commit type and the gitmoji `commit-msg` prepends.
pub struct CommitType {
    pub name: &'static str,
    pub emoji: &'static str,
}

/// Order is user-visible: `commit-msg` lists these when it rejects a message.
pub const COMMIT_TYPES: &[CommitType] = &[
    CommitType {
        name: "build",
        emoji: "👷",
    },
    CommitType {
        name: "chore",
        emoji: "🔧",
    },
    CommitType {
        name: "docs",
        emoji: "📝️",
    },
    CommitType {
        name: "feat",
        emoji: "✨",
    },
    CommitType {
        name: "fix",
        emoji: "🐛",
    },
    CommitType {
        name: "perf",
        emoji: "⚡️",
    },
    CommitType {
        name: "refactor",
        emoji: "♻️",
    },
    CommitType {
        name: "revert",
        emoji: "⏪️",
    },
    CommitType {
        name: "style",
        emoji: "🎨",
    },
    CommitType {
        name: "test",
        emoji: "🚨",
    },
    CommitType {
        name: "add",
        emoji: "➕",
    },
    CommitType {
        name: "remove",
        emoji: "➖",
    },
];

/// A branch prefix, and whether dots are allowed after it.
pub struct BranchPrefix {
    pub name: &'static str,
    /// Dots suit version-bump branches (`chore/duro-1.50.50`) and would only be
    /// noise elsewhere. Git already rejects the dangerous forms (`..`, trailing
    /// `.lock`).
    pub dots: bool,
}

pub const BRANCH_PREFIXES: &[BranchPrefix] = &[
    BranchPrefix {
        name: "add",
        dots: false,
    },
    BranchPrefix {
        name: "automation",
        dots: false,
    },
    BranchPrefix {
        name: "build",
        dots: false,
    },
    BranchPrefix {
        name: "chore",
        dots: true,
    },
    BranchPrefix {
        name: "docs",
        dots: false,
    },
    BranchPrefix {
        name: "feat",
        dots: false,
    },
    BranchPrefix {
        name: "fix",
        dots: false,
    },
    BranchPrefix {
        name: "hotfix",
        dots: false,
    },
    BranchPrefix {
        name: "perf",
        dots: false,
    },
    BranchPrefix {
        name: "refactor",
        dots: false,
    },
    BranchPrefix {
        name: "remove",
        dots: false,
    },
    BranchPrefix {
        name: "revert",
        dots: false,
    },
    BranchPrefix {
        name: "style",
        dots: false,
    },
    BranchPrefix {
        name: "test",
        dots: false,
    },
];

/// Consumed only by the reconciliation test — that IS their job: they are the
/// record of a decision, and the test is what forces one to exist.
#[allow(dead_code)]
/// Commit types deliberately NOT usable as a branch prefix, and why.
/// Empty today — every type is a legitimate thing to open a branch for.
pub const COMMIT_ONLY: &[(&str, &str)] = &[];

#[allow(dead_code)]
/// Branch prefixes deliberately not commit types, and why.
pub const BRANCH_ONLY: &[(&str, &str)] = &[
    (
        "hotfix",
        "an urgency, not a kind of change — the commits inside are still fix:",
    ),
    (
        "automation",
        "bot-authored branches; their commits carry their own types",
    ),
];

pub fn branch_prefix(name: &str) -> Option<&'static BranchPrefix> {
    BRANCH_PREFIXES.iter().find(|p| p.name == name)
}

pub fn emoji_for(commit_type: &str) -> &'static str {
    COMMIT_TYPES
        .iter()
        .find(|t| t.name == commit_type)
        .map(|t| t.emoji)
        .unwrap_or("")
}

/// The type an emoji stands for — [`emoji_for`] read backwards.
///
/// This is what makes the `replace` gitmoji placement survive an amend: the
/// stored subject `✨  add a cart` carries its type only in the emoji, so
/// re-validating it means recovering `feat` from `✨`. Well defined because
/// `each_type_has_its_own_emoji` holds the emojis distinct.
pub fn type_for_emoji(emoji: &str) -> Option<&'static str> {
    COMMIT_TYPES
        .iter()
        .find(|t| t.emoji == emoji)
        .map(|t| t.name)
}

/// The branch contract, rendered for the rejection message so what a user is
/// told always matches what is enforced.
pub fn branch_contract() -> String {
    let plain: Vec<&str> = BRANCH_PREFIXES
        .iter()
        .filter(|p| !p.dots)
        .map(|p| p.name)
        .collect();
    let dotted: Vec<&str> = BRANCH_PREFIXES
        .iter()
        .filter(|p| p.dots)
        .map(|p| p.name)
        .collect();
    format!(
        "^(({})/[[:alnum:]_-]+|({})/[[:alnum:]_.-]+)$",
        plain.join("|"),
        dotted.join("|")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn names<T>(v: &[T], f: impl Fn(&T) -> &'static str) -> BTreeSet<&'static str> {
        v.iter().map(f).collect()
    }

    /// The point of this module. Every name must be shared, or declared as an
    /// exception with a reason — so adding a commit type forces a decision
    /// about whether it is also a branch prefix, instead of silently producing
    /// a rejected push months later.
    #[test]
    fn the_two_vocabularies_are_reconciled() {
        let types = names(COMMIT_TYPES, |t| t.name);
        let prefixes = names(BRANCH_PREFIXES, |p| p.name);
        let commit_only: BTreeSet<&str> = COMMIT_ONLY.iter().map(|(n, _)| *n).collect();
        let branch_only: BTreeSet<&str> = BRANCH_ONLY.iter().map(|(n, _)| *n).collect();

        let unexplained: Vec<_> = types
            .difference(&prefixes)
            .filter(|n| !commit_only.contains(*n))
            .collect();
        assert!(
            unexplained.is_empty(),
            "commit types that are not branch prefixes and not listed in COMMIT_ONLY: {unexplained:?}"
        );

        let unexplained: Vec<_> = prefixes
            .difference(&types)
            .filter(|n| !branch_only.contains(*n))
            .collect();
        assert!(
            unexplained.is_empty(),
            "branch prefixes that are not commit types and not listed in BRANCH_ONLY: {unexplained:?}"
        );

        // An exception must be a real exception, not a stale note.
        for n in &commit_only {
            assert!(
                !prefixes.contains(n),
                "{n} is listed COMMIT_ONLY but IS a branch prefix"
            );
        }
        for n in &branch_only {
            assert!(
                !types.contains(n),
                "{n} is listed BRANCH_ONLY but IS a commit type"
            );
        }
    }

    #[test]
    fn no_duplicates() {
        assert_eq!(names(COMMIT_TYPES, |t| t.name).len(), COMMIT_TYPES.len());
        assert_eq!(
            names(BRANCH_PREFIXES, |p| p.name).len(),
            BRANCH_PREFIXES.len()
        );
    }

    /// Two types sharing an emoji would make [`type_for_emoji`] a coin toss,
    /// and the `replace` gitmoji placement stores the emoji INSTEAD of the type
    /// — so a duplicate would silently rewrite one type into another on the
    /// next amend.
    #[test]
    fn each_type_has_its_own_emoji() {
        let emojis = names(COMMIT_TYPES, |t| t.emoji);
        assert_eq!(
            emojis.len(),
            COMMIT_TYPES.len(),
            "two commit types share an emoji: {emojis:?}"
        );
    }

    /// Every type recovers from its own emoji, and nothing else does.
    #[test]
    fn an_emoji_names_the_type_it_was_written_for() {
        for t in COMMIT_TYPES {
            assert_eq!(type_for_emoji(t.emoji), Some(t.name), "{}", t.name);
            assert_eq!(emoji_for(t.name), t.emoji);
        }
        assert_eq!(type_for_emoji("🚀"), None);
        assert_eq!(type_for_emoji(""), None);
    }

    /// Dots are a chore-only affordance for version bumps.
    #[test]
    fn only_chore_allows_dots() {
        let dotted: Vec<&str> = BRANCH_PREFIXES
            .iter()
            .filter(|p| p.dots)
            .map(|p| p.name)
            .collect();
        assert_eq!(dotted, vec!["chore"]);
    }

    /// The message a user is shown must describe what is actually enforced.
    #[test]
    fn the_contract_string_lists_every_prefix() {
        let c = branch_contract();
        for p in BRANCH_PREFIXES {
            assert!(c.contains(p.name), "{} missing from the contract", p.name);
        }
    }
}
