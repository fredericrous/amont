//! post-commit — bind the pre-commit gate marker to the commit that now
//! exists.
//!
//! The commit object does not exist while pre-commit runs, so this is the
//! earliest moment "the checks ran" can be attached to a hash. git invokes
//! post-commit even for `--no-verify` (the flag skips only pre-commit and
//! commit-msg), which is exactly what makes the stamp trustworthy: a commit
//! that dodged the checks arrives here with no marker and gets no stamp.
//!
//! Notification-only, like the hook itself: git ignores its exit code, so
//! this never blocks, and it prints nothing — a bookkeeping step that talked
//! on every commit would be noise nobody asked for. That includes the bypass
//! ledger: a commit that dodged its gate is COUNTED here, silently — the
//! number's whole value is that it is collected without a lecture. The
//! mechanisms live in [`crate::gate_stamp`] and [`crate::bypass`]; this is
//! only the hook-shaped door to them.

use crate::check::Verdict;

pub fn run(ctx: &crate::registry::Ctx) -> Verdict {
    let stamped = crate::gate_stamp::bind_to_head();
    crate::bypass::note_unverified(ctx.manifest, &stamped);
    rehearse();
    Verdict::Proceed
}

/// The one thing post-commit says: that a background rehearsal of the push
/// gate has started — only in a repository that asked for it
/// (`amont.rehearseOnCommit`), and only outside a rebase or cherry-pick,
/// where the commit being made is not the one that will be pushed and the
/// next replay would cancel this run anyway. The worker itself decides
/// whether there is anything to run; this only pays the spawn.
fn rehearse() {
    if !crate::rehearsal::on_commit_enabled() || !crate::gate_stamp::push_stamps_enabled() {
        return;
    }
    if !crate::git_states_in_progress().is_empty() {
        return;
    }
    match crate::rehearsal::spawn_detached() {
        Ok(_) => println!("rehearsing the push gate in the background (`amont rehearse --status`)"),
        Err(e) => crate::hooks::common::warn(&format!("could not start the rehearsal: {e}")),
    }
}
