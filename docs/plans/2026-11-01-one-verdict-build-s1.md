---
status: active
branch: feat/run-stamp
repos: [amont]
canonical: decisions:docs/plans/2026-11-01-one-verdict-build.md
phases: [S1]
---
# One verdict per tree, build: S1 (pointer)

This file points to the canonical plan in decisions, `docs/plans/2026-11-01-one-verdict-build.md`. This branch builds phase **S1**:

- `amont run --stamp [<gate>…]`: a runner other than the commit hook earns amont's stamp for the tree it verified.
- `amont stamps --json`: the published query that reports each gate's stamp state.
- A new ADR amending ADR-0010.

## Verification record (S1)

- **Tests** on the rebased tree (onto #388):
  - `cargo clippy -D warnings` on amont-runtime and amont: clean.
  - `cargo test --bin amont`: 7 passed.
  - `cargo test --test it` filtered to run_stamp, gate_pairs, busy_defers, push_stamps, cli_dispatch, host_env_pinned and docs_counts: 39 passed. This includes all five S1 tests.
- **Pilot** (scratch repo under /private/tmp, built binary, pair gate logging each run):
  - input: `stamps --json` before any run → expected both gates absent → actual `pre-commit-check: absent`, `pre-push-check: absent`.
  - input: `run --stamp` → expected one suite run, both stamped → actual exit 0, `runs=1`, `stamped pre-commit-check`, `pre-push-check covered by its commit-time twin`.
  - input: `stamps --json` after → expected both stamped → actual both `stamped`.
  - input: pre-push of the same tree → expected "gated at commit instead", no rerun → actual `gated at commit instead — not repeating it here`, `runs=1`.
  - input: `run --stamp` with a changed tracked file → expected refused, exit 2 → actual exit 2.
- **Deferred gate**: covered by `a_deferred_gate_leaves_no_token` (exit 4, no token, `stamps` reports `deferred`).
