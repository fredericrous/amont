---
canonical: decisions:docs/plans/2026-10-17-harness-one-owner-per-concept.md
phases: [2]
status: done
---
# The harness has one owner per concept (pointer)

The plan lives in the decisions repository, which owns ADR-0039. This
repository carries Phase 2's amont slice: the writer's half of the quiet-run
active entry moves here from relais `docs/quiet-runs.md`, and every amont
pointer to the entry names amont's own document.

## Outcome

| Check | Expected | Actual |
|---|---|---|
| `git grep -n 'quiet-runs.md' -- ':!CHANGELOG.md' ':!docs/plans'`, each hit read by hand | every hit names amont's document | `.cargo/config.toml:26`, `crates/amont-runtime/src/active.rs:4` and `docs/SUMMARY.md:18`, all amont's |
| Field table and lifetime steps | each in exactly one document | the field table and the writer's steps (1–2, plus the writer's sweep) are here; the watcher's retention steps stay in relais. relais step 1 mentions the writer sweep without specifying it: deliberate |
| Code diff | comments only, apart from the test fix | no non-comment line in `crates/amont-runtime` or `.cargo` |
| amont pre-commit | pass | 28 checks passed on each commit |

## Decision log

- 2026-10-18: `test: release-gate tests get their own dirs` is outside the plan. `release_gates` named its temporary directory by pid and answer count, so two of #361's tests shared a fake verifier. That failed 10 runs out of 10 when they ran together, and 0 out of 10 after the fix. It blocked every amont pre-commit on this machine.
- 2026-10-18: `docs/quiet-runs.md` is declared on each of the ten gate lines of `.github/attest-inputs`, as every `docs/` child is. PR #365's `attestations` job caught the missing declaration; `make attest-inputs` now prints `ok`.

## Implementation review

approve-with-changes, then approve on the Delta, and approve on a second Delta for the `attest-inputs` declaration.
- Fixed: the `active.rs` header now says a reader deletes an entry by its own protocol.
- Kept deliberately: relais's passing mention of the writer sweep.
- Recorded: the test fix.
- Round 1 used 30k tokens in 31 s; the Delta used 31k in 5 s.
