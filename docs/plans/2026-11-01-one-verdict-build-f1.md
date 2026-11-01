---
status: done
branch: fix/inert-out-of-scope
repos: [amont]
canonical: decisions:docs/plans/2026-11-01-one-verdict-build.md
phases: [F1]
---
# One verdict per tree, build: F1 (pointer)

The canonical plan is in decisions at `docs/plans/2026-11-01-one-verdict-build.md`. This branch is phase **F1**: a pre-commit gate with nothing in scope returns `Inert`, not `Passed`, so a plan-only commit no longer stamps a gate that never ran. That stamp let a push attest a deferred `cargo-test` (programme one-verdict-per-tree, t07).

## Verification

- **Repro** (`/private/tmp/claude-501/t07/repro.sh`). Setup: a heavy gate at both stages, `onBusy defer`, a deferred `a.txt` commit, then a plan-only commit B.
  - Expected: B carries no `gate@` token, the push says "deferring it here too", the gate runs 0 times, and `pre-push-gate` is not in the attestation.
  - Before the fix (1.65.0): B carried `gate@d376c4f0c394588d`, the push said "gated at commit instead", and the attestation listed `pre-push-gate`.
  - After the fix (built binary): no token, "was deferred to CI at commit on 1 pushed commit — deferring it here too", 0 runs, `pre-push-gate` absent.
- **Suites, under `amont slot run`:**
  - `it busy_defers`: 11 passed, including the new `a_plan_commit_after_a_deferred_one_is_never_gated` and the stricter docs test that now runs the hooks.
  - `amont-runtime` manifest: 51 passed.
  - The stamp, scope, bypass and attest integration tests: 19, 14, 3 and 30 passed.
- **CI:** amont#388's first run failed `flows::push::the_push_journey` at step `amend`, as the fix predicts: a reword stages nothing, so it now stamps nothing of its own. With that assertion dropped, `cargo test -p amont --test it flows::` gives 7 passed.
- **CI is the full-suite check:** `make check` across every crate.

## Implementation review

- Approve-with-changes, round 1.
- Fixed: the verification record (above).
- Deliberate: the new test's attestation assertion needs `ssh-keygen`, which is present on macOS and in CI. Without it, the push assertions still catch the bug.
- Delta (after the journey-test change): approve. Follow-up, low: the push.rs comment above step `amend` still mentions the note-union case. That case is covered by `gate_stamp::tests::the_commit_note_does_not_hide_the_tree_notes_push_tokens`.
