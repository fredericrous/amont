---
id: ADR-0021
status: accepted
decisions:
  - key: hooks.host-concurrency
    scope: hook-binary
    choice: "Every heavy job on the host (greengate's gates, relais's verification commands, and any command run through `greengate slot run`) takes host slots through one std-only crate, `greenharness-slots`. The crate counts physical cores (macOS `hw.physicalcpu`, Linux sysfs `core_cpus_list`, then `/proc/cpuinfo`), never more than the logical count, and that one count sizes the cap, the width and the load test. It also resolves the two host keys for every client, `greengate.hostCores` (1..=1024) and `greengate.hostSlots` (0..=64), from `GREENGATE_HOST_CORES` / `GREENGATE_HOST_SLOTS`, then the last `--global` or `--system` row, then the detected default, returning warnings for the caller to print. Automatic sizing (`greengate.hostSlots` unset): at most max(1, cores/2) slots; a free slot is taken only while the 1-minute load is below the core count; when no slot is held, the first waiter is always admitted, whatever its weight; at most one new holder every 60 s. Waiters are served in ticket order, under one admission lock taken without blocking inside each caller's own deadline. A holder records who it is in `holders/`. Only the process that took a slot holds it: slot descriptors are close-on-exec. `greengate.hostSlots N` fixes the count with no load test, and `0` turns slots off"
    replaces: [ADR-0014]
    reason: "On a 6-core, 12-thread laptop the logical count gave 6 slots of 2 jobs, 12 compile and test jobs on 6 cores, at a measured load of 30–55; and relais sized the same queue from its own copy of the count and of `greengate.hostSlots`, so the two tools could disagree"
---
# 0021 — host slots count physical cores

Plan: [`docs/plans/2026-11-10-host-slots-physical-cores.md`](../docs/plans/2026-11-10-host-slots-physical-cores.md).
Replaces ADR-0014's `hooks.host-concurrency`; everything ADR-0014 decided
stays, except the core count and where the host keys are read.

## Context

ADR-0014 sized the slots from `std::thread::available_parallelism()`, the
logical count. On an i7-9750H (6 physical cores, 12 hardware threads) that
was an automatic cap of 6, a width of 2 per holder, and admission while the
load was below 12: 12 jobs on 6 cores. The load was measured at 30–55 on
2026-11-10, with 5 of 6 slots held, each 2 wide.

relais linked the crate for the protocol but computed its commands' width
from its own logical count and its own coordinator cap (4 wide here), and
read the slot count from its own `host_slots` setting, which a person had to
keep equal to `greengate.hostSlots` by hand.

## Decision

- **One count.** `greenharness_slots::cores()` is the physical core count,
  capped at the logical count, which also honours a cgroup or affinity limit.
  `logical_cores()` keeps the old count for what still wants it (relais's
  coordinator cap, relais ADR-0010).
- **One sizing for every client.** `greenharness_slots::host::resolve` reads
  `greengate.hostCores` and `greengate.hostSlots` with the precedence greengate's
  `host_integer_or` already had. It is pure: greengate passes its cached
  `--show-scope` rows, so it starts no extra git process, and
  `from_host(detect_cores())` makes one cached call for a client with no
  rows of its own. git's exit 1 (no `greengate.*` key at all) is no rows,
  never "git cannot report". It returns
  warnings, and each caller prints them its own way.
- **The count travels.** `Machine { cores }` reaches the cap, the width, the
  admission load test and `greengate slots`, so an override moves all four.
- **A waiter says why it waits:** another waiter ahead, slots full, the load
  at or above the core count, or the one-per-minute ramp.

## Why

- Hyper-threads share a core's execution units; two compile jobs on one core
  run about as fast as one, so counting threads doubled the queue's width
  without adding throughput.
- `decisions:ADR-0039` (`harness.shared-code`): a contract has one
  implementation. relais keeping its own count and its own copy of the slot
  key was a second one.
- `decisions:ADR-0047` (`harness.standalone-tools`): relais still links only
  the crate. With no greengate installed, no keys and no working `git`,
  `from_host(detect_cores())` returns the detected sizing, which is relais's
  behaviour today.

## Consequences

- On an SMT host there are half as many automatic slots, each as wide as
  before: here 3 holders × 2 jobs = 6. A second holder needs load below 6,
  not 12. At the load measured (30–55) neither test admitted a second holder,
  so the visible change is the cap and the width.
- Hosts without SMT (Apple Silicon, most CI runners) are unchanged, except
  that `greengate slots` now names threads only where they differ from cores.
- `waits.jsonl` lines carry `"cores"`, and `"cap"` where the sizing is known,
  so lines from before and after a sizing change can be told apart.
- `greengate.hostCores N` (or `GREENGATE_HOST_CORES`) restores the old sizing
  on a machine where it suits better: `greengate.hostCores 12` on this laptop.
