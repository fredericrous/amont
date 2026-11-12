---
canonical: homelab:docs/plans/2026-11-11-attest-signer.md
phases: [3]
status: active
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
