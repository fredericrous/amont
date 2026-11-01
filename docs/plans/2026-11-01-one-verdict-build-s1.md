---
status: active
branch: feat/run-stamp
repos: [amont]
canonical: decisions:docs/plans/2026-11-01-one-verdict-build.md
phases: [S1]
---
# One verdict per tree, build: S1 (pointer)

This file points to the canonical plan in decisions, `docs/plans/2026-11-01-one-verdict-build.md`. This branch builds phase **S1**:

- `amont run --stamp [<gate>…]`: a runner other than the commit hook earns amont's stamp for the tree it verified.
- `amont stamps --json`: the published query that reports each gate's stamp state.
- A new ADR amending ADR-0010.
