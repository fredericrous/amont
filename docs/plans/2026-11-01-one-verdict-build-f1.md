---
status: active
branch: fix/inert-out-of-scope
repos: [amont]
canonical: decisions:docs/plans/2026-11-01-one-verdict-build.md
phases: [F1]
---
# One verdict per tree, build: F1 (pointer)

The canonical plan is in decisions at `docs/plans/2026-11-01-one-verdict-build.md`. This branch is phase **F1**: a pre-commit gate with nothing in scope returns `Inert`, not `Passed`, so a plan-only commit no longer stamps a gate that never ran. That stamp let a push attest a deferred `cargo-test` (programme one-verdict-per-tree, t07).
