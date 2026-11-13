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

- gate_evidence.rs history_in reads only the written ref; it follows WRITE and moved with Phase 4 (11-13).
- Phase 4 (11-13): writers green, `CLUSTER_CONTRACT` = `greengate-cluster-v1` (cluster gates re-run once), key `~/.ssh/greengate-attest` with the old key as fallback until the census; still `active`: Phase 5 (census) remains.
- hooks/audit.rs reads `refs/notes/amont-audit`, a name this plan does not list: left for the programme's census of `amont` names, not Phase 2.
- `forget`/`forget_in` (gate stamps and attest) delete the refs of both name sets, so an uninstall leaves no green note that still vouches.

## Implementation review

Phase 4 (Phase 2's review is in the history of this file):
- implementation-review: approve, then approve on both deltas. Fixed: one `scratch_cleanup` helper, doctor exit-1 comment, `key_move_hint_in` doc; both name candidates in the tests were ruled fine (test-helper convention).
- backend: approve-with-changes ×2, then approve. Fixed: the CHANGELOG notes cluster re-run, lost pending runs and hidden legacy stamps; doctor lists `attest-local/amont-*`; pushes write no `amont-*` ref (tests); each sign failure is named.
- lang:rust: approve-with-changes, then approve. Fixed: full-ref test with `WRITE == GREEN`, `PendingRecord`, `KEY_CONFIG` read in the repository, `forget` calls `forget_in`.
- unix, architect: approve-with-changes, then approve. Fixed: doctor exit-1 docs; legacy provenance payloads are kept and named, never signed, so no `amont` note is written after the switch.
