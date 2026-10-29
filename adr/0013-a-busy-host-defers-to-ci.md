---
id: ADR-0013
status: accepted
decisions:
  - key: hooks.host-concurrency
    scope: hook-binary
    choice: Checks that compile or execute the product, built-in or declared, take one of cores/4 host-wide slots at their first tool spawn, queued with their clocks paused; a declared check is Heavy when it moves a built-in Heavy gate (same short name) or its line carries the `heavy` flag; what a slot that never frees means is the repository's busy policy
    replaces: [ADR-0009]
    reason: The declared gates were never queued, and they are the heaviest in practice (amont's and relais's own `cargo-test`, relais's `plugin-test`); unqueued, they thrashed the host until the plugin suite failed on its own 5 s timeouts four commits in a row
  - key: hooks.busy-policy
    scope: hook-binary
    choice: "`set onBusy wait|defer` in amont.conf, default `wait` (after amont.timeout the check runs unqueued, with a note); under `defer`, a Heavy check whose slot does not free within `set slotWait` (default 60 s) while this amont holds no slot is Deferred: not run, never blocking, no stamp, never attested, so CI runs it; the push twin of a gate deferred at commit defers too; `AMONT_ON_BUSY` overrides the file for one run"
    first: true
    reason: On a host full of other sessions' gates a local suite either waits for minutes or fails on timeouts that are not the code's; where CI already runs every gate the attestation does not name, moving the gate there, out loud, is cheaper and keeps the gate required
---
# 0013 — a busy host defers heavy gates to CI

Plan: [`docs/plans/2026-10-28-busy-host-defers-to-ci.md`](../docs/plans/2026-10-28-busy-host-defers-to-ci.md).
Fleet records unchanged: `decisions:ADR-0037` (`ci.attested-skip`) already
runs, in CI, every gate the signed note does not name.

## Context

ADR-0009 queues Heavy checks on host-wide slots, but Heavy was a list of
built-in names. A gate declared in `amont.conf` never queued, and the
declared gates are the heavy ones: amont moves `cargo-test` to pre-commit by
declaring it, and relais declares `cargo-test` and `plugin-test`. With
several agent sessions committing at once, the host reached a load of 40 to
400, and the relais plugin suite failed on its 5 s test timeouts on commits
that touched no plugin behaviour. The way past was a hand-approved skip.

ADR-0009 also says a slot that never frees runs the check anyway: the queue
was a courtesy, not a gate. That is still right where nothing else will run
the gate. Where CI runs it, waiting an hour or failing on contention buys
nothing.

## Decision

**Weight.** A declared check is Heavy when its short name is a built-in
Heavy gate's (it moves that gate), or when its line carries the `heavy` flag.
The slot is still taken lazily, at the first tool spawn, so a check with
nothing to do never waits.

**Busy policy.** A repository declares `set onBusy defer` only where its CI
runs each Heavy gate. Under `defer`:

- a Heavy check whose slot does not free within `slotWait` is Deferred, but
  only when this amont holds no slot: a commit never defers behind its own
  checks;
- a Deferred check does not run, never blocks, writes no stamp and is never
  attested, so CI runs it;
- the commit records the deferral; pre-push reads it through the per-commit
  pairing, and the push twin defers too (a `PairVerdict::Deferred`, never
  `Gated`), because running the suite at push brings back the idle
  connection reset that moved it to commit;
- once a serial push stage defers one gate, the rest defer without waiting.

`AMONT_ON_BUSY=wait|defer` overrides the file for one run.

**The `run` lines.** Until now no skip decision read a `run` evidence line,
so a forged one could not skip a check. A `deferred` line is now read, and
it can only move a gate to CI, never stamp it: a forged line makes CI run
more, never less.

## Why

- The gate stays required: it runs in CI, which `docs/ci.md` already names
  the backstop, and the hook says so in one line with the command to run it
  now.
- Declaring the policy, rather than inferring it from the remote, keeps the
  promise "CI runs this" reviewable in the file that makes it.

## Consequences

- Under `defer`, a busy host shifts test failures from the commit to the
  pull request. The plan measures how often for 14 days and rolls a
  repository back to `wait` when more than half its Heavy gates defer.
- Load from outside the hooks (agent builds, relais workers) holds no slot
  and causes no deferral; that is a non-goal of this record.
- An amont older than this record reads `heavy` as part of the command, so a
  repository adds the flag only once every amont that reads its file is at
  this release.
