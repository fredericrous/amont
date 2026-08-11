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
//! on every commit would be noise nobody asked for. The mechanism lives in
//! [`crate::gate_stamp`]; this is only the hook-shaped door to it.

use crate::check::Verdict;

pub fn run() -> Verdict {
    crate::gate_stamp::bind_to_head();
    Verdict::Proceed
}
