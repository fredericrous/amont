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

## Implementation review
- implementation-review: approve (delta); fixed: `result` → `bump_run` (names.reveal-intent), book `site-url` → `/greengate/`; the Phase 2 results and the shim deferral are in the canonical plan's Decision log; 62k/48 s + 24k/8 s.
- backend: approve (delta); fixed: `site-url`, release.yaml comment now names the homepage rewrite; 61k/107 s + 23k/20 s.
- unix: approve (delta); fixed: `site-url`; deliberate: the script's `assert` refusals (`python -O`) and its usage exit code, pre-existing and unchanged, the release calls it without `-O`; 66k/47 s + 24k/21 s.

## Verification
- input: `scripts/test_bump_tap.py` on the new and the old `bump-tap.py` → expected: pass, then fail → actual: 4/4 OK; on the old script 3 of 4 fail.
- input: `cargo test -p amont --test it help_exits_zero` → expected: `--help` and `-h` name `https://greenharness.github.io/greengate/` → actual: ok (24.8 s).
- input: grep for `fredericrous/amont`, `fredericrous/amont-agent`, `fredericrous.github.io` → expected: history, the hook shims, the bump-tap patterns and tests → actual: exactly those (plus one test comment).
