---
status: done
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
- 2026-10-15 — **Released and proven.** attest 1.5.0 (`v1` now moves with it) and amont 1.56.0 are out; pilots on social-planner and sre-agent skipped typecheck, test and pytest on a plans-only commit with only the canonical route available, and re-ran them for a new plan file; the 27-consumer audit found no skipped gate that reads plan content; canon cost measured at about 100 ms of shell plus 45 ms for `amont.conf` on a 3,000-file repository at load 12. Records: `decisions:ADR-0035` and amont ADR-0012. The tag is on the version-bump commit, which differs from the pilot-pinned sha by `Cargo.toml` and `Cargo.lock` only.
