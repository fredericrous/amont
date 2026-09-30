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
