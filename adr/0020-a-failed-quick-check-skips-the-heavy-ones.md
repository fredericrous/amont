---
id: ADR-0020
status: accepted
decisions:
  - key: hooks.pre-commit-fail-fast
    scope: pre-commit
    choice: "Pre-commit runs its Light checks first, concurrently, and starts its Heavy checks (built-ins in `host_slots::HEAVY`, declared `heavy` and moved gates) only when no Light check failed at a blocking severity; a Heavy check that would have judged something (its scope touches the staged files and the repository opted in) is then `Skipped`, never stamped or attested, and said with one `amont run <name>` per check. `amont run` follows the same waves. `AMONT_FAIL_FAST`, then `amont.failFast`, then `set failFast` turn it off"
    first: true
    reason: "pre-commit ran every check at once and waited for all of them, so relais #394's `cargo test` ran 13 minutes for a commit `cargo fmt` had refused after 4 seconds, and the corrected commit ran it again (decisions programme proportional-ship, t01)"
---
# 0020 — a failed quick check skips the heavy ones

Plan: [`docs/plans/2026-11-06-a-failed-quick-check-skips-heavy.md`](../docs/plans/2026-11-06-a-failed-quick-check-skips-heavy.md).

## Context

`run_stage_traced` ran every pre-commit check concurrently and waited for
all of them. pre-push was already serial and stopped at the first failure;
pre-commit did not. A format check that blocked the commit in seconds left
clippy and the test suite running for minutes, on a commit already refused.

## Decision

**Two waves, by weight.** The Light checks run first. If none of them failed
at a blocking severity, the Heavy checks run, as before, each taking its host
slot. Otherwise every Heavy check that would have judged something is
`Skipped`. The rest are `Inert`, so a repository with no Rust never reads
"skipped clippy".

**Waves, not cancellation.** The check runner kills only a tool's direct
child (`wait_within`), and check commands have no process group of their
own. Stopping a running `cargo test` would orphan its test binaries. A wave
that never starts needs no kill. Cancelling a running Heavy check when
another Heavy one fails is left until data shows it matters.

**Said, never silent.** The skipped checks get one roll-up line in every
output mode, quiet included, before the blame list, with the command that
runs each one now.

**A fixer's repair comes first.** A Light fixer has restaged its repair when
the Heavy wave starts, so the Heavy checks judge, and are stamped on, the
repaired index. With one wave they could judge the content the fixer was
still rewriting.

## Consequences

- A passing commit's Heavy checks start once its slowest Light check ends
  (about 4 s in relais #394). `<common-dir>/amont-waves` records that wait
  and what was skipped, for the 14-day measure.
- `amont.failFast false`, or `AMONT_FAIL_FAST=false` for one run, restores
  one wave.
- A Light check whose thread died reads `Failed` and, at a blocking
  severity, skips the Heavy wave.
