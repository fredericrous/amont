---
status: active
branch: feat/busy-is-not-stuck
repos: [amont]
adrs: [ADR-0001, ADR-0008, decisions:ADR-0020]
---
# Busy is not stuck: CPU-aware silence budget

## Context
amont kills a check that prints nothing for `amont.idleTimeout` (120 s), on
the premise "a hang is silent; a slow suite talks" (`crates/amont-runtime/
src/hooks/common.rs:361-371`). vitest breaks it: in a non-TTY its default
reporter prints only the final summary. duro-app's pre-push suite (169 files,
~4-7 min, CPU-heavy) was killed twice on 2026-09-29 with **1553/1553
passing**, and passed only with a one-off `amont.idleTimeout=600`. The person
asked for the fix in amont: a quiet child doing measurable work is not judged
stuck, and amont's own progress line says so.

Facts (origin/main 2d623cf):
- Kill clock: `Activity` (common.rs:389-408, `Mutex<Instant>`) touched by
  `run_observed`'s readers (486-539); `wait_within` (596-634) polls every
  25 ms and asks the pure `judge` (644-661); it returns as soon as the ROOT
  exits and kills only the root; `readers.join()` (535) then waits for every
  pipe holder — a surviving worker can hold it indefinitely.
- Progress today: region (`· quiet 45s/2m`, live.rs:369-378) and non-TTY
  `heartbeat`/`beat_line` (410-515) read `Slot.last_output`, not `Activity`.
- No dependencies (`check-no-deps.sh`, decisions:ADR-0020); FFI style
  `staged_only.rs:982-1022`; MSRV 1.74; Windows supported
  (`platform.windows-parity`); CI clippy runs on Linux only.
- Measured: live-process CPU sums go DOWN under fork-per-file; per-process
  CPU incl. reaped children rises monotonically. macOS `rusage_info` times
  are mach ticks (41.67 ns on Apple Silicon); macOS `exec` zeroes the
  reaped-children times.

## Goal
Stuck ⇔ silent AND the process tree showed **insufficient CPU activity**
(< 0.1 core, measured) for the whole silence budget. This is a heuristic, not
accounting: it answers "is something working?", and every figure it shows is
labelled approximate. Where CPU cannot be measured, today's silence rule
applies and the messages say so.

## Non-goals
Process-group spawn/kill of grandchildren and the unbounded `readers.join()`
after a root kill — a follow-up (Ctrl-C semantics); this plan's tests are
built so they never depend on it. Tool-specific reporter flags. Windows CPU
sampling.

## Behaviour
1. **Sampler thread, off the ceiling loop.** `run_observed` starts one
   `amont-cpu` thread per observed child (when `amont.idleCpuCredit`, default
   true, and the platform is linux/macos). It sleeps until `still_for ≥
   max(250 ms, min(30 s, budget/3))`, then samples every
   `clamp(budget/4, 250 ms, 10 s)`, publishing into `Activity` atomics; it
   stops when the child exits (flag + join with a 1 s bound, else detached).
   `wait_within` only READS atomics — nothing the sampler does can delay the
   ceiling.
2. **Bounded snapshot.** One snapshot = the tree under the root plus
   previously SEEN identities still alive. Limits: ≤ 4096 processes, ≤ 2
   buffer-grow retries (macOS `proc_listchildpids`, which returns a COUNT),
   100 ms wall deadline checked between processes. Result is
   `Complete(Vec<Proc>) | Partial | Unavailable` — hitting any limit, an
   unreadable root, or an FFI error on the root is `Partial`/`Unavailable`;
   an ENOENT/ESRCH/EPERM on a non-root pid just skips that pid.
   - Linux: `fs::read("/proc/<pid>/stat")` as BYTES, split at the last `)`;
     ppid [1], utime/stime/cutime/cstime [11..=14] (signed, clamp ≥ 0),
     starttime [19]; ticks→ns via `sysconf(_SC_CLK_TCK)` FFI (≤ 0 → 100).
     Only `stat` is read. Descendants found via ppid over one `/proc` scan.
   - macOS: `proc_listchildpids` BFS; `proc_pid_rusage(RUSAGE_INFO_V2)` into a
     160-byte size-asserted `repr(C)` struct; mach ticks → ns via
     `mach_timebase_info` (u128).
   - Identity = (pid, start time) (stat field 22 / `ri_proc_start_abstime`).
3. **Gain, with reaping compensation.** Credit only between two consecutive
   `Complete` snapshots; the first `Complete` after start or after any
   `Partial`/`Unavailable` is a new baseline (no credit).
   `gain = Σ max(0, Δcpu(id))` over identities in both
   `+ cpu(id)` for identities that STARTED after the previous snapshot
   `− transferred`, where for every tracked identity that vanished, its last
   known total is subtracted from its recorded parent's Δ (floored at 0).
   So CPU already credited while a child was alive is not credited again when
   it is reaped — however late, at any depth (the parent's own last-known
   total already includes what it reaped). Residual heuristic, stated in the
   ADR: a child that lived < one interval and was reaped late is credited
   when reaped (at most one interval of its work). No "≤ N×" claim.
   Gain ≥ 10 % of one core over the window → busy, stamped with the window's
   START (a burst buys one window).
4. **Guarantee scope (orphans).** A descendant reparented away (init/launchd)
   keeps counting only if it was seen in a `Complete` snapshot before it was
   reparented. A worker reparented before first discovery is invisible —
   documented; earlier sampling narrows, cannot close, the race.
5. **Rule & state.** `Activity` lock-free: `base: Instant`, `AtomicU64`
   `last_out`, `last_busy`, `last_measured` (time of the last `Complete`
   window), `AtomicU32` `rate_milli`, `AtomicU8` `cpu_state` (Off | Waiting |
   Measuring | Unavailable). `still_for() = min(quiet, since_busy)` is passed
   to the unchanged `judge`; `Killed` gains `cpu: CpuVerdict` =
   `NotSampled | MeasuredIdle | Unmeasured(for_secs) | BusyAtKill(milli)`;
   `quiet_secs` stays the true output silence. `judge`, `wait_within`'s
   signature, `k8s.rs:347`, `status_within_secs`, the six `judge` tests stay.
6. **Freshness.** A displayed rate expires after 2 intervals without a new
   `Complete` window → the display drops to "CPU unmeasured" (never keeps
   showing busy). "measured idle" is claimed only when every window in the
   last budget-length of time was `Complete` and under threshold; otherwise
   the verdict is `Unmeasured`.
7. **What people see** (≤ 80 cols; width clamped to 80 when `COLUMNS`
   unset; colour optional, only the idle countdown past ⅔, after truncation,
   via `ui::colors_enabled()`):
   - Region: busy `· quiet 2m10s · ~3.9 cores`; measured idle
     `· quiet 2m10s · idle 40s/2m00s`; unmeasured `· quiet 45s/2m00s` (today).
   - Heartbeat: unchanged prefix `… NAME still running: T, last output Q ago`
     + `, busy ~3.9 cores` | `, CPU idle 40s` | `, CPU unmeasured`. First
     beat states the rule "no output and under 0.1 core of CPU"; when not
     sampled: "; CPU not sampled here, silence alone counts".
   - Silence kill: `MeasuredIdle` → "X printed nothing and did no measurable
     CPU work (< 0.1 core) for 2m00s …"; otherwise today's text plus
     "(CPU not measured)" when it was attempted.
   - Ceiling kill with `BusyAtKill` → names the busy loop / end-only reporter
     and `amont.idleCpuCredit false`.
8. **Trade-offs (ADR):** silent spinning hangs (busy loop, polling watcher,
   spinning seen orphan) answer to the ceiling; daemon work outside the tree
   and pre-discovery orphans are not counted; figures are approximate.

## Phases
- [x] Phase 1 — plan commit; ADR `adr/00NN-busy-is-not-stuck.md`, key
  `hooks.liveness` (scope `hook-binary`), citing `platform.windows-parity`
  and decisions:ADR-0020; `aval heads --write`, `aval check`.
- [x] Phase 2 — `src/proctree.rs`: `Proc { pid, ppid, start, cpu_ns }`,
  `parse_proc_stat(&[u8], tick_ns)`, `trait ProcSource { fn snapshot(&mut
  self, root, seen, limits) -> Snapshot }` (real linux/macos impls; test fake),
  `Tracker` (seen set, parents, last totals) with pure `observe(now,
  Snapshot) -> Option<Window { start, milli }>` implementing baseline,
  compensation and resets; FFI in `mod macos`/`mod linux` (`staged_only.rs`
  style, size assert). Pure parts uncfg'd, tested on every OS.
- [x] Phase 3 — lock-free `Activity`; sampler thread lifecycle in
  `run_observed`; `CpuVerdict`/`Killed`; `say_timed_out`;
  `Settings.idle_cpu: OnceLock<bool>` (`amont.idleCpuCredit`, `boolean_or`).
- [x] Phase 4 — slot attach guard (detach on drop); region/heartbeat,
  freshness; docs/configuration.md 511-556; `agents_md.rs` (+ `amont
  agents-md`; changes every consumer's generated block on regeneration);
  `idle_timeout` doc comment.
- [x] Phase 5 — CI: clippy `-D warnings` for `aarch64-apple-darwin` and
  `x86_64-pc-windows-gnu` targets (and at MSRV); FFI smoke on an arm64 macOS
  runner (`macos-14`).
- [ ] Phase 6 — verify → rehearse → PR → merge-when-green.
- [ ] 🧑 decision (asked at the end): release amont and bump duro-app's pin.

## Verification
- **Unit, deterministic (fake `ProcSource`, all OSes):** parse fixtures (comm
  with `)`, spaces, newline, non-UTF-8; negative cutime; pid reuse by start);
  first Complete = baseline; Partial/Unavailable → next Complete re-baselines,
  no credit; limit exhaustion (4097 procs, retry cap, deadline via injected
  clock) → Partial, no credit; new-since-previous full credit; exec drop → 0;
  **delayed reaping** (child burns, sleeps 5 windows, reaped → no credit in
  the reaping window); **deep tree** (6 levels reaped bottom-up late → no
  credit beyond the work windows); unseen short child reaped late → ≤ one
  interval credited; orphan seen-before-reparent keeps counting,
  unseen-before-reparent does not; freshness expiry → Unmeasured; verdict
  selection (MeasuredIdle only with all-Complete windows).
- **FFI smoke** (linux + arm64 macOS CI): a child burning 0.5 s shows ≥ 0.4 s
  while alive and via its parent after reaping.
- **timing.rs fixtures** (unix, `alone()`): every fixture has a PERSISTENT
  root (the script waits on its workers; never exits early unless the test is
  about that), BOUNDED workers (each loop self-terminates ≤ 20 s), EXPLICIT
  cleanup (script `trap 'kill 0' EXIT`; test kills the recorded process group
  afterwards), and an INDEPENDENT watchdog: the test spawns amont with
  `process_group(0)` and a watchdog thread that SIGKILLs the whole group and
  fails the test at 60 s — no assertion relies on a blocking call returning.
  `idleTimeout 2`, `timeout 8`:
  - busy-silent root loop → ceiling (≥ 7 s, < 60 s), busy text;
  - fork-churn (root spawning ~50 ms CPU children and waiting each) →
    ceiling;
  - orphan, synchronized: the intermediate waits (bounded, 10 s) until the
    worker's pid appears in `AMONT_CPU_TRACE` (a diagnostic env var: the
    sampler appends `pid start` per Complete snapshot) before exiting → worker
    keeps the check busy → ceiling;
  - `exec sleep 300`, `idleTimeout 1` → silence "did no measurable CPU work",
    < 10 s;
  - `idleCpuCredit false` + busy root → silence at the budget, today's text.
  - live.rs: region/heartbeat strings ≤ 80 cols; heartbeat prefix
    byte-identical; stale-rate expiry.
- **Pilot:** local amont in a duro-app worktree, DEFAULT budget: `amont
  rehearse --wait` passes (was killed twice), log shows `busy ~N.N cores`;
  then a real `git push` of a throwaway branch lands and is deleted.
- `cargo test --workspace`, clippy (3 targets), fmt, MSRV, `check-no-deps.sh`,
  windows job.

## Decision log
- 2026-09-29 — Signal = process-tree CPU incl. reaped children, no spawn, no
  crate; `ps` sums rejected (non-monotonic under fork-per-file; spawn could
  stall the ceiling); vitest flag rejected (tool-specific).
- 2026-09-29 — Review round 2 (person): sampler moved to its own thread with
  hard limits and a `Complete|Partial|Unavailable` result; reaping
  compensation replaces the unsupported "≤ 3×" bound; policy renamed to
  "insufficient CPU activity (< 0.1 core)", figures approximate; freshness
  and measured-vs-unmeasured verdicts; orphan guarantee limited to seen
  descendants, test synchronized via `AMONT_CPU_TRACE`; timing fixtures get a
  persistent root, bounded workers, cleanup and an external watchdog.

- 2026-09-29 — Implementation deviations, each forced by what the code
  showed:
  - "Measured idle" cannot cover the whole budget: sampling starts only
    after `min(30 s, budget/3)` of silence, so the claim was unreachable as
    specified. `CpuVerdict::MeasuredIdle(secs)` names the unbroken measured
    span ("did no measurable CPU work in the last 1m30s of it") and claims
    nothing about the rest.
  - The ceiling message's busy explanation is keyed to "output silence ≥ the
    idle budget" (`Killed.idle_secs`), not "≥ 30 s": the old rule told a
    busy check that never printed that it was "still printing".
  - The region's width default is 80 (was 100) when `COLUMNS` is unset.
- 2026-09-29 — Found while hardening the timing fixtures: macOS can hold a
  freshly written executable in execve for several seconds (under 1 ms of
  CPU, no children, first line never run) — worst right after a test run has
  written hundreds of files. It failed the busy fixtures intermittently in
  full workspace runs. Such a process is genuinely idle, so the rule is
  right; the fixtures now run `sh ./x.sh` so the fresh file is read, not
  exec'd, and a failing busy fixture prints the `AMONT_CPU_TRACE` it ran
  under (the trace now records ppid and CPU per process and each window).

## Outcome
