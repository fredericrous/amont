---
status: active
branch: feat/slot-handoff-in-rehearsal
repos: [amont]
adrs: [ADR-0013, ADR-0014]
---

# A land waits for a slot once, and not for ever

## Review panel

👉 **Decide:** none (you chose the scope: handoff in every rehearsal, ceiling in land's pre-stage only). Approve if you're willing to bet that keeping a slot through a land's own gates beats re-queuing for each.
📍 amont · planned, nothing built · next: Phase 1 in one amont worktree, rebased over `fix/slot-holder-session`, released as 2.3.0. Panel: backend, lang:rust, unix, tui (all opus-5-5), codex (gpt-6-astra).
**Changed by review:**
- a parked slot is released on every exit path, not at process exit;
- a light check's children inherit the parked slot;
- the pre-stage marker has a fixed format that keeps the detached gates intact.

📄 Full reviews: [2026-11-07-a-land-waits-for-a-slot-once.reviews.md](2026-11-07-a-land-waits-for-a-slot-once.reviews.md)
**Verdicts:** 5 approve-with-changes. 30 findings accepted, 1 sent to you (the scope), 0 rejected; 8 carried into implementation, listed in the full reviews.

## Context

Since 2.2.1 (#434), a heavy gate inside a rehearsal, including land's
pre-stage, waits for a host slot rather than defer. The host's slot log
(`/tmp/amont-slots-<uid>/waits.jsonl`, 1,425 acquisitions over 2.5 days to
2026-11-07) shows how long that can take:

| | Median | p90 | Max | Over 5 min |
|---|---|---|---|---|
| amont gates, admitted | 0.1 s | 65 s | 12.6 min | 10 |
| amont slot run (builds) | 0 s | 114 s | 18 min | 17 |
| relais checks | 0 s | 98 s | 13 min | 13 |

The long waits happen at a moderate load (median load1 11.7 on 12 cores). They
are slots held by long jobs, mostly Rust suites, not CPU saturation. A land
pays the tail more than once, because pre-push runs its gates one at a time
(`dispatch.rs:1847`), and each heavy gate drops its slot when its guard drops
(`dispatch.rs:1787-1797`, `host_slots.rs:188-191`). The next heavy gate then
joins the back of the queue. The 2026-11-07 lands showed it: `cargo-test`
queued for about 4 minutes, then the CLI probe queued again. The log cannot
sum waits per land, because a record has no gate name and no pid
(`amont-slots/src/lib.rs:544-567`).

The person approved this direction (2026-11-07, "go"), and chose its scope:

- the handoff applies to every rehearsal;
- the ceiling applies only to land's pre-stage, where someone is waiting.

Another session's `fix/slot-holder-session` (0f3eb3c1, landing now) adds a
session id and the source worktree to holder records, and a
`amont_slots::session_id()`. This branch rebases over it.

## Approach

### Phase 1: amont, one release (2.3.0)

1. **The handoff, in every rehearsal snapshot** (`rehearsal::in_snapshot()`,
   which covers land's pre-stage and background rehearsals alike). Pre-push
   is serial there, and no connection is open.
   - **Parking.** When a heavy check's `CheckGuard` drops while it holds a
     slot, it first takes `held` out of `Current`, before resetting `Current`
     (`host_slots.rs:188-191`). Then it parks the `Lease` in a process-wide
     `Mutex<Option<Lease>>`, and renames the holder record to `pre-push
     (between gates)`, so `amont slots` and other waiters' `queued behind`
     lines do not name a finished gate.
   - **Taking it.** The next heavy check's first `before_spawn` takes the
     parked lease before calling `wait_for_slot`. It renames the holder to
     itself and records a wait of 0. It sets `held`, `settled = true` and
     `jobs` in `Current` as a grant does (`host_slots.rs:343-354`), so
     `HELD_ENV` is exported and `bound()` caps the tools' width.
   - **Release on every path.** `dispatch::pre_push` creates a
     `host_slots::ParkScope` guard before its loop. Its `Drop` releases the
     parked lease on every exit: the normal end, the early `Verdict::Block`
     returns (`dispatch.rs:1402`, `1882`) and an unwind. A `static` is never
     dropped, so `Lease::drop` (`lib.rs:292-299`) must not wait for process
     exit; the kernel freeing the flock at exit is only the last resort.
   - **`amont_slots::Lease::rename(&self, &Holder)`.** It rewrites each
     `who` file of the lease, keeping the holder's repo and session as the
     other branch sets them, and resets its start time, so the age shown is
     the current gate's. It writes a hidden temp file in `holders/`, then
     renames it over `slot-N.who`, so a concurrent `read_holders`
     (`lib.rs:311-313`) never sees a truncated record. The change is
     additive, and relais keeps compiling against `amont-slots = "1.62"`.
   - Outside a snapshot nothing changes: a plain push and a commit release
     their slot after each check, as today.
   - Pre-push may run cheaper checks first (`registry.rs:1082`), so a parked
     slot can remain held through light checks between two heavy gates.
     While this ParkScope owns a parked lease, light-check spawns must
     export HELD_ENV and apply the same job-width bound: otherwise a child
     `amont slot run` queues for the slot its parent is retaining. Keep the
     lease parked until the next heavy check or scope exit. Document this
     behavior in ADR-0014. Add a one-slot integration test with a heavy
     gate, a light scoped gate invoking `amont slot run`, and another heavy
     gate; assert the nested command executes without a new acquisition,
     inherits the width, and the final heavy gate receives the handoff.
2. **The ceiling, only in land's pre-stage.**
   - **Marking the pre-stage.** When the rehearsal was started by land's
     pre-stage (`AMONT_LAND_PRESTAGE`, `rehearsal.rs:242`, read as
     `prestage`), it marks the snapshot it spawns with a leading `prestage;` in
     the `AMONT_REHEARSAL` value (`prestage;1` or `prestage;detached=a,b`,
     `snapshot_env_value`, `rehearsal.rs:812`). `snapshot_env` strips it
     before the `detached=` parse (`rehearsal.rs:163-166`) into a
     `SnapshotEnv::prestage` field; `rehearsal::in_prestage()` reads that
     field, never the variable, which the first read removes. A background
     rehearsal is not marked. A unit test: `prestage;detached=a,b` gives
     `prestage` true and `detached == [a, b]`.
   - **The bound.** In a marked snapshot, a heavy `warn` check waits at most
     `amont.warnSlotWait` seconds. The default is 600, range 1–3600, and it
     never exceeds `amont.timeout`. The 600 sits above every p90 (65–114 s)
     and below every maximum (12.6–18 min); 40 of the 1,425 waits (2.8%) ran
     over 5 min.
   - **Then it runs unqueued, never deferred.** `dispatch::enter` calls a new
     `host_slots::wait_bounded_for_warn()` for such a check, setting a flag
     in `Current`. With the flag set, `before_spawn` uses the policy `Wait`
     with budget `min(warnSlotWait, ceiling)`, whatever `amont.onBusy` says,
     so a timeout is `unqueued`, never `deferred` (`host_slots.rs:272-281`,
     `319-325`). The note then names the key that cut the wait: `no host slot
     freed in 10m00s (amont.warnSlotWait 600); running it anyway`, not
     `amont.hostSlots` (`host_slots.rs:428-431`).
   - **The key.** Git config wins over `set warnSlotWait <s>` in a trusted
     `amont.conf`, which wins over the default 600; docs/host-slots.md states
     this order once. It is strict and set once, added to the manifest list
     beside `slotWait` (`manifest.rs:505`) with its parse validation.
   - A `block` gate keeps waiting up to `amont.timeout`: its verdict can stop
     the push. A background rehearsal keeps waiting without a ceiling, so it
     never runs above the slot cap.
3. **The log.**
   - A new `amont_slots::record_wait_named(dir, tool, name, weight, jobs,
     waited_ms, outcome)` writes `"name"`, `"pid"` and `"session"`
     (`session_id()`). A line from a snapshot also carries
     `"rehearsal":true`, and one from land's pre-stage `"prestage":true`.
   - `record_wait_jobs` and `record_wait` delegate to it with no name; their
     lines gain only `"pid"`. That is additive JSON, so relais's calls are
     unchanged.
   - `host_slots` records with the check's short name. A handoff is recorded
     with the outcome `handed` and a wait of 0.
4. **Docs.**
   - Amend ADR-0014 with a dated note: inside a rehearsal, a pre-push keeps
     its slot from one heavy gate to the next. Say why (one wait per land),
     what it costs (it holds the slot through its own light checks), and
     that the ceiling is for land's pre-stage only.
   - Amend ADR-0013 with `warnSlotWait`.
   - Update `docs/host-slots.md`: the handoff, the key and its precedence,
     and the new log fields.
   - Add a CHANGELOG `### Added` entry under Unreleased.
5. **Release and install.** Release 2.3.0 with `tag-release`, then
   `brew update && brew upgrade fredericrous/tap/amont`.

## Non-goals

- Overflow to the cluster. It is homelab-offload's enable phase, and it
  waits for t13. Rust gates need a pod build cache: a ticket on that
  programme's map, charted separately.
- The slot cap (`cores/2`, ADR-0014, the person's decision).
- A handoff on a plain push or a commit, and a ceiling for background
  rehearsals or `block` gates.
- relais's own `record_wait` calls naming their check, as a later change in
  relais.

## Code review

- amont: backend, lang:rust, unix, tui

## Verification

- **amont-slots unit tests:**
  - `Lease::rename` rewrites every `who` file, keeps repo and session, and
    resets the start time.
  - A reader polling `read_holders` while `rename` runs in a loop sees no
    `unknown` record.
  - `record_wait_named` writes `name`, `pid` and `session`; `record_wait`
    writes the same fields as before, plus `pid`.
- **amont-runtime unit test:** a `ParkScope` that returns early leaves
  `held_in_process()` at 0 and no `who` file.
- **Integration tests** in `crates/amont/tests/it/busy_defers.rs`, using its
  `hook()`, `slot_dir`, `Holder::hold` and `release_after` harness, with
  `AMONT_HOST_SLOTS=1` and a private slot directory:
  - **Handoff.** A rehearsal-shaped pre-push (`AMONT_REHEARSAL=1`,
    `AMONT_ON_BUSY=wait`) runs two heavy gates that each touch a marker,
    with the only slot held 3 s. Once the first gate holds the slot, a
    second outside waiter queues and keeps the slot 10 s once granted,
    recording its grant time. Expected: both gates run, the total stays
    under 6 s (it would take over 13 s without the handoff), and the outside
    waiter's grant comes after the second gate's marker. `waits.jsonl` has a
    `handed` line naming the second gate, with the first gate's pid.
  - **No handoff outside a snapshot.** The same two gates on a plain
    pre-push with `amont.pushGates inline`: no `handed` line.
  - **The slot is freed after a block.** A heavy gate parks, then a `block`
    gate fails. Expected: `amont slots` shows no held slot and the slot
    directory has no `who` file once the hook returns.
  - **Ceiling in a pre-stage.** A marked snapshot (`prestage`), a heavy
    `warn` gate, `warnSlotWait 2`, the slot held 30 s. Expected: it names
    `amont.warnSlotWait` in its `running it anyway` note, leaves its marker,
    exits 0, and takes under 20 s.
  - **No ceiling in a background rehearsal.** The same gate in an unmarked
    snapshot: it waits for the slot, at least 28 s, with no `running it
    anyway`.
  - **A `block` gate is not cut short.** The same pre-stage setup with a
    `block` gate: at least 28 s, no `running it anyway`.
  - The 2.2.1 tests in `busy_defers` and `land` stay green unchanged.
- **The commit gate:** each commit of the branch exits 0 under amont's own
  pre-commit, with no skip.
- **Live, in a fresh clone** with a private one-slot directory, as in 2.2.1's
  check. Hold the slot 120 s, then run `amont rehearse --wait` on a tree that
  owes `cargo-test` and the probe. Expected:
  - one `(waited …)` line, for `cargo-test`;
  - while the probe runs, `amont slots` names the probe, not `cargo-test`;
  - `waits.jsonl` has the probe's `handed` line under the same pid.
- **Seven days after install, from `waits.jsonl`:** group the
  `"prestage":true` lines by pid within one hour of their first line (pids
  are reused). Report each land pre-stage's summed wait (median and
  maximum), the share of `handed` lines, and how many warn runs went
  unqueued past the ceiling. `evidence-missing` refusals are still expected
  to be 0.
- **Rollback:** revert the PR. Each gate releases its slot again, and the
  log lines lose their new fields. An old reader ignores them either way.

<!-- panel: repos=amont adds= reviewers=backend,language,tui,unix,codex body-sha=4da7e3da84b9 -->
