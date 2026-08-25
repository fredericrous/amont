//! pre-push-branch-pattern — reject branch names that don't follow the
//! convention, unless the branch is already on the remote.
//!
//! Ported from zsh. The shell version needed `rg` with a `grep` fallback and a
//! pattern written to satisfy both engines; here the match is a dozen lines of
//! ASCII checks, so the `rg`/`grep` split and `HOOKS_FORCE_GREP` disappear
//! entirely — as does the failure mode that motivated them, where a missing
//! `rg` made `! rg …` true and the hook rejected EVERY branch name.

use crate::check::Outcome;
use crate::git;
use crate::pushrefs::PushRef;
use crate::ui::{error_sign, highlight, valid_sign, warning_sign};

use crate::vocabulary;

/// The same contract, said at the FIRST COMMIT — `pre-commit-branch-pattern`.
///
/// pre-push is the enforcement point, and it is also the worst possible
/// moment to learn the rule: the work is done and stacked on a name that now
/// has to change, usually by an agent or a person who never saw the contract
/// before creating the branch. This is `usual-name`'s argument applied to
/// branch names — say it at the first commit, when the fix is one
/// `git branch -m` and nothing is built on top.
///
/// A warning, never a block, and quiet in every state where the push check
/// would not judge this branch:
///
///   - a DETACHED head (rebase, cherry-pick, bisect) names no branch;
///   - a repository with no remote has nothing this contract gates;
///   - a branch with a remote-tracking ref under any remote already exists
///     on a server. pre-push authorises an existing branch by its non-zero
///     remote oid; the remote-tracking ref is the local mirror of that same
///     fact, and costs no network to consult.
pub fn early() -> Outcome {
    let Some(branch) = git::current_branch() else {
        return Outcome::Passed;
    };
    if conforms(branch) {
        crate::say!(
            "{} Branch name conforms with authorized pattern",
            valid_sign()
        );
        return Outcome::Passed;
    }
    if !git::has_remote() {
        return Outcome::Passed;
    }
    // `*` in a for-each-ref pattern does not cross `/`, so this matches the
    // branch under any single remote name and nothing else.
    let tracking = format!("refs/remotes/*/{branch}");
    if git::stdout(&["for-each-ref", "--format=%(refname)", &tracking])
        .is_some_and(|refs| !refs.is_empty())
    {
        return Outcome::Passed;
    }
    let prefixes = vocabulary::BRANCH_PREFIXES
        .iter()
        .map(|p| p.name)
        .collect::<Vec<_>>()
        .join(", ");
    crate::say!(
        "{} Branch {} will be refused at push time — it does not match
    {}.
    Rename it now, while nothing is stacked on the name: {} <prefix>/…
    Prefixes: {prefixes}",
        warning_sign(),
        highlight(branch),
        highlight(&vocabulary::branch_contract()),
        highlight("git branch -m")
    );
    Outcome::Warned
}

/// Both the rule and the message now come from `vocabulary`, so what a user is
/// told and what is enforced cannot drift — and neither can the branch prefixes
/// and the commit types, which had diverged to the point that `docs/…` was
/// unpushable while `docs:` was a valid commit type.
///
/// `[[:alnum:]]` is ASCII in both rg and `grep -E` under the C locale, which is
/// what the shell version enforced; `is_ascii_alphanumeric` keeps that. A
/// Unicode-aware check would silently LOOSEN the rule.
pub fn conforms(branch: &str) -> bool {
    let Some((prefix, rest)) = branch.split_once('/') else {
        return false;
    };
    if rest.is_empty() {
        return false;
    }
    let Some(p) = vocabulary::branch_prefix(prefix) else {
        return false;
    };
    rest.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || (p.dots && c == '.'))
}

/// A delete pushes no name to validate. Same rule as `branch_protect::is_delete`
/// — the all-zero local oid is how git spells it.
fn is_delete(r: &PushRef) -> bool {
    r.local_oid.chars().all(|c| c == '0')
}

/// The branch name this ref would CREATE on the server, if it creates one.
///
/// `None` for a delete, for anything that is not `refs/heads/` (tags, notes),
/// and for a ref whose remote oid is non-zero — that last one means the branch
/// already exists on the server, so its name was authorised the day it was
/// created.
fn name_to_validate<'a>(r: &'a PushRef, zero: &str) -> Option<&'a str> {
    if is_delete(r) {
        return None;
    }
    if r.remote_oid != zero {
        return None;
    }
    r.remote_ref.strip_prefix("refs/heads/")
}

/// Judged on the REFS BEING PUSHED, not on the branch that happens to be
/// checked out.
///
/// It used to ask `rev-parse --abbrev-ref HEAD`, exactly the mistake
/// `branch_protect` documents avoiding, and it cost two things:
///
///   - `git push origin local:refs/heads/other` validated the wrong name
///     entirely — the one you are standing on rather than the one being
///     created;
///   - on a DETACHED HEAD, `--abbrev-ref HEAD` returns the literal string
///     `"HEAD"`, which `conforms` rejects. The `show-branch
///     remotes/origin/HEAD` short-circuit hid that in a normal clone, but in a
///     repository with no `refs/remotes/origin/HEAD` — a bare `git init` plus
///     `git remote add`, or after `git remote set-head --delete` — a perfectly
///     ordinary `git push origin HEAD:refs/heads/feat/x` was BLOCKED.
///
/// The `show-branch` probe is gone, replaced by the non-zero remote oid: git
/// has already told us whether the branch exists on the server, and the probe
/// depended on a remote-tracking ref that a fresh clone may not have.
pub fn run(refs: &[PushRef], args: &[std::ffi::OsString]) -> Outcome {
    // Matches `branch_protect::no_refs_is_a_pass`: nothing pushed, nothing to
    // judge.
    if refs.is_empty() {
        return Outcome::Passed;
    }
    let zero = git::stdout(&["hash-object", "--stdin"])
        .map(|h| "0".repeat(h.len()))
        .unwrap_or_else(|| "0".repeat(40));

    let candidates: Vec<&str> = refs
        .iter()
        .filter_map(|r| name_to_validate(r, &zero))
        .collect();
    if candidates.is_empty() {
        crate::say!(
            "{} No new branch name to validate. Push is authorized.",
            valid_sign()
        );
        return Outcome::Passed;
    }

    // Initial push to a brand-new empty remote: there's no feature-branch
    // convention to enforce while initializing a repo, and the default branch
    // (main/master) doesn't match the pattern anyway. Still needed alongside
    // the remote-oid rule above, because on a brand-new remote EVERY remote
    // oid is zero. git passes the remote name as the first argument.
    let remote = args
        .first()
        .and_then(|a| a.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("origin");
    // `--exit-code` turns the answer into exit codes — 2 for "connected,
    // zero refs", 0 for "has branches" — so nothing here reads (or waits
    // on) the listing itself. Bounded: this is a network round-trip asked
    // before a purely local check, and offline used to hang it. Anything
    // other than a clean "zero refs" answer falls through to the pattern
    // check — git failing is NOT the same as "no branches".
    match git::probe(
        &["ls-remote", "--exit-code", "--heads", remote],
        super::common::network_probe_budget(),
    ) {
        git::Probe::Exit(2) => {
            crate::say!(
                "{} Remote has no branches yet (initial push). Name is authorized.",
                valid_sign()
            );
            return Outcome::Passed;
        }
        git::Probe::TimedOut(secs) => {
            crate::say!(
                "{} {remote} did not answer within {secs}s — checking the name anyway.",
                warning_sign()
            );
        }
        _ => {}
    }

    // Per offending ref, so a multi-ref push names them all rather than
    // stopping at the first.
    let offenders: Vec<&str> = candidates
        .iter()
        .copied()
        .filter(|name| !conforms(name))
        .collect();
    if !offenders.is_empty() {
        for name in &offenders {
            crate::say!(
                "{} Branch name {} does not adhere to this project's contract:
    {}.
    Rename your branch with: {} <branch name>
    Or bypass this check with git -c hook.skip=branch-pattern push",
                error_sign(),
                highlight(name),
                highlight(&vocabulary::branch_contract()),
                highlight("git branch -m")
            );
        }
        return Outcome::Failed;
    }

    crate::say!(
        "{} Branch name conforms with authorized pattern",
        valid_sign()
    );
    Outcome::Passed
}

#[cfg(test)]
mod tests {
    use super::{conforms, name_to_validate, run, Outcome, PushRef};

    const ZERO: &str = "0000000000000000000000000000000000000000";

    fn r(local_oid: &str, remote_ref: &str, remote_oid: &str) -> PushRef {
        PushRef {
            local_ref: "refs/heads/whatever-is-checked-out".into(),
            local_oid: local_oid.into(),
            remote_ref: remote_ref.into(),
            remote_oid: remote_oid.into(),
        }
    }

    /// The table `branch_protect`'s tests are built from, applied to the
    /// question this check actually has to answer.
    ///
    /// `name_to_validate` rather than `run`, because `run`'s remaining branches
    /// consult `ls-remote` and a unit test must not reach a network.
    #[test]
    fn only_a_new_branch_ref_carries_a_name_to_validate() {
        // Judged on the REMOTE ref, not on whatever is checked out.
        // `git push origin local:refs/heads/other` creates `other`.
        assert_eq!(
            name_to_validate(&r("a", "refs/heads/feat/x", ZERO), ZERO),
            Some("feat/x")
        );
        // Already on the server: git has told us so with a non-zero remote
        // oid, and the name was authorised the day it was created.
        assert_eq!(
            name_to_validate(&r("a", "refs/heads/off-pattern", "b"), ZERO),
            None
        );
        // A delete pushes no name.
        assert_eq!(
            name_to_validate(&r(ZERO, "refs/heads/off", ZERO), ZERO),
            None
        );
        // A tag is not a branch, and neither is anything else outside
        // `refs/heads/`.
        assert_eq!(
            name_to_validate(&r("a", "refs/tags/v1.0", ZERO), ZERO),
            None
        );
        assert_eq!(
            name_to_validate(&r("a", "refs/notes/commits", ZERO), ZERO),
            None
        );
    }

    /// Matches `branch_protect::no_refs_is_a_pass`, and reaches no git at all.
    #[test]
    fn no_refs_is_a_pass() {
        assert_eq!(run(&[], &[]), Outcome::Passed);
    }

    #[test]
    fn accepts_every_declared_prefix() {
        for p in crate::vocabulary::BRANCH_PREFIXES {
            assert!(conforms(&format!("{}/some-work", p.name)), "{}", p.name);
        }
    }

    /// The divergence this module exists to end: these were all REJECTED as
    /// branch names while being perfectly valid commit types.
    #[test]
    fn accepts_the_prefixes_that_used_to_be_rejected() {
        for b in [
            "docs/rust-migration",
            "refactor/hook-registry",
            "perf/faster-startup",
            "build/bump-toolchain",
            "style/reformat",
            "revert/bad-change",
            "add/new-thing",
            "remove/dead-code",
        ] {
            assert!(conforms(b), "{b} should be allowed now");
        }
    }

    #[test]
    fn rejects_everything_else() {
        assert!(!conforms("off-pattern"));
        assert!(!conforms("duro-1.50.50"));
        assert!(!conforms("feat/"));
        assert!(!conforms("/x"));
        assert!(!conforms("feat/a/b"));
        assert!(!conforms("main"));
        assert!(!conforms("release/1")); // not a declared prefix
    }

    /// Dots stay a chore-only affordance for version bumps.
    #[test]
    fn dots_are_chore_only() {
        assert!(conforms("chore/duro-1.50.50"));
        assert!(!conforms("feat/duro-1.50.50"));
        assert!(!conforms("docs/1.2.3"));
    }

    /// `[[:alnum:]]` is ASCII under the C locale in both engines the shell
    /// version used; a Unicode-aware check would LOOSEN the rule.
    #[test]
    fn alnum_stays_ascii() {
        assert!(!conforms("feat/café"));
        assert!(!conforms("chore/日本語"));
    }
}
