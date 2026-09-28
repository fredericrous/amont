//! post-rewrite — a rebase rewrote the branch, so its test stamps no longer
//! vouch for it: rehearse the push gate again, in the background.
//!
//! A stamp is bound to the commit (and the tree) the suite actually read. A
//! rebase replays the work onto a new base, which is new content — the old
//! stamp is rightly worthless, and without a fresh one the next push runs
//! the whole suite while git holds its connection to the remote open, the
//! exact failure the rehearsal exists to prevent. post-commit cannot cover
//! it: git calls post-commit for each replayed commit, while the rebase is
//! still IN PROGRESS, and post-commit deliberately stands down then (the
//! commit being made is not the one that will be pushed). post-rewrite is
//! the first moment the rebased branch exists as a whole.
//!
//! `git commit --amend` also calls post-rewrite (`amend`), but post-commit
//! has already rehearsed that commit — only `rebase` is handled here.
//!
//! Same opt-in and same silence rules as post-commit
//! (`amont.rehearseOnCommit`); notification-only, never blocks.

use crate::check::Verdict;

pub fn run(settings: &crate::config::Settings, ctx: &crate::registry::Ctx) -> Verdict {
    let rewritten_by = ctx.args.first().and_then(|a| a.to_str());
    if rewritten_by == Some("rebase") {
        crate::hooks::post_commit::rehearse(settings, true);
    }
    Verdict::Proceed
}
