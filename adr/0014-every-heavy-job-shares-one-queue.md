---
id: ADR-0014
status: accepted
decisions:
  - key: hooks.host-concurrency
    scope: hook-binary
    choice: "Every heavy job on the host (amont's gates, relais's verification commands, and any command run through `amont slot run`) takes host slots through one std-only crate, `amont-slots`. Automatic sizing (`amont.hostSlots` unset): at most max(1, cores/2) slots; a free slot is taken only while the 1-minute load is below the core count; when no slot is held, the first waiter is always admitted, whatever its weight; at most one new holder every 60 s. Waiters are served in ticket order, under one admission lock taken without blocking inside each caller's own deadline. A holder records who it is in `holders/`. Only the process that took a slot holds it: slot descriptors are close-on-exec. `amont.hostSlots N` fixes the count with no load test, and `0` turns slots off"
    replaces: [ADR-0013]
    reason: Only amont's hooks queued, with relais holding a copy of the protocol and agents' own builds never queueing, so outside load filled the host past every queue; a fixed cores/4 ignored whether the machine was idle or saturated, and a waiter could not see whom it waited behind
---
# 0014 — every heavy job on the host shares one queue

Plan: [`docs/plans/2026-10-29-every-heavy-job-shares-one-queue.md`](../docs/plans/2026-10-29-every-heavy-job-shares-one-queue.md).
Fleet records: `decisions:ADR-0039` (`harness.shared-code`: one
implementation of a shared contract) and `decisions:ADR-0047`
(`harness.standalone-tools`).

## Context

ADR-0013 lets a busy host defer amont's heavy gates to CI, but only amont's
hooks joined the queue. relais took the same slot files through its own copy
of the protocol, opt-in per machine. An agent's `cargo test` or
`xcodebuild` in Bash queued nowhere. The load on the 12-core development Mac
reached 240–600, and nearly all of it came from outside the queue.

## Decision

**One implementation.** `amont-slots` is a std-only crate, published with
amont like `amont-quiet`. It owns the slot directory, the lock, the guard,
the admission rule, the ticket queue and the holder record. amont-runtime
builds its busy policy on top of it, and relais links it in place of its
copy. The contract is written down in `docs/`, beside the crate, in the
same change as the crate.

**Sizing follows the machine.** With `amont.hostSlots` unset:
- the cap is max(1, cores/2);
- a free slot is taken only while the 1-minute load is below the core count;
- the first waiter on an empty host is always admitted, whatever its weight;
- at most one new holder is admitted every 60 s, the load average's own lag.

One condition: before relais adopts the crate, the load with a single
`cargo test` holder is measured on an idle host. If that one holder alone
reaches the core count, this rule never admits a second holder. The
threshold then returns to the person, with the measurement, before it
ships.

**Fair and visible.** Waiters take tickets in order under one admission lock.
That lock is taken without blocking, so a hook's busy policy still defers
within its own deadline. A holder writes `holders/slot-N.who`: tool,
repository, command, session and start time. A waiter says whom it waits
behind. `amont slots` lists holders and waiters, and every acquisition
appends a line to `waits.jsonl`.

**Anyone can queue.** `amont slot run [--weight N] [--max-wait S] -- <cmd>`
works as follows:
- it runs the command in a slot, with `AMONT_HOST_SLOT=held` set;
- after `--max-wait` (default 90 s) it exits 75 without launching anything,
  and `--on-timeout=run` is the explicit way to run unqueued;
- the wrapper alone holds the slot, and frees it when the command's process
  group ends, so a daemon a build leaves behind never holds it.

## Why

- One protocol in two copies drifts, and `harness.shared-code` forbids it.
- A queue that only some jobs join protects nobody: the jobs outside it fill
  the machine.
- The 60 s ramp exists because the 1-minute load is the signal: within the
  first 10 s it shows only about 15% of a new holder's load.

## Consequences

- amont up to 1.61 and today's relais still take slot files with no ticket
  and no load test, so they jump the queue until they are upgraded.
  `amont slots` shows them as `unknown (old client)`.
- Killing only the wrapper while its child keeps building frees the slot
  early. This is accepted: Claude Code kills the whole process tree.
- relais's own coordinator cap stays. It orders relais runs among
  themselves, while the slots are for the whole machine.
- `harness.standalone-tools`: relais links the library, not the amont
  binary, so it still runs with amont absent. amont-agent's advice names
  `amont slot run` only when an `amont` that has it is on PATH; when none is,
  it reports a gap, never a pass.
