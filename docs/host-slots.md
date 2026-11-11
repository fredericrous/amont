# Host slots

The contract of the `greenharness-slots` crate (greengate ADR-0014, decisions
ADR-0049). A test suite, a build or a type checker already uses every core
on its own, so a heavy job takes a host slot before it runs. Every heavy job
on the machine takes its slot through this one queue: greengate's git-hook gates,
relais's verification commands, and any command run through
`greengate slot run`. relais links the crate; it does not carry a copy.

## The directory

`GREENHARNESS_SLOT_DIR`, else `$XDG_RUNTIME_DIR/greenharness-slots` on Linux, else
`/tmp/greenharness-slots-<uid>`. It is a fixed path, never `$TMPDIR`, which macOS
and agent sandboxes set per session. It must be a real directory owned by
this user with mode 0700. Otherwise slots are off for the job, and it says
why.

| Path | What it is |
|---|---|
| `slot-0` … `slot-{cap-1}` | the slots. A slot is held while a process holds `flock(LOCK_EX)` on its file. |
| `admission` | the one admission lock. Every admission decision is made under it, taken with `LOCK_NB` and retried inside the caller's own deadline, so a stopped holder of it never holds a hook past its own deadline. |
| `admitted-at` | the time of the last admission, in ms since the epoch, for the ramp. |
| `queue/<nanos>-<pid>-<n>` | a waiter's ticket, flocked for as long as it waits. It is created hidden (`.new-…`), locked, then renamed into place, so a visible ticket is never unlocked. A visible ticket whose lock is free belongs to a waiter that died, and is removed. |
| `holders/slot-N.who` | who holds slot N, as a JSON object (`pid`, `tool`, `repo`, `command`, `session`, `started_ms`, and `jobs` when the writer knows it). It is written after the slot is won and removed before it is released. A reader trusts it only while `pid` is alive. A hidden `.slot-N.who.<pid>.tmp` beside it is a rename in flight; when a slot is granted, those whose `pid` is dead are removed, as dead queue tickets are. It lives in a subdirectory so an old client counting `slot-*` files never mistakes it for a slot. |
| `waits.jsonl` | one line per acquisition: `at_ms`, `tool`, `weight`, `waited_ms`, `load1`, `outcome` (`admitted`, `deferred`, `unqueued`, `timeout`, `handed`), `pid`, and `jobs` when the writer knows it. Lines written by greengate's checks also carry `name` (the check), `session` when one is known, and `"rehearsal":true` / `"prestage":true` inside a rehearsal snapshot / land's pre-stage. |

## Admission

Only the oldest live ticket may take slots. It takes `weight` slots, all or
none, under the admission lock, when:

- **no slot is held:** the floor. The first waiter on an empty host is
  always admitted, whatever its weight up to the cap;
- **`greengate.hostSlots` is unset (automatic):** a free slot exists, the
  one-minute load is below the core count, and at least 60 s have passed
  since the last admission, unless the waiter's own process already holds a
  slot (one commit's checks are not spaced apart). The cap is
  `max(1, cores/2)`, with `cores` as [Cores](#cores) counts them;
- **`greengate.hostSlots N`:** N free slots are enough, with no load test and no
  ramp. `0` turns slots off.

The 60 s ramp is the load average's own lag: 10 s after a new holder starts,
the one-minute load shows only about 15% of it.

A waiter that is not admitted says why, after whom it waits behind:
`(2 ahead)`, `(slots full 3/3)`, `(load 31.0 >= 6 cores)` or
`(one new holder per minute)`. The first such line is followed, once, by
`see: greengate slots`.

## Cores

One count sizes the cap, the width and the load test (ADR-0021): the
physical cores, never more than the logical count, which also honours a
cgroup or affinity limit. macOS reads `hw.physicalcpu`; Linux counts the
distinct `core_cpus_list` entries in sysfs, then the distinct
(`physical id`, `core id`) pairs in `/proc/cpuinfo`, and falls back to the
logical count where neither says (aarch64 VMs). Hyper-threads share a
core's execution units, so on a 6-core, 12-thread host the count is 6:
3 automatic slots, each 2 jobs wide.

`greengate.hostCores N` (1 to 1024) replaces the detected count, and
`GREENGATE_HOST_CORES` overrides it for one process. The slots crate
resolves both host keys for every client
(`greenharness_slots::host::resolve`), so greengate and relais always size
the queue from the same numbers. Detection reports how it found the count:
`detected`, `detected (logical fallback)` where the OS gave no physical
count, or `limited by affinity` where an affinity or cgroup limit allows
fewer cores.

`greengate slots` names the threads only where they differ from the cores,
and tags a count that plain detection did not set, inside 80 columns
(`6 cores, 12 threads`, `4 cores, 12 threads (env)`, `(git)`, `(logical)`,
`(affinity)`). `--json` carries `cores` and `cores_source`, whose value is
a stable token: `env`, `global`, `system`, `detected`, `logical_fallback` or
`limited_by_affinity`. `greengate doctor` prints the full sources on
stderr, one line per key:

```text
host cores: 4, 12 threads (GREENGATE_HOST_CORES)
host slots: auto (detected)
```

An invalid value is a warning that names the fix for its source
(`unset GREENGATE_HOST_CORES`, or the `git config` command).

## Who holds a slot

Only the process that took the slot holds it. Slot descriptors are
close-on-exec, so a command's children, and any daemon it leaves behind,
never hold it. `greengate slot run` frees the slot when its command exits.
`GREENHARNESS_HOST_SLOT=held` in a command's environment means its parent holds a
slot, and an greengate or wrapper started under it does not queue again.

## Handoff inside a rehearsal

Inside a rehearsal snapshot a pre-push keeps its slot from one heavy gate to
the next. The finished gate parks its lease and renames the holder record to
`pre-push, between checks`; the next heavy gate takes it with a `handed` line
in `waits.jsonl` (a wait of 0, the same `pid`). The pre-push releases a parked
slot when it ends, however it ends. While a slot is parked, a light check's
tools run under it, width-bound, and a child `greengate slot run` does not queue. A gate that runs on the cluster executor releases the parked slot first.
A plain push and a commit release their slot after each check.

## The ceiling in land's pre-stage

Only in land's pre-stage, a heavy `warn` check waits at most
`greengate.warnSlotWait` seconds (default 600, range 1 to 3600, never above
`greengate.timeout`), then runs unqueued. Precedence: git config, then
`set warnSlotWait <s>` in a trusted `greengate.conf`, then the default. A `block`
gate and a background rehearsal wait as before.

## Width

A slot bounds how many heavy jobs run, and the width bounds how hard each
one runs. Left alone, `cargo` builds and its test harness runs on every
core, so `cap` holders ran `cap × cores` jobs at once. A holder of `weight`
slots therefore runs `jobs = max(1, weight × cores / cap)` wide, with `cores`
as [Cores](#cores) counts them. greengate sets
`MAKEFLAGS=-j{jobs}`, `CARGO_BUILD_JOBS`, `RUST_TEST_THREADS` and
`NEXTEST_TEST_THREADS` on every heavy hook check and every `greengate slot run`
command (`amont_slots::bound_width`). relais uses the same formula for its
verification commands.

- **Precedence:** the check's own environment first, then the parent's
  environment, then greengate's computed width. A key already set to a
  non-empty value is kept, so a check that sets its own width, or a relais
  run that bounded its command, wins. An empty value counts as unset.
  A `MAKEFLAGS` that carries flags but no job bound gains ` -j{jobs}`.
- **Unqueued runs are bounded too.** A check that runs after its slot wait
  timed out runs when the host is busiest.
- **Nothing is set** when slots are off (`greengate.hostSlots 0`), unsupported
  (Windows), or the command is nested under a holder (`GREENHARNESS_HOST_SLOT=held`),
  whose bounds it inherits.
- `greengate slots` shows each holder's `jobs`, or `-` for a record written
  without it.

## Old clients

greengate up to 1.61 and relais before it linked this crate take `slot-0..N`
with no ticket and no load test. They jump the queue until they are
upgraded. Their slots show as `unknown (old client)`, because they write no
holder record.

## Commands

- `greengate slot run [--weight N] [--max-wait S] [--on-timeout run|exit] -- <cmd…>`
  waits for a slot, then runs the command with `GREENHARNESS_HOST_SLOT=held` and
  passes its exit status through. After `--max-wait` (default 90 s, under
  Claude Code's 120 s Bash timeout) it exits 75 and runs nothing, unless
  `--on-timeout run` is given.
- `greengate slots [--json]` lists the holders, the old-client slots and the
  number of waiters.
