---
status: active
branch: feat/canonical-tree
canonical: https://github.com/fredericrous/attest/blob/feat/canonical-tree/docs/plans/2026-10-15-plan-only-commits-skip-no-declaration.md
phases: [2]
repos: [attest, amont, decisions]
adrs: [ADR-0011]
---
# Plan-only commits skip with no per-repository declaration (pointer)

The plan lives in the attest repository, which owns the verifiers and the
note format. This repository carries **step 2**: amont 1.56.0, the canonical
tree on the laptop side — `canon <fp>` in the pushed block and under `Kc`, the
canonical gate stamps, the `set planExempt off` opt-out read by one tree-based
policy, and the tests that go with them.

Read the canonical plan for the design, the review panel and the verification
list. Implementation decisions taken here are logged below.

## Decision log

- 2026-10-15 — **Quoted listing, not NUL records:** attest 1.5.0 hashes
  `ls-tree -r --full-tree` with `core.quotePath=true` (one record per line)
  instead of the `-z` listing the plan names, because awks disagree on a NUL
  record separator. amont computes the same text in Rust.
- 2026-10-15 — **Verification observed before the push:** `make check` exit 0 (ends `check-attest-inputs: ok`); clippy `-D warnings` clean; the Rust known-answer test pins attest's shell fingerprint (tree `fd4804a2…` → canon `e942ded7…`).
- 2026-10-15 — **Implementation review:** approve-with-changes, then a delta: unicode-blank `planExempt off` now loud, ambiguous-spec stamp test added, comments fixed. Kept as deliberate: ~8 git spawns per pushed commit with no per-tree cache (timing measured in a later step), and silent read failures on the stamp path (fail-open to "run the gate"), consistent with the existing declared route.
