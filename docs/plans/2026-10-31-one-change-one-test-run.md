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
