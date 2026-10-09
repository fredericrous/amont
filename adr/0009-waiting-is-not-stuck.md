---
id: ADR-0009
status: accepted
decisions:
  - key: hooks.liveness
    scope: hook-binary
    choice: Stuck means silent, not in a declared lock wait, and under 0.1 core of measured CPU for the whole load-scaled silence budget; a bounded extended budget where measurement is attempted but incomplete; silence alone where it is not attempted
    replaces: [ADR-0008]
    reason: A cargo lock wait prints one line and uses no CPU, so the busy-is-not-stuck rule killed a healthy clippy at two minutes on a loaded machine
  - key: hooks.host-concurrency
    scope: hook-binary
    choice: Checks that compile or execute the product take one of cores/4 host-wide slots before they run, queued with their clocks paused; a slot that never frees runs the check unqueued with a note
    first: true
    reason: Several worktrees and agents on one machine thrash the same cores and the same cargo lock, and the kill clocks counted that thrash as the check's own time
  - key: hooks.clippy-scope
    scope: pre-commit
    choice: The packages owning the staged files, by running cargo in each package root without --workspace; the whole workspace when a manifest, lockfile or toolchain pin is staged
    first: true
    reason: A cold --workspace clippy per worktree is the longest and most lock-hungry pre-commit check, and CI runs the workspace clippy on every pull request anyway
---
# 0009 — waiting is not stuck

Plan: [`docs/plans/2026-10-09-waiting-is-not-stuck.md`](../docs/plans/2026-10-09-waiting-is-not-stuck.md).

## Context

ADR-0008 made a silent check stuck only when its process tree also showed
under 0.1 core of CPU for the whole silence budget. On 2026-10-09, on a
machine running several worktrees and relais workers, the pre-commit gate
of a large Rust repository was killed at that budget with this in its log:

```
… clippy still running: 1m00s, last output 58s ago, CPU idle 58s (killed after 2m00s …)
    Blocking waiting for file lock on package cache
    Blocking waiting for file lock on build directory
```

Clippy was queued behind another cargo holding the lock. A lock wait prints
one line and then does nothing measurable, which is exactly what ADR-0008
calls stuck. Load made the holder slower, so the wait outlived the budget;
the misclassification was the defect. Two things compounded it: one
incomplete CPU snapshot dropped every credit and fell back to the silence
rule, and every worktree's clippy was a cold `--workspace` check contending
for the same locks.

## Decision

**Liveness** (`hooks.liveness`, replacing ADR-0008's choice). A check is
stuck when it is silent, not in a declared wait, and under 0.1 core of
measured CPU for the whole silence budget, where:

- A **declared wait** is a line a tool prints on stderr to say it is
  blocked on a lock: cargo's `Blocking waiting for file lock on <what>`,
  uv's `Waiting to acquire <kind> lock for \`<path>\``. The silence clock
  does not run while the last stderr line is such a marker; the wait has
  its own budget, `amont.lockWait` (ten minutes), bounded by the ceiling.
  Only exact markers from the real binaries pause the clock; a line that
  merely looks like a wait earns one retry after a silence kill, inside
  the same ceiling, and only for a built-in check.
- The **silence budget stretches with the host's load**: `amont.idleTimeout`
  times `clamp(load1 / cores, 1, amont.idleLoadScale)`, capped at four by
  default. A false kill costs a parked commit; a slower verdict on a real
  hang costs minutes.
- **Measurement attempted but incomplete** (a partial snapshot, a stale
  rate, the window before the first sample) applies the extended budget,
  `idleTimeout × idleLoadScale`, and never the bare ceiling: with
  `amont.timeout 0` a check is still bounded. A partial snapshot no longer
  drops the baseline; the next complete one spans the gap, may count as
  busy, and never counts as measured idle.
- **Not attempted** (Windows, `amont.idleCpuCredit false`) keeps the
  silence-only rule, and the messages say CPU was not measured.

**Host concurrency** (`hooks.host-concurrency`). Checks that compile or
execute the product are Heavy. A Heavy check takes one of `amont.hostSlots`
slots (cores/4, at least one) before it runs: `flock` on a file under a
fixed per-user directory, so every worktree, session and agent on the host
shares one queue. While queued, the check's clocks have not started and the
display says so. The slot count and the load cap are host keys, read from
global or system git config only, so two repositories cannot disagree. A
nested amont, run by a check that holds a slot, does not queue. A slot that
never frees within `amont.timeout` runs the check unqueued, with a note:
the queue is a courtesy to the machine, not a gate on the code.

**Clippy scope** (`hooks.clippy-scope`). Pre-commit clippy runs in each
package root that owns a staged `.rs` file without `--workspace`, so cargo
judges that package. When a manifest, lockfile, `clippy.toml` or toolchain
pin is staged, or under `--all-files`, the whole workspace is judged.

## Consequences

- A healthy check waiting on a lock is no longer killed; a check whose
  lock holder never finishes is killed at `amont.lockWait` with a message
  naming the lock and the likely holders.
- Worst case for a genuinely stuck, silent tool rises from 120 s to at most
  480 s under full load, or 600 s in a declared wait, against a ceiling of
  one hour that does not change.
- A Heavy check can wait for a slot for up to `amont.timeout` on top of its
  own run, and a serial pre-push adds those waits up. The display names the
  queue and the key.
- Lints in a crate that depends on a staged crate are caught by CI's
  workspace clippy, not by pre-commit. Accepted: pre-push `cargo test`
  still compiles the workspace.
- Fairness in the queue is polling, not first-come; the lock holder is not
  named. Both are deferred with a stated ceiling in the code.
- **Windows parity.** `platform.windows-parity` (ADR-0001) keeps one test
  suite; host slots and load scaling are unix-only, their tests `cfg(unix)`,
  the same stated divergence as ADR-0008.
