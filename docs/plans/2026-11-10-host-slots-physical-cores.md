---
status: active
branch: feat/physical-core-slots
repos: [amont, relais]
adrs: [ADR-0014, ADR-0021]
---
# Host slots count physical cores

## Review panel

👉 **Decide:** none. Approve if you accept these numbers on this laptop: physical cores (cap 3 × 2 jobs, admit below load 6), with relais on the same width.
📍 amont, relais · nothing built; the greengate rename merged (4.0.0) · next: Phase 1 in greenharness-slots. Panel: backend, lang:rust, architect, po, tui, unix (all opus-5-5); ⚠ codex: usage limit until 11-11.
**Changed by review:** the core count travels with `Machine` into the cap, admission and width. **At your request, the crate now reads the host keys itself (`HostSizing`), so relais copies nothing by hand and both tools still run standalone.** The relais coordinator cap stays on logical cores.
**Verdicts:**
- Round 1: 6 approve-with-changes (1 blocking).
- Round 2: 6 approve-with-changes (1 high, applied).
- Final: 5 approve, 1 approve-with-changes.
- Delta on your change: backend approve, lang:rust approve-with-changes (2 high, applied).
- 0 rework. 19 findings carried into implementation.

## Picture

```
 today (6 cores, 12 threads)          after this plan
 ---------------------------          ---------------
 cores() = 12 logical                 cores() = 6 physical
 cap 12/2 = 6 slots                   cap 6/2 = 3 slots
 admit while load1 < 12               admit while load1 < 6
 6 holders x 2 jobs = 12 jobs         3 holders x 2 jobs = 6 jobs
 relais command: 12/3 = 4 wide        relais command: 6/3 = 2 wide
```

A heavy job used to be admitted against 12 logical threads, and 12 jobs
ran on 6 real cores. After this plan the slots count physical cores, so 6
jobs run, one per core.

## Context

On this laptop (i7-9750H: 6 physical cores, 12 hardware threads) the host
slots size everything from `std::thread::available_parallelism()`, which
counts logical threads: auto cap `12/2 = 6`, each holder's width
`1 × 12 / 6 = 2`, and admission allowed while the load is below 12.
6 holders × 2 jobs = 12 compile/test jobs on 6 real cores. On top of that
come about 14 Claude sessions and Gatekeeper scans. The load was measured
at 30–55 on 2026-11-10, and `amont slots` listed 5 of 6 slots held, each
at `jobs 2`.

Separately, relais computes its verification commands' width with its own
`cores()` (logical) divided by its own coordinator cap (`cores/4 = 3`), so
a relais heavy command runs `12/3 = 4` wide. That breaks the crate's rule
that "a weight means the same width whoever holds it"
(`crates/amont-slots/src/lib.rs:94-96`).

The person chose "physical everywhere": the slots count physical cores for
the cap, the width and the load test, with an `greengate.hostCores N` override.
This is already a setting decided for the machine on 2026-11-10;
`~/.cargo/config.toml` (dotfiles#61) caps cargo run outside the slots at 4.

## Goal

On an SMT machine, the slots run at most about one job per physical core:
here 3 holders × 2 jobs = 6. Each build keeps today's per-job speed, so
fewer cold builds run at once. Machines without SMT (Apple Silicon, most
CI runners) behave exactly as today; the only output change there is the `amont slots` header, and consumers of that line are grepped first. relais's verification commands use
the same width formula and core count as amont.

## Non-goals

- `amont.idleLoadScale` / `crates/amont-runtime/src/load.rs` (the timing
  stretch) keeps its logical count; it scales kill bounds, not admission.
- relais's own coordinator cap (`default_heavy_commands = cores/4`, relais
  ADR-0010) keeps its role and keeps reading the logical count
  (`available_parallelism`): its default stays 3 here, so preflight does
  not start refusing weight-2 commands (`verify::overweight`).
- Gatekeeper / Developer Tools exemption: the person's call, not in this
  plan.
- No relais setting that copies an amont key: relais neither gains
  `host_cores` nor keeps relying on a hand-copied `host_slots` (see
  Behaviour, "one sizing").

## Code review

- amont: backend, lang:rust, unix, tui, architect (po left out: no user-facing flow changes beyond one status line)
- relais: backend, lang:rust, unix, tui, architect

## Sequencing
`feat/greengate-name` and `feat/greengate-hard-rename` are merged (greengate
4.0.0, PR #453, origin/main 12534486). The slots crate is
`crates/greenharness-slots`. amont 4.0 reads only the new names, with no
fallback: git keys `greengate.hostCores` / `greengate.hostSlots`, and the
environment variables built by `host_env_name`, `GREENGATE_HOST_CORES` /
`GREENGATE_HOST_SLOTS`. A new test rejects any `AMONT_` or `amont.<key>`
literal outside `names.rs` and `migrate.rs`; a genuine exception goes in
`ALLOWED_DATA`. Any binary a test starts by hand goes through
`common::sandbox_env`. `host_env_name` lives in `greengate-runtime`, which
links the slots crate, so the crate hardcodes the four names
(`greengate.hostCores`, `greengate.hostSlots`, `GREENGATE_HOST_CORES`,
`GREENGATE_HOST_SLOTS`); a test in `greengate-runtime` asserts they equal
`host_env_name` output and `names::CONFIG_PREFIX`, so they cannot drift.

## Behaviour

- `greenharness_slots::cores()` returns
  `min(physical cores, available_parallelism())` (the min keeps a cgroup
  or affinity limit). Physical cores come from:
  - macOS: `sysctlbyname("hw.physicalcpu")`, declared `extern "C"` the way
    `getloadavg` already is in the crate's `platform` module (std-only
    stays);
  - Linux: unique `/sys/devices/system/cpu/cpu*/topology/core_cpus_list`
    entries, else distinct `core id` / `physical id` pairs in
    `/proc/cpuinfo`; when unreadable or zero (aarch64 has neither field),
    the logical count;
  - other platforms: the logical count.
- **One sizing, read by the crate.** `greenharness_slots::HostSizing::
  from_host()` resolves both host keys once, for every client:
  - `greengate.hostCores N` (1..=1024) replaces the detected count;
    `greengate.hostSlots N` (0..=64, `0` = off) fixes the slot count, as today;
  - each is read as `GREENGATE_HOST_CORES` / `GREENGATE_HOST_SLOTS`, then
    `git config --global`, then `--system`, then detected / auto. The
    crate exposes a pure `HostSizing::resolve(env, rows)` over
    (scope, key, value) rows, and `from_host()`, which runs one
    `git config --show-scope --get-regexp '^greengate\.host'`
    (std `Command`, std-only), cached in a `OnceLock`. amont passes its
    cached `Settings` rows to `resolve`, so it starts no extra git process
    and `spawn_budget.rs` stays unchanged; amont's `host_slots::sizing()`
    calls the crate, so one implementation remains. The precedence tests
    target `resolve`;
  - `resolve` reads only the `greengate.*` names and `GREENGATE_HOST_*`
    variables (amont 4.0 has no fallback to `amont.*` / `AMONT_*`), with a
    test that an `amont.hostCores` row and an `AMONT_HOST_CORES` variable
    are ignored (its two old-name literals get an `ALLOWED_DATA` entry for
    `greenharness-slots/src/lib.rs`, count 2, "a test that the old names are
    ignored", in `crates/greengate/tests/it/names.rs`);
  - today's behaviour is kept: an invalid value falls back as
    `config.rs:483-484` does, and a key set only in a repository's config
    is warned about (`config.rs:475-481`);
  - **standalone stays:** relais links the crate, never the binary. With
    no amont installed, no `GREENGATE_HOST_*` variables, no host keys, or no
    working `git`, `from_host()` returns detected / auto, which is
    relais's behaviour with defaults today. amont is unchanged apart from
    the crate it already links;
  - `from_host()` returns the values and each one's source (`env`,
    `global`, `system`, `detected`, `detected (logical fallback)`,
    `limited by affinity`) and a list of warnings. A non-numeric or
    out-of-range value becomes a warning with the fix for its source
    (`unset GREENGATE_HOST_CORES`, `git config --global --unset
    greengate.hostCores`). Each caller prints it its own way (amont once on
    stderr, relais as an event), and both doctors repeat it. The crate
    itself writes nothing.
- The count travels with the sizing, so no internal `cores()` call
  remains: an additive `Machine { cores }` is read by `cap_for`, `Probe`
  and `holder.jobs` (today each calls `cores()`, `lib.rs:64,422,467`).
  The existing `Sizing::cap`, `Probe::now` and `Waiter::new_in` default to
  the detected count, so the crate release stays minor.
- Cap, width and the admission load test all use the same number:
  - auto cap `max(1, cores/2)`: 3 here;
  - width `max(1, weight × cores / cap)`: 2 here;
  - admission while `load1 < cores`: 6 here.
  At the measured load (30–55), both `< 12` and `< 6` fail, so only the
  floor holder (empty host) and same-process siblings get in; separate
  jobs effectively run one at a time, as today. The cap (6 → 3) and the
  width are what cut the work. The lower load bar matters only between 6
  and 12.
- A waiter prints the reason and the next command, e.g.
  `amont: queued: load 31.0 >= 6 cores (run: amont slots)` or
  `slots full 3/3`.
- `waits.jsonl` lines gain `"cores"` and `"cap"`, so lines written before
  and after the install can be told apart.
- `amont slots` prints `(auto: load 31.0, 6 cores, 12 threads)` and
  `amont doctor` names the count and its source (`detected`,
  `detected (logical fallback)`, `limited by affinity`, `greengate.hostCores`).
- relais `verify::heavy`: `cores()` is renamed `logical_cores()` and keeps
  `available_parallelism`; it alone feeds `heavy_cap` and
  `default_heavy_commands` (`engine.rs:839,846`, `preflight.rs:186`,
  `coordinator/mod.rs:3071`). For width only, relais reads
  `greenharness_slots::cores()` and computes width with
  `greenharness_slots::parallelism(weight, cores, slots cap)`, where the
  slots cap is the crate's `cap_for` on that count. Its own `heavy_cap`
  stays the coordinator's limit (see Non-goals). relais takes its slot
  sizing and core count from `HostSizing::from_host()`, so both tools size
  one queue alike with nothing copied by hand. relais's
  `machine.resources.host_slots` (`policy/mod.rs:931-937`, today "set it
  to the same value as `greengate.hostSlots`") stays as a deprecated
  override: when set, it still wins, and doctor warns that it is
  deprecated and names `greengate.hostSlots`, plus the mismatch when the two
  differ. relais records waits with
  `record_wait_jobs` and the width it gives `child_environment`; today it
  passes `jobs: None`. relais doctor prints
  `heavy: width W (N cores / slots cap C), coordinator cap K`. relais
  bumps its dependency to the renamed crate version that carries this.

## Phases

- [x] Phase 1 — physical core count and override in the slots crate:
  `crates/greenharness-slots/src/lib.rs` (`cores()`,
  `platform::physical_cores()`, `HostSizing::from_host()` with sources,
  `Machine { cores }` read by `cap_for`,
  `Probe` and `holder.jobs`, `"cores"`/`"cap"` in `waits.jsonl`; unit
  tests on canned `/proc/cpuinfo` and sysfs, on zero pairs, and on the
  min with the logical count; `resolve` tests for a repo-scope-only row,
  `abc`, cores 0 and slots 65, each returning a warning and the fallback).
- [ ] Phase 2 — amont takes its sizing from `HostSizing::from_host()`
  (its own `greengate.hostSlots` read is removed) and threads it through cap,
  width, admission, `amont slots` and doctor:
  `crates/greengate-runtime/src/host_slots.rs`,
  `crates/greengate-runtime/src/slot_cmd.rs`,
  `crates/greengate-runtime/src/config.rs`,
  `crates/greengate/tests/it/slot_run.rs` and `tests/it/busy_defers.rs`
  (pin `GREENGATE_HOST_CORES` instead of `available_parallelism`),
  `docs/host-slots.md`,
  `docs/configuration.md`, `adr/0021-slots-count-physical-cores.md`
  (amends ADR-0014's sizing: the core count is physical), `adr/HEADS.md`,
  `CHANGELOG.md`.
- [ ] Phase 3 — release greengate (minor) through the `tag-release` skill,
  and publish the crate: `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md`.
- [ ] Phase 4 — relais takes the shared count and formula:
  `crates/relais/src/verify/heavy.rs`,
  `crates/relais/src/verify/heavy/tests.rs`,
  `crates/relais/src/runner/engine.rs`,
  `crates/relais/src/runner/preflight.rs`,
  `crates/relais/src/coordinator/mod.rs`, `crates/relais/src/verify/mod.rs`
  (re-export; relais's own `parallelism` is deleted too),
  `crates/relais/src/policy/mod.rs` (`host_slots` deprecated),
  `crates/relais/src/doctor/mod.rs` (also notes a legacy `amont.hostCores` /
  `amont.hostSlots` in global config as ignored, with the `amont migrate`
  fix), `crates/relais/Cargo.toml`,
  `Cargo.lock`, `CHANGELOG.md`; release relais. Installing it and
  reinstalling the plugin are the person's. Until relais is installed, set
  relais `host_slots = 3` so the old build (cap 6, load < 12) does not take
  slots 3–5 that new amont no longer counts; remove it after the install.

## Decision log

- 2026-11-10 — physical cores for cap, width and load test, with an
  override key, because 12 logical-thread jobs on 6 cores is what the
  person saw as "cargo uses too much CPU". The person chose this over
  width-only (single-threaded test runs) and override-only (other SMT hosts
  unchanged).
- 2026-11-10 — relais's width moves to the shared formula with the slots
  cap, because its `12/3 = 4` width breaks the crate's same-weight,
  same-width contract. Its coordinator default stays on the logical count
  (review), so relais ADR-0010 is unchanged.
- 2026-11-10 — the slots crate resolves the host keys itself
  (`HostSizing::from_host()`), and relais stops copying `greengate.hostSlots`
  into `host_slots` by hand. The person chose this over mirrored settings
  (two hand-copied values that can drift) and over leaving relais alone
  (4-wide commands, and two sizings on one queue). It fits ADR-0064:
  greengate owns the host slots, and relais reaches them only through
  greengate's crate.
- 2026-11-11 — the `"cap"` field on wait lines moved into Phase 1 (the
  person's choice): `WaitContext` gains `cap`, `record_wait_sized` carries
  it for `slot run`, and `host_slots.rs` / `slot_cmd.rs` pass it. A handed
  slot writes no cap.
- 2026-11-11 — the rename merged (greengate 4.0.0, #453), so the keys are
  `greengate.hostCores` / `greengate.hostSlots` and the variables
  `GREENGATE_HOST_CORES` / `GREENGATE_HOST_SLOTS`. Read those only, with no
  fallback to `amont.*` or `AMONT_*`. The plan's earlier `amont.*` names
  are superseded; the behaviour is unchanged.

## Verification

- Phase 1: unit tests. A canned 6-core/12-thread cpuinfo gives 6, a
  missing file or zero pairs gives the logical count, and an override of 2
  gives 2. A `poll_with` test with `Machine { cores: 4 }` gives cap 2 and
  refuses admission at load 4.0. On this Mac, `cores()` equals
  `sysctl -n hw.physicalcpu` (6).
- Phase 2, on this laptop with the branch binary installed through
  `--hooks-dir` only (memory: never replace the machine binary from a
  branch):
  - `amont slots` should show `6 cores, 12 threads` and cap 3, in under 80 columns, with no colour under `NO_COLOR=1` or a pipe;
  - `env -u MAKEFLAGS -u CARGO_BUILD_JOBS -u RUST_TEST_THREADS -u NEXTEST_TEST_THREADS amont slot run -- sh -c 'echo $CARGO_BUILD_JOBS $RUST_TEST_THREADS'`
    should print `2 2`;
  - `GREENGATE_HOST_CORES=4 amont slots` should give cap 2, width 2 (one
    process only; the global config is never written).
  - `GREENGATE_HOST_CORES=0 amont slots` and `=abc` print the actionable
    error; a waiter under load above 6 prints its reason line;
  - `cargo nextest run -p greengate --test it` passes with no override.
- Phase 3: read the release run's `conclusion`, and confirm the crate is
  published at the new version.
- Phase 4: a unit test asserts `environment(1)` equals
  `parallelism(1, cores, cap_for(cores))`. A relais run with one heavy
  verification command: its `waits.jsonl` line shows `"jobs":2` and the
  child sees `CARGO_BUILD_JOBS=2`. With `GREENGATE_HOST_CORES=4` alone, both
  `amont slots` and relais doctor show cap 2 and source `env`. With relais
  `host_slots = 5` set, relais doctor prints the deprecation and the
  mismatch with `greengate.hostSlots` (worded "when greengate is
  installed"). Standalone: relais run with `PATH` lacking amont,
  `HOME` pointing at an empty dir and `GIT_CONFIG_NOSYSTEM=1` sizes from
  detection and passes its heavy command. `spawn_budget.rs` passes
  unchanged.
- Before the install: record one working day's baseline from
  `waits.jsonl` (p50/p90 `waited_ms`, `load1` at admission) and `uptime`
  every 5 min.
- After the install, the same weekday, over lines carrying `"cores":6`:
  - 0 weight-1 lines with jobs above 2;
  - at most 3 holders in per-minute `amont slots` samples;
  - median `load1` while a slot is held drops by at least 6.
  Roll back (`git config --global greengate.hostCores 12`, which moves both
  tools) if p90 `waited_ms` more than doubles while the median load drops
  by less than 6.

## Outcome

## Full reviews (reference)

**Round 1** (all approve-with-changes):
- **backend** (36k, 63 s):
  - Accepted: #1 the override does not reach the cap, Probe or width, and relais ignores it; #2 old and new builds share one queue; #3 the admission arithmetic; #4 waits.jsonl lacks cores and cap, and no threshold is set; #5 the code-review reasons.
  - #6 (the Linux zero count) was taken as lang:rust#4.
- **architect** (33k, 51 s): accepted #1 (blocking: the override must travel with the sizing, and relais needs host_cores), #2 (the coordinator default stays logical) and #3 (the relais file list).
- **lang:rust** (38k, 74 s): accepted #1 (an additive `Machine`), #2 (relais host_cores), #3 (record_wait_jobs and the environment test), #4 (sysfs, then cpuinfo, logical on zero) and #5 (a poll_with test).
- **po** (27k, 42 s): accepted #1 (baseline and rollback) and #2 (only the floor admits at the measured load). Rejected #3 (a duplicate of unix#2) and #4 (covered by #1).
- **tui** (22k, 24 s): accepted #1 (the waiter's reason line), #2 (the error for a bad value), #3 (the doctor source labels), #4 (the slots header on non-SMT hosts), #5 (the relais doctor line) and #6 (80 columns and NO_COLOR).
- **unix** (30k, 42 s): accepted #1 (AMONT_HOST_CORES precedence), #2 (pin the integration tests), #3 (no global config write in verification) and #4 (`env -u` on the width keys). #5 was taken as lang:rust#4.

**Round 2:**
- **architect:** accepted #4 (high): relais `logical_cores()` feeds only the coordinator cap.

**Carried into implementation:**
- backend: #1 the interim `host_slots = 3` is Fixed sizing, with no load test, so record the baseline before setting it; #2 a p90 that doubles on its own goes to the person; #3 the relais host_cores range check is 1..=1024.
- po: #1 ship and measure Phases 1–3 before Phase 4, and drop the waiter and doctor lines first if Phase 4 slips; #2 the rollback rule uses OR; #3 verify that host_slots is unset after the install.
- tui: A the error and doctor name the source of the value (`unset GREENGATE_HOST_CORES` for the environment); B after a rollback, confirm the source with `amont doctor`; label the doctor counts logical and physical.
- architect: #5 the same point as backend #1.
- unix: #1 a bad value warns once and falls back with exit 0; #2 the waiter line goes to stderr, once per change of reason; #3 relais reads GREENGATE_HOST_CORES, then policy, then detected; #4 the rollback unsets the environment variable first.
- lang:rust: #7 read the key with `host_integer_or(.., 0, 1..=1024)`, where 0 means unset.
- Final round:
  - backend: keep the slots count and the logical count in separate fields in HeavyAdmission; doctor:340 uses logical_cores().
  - po: a test where the coordinator cap differs from the slots cap (6 cores / 16 threads).

**Final:**
- Approve: backend (47k), architect (41k), lang:rust (48k), tui (30k), unix (39k).
- Approve-with-changes: po (33k; its points are carried above).

**codex:** ⚠ exit 1, usage limit until 2026-11-11 23:43, so there was no review.

**Delta (the person's change: the crate reads the sizing; relais copies nothing by hand):**
- backend (58k, 43 s) approve-with-changes:
  - accepted: #1 one cached git call; #2 no stderr from the library; #3 renamed keys;
  - carried: #4 relais doctor shows no deprecation once the interim `host_slots` is removed.
  - Final: approve. The standalone check must not warn when git is missing.
- lang:rust (57k, 55 s) approve-with-changes:
  - applied by hand: #8 (high) pure `resolve(env, rows)` and one `from_host()` call cached in a OnceLock, so spawn_budget is unchanged; #9 (high) greengate.* aliases; #10 today's fallback and the repo-only warning are kept; #11 warnings come back as data.
  - Final: approve-with-changes. Carried: #12 match exactly the four key names, lowercased, and widen amont's cached scan to fetch greengate.*.
- Added for the person's standalone question: a standalone bullet and its check.

<!-- panel: repos=amont,relais adds=contract reviewers=backend,lang:rust,architect,po,tui,unix body-sha=3f21d15c2d5a -->

