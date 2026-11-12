---
canonical: homelab:docs/plans/2026-11-11-attest-signer.md
phases: [3]
status: done
branch: feat/cluster-signer
---
# In-cluster attestation signer: `cluster sign` and collect ingest (pointer)

This repository's slice of the homelab plan: Phase 3. The plan, its review
panel (body-sha 26070db06523) and its Decision log, which carries the
round-2 panel findings this phase builds, are canonical in the private
homelab repository (homelab#1169; Phase 1 recorded in #1172, Phase 2 in
#1177, shipped as git-attest 1.8.0).

## Phase 3

- **Pin:** git-attest `1.8` (platform-bound principals) in
  `crates/greengate-runtime` and `crates/greengate-fleet`.
- **`greengate cluster sign [--watch]`:** signs finished, Workflow-owned
  gate pods in the runner namespaces from Kubernetes-recorded facts.
  - It keeps only pods whose `ownerReferences` name an existing Workflow,
    and reads `gates` from that Workflow object.
  - It writes one ConfigMap per run, holding the signed block, with a
    7-day expiry.
  - Without `--watch` it does one pass. Its exit codes are 0, 1 and 2. A
    failed pass under `--watch` is logged and the loop continues.
  - SIGTERM and SIGINT finish the current run, through the shared
    self-pipe handler, and the idle wait sleeps in slices of at most 1 s.
  - The heartbeat is at `$HOME/heartbeat`. Logs are one plain stderr line
    per run.
- **collect:**
  - It reads the signer's ConfigMaps in one batched call per group, in
    `greengate.cluster.signer-namespace` (default `attest-signer`).
  - It verifies each block with git-attest, refuses a block whose tree
    differs from the record's, and files it into the local note.
  - A record still awaiting a signer block persists in a new pending state
    until the Workflow TTL passes. Old pending files still parse.
  - Its stderr lines: signed, not ready, refused, and "the TTL passed, the
    laptop signs it".
- **dispatch:** with `greengate.cluster.signer = in-cluster`, the laptop
  skips signing only the trees that have a filed signer block. A push never
  waits for one. When the setting is unset, today's behaviour holds.
- **Release:** released with `tag-release`. git-attest's `v1` alias then
  moves to 1.8.0, after this repository's CI passes on the pin.

## Code review

- amont: backend, lang:rust, unix, tui (a new subcommand and three collect stderr lines)

## Verification (row 4)

| Check | Observed |
| --- | --- |
| Unit tests (`cargo test -p greengate-runtime --lib cluster:: attest::`) | 74 passed |
| Integration tests (`cargo test -p greengate --test it -- detached_cluster cluster_gates names::`) | 81 passed. The signer cases: one pass writes one ConfigMap and deletes an expired one; a bare Job is ignored; the create race is done; a bad key exits 2; `-h` goes to stdout; SIGTERM stops in under 2 s; a second signal kills. The collect cases: a filed block is published by the push; tampered, wrong-tree and skipped blocks fall back to the laptop; a late block is filed by the next collect; a filed block survives an attest-mirror sync; a pending file from the previous release still parses. |
| Full workspace suite | All 29 pre-commit checks passed on 492c87a3, including `cargo-test`. On 01a56478 it was deferred because the host was busy, so the push gate runs it. |
| clippy `-D warnings`, fmt, `check-no-deps`, `cargo check --target x86_64-pc-windows-gnu` | clean |

## Decision log

- 2026-11-12 · Implemented by two implementation agents rather than relais. relais.toml still named the pre-4.0 packages, and a full `make check` takes longer than relais's wall-clock budget.
- 2026-11-12 · The `--watch` stop is an atomic flag checked before every action and every second while idle, not a self-pipe. A second signal restores SIG_DFL and re-raises.
- 2026-11-12 · Filed signer blocks live in `refs/notes/attest-local/amont-attest`, git-attest's local-only prefix, because an origin sync overwrites the local copy of `NOTES_REF`.
- 2026-11-12 · The dispatch change is in `attest_push_per_tip` (attest.rs), not dispatch.rs. The signer is `cluster/detached/sign.rs`.
- 2026-11-12 · Gate ids are checked as one non-empty token, not against a copy of git-attest's private grammar (ADR-0039).
- 2026-11-12 · Fixed on the way: `sign.rs` read the old runner namespace as a literal, so the 4.0 old-data-names guard failed. It now uses `names::old::CLUSTER_NAMESPACE`.

## Implementation review

- **implementation-review:** round 1 approve-with-changes, delta approve-with-changes, delta 2 approve-with-changes.
  - Fixed: the stop within a pass, the names, the flag arguments.
  - deliberate: `say_collected_unsigned` keeps its name; the rename is in a follow-up (cosmetic).
- **backend:** round 1 approve-with-changes, then approve.
  - Fixed: the transient pod wait, expiry/Job/race tests, the heartbeat verdict.
- **lang:rust:** round 1 approve-with-changes, delta approve-with-changes (one blocking finding: the Windows compile, fixed), delta 2 approve-with-changes.
  - deliberate: `heartbeat` takes the verdict word, not a `PassVerdict` enum; its one caller closes the set. Follow-up.
- **unix:** round 1 approve-with-changes, then approve.
  - Fixed: help, a second signal, SIGINT 130, the key-error exit documented.
- **tui:** round 1 approve-with-changes, delta approve-with-changes, delta 2 approve-with-changes.
  - Fixed: one line per gate, the cause-specific hint, the `--kubeconfig` in the hint.
  - deliberate: the hint is not yet on its own line, and the path is not shell-quoted. The machine-key path (`~/.config/amont/kube/homelab.yaml`) has no space. Follow-up.
