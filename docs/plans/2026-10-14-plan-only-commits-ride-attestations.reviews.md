# Full reviews: plan-only commits ride attestations

Earlier designs (scope job; then CI-only fingerprints) went through two full panels (backend, rust, tui, unix; 5 and 4 approve-with-changes) and were superseded by your six points; what still applied was folded into this body.
This body: backend delta (31k tokens, 45 s) found one blocker (notes keys must be objects), rejected with attest SPEC.md:312-320 and `attest.rs` `INPUTS_REF`, plus two high findings, applied. Delta on the final body (20k, 22 s): blocker withdrawn, approve-with-changes. Rust, tui and unix were not re-run: `plan-panel` asked for backend alone on this change.

**Carried into implementation (override the body where they differ):**
- State which of identity or declared inputs carries a setting: identity is mandatory for any setting not in a tracked file; test both routes with a scope-only `amont.conf` edit.
- Add tests that `stamp_tree` and `tree_tokens` write and read the inputs ref.
- Batch spec reads and cache by spec blob id in `stamps_for`; report the git spawn count for a 20-commit push against today's.
- On `pull_request` the checked-out tree is the merge ref's, so the sign job's tree-equality check only guards a mismatched checkout; the payload's input fingerprint carries the binding.
