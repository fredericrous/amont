---
status: active
branch: feat/waiting-is-not-stuck
repos: [amont]
adrs: [ADR-0001, ADR-0008, decisions:ADR-0020, decisions:ADR-0021]
---
# Waiting is not stuck: a pre-commit gate that survives a loaded machine

## Review panel
👉 **Decide:** when a Heavy check gets no host slot within `amont.timeout`: run it unqueued with a note (recommended, as written) or fail it.
📍 amont · plan reviewed, nothing built · next: approve, then Phase 1 (plan commit + ADR-0009). Panel: backend, lang:rust, tui, unix.
**Changed by review:** slot dir is a fixed per-user path with an owner check, not `$TMPDIR`; "unmeasured CPU" gets a bounded extended budget, never the bare ceiling; the retry shares one ceiling, matches whole phrases, builtins only.
📄 Full reviews: [2026-10-09-waiting-is-not-stuck.reviews.md](2026-10-09-waiting-is-not-stuck.reviews.md)
**Verdicts:** round 1: 3 approve-with-changes, 1 rework (rust). Round 2: rust and backend approve-with-changes; backend bound the final body in two more passes (approve). Two low, optional tightenings of the real-load fixture are left in the full reviews.

## Context
On a machine running several worktrees and relais workers, amont's
pre-commit gate on the happier rework (Rust: cargo-fmt, clippy) is killed
at the 120 s silence budget. The log of one such commit:

```
… clippy still running: 1m00s, last output 58s ago, CPU idle 58s (killed after 2m00s …)
    Blocking waiting for file lock on package cache
    Blocking waiting for file lock on build directory
```

Clippy was queued behind another cargo holding the lock. A lock wait prints
one line and uses no CPU, which is exactly what ADR-0008 (`hooks.liveness`)
calls stuck. Load makes the holder slower, so the wait outlives the budget;
the misclassification is the defect. Two things compound it under load:
CPU sampling degrades to the harsher silence-only rule on one incomplete
snapshot, and every worktree's pre-commit clippy is a cold `--workspace`
check contending for the same locks.

Facts (origin/main caa1387):
- `run_observed` (`crates/amont-runtime/src/hooks/common.rs:675-732`) pipes
  both streams; one reader thread per pipe reads 4096-byte chunks and only
  calls `activity.touch()` then `on_output(bytes)` (:708-714). Nothing reads
  line content; `Killed` (:618-632) is `Copy` and carries no text.
  `status_streamed` (:872-887) is the wrapper every streamed tool goes
  through (`bounded_success` :1054 → `run`/`run_tool` :1066; run-tests-js
  `run_tests.rs:489`; declared externals `manifest.rs:814`); it sets
  `CARGO_TERM_COLOR=always` when the display is live (:879-884), so cargo's
  status lines carry CSI colour codes on a terminal.
- `wait_within` (:916-960) polls every 25 ms, restarts its own `started`
  on every call (:922), passes `activity.still_for()`
  (= `quiet_for().min(since(last_busy))`, :477-480) to the pure `judge`
  (:970-987): ceiling first, then `quiet >= idle`. The kill is `child.kill()`
  on the direct child only. With `timeout 0` only the silence rule stops a
  check (:928-930).
- `CpuSampler` (:739-823): starts after `first = (budget/3).min(30s)` of
  OUTPUT silence (`quiet_for`), samples every `(budget/4).clamp(250ms,10s)`;
  `stop()` waits ≤ 1 s (:759-768). `Tracker::observe` (`proctree.rs:263-278`)
  turns ANY non-`Complete` snapshot into `Observation::Unmeasured` and drops
  `prev`, so recovery needs two good samples; `Activity::record(Unmeasured)`
  sets `measured_since=0`, state `Unavailable` (:561-564); `verdict()` says
  `Unmeasured` for `Waiting`, `Unavailable` and a stale rate (:518,529), and
  the silence kill still fires (judge ignores CPU state). `Limits`
  (`proctree.rs:74-89`): 4096 procs, 2 retries, **100 ms** wall deadline →
  `Partial`. `proctree.rs:60-61` forbids an error in the "stuck" direction.
- Display: `live::row_of` (`live.rs:357-381`), `region()` (:425-483, notes
  only once `quiet >= 30s`, truncated at the width :472-474, `human_secs`
  :409-415), non-TTY `beat_line()` (:552-609, prefix
  `… NAME still running: T, last output Q ago`, first beat prints the
  configured budgets :569-583; a heartbeat is a ~190-char log line),
  `budgets()` (:391-396). Kill text: `say_timed_out` (common.rs:994-1050)
  through `fail()` (:1224), `Why::{Ceiling,Silence}`.
- Stages: pre-commit fans every check out on its own thread, no pool
  (`dispatch.rs:304-335`, `run_stage_traced` :562-613, clock at :583 just
  before `check.run`); pre-push is serial (:1008-1187, clock :1125). The
  rehearsal worker runs the normal pre-push in a snapshot (`rehearsal.rs:493-649`).
  amont's own test suite runs under its own `pre-push-cargo-test`
  (`tests/common/mod.rs:115-118`); `Repo::new` sets `amont.quiet never`
  (:77); `alone()` (`timing.rs:42-46`) is a mutex inside one test binary.
- `Builtin` (`check.rs:458-468`): name, stage, scope, severity, run, fix,
  reach. No weight. Scoped push gates = the four test suites
  (`dispatch.rs:1288-1300`, `registry.rs:513-557`).
- Locks today: `tree_cache.rs:204-241` declares `flock` via `extern "C"`
  (unix; Windows takes no lock) over `$GIT_DIR/amont-cache/<gate>/.lock`,
  `create_dir_all` with no owner check. No host-wide state dir exists.
  `std::env::temp_dir()` follows `$TMPDIR`, per-session on macOS
  (`/var/folders/…`) and per Claude Code session
  (`${CLAUDE_CODE_TMPDIR:-/tmp}/claude-<uid>`). `std::fs::File::lock` is
  1.89+, MSRV 1.74.
- Clippy: `rust_tools.rs:387-431`, argv
  `clippy --workspace --all-targets --all-features -- -D warnings`, run with
  `current_dir` = each `cargo_roots` entry (:80-88, nearest `Cargo.toml`
  per staged file via `cargo_root_for` :64-78). Without `--workspace`,
  cargo selects the package of the cwd (cargo book, Package Selection); at
  a virtual manifest it selects the default members. `RUST_PATHS` (:32-41)
  lists the manifests/pins whose change re-judges.
- Config: readers `integer_or`/`boolean_or` (`config.rs:344,328`) cached in
  `Settings` `OnceLock`s (:196-211); `resolve` (:103-118) applies
  system < global < policy < local; accessors `idle_timeout`,
  `check_timeout`, `idle_cpu_credit` (common.rs:347-389); `amont.conf`
  allow-list `manifest::SETTABLE` (`manifest.rs:366-414`; `timeout` is in,
  `idleTimeout`/`idleCpuCredit` are not); docs sections
  `docs/configuration.md:524-612`; `docs/custom-checks.md:210-215` lists the
  settable keys; `agents_md.rs:31-32,67-73` prints the two clock values;
  `spawn_budget.rs:80,225-235` caps git spawns (≤ 26 / ≤ 35) and says one
  read per documented key is the intended way it grows.
- Retries: only transient spawn errors (`git.rs:16-42`); no check is retried.
- FFI style: `cfg`-gated `mod` with `extern "C"` + size assert
  (`proctree.rs:336,398,528`); `SUPPORTED = cfg!(linux|macos)` (:334).
- Rules in force: `deps.budget` (no crate), `platform.windows-parity` (one
  suite, divergences stated), `change.a-deferral-names-its-ceiling`
  (`holds-until:`), `cli.robust.timeouts` (every wait bounded),
  `cli.config.precedence`, `resources.lifecycle-is-scoped`,
  `purity.calculations-from-actions`, `errors.typed-values`,
  `guidance.generated-block-rides-the-change` (`amont agents-md` in the same
  commit). CI (`.github/workflows/ci.yaml`): fmt, clippy `-D warnings` on
  host + `make lint-cross` (aarch64-apple-darwin, x86_64-pc-windows-gnu,
  x86_64-unknown-linux-gnu), `cargo +1.74.0 check`, `check-no-deps.sh`,
  `make test` on ubuntu+macOS, windows `cargo test`.
- Real wait lines (verified from the binaries on this machine): cargo, on
  **stderr**, `    Blocking waiting for file lock on build directory` /
  `… on package cache`; uv, on stderr, `Waiting to acquire <kind> lock for
  \`<path>\``. rustup and pnpm print none.

## Goal
A check is stuck when it is silent, **not in a declared wait**, and under
0.1 core of measured CPU for the whole silence budget, where the budget
stretches with the machine's load; while CPU measurement is attempted but
incomplete, a bounded extended budget applies, never no budget. Heavy
checks queue host-wide instead of thrashing, with their clocks paused.
Pre-commit clippy judges the staged packages. A check killed while it
looked like it was waiting is retried once inside the same ceiling. Every
message still claims only what was observed (ADR-0008).

### Worst case for a genuinely stuck silent tool (defaults)
| situation | today | after |
|---|---|---|
| CPU measured idle | 120 s | 120 s × load factor (≤ 4) = ≤ 480 s |
| CPU unmeasured (partial, stale, Windows off) | 120 s | ≤ 480 s (`idle × idleLoadScale`), with `timeout 0` too |
| in a declared cargo/uv lock wait | 120 s | `lockWait` 600 s (0: ceiling; ceiling off: 480 s) |
| retried after a wait-like last line | n/a | both attempts inside ONE `amont.timeout` |
| queued for a host slot (Heavy checks) | n/a | ≤ `amont.timeout` (0 → 3600 s), on top of the run; pre-push queues add up across its serial checks |
| ceiling | 3600 s | 3600 s, unchanged |

## Decide
**What a Heavy check does when no host slot frees within `amont.timeout`.**
- Run it anyway, unqueued, with one stderr note (recommended: the queue is
  a courtesy to the machine, not a gate on the code; failing a commit
  because another repository's suite is slow teaches `--no-verify`).
- Fail the check with its own `Why::Queued` text and the usual exit status
  (the unix reviewer's position: a bound that is never enforced is not a
  bound, and the person learns the host is oversubscribed).
The plan is written for the first; the second is one match arm.

## Non-goals
Naming the lock HOLDER (cargo does not print it; `lsof` is a spawn — a
later `holds-until`). FIFO fairness in the host queue. Windows CPU/load
sampling and Windows host slots (silence-only and unqueued there, stated).
Re-linting in-workspace dependents of a staged crate (decided 2026-10-09:
CI's workspace clippy owns that). Tree gates (`tree_run`) keep their own
deadline model. Process-group kill of a check's grandchildren.

## Behaviour
1. **Wait markers pause the silence clock** (common.rs, new `hooks/wait.rs`).
   The **stderr** reader in `run_observed` frames complete lines (carry an
   unterminated tail, cap 4 KiB, a longer line is dropped unmatched), strips
   CSI escape sequences (pure `strip_csi`), and hands each to a pure
   `wait::marker(line: &str) -> Option<WaitKind>`; `WaitKind` is
   `CargoLock(CargoLockWhat)` (`BuildDirectory | PackageCache | Other`) from
   `Blocking waiting for file lock on <what>` and `UvLock` from
   `Waiting to acquire ` … ` lock for `. stdout is never matched (cargo's
   status goes to stderr; a test printing the words must not pause its own
   clock). A match calls `Activity::begin_wait(kind)`; a later chunk on
   either pipe ends it (`end_wait` = clear + `touch`) unless that chunk is
   itself another marker line, in which case the wait continues with its
   original start (cargo prints `package cache` then `build directory`;
   the table's 600 s bounds the whole run of consecutive markers, not each). `quiet_for()` stays
   the true output silence for the displays; a new `silence_for()` returns
   0 while waiting and `quiet_for()` otherwise, and it is what `still_for`
   and the sampler's start gate read. `judge` gains `waited: Option<Duration>`
   judged against `amont.lockWait` (seconds, default 600, range 0..=86_400,
   settable from `amont.conf` like `timeout`; `0` = until the ceiling, and
   when the ceiling is also off, the extended budget of Behaviour 3 applies
   instead, so nothing is unbounded): `Why::Waited(kind, budget)`. Ceiling
   still wins. Kill text: "`clippy` waited 10m00s for the cargo lock on the
   build directory and was killed — another cargo holds it (a second
   worktree, rust-analyzer, a build in another session). `git config
   amont.lockWait <secs>` raises it (0: until the ceiling)." Region row:
   `· cargo lock 1m30s/10m`; heartbeat keeps its prefix and appends
   `, waiting for the cargo build-directory lock 1m30s (amont.lockWait 10m00s)`.
   `Killed` gains `last_line: Option<String>` (last non-empty framed stderr
   line, CSI stripped, ≤ 200 chars, in a `Mutex<String>` on `Activity`), so
   it becomes `Clone` not `Copy`.
2. **Unmeasured gets a bounded extension, not the ceiling** (proctree.rs,
   common.rs). `Tracker` keeps `prev` across a `Partial` and returns a new
   `Observation::Skipped` (recorded as no change to `Activity`); the next
   `Complete` yields a `Window { gapped: true }` spanning the gap. A gap
   window may set `last_busy` (busy is still busy) and **resets
   `measured_since` to its own end**, so `MeasuredIdle` never claims the gap
   (proctree.rs:60-61: no error in the stuck direction); an unbroken run of
   complete windows is claimed as today. `BusyAtKill` maps to
   `CpuGate::Measured`. `prev` is dropped only on `Unavailable` or after 3 consecutive
   `Partial`s (→ `Unmeasured`). `Limits.deadline` 100 ms → 500 ms (under the
   sampler's 1 s stop bound, which keeps its comment true). `judge` gains
   `cpu: CpuGate { NotSampled | Measured | Unmeasured }` from
   `Activity::verdict()`: `NotSampled` (platform or knob off) and `Measured`
   (`MeasuredIdle`) use the silence budget as today; `Unmeasured` (Waiting,
   Unavailable, stale) uses the **extended budget** `idle × idleLoadScale`
   (480 s by default, bounded even with `timeout 0`), `Why::Silence` naming
   it. Region under Unmeasured: `· quiet 45s/8m (CPU unmeasured)`; heartbeat:
   `, CPU unmeasured — extended budget 8m00s`; the first beat states the
   budget that applies to this check, not the configured one. Diagnostic
   `AMONT_CPU_MAX_PROCS=<n>` overrides the process cap (documented beside
   `AMONT_CPU_TRACE`) so a fixture can force `Partial`.
3. **Load-aware budget** (new `crates/amont-runtime/src/load.rs`). Pure
   `scaled_budget(idle, load1, cores, cap) -> u64` =
   `idle × clamp(load1/cores, 1, cap)`, rounded, then `min(ceiling)` when the
   ceiling is on. `cap` = `amont.idleLoadScale` (default 4, `1` disables,
   range 1..=16; a **host key**, read from global/system config only, never
   from a repository or `amont.conf`). Host keys get their own reader,
   `config::host_integer_or`: a second cached `git config --show-scope
   --list` scan keeping only `system|global` scopes (the existing cache at
   `config.rs:148-162` keeps above-policy scopes and runs only with a
   policy), one spawn counted in `spawn_budget.rs`; a value found at a
   lower scope is ignored with one warning naming the key and `--global`.
   The environment override the precedence sentence names is
   `AMONT_HOST_SLOTS` / `AMONT_IDLE_LOAD_SCALE` (diagnostic and fixtures).
   Reader for the load: `getloadavg(&mut [f64;3], 3)`
   FFI (linux/macos, `cfg` module, factor 1 elsewhere) +
   `std::thread::available_parallelism()`. `wait_within` re-reads the load
   every 5 s and passes the effective budget to `judge`;
   `Activity.budget_secs` (u32) publishes it for the displays: region
   `· quiet 45s/8m (load ×4)`, heartbeat `, budget 8m00s (load avg 31.2 on
   8 cores, ×4 — amont.idleLoadScale)`; the first beat and the kill text
   name the budget that applied. `live::budgets()` keeps the configured
   values; the row applies the scale.
4. **Host-wide slots for heavy checks** (new `crates/amont-runtime/src/host_slots.rs`).
   `Builtin` gains `weight: Weight { Light, Heavy }`; Heavy = checks that
   compile or execute the product: `pre-commit-clippy`, `pre-push-cargo-test`,
   `pre-push-run-tests-js`, `pre-commit-typecheck`, `pre-commit-pyright`,
   `pre-push-pytest`, `pre-push-go-test`, `pre-commit-go-vet` (registry test
   `HEAVY` pins the list). `amont.hostSlots` (default `cores/4`, min 1; `0`
   disables; range 0..=64; a host key like `idleLoadScale`: global/system
   only, so two repositories cannot disagree on the slot count).
   `run_stage_traced` (pre-commit thread) and the pre-push loop acquire a
   slot before `check.run` for a Heavy check: `host_slots::acquire(n, budget)
   -> Acquired { Slot(File) | Unqueued(Reason) }`, `Reason =
   { Disabled | Nested | DirUnsafe | Windows | TimedOut }`. Acquisition tries
   `flock(LOCK_EX|LOCK_NB)` on `<dir>/slot-<i>` for `i in 0..n`, sleeps
   200 ms between rounds, bounded by `amont.timeout` (`0` → 1 h), heartbeat
   printing meanwhile. `<dir>` = `AMONT_SLOT_DIR` if set (diagnostic, for
   fixtures), else `$XDG_RUNTIME_DIR/amont-slots` on Linux when set, else
   `/tmp/amont-slots-<uid>` (`getuid` FFI; a fixed path, NOT `temp_dir()`,
   which follows the per-session `$TMPDIR`). After `create_dir_all` with
   mode 0o700, `symlink_metadata` must say: not a symlink, owner == uid,
   mode 0700; otherwise `Unqueued(DirUnsafe)` with one stderr note. A held
   slot exports `AMONT_HOST_SLOT=held` into the check's environment, and an
   amont that sees it skips acquisition (`Unqueued(Nested)`: amont's own
   `pre-push-cargo-test` runs fixtures that run amont). The test harness's
   command builder (`tests/common/mod.rs` `hook`/`hook_at`/`hook_watched`)
   sets `AMONT_HOST_SLOT=held` in the environment of every amont it runs
   (not git config: a repository value is ignored by design); the slot and
   load fixtures alone `env_remove` it and set their keys through
   `GIT_CONFIG_GLOBAL` pointing at a per-test file. The slot is RAII: released on drop and by the kernel if amont dies. Windows:
   `Unqueued(Windows)` (the `tree_cache` divergence, stated). Display: `Slot`
   gains `queued: Option<Instant>`; region `· queued 42s (slots 2/2)`;
   heartbeat keeps `still running:` and appends `, queued 1m00s for a host
   slot (amont.hostSlots 2)`. The check's kill clocks start at spawn, after
   the slot; `started` for `record_durations` moves after acquisition so the
   duration record measures the check, not the queue. A queue timeout
   (`Unqueued(TimedOut)`) runs the check anyway, unqueued, with one stderr
   note — see Decide. Fairness is polling, not FIFO: `holds-until:` more
   than a handful of gates contend on one host.
5. **Clippy judges the staged packages** (rust_tools.rs). For each
   `cargo_roots` entry of a staged `.rs`, drop `--workspace`: cargo selects
   the package of the cwd (a member root) or the default members (a virtual
   root), so no manifest parsing and no `-p` name collision. Keep
   `--workspace` for every root when any staged path matches `RUST_PATHS`
   other than `.rs` (manifest, lockfile, `clippy.toml`, `rust-toolchain*`) or
   under `--all-files`. Decided 2026-10-09: dependents are CI's; the ADR and
   CHANGELOG say so.
6. **One retry after a wait-like kill** (common.rs `status_streamed`, which
   gains a `Retry { Once | Never }` argument: builtins pass `Once`; declared
   externals (`manifest.rs:814`) pass `Never`, since a user's command may
   not be safe to run twice). Pure `wait::looks_like_wait(line) -> bool` matches whole
   phrases only (`waiting for file lock`, `waiting to acquire`, `blocking
   waiting`, `waiting for lock`), never the bare word `lock`. When the first
   attempt ends in `TimedOut` with `Why::Silence`, `last_line` looks like a
   wait, and the ceiling has at least one silence budget left: the first
   kill prints only "`clippy` was killed while it looked like it was waiting
   (`<line>`); retrying once with 48m left." (no `fail()` line, so a run that
   passes has no failure text), and the second attempt runs under the
   **remaining** ceiling (`wait_within` takes a deadline, not a fresh
   `started`). A second kill is final and prints `say_timed_out`. Never after
   `Waited` (a deliberate budget) or `Ceiling`. No knob.
7. **ADR-0009 `adr/0009-waiting-is-not-stuck.md`** supersedes ADR-0008's
   `hooks.liveness` choice at scope `hook-binary`: "Stuck means silent, not in
   a declared lock wait, and under 0.1 core of measured CPU for the whole
   load-scaled silence budget; a bounded extended budget where measurement
   is attempted but incomplete; silence alone where it is not attempted".
   New keys in `.adr.yaml`: `hooks.host-concurrency` (how many heavy checks
   one host runs at once, scope `hook-binary`) and `hooks.clippy-scope`
   (what pre-commit clippy judges, scope `pre-commit`), decided in the same
   ADR. `aval heads --write`, `aval check`. Docs: new sections
   `amont.lockWait`, `amont.idleLoadScale`, `amont.hostSlots` in
   `docs/configuration.md` (after `amont.timeout`), each host key's section
   stating "read from global or system config only; a repository value is
   ignored with one warning", one precedence sentence
   (flag/env diagnostic > global > system > default for host keys;
   `cli.config.precedence`); the "Busy is not stuck" subsection gains
   "Waiting is not stuck" and the worst-case table; `docs/custom-checks.md`
   settable list (+ `lockWait`, and the five keys it already misses);
   `agents_md.rs` names the three keys and the lock-wait rule (regenerate
   with `amont agents-md`, staged in the same commit); CHANGELOG `## v1.48.0`
   with `### Added` / `### Changed`. `spawn_budget.rs` caps grow by the
   reads this adds on the commit path (+1 for the host-key scan, +1 for
   `lockWait` when a tool runs), with the comment it asks for.

## Phases
- [x] Phase 1 — plan commit; ADR-0009 + `.adr.yaml` keys; `aval heads --write`; `aval check`.
- [x] Phase 2 — Behaviour 1: `wait.rs` (markers, framing, `strip_csi`), `Activity` wait state + `silence_for`, `judge` input, `amont.lockWait`, messages, region/heartbeat.
- [x] Phase 3 — Behaviour 2: `Observation::Skipped`, partial tolerance, 500 ms deadline, `CpuGate` + extended budget in `judge`, `AMONT_CPU_MAX_PROCS`, messages.
- [x] Phase 4 — Behaviour 3: `load.rs`, host-key reader, `amont.idleLoadScale`, scaled budget in `wait_within`, displays.
- [x] Phase 5 — Behaviour 4: `host_slots.rs`, `Weight`, registry `HEAVY` test, acquisition in both stage runners, `AMONT_HOST_SLOT`/`AMONT_SLOT_DIR`, harness env opt-out, `amont.hostSlots`, queued display.
- [x] Phase 6 — Behaviour 5: drop `--workspace` for member roots, fallbacks.
- [x] Phase 7 — Behaviour 6: `last_line` in `Killed`, `looks_like_wait`, one retry in `status_streamed` under the remaining ceiling.
- [x] Phase 8a — docs, `agents_md.rs` + `amont agents-md`, CHANGELOG v1.48.0, spawn budget; `make check`, `make lint-cross`, MSRV check, `check-no-deps.sh`; pilot; implementation review.
- [ ] Phase 8b — PR; merge-when-green.
- [ ] Phase 9 — release v1.48.0 (`tag-release`), reinstall locally (`amont install --force` in happier), verify the next happier commit under load.

## Verification
- **Unit (pure, every OS):** `wait::marker` on the two real cargo lines, the
  same lines with `CARGO_TERM_COLOR=always` colour codes (captured from the
  real cargo), the uv line, a `Checking foo` line (None), the marker text
  mid-line (None); framing across chunk boundaries (marker split in two
  reads; a 5 KiB line without newline is not matched and does not grow);
  `judge` with `waited` under/over `lockWait`, `lockWait 0` with ceiling on
  (no kill) and ceiling off (extended budget kills), ceiling winning;
  `judge` with `CpuGate::Unmeasured` killing at `idle×scale` and never
  before, `NotSampled` and `Measured` unchanged (the six existing `judge`
  tests stay); `Tracker`: `Complete, Partial, Complete` → `Skipped` then a
  `Window { gapped: true, start = t0 }` that sets `last_busy` when busy and
  resets `measured_since` to its end (the `MeasuredIdle` span excludes the
  gap), `Partial×3` → `Unmeasured`, `Unavailable` → `Unmeasured`; two
  consecutive markers keep one wait start; `host_integer_or` ignores a
  local-scope value with one warning and honours `AMONT_HOST_SLOTS`;
  `scaled_budget` (load 1× → idle; 3.9 over 8 cores → ×1; 31.2 over 8 →
  cap ×4; cap 1 → idle; ceiling clamps); `looks_like_wait` (`test_lock.py::x`
  → false); `Weight` list test; `host_slots::dir()` honours
  `AMONT_SLOT_DIR`, refuses a symlink / foreign owner / 0755 dir
  (`DirUnsafe`), skips under `AMONT_HOST_SLOT=held` (`Nested`); every new
  **region** row with the longest Heavy name (`pre-push-run-tests-js`) fits
  80 columns with the `amont.` key or the figure intact (heartbeats are log
  lines and are not width-bounded); first-beat text under Unmeasured and
  under load names the budget that applies.
- **timing.rs fixtures** (unix, `alone()`, watchdog 60 s, `idleTimeout 2`,
  `timeout 12`; the harness builder also sets `AMONT_IDLE_LOAD_SCALE=1` so
  the existing kill-time bounds (`timing.rs:381-411`) do not stretch on a
  loaded host; the slot, load and CPU-unmeasured fixtures alone
  `env_remove` those two variables, set `AMONT_SLOT_DIR` to a per-test dir and their host keys
  through a per-test `GIT_CONFIG_GLOBAL` file, so they pass both under a
  bare `cargo test` and under amont's own pre-push):
  - fake tool prints the cargo line on stderr, sleeps 6 s, prints `Checking x`,
    exits 0 → passes in < 10 s; output has `cargo lock`.
  - same, `lockWait 2`, sleeps 30 → killed < 6 s, text names "waited 2s for
    the cargo lock on the build directory" and `amont.lockWait`.
  - the cargo line on **stdout** only, sleeps 30 → killed at the 2 s budget
    (stdout is not matched).
  - `AMONT_CPU_MAX_PROCS=0` + `exec sleep 30`, `idleLoadScale 4`, `timeout 0`
    → survives 2 s, killed at ≈ 8 s, text has "CPU unmeasured" and the
    extended budget; < 60 s with the ceiling off.
  - `hostSlots 1`, `AMONT_SLOT_DIR` shared by two `hook_watched` amont runs
    in two repos, `pre-commit-clippy` with a fake `cargo` on PATH that
    sleeps 3 s → the second's `Checking` stamp is ≥ 3 s after the first's
    and its output has `queued`; the same two runs with the dir pre-created
    0755 by the test → both run at once and each prints the `DirUnsafe` note.
  - retry, run as the builtin `pre-commit-clippy` with a fake `cargo` on
    PATH and a staged `.rs` (declared externals never retry): the fake
    prints `waiting for lock on x` (a line `marker` rejects and
    `looks_like_wait` accepts; a unit test pins both) then sleeps 30 on its
    first run (a marker file), exits 0 on the second → passes in < 8 s,
    output has "retrying once" exactly once and no `fail()` text; marker
    absent on both runs → fails with the silence text, exactly one
    "retrying once", total < 12 s (one ceiling); the same script declared
    in `amont.conf` → fails at the budget with no "retrying once".
  - clippy scope: fixture workspace `crates/{a,b}`, stage `crates/a/src/lib.rs`,
    fake `cargo` records argv and cwd → no `--workspace`, cwd `crates/a`;
    stage `Cargo.lock` too → `--workspace`.
- **Load scale, real:** `idleLoadScale 4`, `idleTimeout 2`, a `burn` tree of
  `4 × cores` workers held by a separate process for the test's duration,
  fixture `exec sleep 30`. The 1-minute average rises as
  `4·(1−e^(−t/60))` per core, so the test does not assume a factor: it
  reads `getloadavg` when amont starts and asserts the kill came no earlier
  than `2 s × clamp(load1/cores, 1, 4) − 0.5 s` and that the text names
  that factor; it runs only when `load1/cores ≥ 1.5` at start (else it
  passes with a printed skip note), so it is meaningful on a loaded CI
  runner and never flaky on an idle one. Gated `cfg(any(linux, macos))`.
- **Gate:** `make check` (fmt, clippy `-D warnings`, `check-no-deps.sh`,
  `cargo test -- --show-output`), `make lint-cross`, `cargo +1.74.0 check -p amont -p amont-runtime`, `aval check`, `aval traits --check`.
  `git push` on this branch → expected: `pre-push-cargo-test` output has
  no `queued`, and its wall time is within 10 % of a bare `make test`.
- **Pilot (happier worktree, release build of this branch, DEFAULT budgets):**
  1. hold the cargo lock in another shell (`cargo clippy --workspace` on a
     cold target), run `amont run clippy` → expected: passes; log shows
     `waiting for the cargo build-directory lock` past 2m00s, no kill.
  2. `AMONT_CPU_TRACE` on, relais at 3 workers: count `unmeasured` windows
     over 10 minutes of gates, the caa1387 release build (100 ms deadline)
     against this branch (500 ms), on the same load → expected: the
     branch's rate is under 5 %.
  3. two worktrees committing at once with `hostSlots 1` → expected: one
     shows `queued`, both land, neither killed.
  4. a real commit on the branch with the machine under relais load →
     expected: lands; the heartbeat names the load factor when load > cores.

### Observed (2026-10-09/10)
- **Pilot, before/after on a real lock (the person's failure).** A process
  held real cargo's build-directory lock (`target/debug/.cargo-lock`) for
  150 s; a local clone with one staged `.rs` ran the `pre-commit` stage with
  DEFAULT budgets and that target dir. Released 1.47.3: `clippy … printed
  nothing for 2m00s and did no measurable CPU work … killed after 2m00s`,
  rc=1, 148 s. This branch: heartbeats `waiting for the cargo lock on the
  build directory 1m59s (amont.lockWait 10m00s)`, then `(waited 2m27s for
  the cargo lock on the build directory)`, `23 check(s) passed`, rc=0, 165 s.
  The first pilot run's first heartbeat stated the CPU-unmeasured rule
  during the wait; fixed to state the wait's own rule (test added).
- **Fixtures (timing.rs, rust_toolchain.rs), all green:** declared wait
  outlives a 2 s budget (passes in ~6 s, names the lock); `lockWait 2` kills
  in < 8 s naming lock and key; marker on stdout does not pause (killed at
  the budget); `AMONT_CPU_MAX_PROCS=0` + `timeout 0` killed at ≈ 8 s
  (extended budget), not 2 s; two repos, one host slot, two 3 s clippys take
  ≥ 5.5 s, and with a 0755 slot dir run together and print the note; retry:
  passes once with one "retrying once" and no failure text, a second kill
  fails inside one 12 s ceiling, a declared check is never retried; clippy
  runs in `crates/a` without `--workspace`, and with `Cargo.lock` staged
  with it.
- **Unit:** 615 runtime tests pass, including wait markers on the real and
  colour-coded cargo lines, framing across chunks and past the 4 KiB cap,
  `judge` under lockWait/extended/ceiling, the Tracker gap, the host-key
  reader, `scaled_budget`, `retry_budget`, slot exclusivity and the 0700
  check, every new region row within 80 columns.
- **Gate:** `make check` rc=0, `make lint-cross` rc=0 (three targets),
  `cargo +1.74.0 check` rc=0 for the host and aarch64-apple-darwin (with the
  lockfile removed, as CI does), `aval check` and `aval traits --check`
  clean. Every phase commit went through amont's own pre-commit gate.
- **Not done, stated:** the "Load scale, real" fixture (it would burn 4 ×
  cores processes in every `make test`; the formula is covered by the pure
  `scaled_budget` test and the display by `a_load_stretched_budget…`);
  pilot step 2 (partial-snapshot rate, 100 ms vs 500 ms) and step 4 (load
  factor shown on a real commit: the host's load was ≈ 1 per core during
  the pilot, so the factor stayed ×1).

## Decision log
- 2026-10-09 — Root cause from the person's log: cargo lock waits judged
  stuck; six measures asked for in one change. Clippy scope: staged
  packages only (dependents are CI's). `amont.hostSlots` default `cores/4`,
  min 1.
- 2026-10-09 — Two tiers of wait vocabulary: exact markers from the real
  binaries pause the clock; look-alikes only earn one retry after a kill.
  Holder identification and FIFO fairness deferred with `holds-until:`.
- 2026-10-09 — Review round 1 (backend, rust, tui, unix): slot dir moved
  from `temp_dir()` to a fixed per-user path with an owner/mode check,
  `AMONT_HOST_SLOT` so amont's own suite does not queue on itself, host
  keys read from global/system only; "Unmeasured defers to the ceiling"
  replaced by a bounded extended budget so `timeout 0` stays bounded; CSI
  stripped and stderr-only matching; retry moved to `status_streamed`,
  whole-phrase match, one shared ceiling, no `fail()` on the first kill;
  clippy scope by dropping `--workspace` instead of parsing `[package]`;
  gap windows keep `measured_since` honest; deadline 500 ms; region rows
  shortened to fit 80 columns, heartbeats keep their prefix.
- 2026-10-09 — Review round 2 (backend, rust): the harness opt-out moves
  from repository config (ignored by the host-key rule) to
  `AMONT_HOST_SLOT=held` in the env; slot/load fixtures use
  `GIT_CONFIG_GLOBAL`; host keys get a scoped reader and env overrides;
  consecutive markers share one wait; gap windows reset `measured_since`;
  retry limited to builtins; queue bound added to the worst-case table;
  queue-timeout outcome handed to the person (Decide).
- 2026-10-09 — Backend binding pass: the retry fixture's line must be a
  look-alike, not an exact marker; the harness pins
  `AMONT_IDLE_LOAD_SCALE=1`; the pre-push gate check is now observable.
  Second pass: the retry fixture runs as a builtin (externals never retry),
  the unmeasured fixture joins the env opt-out, the real-load fixture asserts
  against the load it measured instead of a fixed factor.

- 2026-10-10 — Implementation deviations, each forced by what the code
  showed:
  - Heavy is a name list (`host_slots::HEAVY`, pinned against the registry
    by a test), not a `Builtin` field: same effect without touching 39
    registry literals. `pre-commit-typecheck` does not exist; the list has
    clippy, go vet, pyright and the four test suites.
  - The slot is taken lazily, at a heavy check's first tool spawn
    (`run_observed`, `status_within`), not before `check.run`: pre-commit
    runs clippy whatever is staged, and a check with nothing to do must not
    wait behind another repository's suite. The duration record subtracts
    the wait.
  - A check run by name (`amont run`, `--hooks-dir … pre-commit-clippy`)
    has no live stage; its tool inherits the terminal, so only the ceiling
    applies there and the wait markers and retry are stage-path features,
    which is how git runs hooks. The slot is still taken on that path.
  - The test binaries get `AMONT_HOST_SLOT=held` and
    `AMONT_IDLE_LOAD_SCALE=1` from `.cargo/config.toml` `[env]` (a test
    proves it reaches them), because about forty test files spawn amont
    directly; the slot/load fixtures remove both.
  - `spawn_budget.rs` caps unchanged: its fixtures commit a `.txt` and
    run no tool, so the host-key scan (read at the first observed tool of
    ANY check) never runs there. A new test pins its cost instead: the
    same commit running one declared tool spawns git exactly once more
    without the scale pinned than with it (implementation review, finding 2).
  - The by-hand reproduction of a fixture ran `amont install` from the
    branch build and replaced `~/.local/bin/amont` for a day; restored to
    the 1.47.3 release with `install.sh` before the before-pilot reran.

## Implementation review
- **approve** (Delta, after round 1 approve-with-changes); 94k + 105k tokens, 111 s + 28 s.
- Fixed from round 1: slot I/O errors no longer read as a full queue; host-key scan cost pinned by a spawn-budget test and its comment corrected; nested amont skips the scan; a slot wait is announced; the retry names the check; a nested-heavy test; Phase 8 split.
- Left as noted by the reviewer, not findings: `nested()` read twice; EINTR on `flock` runs the check unqueued; 5 s fixture shims add ~10 s to `make test`.
- Next: Phase 8b (push, PR, merge on green), then Phase 9 (release v1.48.0, reinstall, observe a happier commit under load).

## Outcome

<!-- panel: repos=amont reviewers=backend,language:rust,tui,unix body-sha=f10ac088251d -->
