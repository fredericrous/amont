---
canonical: fredericrous/decisions:docs/plans/2026-11-12-greengate-signs-notes-under-green-names.md
phases: [2, 4, 5]
status: active
branch: feat/greengate-notes-names
---
# greengate signs its notes under green names (pointer)

This repository's slice of the decisions plan (ADR-0068). The plan, its
review panel and its Decision log are canonical in the decisions
repository; git-attest's Phase 1 shipped as git-attest 1.9.0.

- **Phase 2 (reads only):** git-attest pinned to `1.9`; `gate_stamp.rs`
  reads `refs/notes/greengate-gate(-*)` first and the `amont-gate(-*)`
  refs only when the green ref has nothing for the key, a note's header
  matching its ref; `attest.rs` and `attest_inputs.rs` read both name sets
  (`git_attest::format::{LEGACY, GREEN}`), the header deciding namespace and
  domain, never mixed, and print `verified under <ns>`. Writers stay
  `amont-*`.
- **Phase 4:** writers switch to the green names (`names.rs`,
  `gate_stamp.rs`, `attest.rs`, `cluster/detached/sign.rs`, `migrate.rs`).
- **Phase 5:** the census removes the old reads.

## Code review
- amont: backend, lang:rust, unix, architect (lang:typescript left out: no TypeScript here; tui left out: one doctor line)
