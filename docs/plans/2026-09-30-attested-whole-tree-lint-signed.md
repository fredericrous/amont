---
status: active
branch: feat/tree-lint-attest
canonical: https://github.com/fredericrous/decisions/blob/feat/attested-skip/docs/plans/2026-09-30-attested-whole-tree-lint-signed.md
phases: [2]
repos: [decisions, amont]
adrs: [ADR-0024]
---
# Attested whole-tree lint, signed only when warm (pointer)

The plan lives in the decisions repository, which owns ADR-0024
(`ci.attested-skip`). This repository carries **Phase 2**: repository-declared
`tree` gates in `amont.conf`, the `tree-parity` check, the side-car runner
with an enforced slack, namespaced caches with a warm-up, the exact-tree
push reader, rehearsal integration, skew withholding and evidence.

Read the canonical plan for the design, the review panel and the
verification list. Implementation decisions taken here are logged below.

## Decision log

- 2026-09-30 — **Options grammar:** options after `attest` are lowercase
  `key=value` tokens. An uppercase `NAME=value` begins the command, so an
  env-prefixed command stays expressible.
- 2026-09-30 — **Inherited `shell: bash` accepted for a simple command**
  (the person's decision). The pilots' workflows set
  `defaults: run: shell: bash` workflow-wide for `pipefail`, and the reviewed
  fail-closed rule rejected every inherited default. tree-parity now accepts
  exactly that block when the gated command has no pipe, list, redirection or
  substitution. Inherited `env:`, `working-directory` and any other shell stay
  rejected. Checked on both pilots' real workflows: all gated steps pass, and
  a drifted `run:` is caught with its line.
- 2026-09-30 — **tree-parity is `Reach::Convention`, not Safety.** It fires
  only in a repository that declares tree gates. The safety net stays the
  low-false-positive set.
- 2026-09-30 — **`.env` in application-landscape's allow-list** (the
  person's decision). The live checkout holds a gitignored `.env`, and
  `snapshotCarry .env` copies it into rehearsal snapshots, so without it no
  commit or rehearsal there could stamp. The allow-list only decides whether
  the file's presence withholds a stamp. Nothing reads, hashes or uploads
  it: the namespace hashes staged files only, and the attestation carries
  gate names and a tree id. Kept per repository, not a default: a linter
  whose verdict depended on `.env` would pass here and not in CI, and
  eslint, prettier and ruff do not read it.
