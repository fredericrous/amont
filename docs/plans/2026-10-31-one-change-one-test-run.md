---
status: active
branch: feat/slot-holder-width
repos: [relais, amont]
canonical: relais:docs/plans/2026-10-31-one-change-one-test-run.md
phases: [A2]
adrs: [amont:ADR-0013, amont:ADR-0014]
---
# One change, one test run (pointer)

The canonical plan lives in relais at `docs/plans/2026-10-31-one-change-one-test-run.md` (relais#363, phase A1). This branch is phase **A2**: amont bounds the width of every heavy-slot holder (`MAKEFLAGS`, `CARGO_BUILD_JOBS`, `RUST_TEST_THREADS`, `NEXTEST_TEST_THREADS`).

## Verification record (A2)

- Targeted tests:
  - `amont-slots`: 20 passed, covering `parallelism`, `width_env` precedence, the `MAKEFLAGS` append, `bound_width`'s own-env-first rule, a waiter's `jobs`, and the record round-trip with and without `jobs`.
  - `amont-runtime` `host_slots`: 7 passed, including that a nested or light check sets no width.
  - `slot_cmd`: 2 passed.
  - `it slot_run`: 7 passed, covering weight 1 and 3, a preset kept, an empty preset replaced, `MAKEFLAGS` append, an unqueued run after a timeout, and `amont slots` showing `jobs`.
- Piloted on the built binary (12 cores, `AMONT_HOST_SLOTS=6`):

  | Input | Expected | Actual |
  |---|---|---|
  | `slot run --weight 1 -- env` | 2 | `CARGO_BUILD_JOBS=2`, `RUST_TEST_THREADS=2`, `NEXTEST_TEST_THREADS=2`, `MAKEFLAGS=-j2` |
  | `--weight 2` | 4 | all four keys at 4 |
  | preset `CARGO_BUILD_JOBS=9` | kept | 9 |
  | `AMONT_HOST_SLOTS=0` | nothing set | 0 keys |
  | `amont slots` while held | shows `jobs` | `… pid 69804  jobs 2` |
- After `amont install` (A2 release): check the env of a pre-push `cargo-test` child with `ps eww`, and take the one-week before/after measurement.

## Decision log

- 2026-10-31: the cost check (cap-based vs held-based width) was not timed: the host never goes idle (load 40–160). The person chose "Ship cap-based now". Rollback rule: revert A2 if a lone pre-push `cargo-test` is more than 25% slower than before.
- 2026-10-31: A2 was implemented directly, not through relais: the person asked to stop using relais this session because the change is urgent for CPU load.
