---
id: ADR-0008
status: accepted
decisions:
  - key: hooks.liveness
    scope: hook-binary
    choice: Stuck means silent AND under 0.1 core of measured CPU in the check's process tree for the whole silence budget; silence alone where CPU cannot be measured
    first: true
    reason: A test runner that prints only its summary looked hung to a silence-only rule and was killed with every test passing
---
# 0008 — busy is not stuck

Plan: [`docs/plans/2026-09-29-busy-is-not-stuck.md`](../docs/plans/2026-09-29-busy-is-not-stuck.md).

## Context

`amont.idleTimeout` (default 120 s) kills a check that prints nothing for that
long. The rule rested on a premise: a hang is silent, a slow suite talks —
`cargo test` prints a line per test. vitest does not: without a terminal its
default reporter prints the summary and nothing before it. duro-app's pre-push
suite (169 files, 4-7 minutes, several cores busy) was killed twice on
2026-09-29 with all 1553 tests passing, and passed only once the budget was
raised to ten minutes for that one run. Raising budgets per repository gives up
the fast hang detection the clock exists for.

## Decision

A check is stuck when it has printed nothing **and** its process tree showed
insufficient CPU activity — under 0.1 of one core, measured — for the whole
silence budget. It is a heuristic answering "is anything working?", not CPU
accounting, and every figure amont shows from it is marked approximate.

- **What is measured.** The CPU of the child and its descendants, each process
  counted with the children it has already reaped, so work done by short-lived
  forks is not lost when they exit (a sum over live processes measurably goes
  *down* while a fork-per-file suite works). Processes seen in the tree once
  keep counting after they are reparented away. CPU already credited while a
  child was alive is subtracted when its parent reaps it.
- **How.** Linux reads `/proc/<pid>/stat`; macOS calls libproc and converts
  mach time with the timebase. No process is spawned and no crate is added —
  decisions:ADR-0020 (`change.dependency-bar`) holds.
- **Where it runs.** On its own thread, with hard limits per snapshot
  (process count, retries, a wall deadline). The loop that enforces
  `amont.timeout` only reads the result, so nothing the sampler does can delay
  the ceiling.
- **When it cannot measure** — Windows, any other OS, a failed or incomplete
  snapshot, or `amont.idleCpuCredit false` — the silence-only rule applies and
  the messages say CPU was not measured. They never claim "no CPU" without a
  measurement behind it.

## Consequences

- A silent tool doing real work runs on to the ceiling (`amont.timeout`,
  one hour) instead of being killed at two minutes.
- **A silent spinning hang** — a busy loop, a polling file watcher, a spinning
  orphan the sampler had already seen — now answers to the ceiling too. That is
  the price, accepted: such hangs are rarer than quiet test runners, and the
  ceiling still ends them. The ceiling message names this case and
  `amont.idleCpuCredit false`.
- **Not counted:** work done by daemons outside the tree (build daemons,
  container engines), as before; and a worker reparented before the sampler
  first saw it. Sampling earlier narrows that race but cannot close it.
- **Reaping heuristic.** A child that lived less than one sampling interval
  and was reaped late is credited when it is reaped: at most one interval of
  its work, possibly in a later window.
- **Windows parity.** `platform.windows-parity` (ADR-0001) keeps one test
  suite; this behaviour is unix-only and its tests are `cfg(unix)`, the same
  stated divergence as the hook-file link refusal. Windows keeps the
  silence-only rule.
