---
status: done
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

## Decision log (S1 review round 1)

- **Detached HEAD is allowed.** The coordinator's ruling: relais verifies in a detached worktree, so `run --stamp` refuses only tracked changes, not a detached HEAD. Test: `a_detached_head_is_stamped`.
- **Tracked changes are still refused** (exit 2), because the gates would judge content no tree holds.
- **Unix-only tests.** The busy-host test (perl holding a slot) and the attestation test (ssh-keygen, shell shims) are behind `#[cfg(unix)]`, with the slot fixtures in their own module, as `busy_defers.rs` is.
- **Output is an enum, not a bool.** `stamps --json` / text is `run_stamp::Output`.
- **Flags `--stamp` cannot honour are refused**, naming the flag: `--all-files`, `--quiet`, `--raw`, `--hooks-dir`. Test: `flags_that_stamp_cannot_honour_are_refused`.
- **`stamps --tree <other>` reads the attestation for that tree.** It judges a throwaway detached checkout of the tree's commit (or of a commit made for the bare tree id), which is removed afterwards. Before this, it read only HEAD's attestation, so an attested other tree read `absent`.

## Verification record (S1, round 1 fixes)

- **Attestation end to end** (`an_attested_tree_reads_as_attested_even_when_it_is_not_checked_out`, unix): a signer is configured (ed25519 key, `allowed_signers` with `amont-attest`), `run --stamp` stamps the tree, the push skips the suite (`runs` stays 1) and publishes the attestation. The remote's signed note names `pre-push-check`. With the local stamp refs removed, `stamps --json` reports `attested` for both gates. After a new commit, HEAD reads `absent`, `--tree HEAD~1` and the stamped tree id both read `attested`, and the throwaway checkout is gone afterwards.
- **Tests:** clippy `-D warnings` clean on amont-runtime and amont; `cargo test -p amont --test it -- run_stamp`: 8 passed (under `amont slot run`).

## Implementation review

- Approved after three rounds. Round 1 was approve-with-changes:
  - 2 blocking: the Windows build, and boolean flag arguments.
  - 2 medium: no attestation shown end to end, and `--tree <other>` never read attestations.
  - 1 low: flags ignored with `--stamp`.
  - All were fixed in `718cb0f`.
- The Delta found 2 nits: a reason on the line after its discard, and a unix-only import. Both were fixed in "reason on the discard, unix import". The re-bind approved.
- Still to check: the Windows CI leg on the PR.
