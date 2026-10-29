---
status: active
branch: feat/one-host-slot-queue
repos: [amont, relais, amont-agent, decisions]
adrs: [ADR-0013, ADR-0014, decisions:ADR-0049]
---
# Every heavy job on the host shares one slot queue

## Review panel

📄 Full reviews: [2026-10-29-every-heavy-job-shares-one-queue.reviews.md](2026-10-29-every-heavy-job-shares-one-queue.reviews.md)
👉 **Decide:** none. Approve if one shared queue for hooks, relais and agents (the `amont-slots` crate, `amont slot run`, an advise rule) is worth three releases. The load threshold is measured with one holder before phase 2, and comes back to you if it would stall everyone at one slot.
📍 amont, relais, amont-agent, decisions · plan only · next: ADR-0014 and the crate. Panel: backend (sonnet-5-5), codex. `plan-panel` computed a delta, because the file path was reused; say if you want the full panel.
**Changed by review:** the load test counts the holders' own load, so it is measured first; `--max-wait` exits 75 without launching; tickets and admission sit under one non-blocking lock; `.who` files move into `holders/`; you chose that the wrapper alone holds the slot.
**Verdicts:** backend approve-with-changes, approve-with-changes, then approve. Codex approve-with-changes. Carried into implementation: poll the process group's end with a stated interval, and document daemons that stay in the group.

## Context

amont 1.61.1 (ADR-0013) defers a heavy git-hook gate to CI when no host slot frees. But the queue only helps if everything heavy on the machine joins it. Today three kinds of jobs load this 12-core Intel Mac:

- **amont's hook gates.** These queue on `/tmp/amont-slots-<uid>/slot-N` (`amont-runtime/src/host_slots.rs`).
- **relais's verification commands.** These queue on the same files, but through relais's **own copy** of the protocol (`relais/crates/relais/src/verify/heavy.rs:894-1040`, `procs::try_flock`, `ancestor_holds_slots`). It is opt-in through `machine.toml` `[resources] shared_host_slots`, and it does not create the directory. Two implementations of one contract is what `harness.shared-code` forbids.
- **Agents' own builds in Bash.** A worker's or session's `cargo test`, `pnpm test` or `xcodebuild` never queues, and nothing tells the agent to.

Across the day the load reached 240–600, and the gate failures were the outside load the ADR-0013 plan named as a non-goal.

Two more gaps:
- **The slot count ignores the machine.** It is a fixed `cores/4`, which is 3 here, whatever the load is.
- **A waiter sees `slots 2/2` and nothing more.** It cannot tell who holds the slots or how long they have.

Decided with the person:
- **The agent rule ships as advise.** Its firings are counted, and the rule is tightened later on that evidence.
- **The slot count is load-aware.** It is at least 1 and at most cores/2. A new holder is admitted only while the 1-minute load is below the core count. `amont.hostSlots N` still overrides it.

## Design

**1. One implementation: a new std-only crate, `amont-slots`.** It is published with amont, as `amont-quiet` was (`crates/amont-quiet`; dependency-free so it can sit on the commit path, allowed by name in `scripts/check-no-deps.sh`). It takes the protocol out of `host_slots.rs`:
- the slot directory, and `safe_dir` (0700, owned by this user);
- `try_lock` (flock `LOCK_EX|LOCK_NB`);
- the `Slot` guard, with its process-wide `HELD` count and drop order. Slot descriptors are close-on-exec: only the process that took the slot holds it, never its children (decided with the person: a detached daemon must not keep a slot held);
- `HELD_ENV` (`AMONT_HOST_SLOT=held`);
- a persistent waiter with nonblocking `poll` and cancellation-safe drop, retaining one ticket across polls, plus a blocking `acquire(weight, budget)` convenience wrapper; acquisition is all-or-none for a `weight` above 1. relais drives polling through its existing queue loop, preserving coordinator heartbeats, cancellation, admission and total budget charging, and progress events. Test cancellation and budget expiry while queued, and a simulated wait exceeding the 300 s coordinator lease grace without losing the lease.

`amont-runtime` keeps the hook-specific parts on top of it: the busy policy, deferral, `CURRENT` and the queue row. The contract is written in `docs/host-slots.md`, as `docs/quiet-runs.md` is for amont-quiet.

**2. Holders say who they are.** Holder metadata lives in a private `holders/` subdirectory, so old relais's `slot-*` inventory cannot mistake it for capacity. Add a mixed-version regression using the existing relais inventory reader: metadata must not increase the slot count or permit admission while all real slots are held. A holder writes `holders/slot-N.who`, a small JSON file: pid, tool (`amont` / `relais` / `amont slot run`), repository, command, session, and start time. It is written after the flock is won and unlinked on drop BEFORE the flock is released (today `Slot::drop` releases the lock first, `host_slots.rs:113`), so a releasing holder never deletes its successor's file. A reader trusts it only while the pid it names is alive with the recorded start time. It never probes `slot-N` with a flock, because the probe would take the slot from a waiter.

Waiters say who they wait behind, in amont's row and in relais's `WaitingForSlot` event:
`· queued 42s behind relais verify: cargo test (amont-wt-x, 4m) · 2 waiting`

**3. Load-aware admission.** `amont.hostSlots` unset means automatic:
- **The cap** is `max(1, cores/2)`, 6 here. That is how many slot files exist.
- **A free slot is taken only while the 1-minute load is below the core count**, read by `amont-runtime/src/load.rs::read`, which moves into the crate. Before Phase 2 ships, measure load1 with one holder running amont's `pre-push-cargo-test` on an otherwise idle host. If it reaches 12 or more, this rule never admits a second holder and the cap of 6 is never reached. The person then chooses: subtract the holders' expected load (`load1 - holders x cores/2 &lt; cores`), or keep floor-only on purpose and record the throughput that gives.
- **The floor:** when no slot on the host is held, the first holder is always admitted, whatever its weight up to the cap, so a relais weight-2 command at the head of the queue does not starve while load stays at or above the core count.
- **Ramp:** the 1-minute load lags, so on an idle machine a burst of waiters would all see a low load and pile in. So at most one new holder is admitted per 60 s (the load average's time constant: after 10 s it shows only about 15% of a new holder's load), through a shared `admitted-at` stamp under the admission flock of section 4; there is exactly one admission lock. The floor check, the load read and the stamp all happen under that one flock, so two waiters cannot both see an empty host.

`amont.hostSlots N` keeps today's meaning: a fixed N, no load test. `0` still turns slots off. amont up to 1.61 (npm-pinned baked hooks included) and relais up to today's release still poll `slot-0..2` with no ticket and no load test, so they jump the queue until upgraded. `amont slots` shows a holder with no `.who` as `unknown (old client)`.

**4. Fair order.** `host_slots.rs:17` says first-come ordering is needed once "more than a handful of gates contend on one host", and that time has come. Under the shared admission flock, a waiter allocates a monotonically increasing sequence number, creates `queue/<sequence>-<pid>`, and locks its ticket before releasing the admission flock. Ticket scans, stale-ticket removal, head selection and all-or-none slot acquisition also occur under that admission flock, including in fixed-cap mode. Only the oldest live ticket may take slots. A ticket whose flock is free is removed while holding the admission flock. The admission flock is taken with `LOCK_NB` plus retry inside each caller's own deadline, so a hook's busy policy still defers on time. Test with a SIGSTOPped process holding the admission flock: a hook gate defers within its deadline, and `waits.jsonl` records how long the admission lock was held. Test suspension between ticket creation and locking, concurrent waiters in one process, cancellation of the head, and stale-ticket cleanup.

**5. `amont slot run [--weight N] -- <cmd…>`** wraps a command anyone can run. It waits for a slot, showing whom it waits behind on stderr. It then runs the command with `AMONT_HOST_SLOT=held`, so a nested amont does not queue again, passes the exit status through, and releases the slot on exit. The wrapper alone holds the slot (decided with the person). It frees the slot when the command's process group ends, so a daemon a build leaves behind (sccache, an Xcode build service, `setsid`) never holds it. The cost, accepted: killing only the wrapper while its child builds on frees the slot early; Claude Code kills the whole tree, so this is rare. Missing or dead-pid metadata is not proof that capacity is free. The wrapper forwards termination signals to the command's process group and reaps it. relais moves to the same rule: its runner holds the slot, and `inherit_fds` no longer passes slot descriptors to checks. Test `amont slot run -- sh -c '(setsid sleep 300 &); exit 0'`: the slot frees when the direct child's group exits. When an ancestor already holds a slot, the wrapper runs the command at once. It waits at most `--max-wait` (default 90 s, under Claude Code's 120 s Bash timeout), then exits 75 without launching the command, with a note on stderr naming the holders. An explicit `--on-timeout=run` permits unqueued execution and records that outcome; it must not set `AMONT_HOST_SLOT=held`. Test three simultaneous long-running commands with a simulated clock: the third times out without spawning under the 60 s ramp. This wrapper policy does not change hook busy policies. **`amont slots`** lists holders and waiters, and has a `--json` form.

**6. relais uses the crate.**
- The `procs::try_flock`, `heavy.rs mod slots` and `ancestor_holds_slots` copies are replaced by `amont-slots`.
- `shared_host_slots` defaults to amont's slot directory, so it is on without `machine.toml`, and relais creates the directory through `safe_dir`. `shared_host_slots = "off"` turns it off (today `None` means off, `policy/mod.rs:890`). That is the rollback for a bad crate release without a new relais release.
- relais emits the holder metadata (tool `relais`, the run id, the command name).
- Queue time stays out of `timeout_seconds`, as it is now (`verify/heavy.rs:25-26`).
- relais's own coordinator cap (`max_heavy_commands`) stays. It orders relais runs among themselves; the host slots are for the whole machine.

**7. amont-agent: rule `heavy-build-unqueued`, advise only.**
- **When it fires:** a readable Bash clause runs a heavy build or test that is not wrapped by `amont slot run --`, `amont run` or `amont rehearse`.
- **What counts as heavy:** `test_output_trimmed::runner_words` (`amont-agent-guard/src/rules/test_output_trimmed.rs:62`), plus `cargo build`, `cargo check`, `xcodebuild` and `swift build`.
- **Stance:** `default_stance: Advise`, `max_stance: Advise`. The cap is lifted later, after the evidence.
- **`prevents`:** "A build or test suite run beside every other session's, unqueued, thrashing the host until gates time out".
- **Advice text:** names the wrapper, `amont slot run -- <cmd>`, and the current holders when readable.
- **`confirm`:**
  - stays silent when `amont` on PATH is older than the release with `slot run` (the version is cached per session);
  - stays silent when `AMONT_HOST_SLOT` is in the hook's environment.
- **Counting:** firings land in the journal as usual. `amont-agent status` and `harness-review` already count them per 1000 Bash calls, along with compliance (did the next call use the wrapper). A call using `--on-timeout=run` counts as non-compliant, and `waits.jsonl` outcomes `unqueued` and exit-75 timeouts are reported beside it. Precision never-fire excludes `amont slot run --on-timeout=run -- cargo test`, which fires.

## Phases

1. **ADRs.**
   - amont `adr/0014-every-heavy-job-shares-one-queue.md`: replaces ADR-0013's head of `hooks.host-concurrency` (load-aware admission, fair order, holder metadata, the `slot run` surface).
   - The fleet corpus: decisions ADR-0049 replaces ADR-0044's head of `harness.tool-boundaries`: amont owns the host slots, implemented once in `amont-slots`; `harness.shared-code` needs no change.
2. **amont.** The `amont-slots` crate and the `host_slots.rs` move onto it; admission, tickets and `.who` files; `amont slot run` and `amont slots`; `docs/host-slots.md`. Every acquire appends one JSON line to `&lt;slot dir&gt;/waits.jsonl` (tool, weight, waited_ms, load1, outcome admitted|deferred|unqueued), the source of the day-14 p50/p95. Release 1.62.0.
3. **relais.** Use `amont-slots`, default the shared slots on, and report the holder in `WaitingForSlot`. Release.
4. **amont-agent.** The `heavy-build-unqueued` rule at advise. Release.
5. **Measure for 14 days, then decide** whether to tighten the rule, and whether the cap and the ramp hold.

Implementation is routed through relais per task where a run can finish here. amont runs ended `baseline_unrunnable` under load three times, so a run that stops that way is done directly, as before.

## Verification

- **`amont-slots` unit tests:**
  - admission math: idle → up to cap; load ≥ cores → queue; no holder → floor admits; ramp holds the second admission for 60 s;
  - tickets: the oldest live ticket wins, and a dead ticket is skipped;
  - `.who` is ignored when its slot is free;
  - all-or-none weight 2.
- **amont integration tests** (unpinned, as in `busy_defers.rs`):
  - two `amont slot run -- sleep 2` with one slot run one after the other, and the second prints `queued … behind amont slot run: sleep`;
  - the exit status passes through;
  - a nested `amont slot run` under `AMONT_HOST_SLOT=held` does not queue;
  - `amont slots --json` lists the holder.
- **relais:** its existing slot tests, now against the crate; a run with no `machine.toml` takes slots; `WaitingForSlot` names the holder.
- **amont-agent:**
  - `tests/precision.rs`: must-fire `cargo test`, `cd x && pnpm test`, `xcodebuild …`; never-fire `amont slot run -- cargo test`, `amont run`, `cargo fmt`, `cargo test --help`;
  - `tests/hook.rs`: advise text emitted, journal row written.
- **Live:**
  - `amont slot run -- sleep 60` in session A, then `amont slot run -- true` in session B: B's stderr prints `queued … behind amont slot run: sleep`, and B starts after A ends;
  - with `sysctl -n vm.loadavg` ≥ 12 and one holder, a second `amont slot run -- true` queues until the holder exits, and `waits.jsonl` records its waited_ms;
  - `amont slots` during a relais run lists relais.
- **After 14 days:** the rule's firings per 1000 Bash calls and its compliance rate from `amont-agent harness-review`; slot wait p50/p95; gate timeout failures against before. Bars to tighten: compliance under 50% → deny with a remedy; a p95 wait above 10 min → revisit the cap.

## Decision log

- 2026-10-29, phase 1: the fleet half is decisions ADR-0049 on `harness.tool-boundaries`, not an edit to `harness.shared-code`: that choice is a rule with no list of contracts to extend, and the owner list lives in `tool-boundaries` (implementation review, decisions branch).
- Phase 4 reads holders from `amont slots --json`, never `holders/*.who` directly (`harness.cross-tool-reads`; ADR-0049 names `amont slot run` and `amont slots` as amont-agent's only surface).

<!-- panel: repos=amont,relais,amont-agent,decisions reviewers=backend,codex body-sha=76ca38ab5f73 -->
