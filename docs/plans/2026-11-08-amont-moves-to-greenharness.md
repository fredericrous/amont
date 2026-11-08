---
canonical: fredericrous/decisions docs/plans/2026-11-08-amont-moves-to-greenharness.md
phases: [4, 5]
status: active
branch: fix/repo-moved-greengate
repos: [amont]
---
# amont and amont-agent move to greenharness (pointer)

This repository's slice of the fleet plan: Phase 4 (its strings name the new
home) and Phase 5 (release 2.3.1 from it).

## Code review
- amont: backend, unix (tui left out: no screen or output change beyond one URL in help text; lang:rust left out: one string literal in `main.rs` and a manifest field)
