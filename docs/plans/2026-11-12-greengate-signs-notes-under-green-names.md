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

## Decision log

- gate_evidence.rs history_in reads only the written ref; it follows WRITE and moves with Phase 4 (`holds-until` in the code; implementation review, 11-12).
- hooks/audit.rs reads `refs/notes/amont-audit`, a name this plan does not list: left for the programme's census of `amont` names, not Phase 2.
- `forget`/`forget_in` (gate stamps and attest) delete the refs of both name sets, so an uninstall leaves no green note that still vouches.

## Implementation review

- implementation-review: approve-with-changes ×2, then approve (c9ef4aaa). Fixed: 7 discards given reasons, 4 single-letter names, `holds-until` marks. Deliberate: the `verified under` test rebuilds its wording.
- backend: approve-with-changes ×2, then approve. Fixed: both-set `forget` with `done` refs, `is_publishable` tests. `history_in` deferred to Phase 4.
- lang:rust: approve-with-changes, then approve. Fixed: `pub(crate)` name sets, comments, `read_in` doc. Deliberate: `verified_lines` returns a Vec (one caller).
- unix: approve-with-changes, then approve. The `stamps` path already prints `verified under`; the CHANGELOG names the stable wording.
- architect: approve-with-changes ×2, then approve. `READ_NAMES` copy commented, removed at the census. Deliberate: test helpers shared, as the crate already does.
