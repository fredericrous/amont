//! pre-push-branch-protect — refuse a direct push to a protected branch.
//!
//! Absorbed from a hand-written `pre-push-branch-protect.sh` that two repos in
//! the fleet carried. Two things changed in the move:
//!
//!   1. It reads the refs from the shared [`PushRefs`] rather than draining
//!      stdin. The original's `while read` loop consumed the whole list, and it
//!      sorted before `pre-push-run-tests-js`, which therefore saw EOF and ran
//!      no gate at all. Silently, for as long as both existed.
//!   2. It is on everywhere, protecting `main` AND `master`, rather than being
//!      installed by hand in the repos someone remembered.
//!
//! The escape hatch is git's own: `git push --no-verify` skips every hook. That
//! is deliberate — a hook that cannot be bypassed is a hook people delete.

use crate::check::Outcome;
use crate::git;
use crate::pushrefs::PushRef;
use crate::ui::{error_sign, highlight, warning_sign};

/// Matched against the REMOTE ref — what you are writing to, not what you are
/// pushing from. `git push origin feature:main` is a push to main however the
/// local branch is named, and that is exactly the case a name-based check
/// misses.
const PROTECTED: [&str; 2] = ["main", "master"];

fn protected_name(remote_ref: &str) -> Option<&'static str> {
    let name = remote_ref.strip_prefix("refs/heads/")?;
    PROTECTED.iter().copied().find(|p| *p == name)
}

/// A delete (`git push :main`) is still a write to the branch, and the most
/// destructive one. The all-zero local oid is how git spells it.
fn is_delete(r: &PushRef) -> bool {
    r.local_oid.chars().all(|c| c == '0')
}

/// The same refusal, said at COMMIT time — `pre-commit-branch-protect`.
///
/// `run` above fires at the first moment a push to `main` exists, which is
/// the right moment for a `git push feature:main`. It is the wrong moment
/// for the other way this happens: the checkout was left on `main`, the
/// commits landed there, and the push is refused after the work is done. At
/// that point moving the commits is still one `git switch -c`, but it is
/// one an agent — or a person in a hurry — answers with `--no-verify`
/// instead, and the guard has taught the bypass.
///
/// A warning, never a block: `git commit` on `main` is legitimate in a
/// repository nobody pushes to by pull request, and a commit-time block is
/// exactly what makes people delete hooks. Quiet on a detached head, which
/// names no branch, and in a remoteless repository, where there is no push
/// for the contract to gate. `hook.skip branch-protect` silences both
/// voices, as with `branch-pattern`.
pub fn early() -> Outcome {
    let Some(branch) = git::current_branch() else {
        return Outcome::Passed;
    };
    if !PROTECTED.contains(&branch) {
        crate::hooks::common::ok("Not committing on a protected branch");
        return Outcome::Passed;
    }
    if !git::has_remote() {
        return Outcome::Passed;
    }
    crate::say!(
        "{} Committing on {} — pushing it will be refused by {}.
    Move the work to a branch now, while it is one command: {} <prefix>/…
    (the commit comes along; {} stays where it was)",
        warning_sign(),
        highlight(branch),
        highlight("pre-push-branch-protect"),
        highlight("git switch -c"),
        highlight(branch)
    );
    Outcome::Warned
}

pub fn run(refs: &[PushRef]) -> Outcome {
    let mut blocked = Vec::new();
    for r in refs {
        if let Some(name) = protected_name(&r.remote_ref) {
            blocked.push((name, is_delete(r)));
        }
    }
    if blocked.is_empty() {
        crate::hooks::common::ok("No push to a protected branch");
        return Outcome::Passed;
    }
    for (name, deleting) in &blocked {
        let what = if *deleting { "Deleting" } else { "Pushing to" };
        crate::say!(
            "{} {what} branch {} is forbidden. Open a Pull Request.",
            error_sign(),
            highlight(name)
        );
    }
    crate::say!(
        "    (if you really mean it: {})",
        highlight("git push --no-verify")
    );
    Outcome::Failed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(local_oid: &str, remote_ref: &str) -> PushRef {
        PushRef {
            local_ref: "refs/heads/whatever".into(),
            local_oid: local_oid.into(),
            remote_ref: remote_ref.into(),
            remote_oid: "b".into(),
        }
    }

    #[test]
    fn blocks_main_and_master() {
        assert_eq!(run(&[r("a", "refs/heads/main")]), Outcome::Failed);
        assert_eq!(run(&[r("a", "refs/heads/master")]), Outcome::Failed);
    }

    #[test]
    fn allows_any_other_branch() {
        assert_eq!(run(&[r("a", "refs/heads/feat/x")]), Outcome::Passed);
        assert_eq!(run(&[r("a", "refs/heads/maintenance")]), Outcome::Passed);
        assert_eq!(run(&[r("a", "refs/heads/mainline")]), Outcome::Passed);
    }

    /// Tags and other non-branch refs are not branches.
    #[test]
    fn allows_tags_even_named_main() {
        assert_eq!(run(&[r("a", "refs/tags/main")]), Outcome::Passed);
    }

    /// The check is on the REMOTE ref: pushing a differently-named local branch
    /// onto main is still a push to main.
    #[test]
    fn a_renamed_push_to_main_is_still_blocked() {
        let mut p = r("a", "refs/heads/main");
        p.local_ref = "refs/heads/my-feature".into();
        assert_eq!(run(&[p]), Outcome::Failed);
    }

    #[test]
    fn a_branch_delete_is_blocked_too() {
        assert_eq!(
            run(&[r(
                "0000000000000000000000000000000000000000",
                "refs/heads/main"
            )]),
            Outcome::Failed
        );
    }

    #[test]
    fn no_refs_is_a_pass() {
        assert_eq!(run(&[]), Outcome::Passed);
    }

    /// One bad ref among several still fails the push.
    #[test]
    fn a_mixed_push_is_blocked() {
        assert_eq!(
            run(&[r("a", "refs/heads/feat/x"), r("a", "refs/heads/main")]),
            Outcome::Failed
        );
    }
}
