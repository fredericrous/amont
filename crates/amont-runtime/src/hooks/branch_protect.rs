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
    all_zero(&r.local_oid)
}

/// The push that CREATES the branch on the remote — there is nothing there
/// yet to protect.
///
/// git spells this with an all-zero REMOTE oid, documented in githooks(5)
/// ("if the remote branch does not yet exist, `<remote-sha1>` will be 40
/// zeroes") and verified against git itself in
/// `crates/amont/tests/branch_protect_early.rs`.
///
/// Refusing it was wrong in a way that taught the bypass, which is the one
/// outcome this whole check is written to avoid. The first push of a new
/// repository is always a push to `main`, and the advice the refusal gives —
/// "Open a Pull Request" — cannot be followed: there is no base branch to
/// open one against. The only way past is `--no-verify`, which switches off
/// every other pre-push gate too, and having been taught it once people
/// reach for it again.
///
/// This does NOT weaken the check. Protecting `main` means protecting the
/// history somebody else might be relying on; a branch the remote has never
/// heard of has no history, no reviewers and no PR to bypass.
fn is_creation(r: &PushRef) -> bool {
    all_zero(&r.remote_oid)
}

/// git's spelling of "no object": 40 zeros. Length is not checked because
/// the caller is comparing git's own output, and an empty string — which no
/// git produces here — would be a false "yes" this guards against.
fn all_zero(oid: &str) -> bool {
    !oid.is_empty() && oid.chars().all(|c| c == '0')
}

/// Does `branch` exist on any remote we have fetched?
///
/// Reads `refs/remotes/*/<branch>` — local, no network, one process. A
/// repository with a remote it has never fetched from has no such ref, and
/// that is the right answer: nothing has been pushed, so nothing is
/// protected yet.
fn on_a_remote(branch: &str) -> bool {
    let pattern = format!("refs/remotes/*/{branch}");
    git::stdout(&["for-each-ref", "--format=%(refname)", &pattern])
        .is_some_and(|out| !out.trim().is_empty())
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
///
/// Also quiet when the branch does not exist on any remote yet, for the same
/// reason [`is_creation`] exists: this warning's whole content is "pushing
/// it will be refused", and in a repository whose first commit has not been
/// pushed anywhere that is simply false. Saying it anyway would send someone
/// to `git switch -c` to escape a refusal that is not coming.
pub fn early() -> Outcome {
    let Some(branch) = git::current_branch() else {
        return Outcome::Passed;
    };
    if !PROTECTED.contains(&branch) {
        crate::hooks::common::ok("Not committing on a protected branch");
        return Outcome::Passed;
    }
    if !git::has_remote() || !on_a_remote(branch) {
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
        // A creation is checked BEFORE the name: `main` that the remote has
        // never heard of is not the `main` this protects. Ordering matters
        // only for readability here, but the comment is the point — a later
        // reader must not "simplify" this into the name check alone.
        if is_creation(r) {
            continue;
        }
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

    const ZEROS: &str = "0000000000000000000000000000000000000000";

    /// An UPDATE to a ref that already exists on the remote.
    ///
    /// The `remote_oid` here is what this fixture used to hard-code as `"b"`
    /// for every case, which is why no test ever exercised a ref the remote
    /// did not have — and why `run` shipped refusing the first push of every
    /// new repository. Use [`creating`] for that case; it is a real one.
    fn r(local_oid: &str, remote_ref: &str) -> PushRef {
        PushRef {
            local_ref: "refs/heads/whatever".into(),
            local_oid: local_oid.into(),
            remote_ref: remote_ref.into(),
            remote_oid: "b".into(),
        }
    }

    /// A push that CREATES the ref on the remote: git sends 40 zeros as the
    /// remote oid. Verified against real git in
    /// `crates/amont/tests/branch_protect_early.rs`.
    fn creating(remote_ref: &str) -> PushRef {
        PushRef {
            local_ref: "refs/heads/main".into(),
            local_oid: "a".into(),
            remote_ref: remote_ref.into(),
            remote_oid: ZEROS.into(),
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

    /// The first push of a new repository is always a push to `main`, and
    /// there is nothing on the remote to protect.
    ///
    /// The refusal used to fire here, and its advice — "Open a Pull Request"
    /// — could not be followed: there is no base branch to open one
    /// against. The only way past was `--no-verify`, which switches off
    /// every other pre-push gate too. A guard that can only be satisfied by
    /// the blanket bypass has taught the bypass.
    #[test]
    fn creating_main_on_the_remote_is_allowed() {
        assert_eq!(run(&[creating("refs/heads/main")]), Outcome::Passed);
        assert_eq!(run(&[creating("refs/heads/master")]), Outcome::Passed);
    }

    /// And the protection is unchanged the moment the branch exists: the
    /// SECOND push is a normal update, and normal updates are refused.
    #[test]
    fn the_next_push_to_that_same_branch_is_refused() {
        assert_eq!(run(&[r("a", "refs/heads/main")]), Outcome::Failed);
    }

    /// Creating one branch does not smuggle an update to another through in
    /// the same push.
    #[test]
    fn a_creation_beside_a_real_update_still_fails() {
        assert_eq!(
            run(&[creating("refs/heads/feat/x"), r("a", "refs/heads/main")]),
            Outcome::Failed
        );
    }

    /// Deleting a branch that the remote does not have is not a deletion of
    /// anything. Both oids zero is a push git would not send, and the answer
    /// either way must not be "blocked".
    #[test]
    fn a_delete_of_a_nonexistent_remote_branch_is_not_blocked() {
        let mut p = creating("refs/heads/main");
        p.local_oid = ZEROS.into();
        assert_eq!(run(&[p]), Outcome::Passed);
    }

    /// An empty oid is not "all zeros". Nothing git emits looks like this,
    /// and the guard must not read a parse failure as "nothing to protect".
    #[test]
    fn an_empty_remote_oid_is_not_a_creation() {
        let mut p = r("a", "refs/heads/main");
        p.remote_oid = String::new();
        assert!(!is_creation(&p));
        assert_eq!(run(&[p]), Outcome::Failed);
    }
}
