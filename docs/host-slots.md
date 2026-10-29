# Host slots

The contract of the `amont-slots` crate (amont ADR-0014, decisions
ADR-0049). A test suite, a build or a type checker already uses every core
on its own, so a heavy job takes a host slot before it runs. Every heavy job
on the machine takes its slot through this one queue: amont's git-hook gates,
relais's verification commands, and any command run through
`amont slot run`. relais links the crate; it does not carry a copy.

## The directory

`AMONT_SLOT_DIR`, else `$XDG_RUNTIME_DIR/amont-slots` on Linux, else
`/tmp/amont-slots-<uid>`. It is a fixed path, never `$TMPDIR`, which macOS
and agent sandboxes set per session. It must be a real directory owned by
this user with mode 0700. Otherwise slots are off for the job, and it says
why.

| Path | What it is |
|---|---|
| `slot-0` … `slot-{cap-1}` | the slots. A slot is held while a process holds `flock(LOCK_EX)` on its file. |
| `admission` | the one admission lock. Every admission decision is made under it, taken with `LOCK_NB` and retried inside the caller's own deadline, so a stopped holder of it never holds a hook past its own deadline. |
| `admitted-at` | the time of the last admission, in ms since the epoch, for the ramp. |
| `queue/<nanos>-<pid>-<n>` | a waiter's ticket, flocked for as long as it waits. It is created hidden (`.new-…`), locked, then renamed into place, so a visible ticket is never unlocked. A visible ticket whose lock is free belongs to a waiter that died, and is removed. |
| `holders/slot-N.who` | who holds slot N, as a JSON object (`pid`, `tool`, `repo`, `command`, `session`, `started_ms`). It is written after the slot is won and removed before it is released. A reader trusts it only while `pid` is alive. It lives in a subdirectory so an old client counting `slot-*` files never mistakes it for a slot. |
| `waits.jsonl` | one line per acquisition: `at_ms`, `tool`, `weight`, `waited_ms`, `load1`, `outcome` (`admitted`, `deferred`, `unqueued`, `timeout`). |

## Admission

Only the oldest live ticket may take slots. It takes `weight` slots, all or
none, under the admission lock, when:

- **no slot is held:** the floor. The first waiter on an empty host is
  always admitted, whatever its weight up to the cap;
- **`amont.hostSlots` is unset (automatic):** a free slot exists, the
  one-minute load is below the core count, and at least 60 s have passed
  since the last admission, unless the waiter's own process already holds a
  slot (one commit's checks are not spaced apart). The cap is
  `max(1, cores/2)`;
- **`amont.hostSlots N`:** N free slots are enough, with no load test and no
  ramp. `0` turns slots off.

The 60 s ramp is the load average's own lag: 10 s after a new holder starts,
the one-minute load shows only about 15% of it.

## Who holds a slot

Only the process that took the slot holds it. Slot descriptors are
close-on-exec, so a command's children, and any daemon it leaves behind,
never hold it. `amont slot run` frees the slot when its command exits.
`AMONT_HOST_SLOT=held` in a command's environment means its parent holds a
slot, and an amont or wrapper started under it does not queue again.

## Old clients

amont up to 1.61 and relais before it linked this crate take `slot-0..N`
with no ticket and no load test. They jump the queue until they are
upgraded. Their slots show as `unknown (old client)`, because they write no
holder record.

## Commands

- `amont slot run [--weight N] [--max-wait S] [--on-timeout run|exit] -- <cmd…>`
  waits for a slot, then runs the command with `AMONT_HOST_SLOT=held` and
  passes its exit status through. After `--max-wait` (default 90 s, under
  Claude Code's 120 s Bash timeout) it exits 75 and runs nothing, unless
  `--on-timeout run` is given.
- `amont slots [--json]` lists the holders, the old-client slots and the
  number of waiters.
