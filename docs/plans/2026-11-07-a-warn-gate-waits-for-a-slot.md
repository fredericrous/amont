---
status: done
branch: fix/warn-gate-waits-in-rehearsal
repos: [amont]
adrs: [ADR-0013]
---

# A warn gate waits for a slot where no push is waiting

## Review panel

👉 **Decide:** none. Approve if you're willing to bet that a land waiting for its warn gate (up to `amont.timeout`) is better than a land refused.
📍 amont · planned, nothing built · next: Phase 1 in one amont worktree, released as 2.2.1, then the `hook.skip` removed. Panel: backend, lang:rust, unix, tui (all opus-5-5), codex (gpt-6-astra).
**Changed by review:** the test gate leaves a marker and bounds its wait; the `hook.skip` removal is comma-safe and checked; the live check runs in a fresh clone (the live checkout still skips the probe).
📄 Full reviews: [2026-11-07-a-warn-gate-waits-for-a-slot.reviews.md](2026-11-07-a-warn-gate-waits-for-a-slot.reviews.md)
**Verdicts:** 5 approve-with-changes. 22 findings accepted, 0 rejected; 6 carried into implementation, among them Codex's two: the advice must be `AMONT_ON_BUSY=wait amont run --raw <gate>`, and a real `amont land` test before the release.

## Context

amont#431 (open since 2026-11-06): `amont land` refuses with `amont-land:
evidence-missing … pre-push-adr-lint-cli-probe` whenever another session holds
or queues for a host slot. It happened 5 times on 2026-11-06 for #430, and 4
more times on 2026-11-07 (3 for relais#396, 1 for the amont-agent 3.21.0
release).
The current workaround is a person-approved `hook.skip
pre-push-adr-lint-cli-probe` in amont's own repo config.

Why it happens, from the code:

- The probe is declared `warn heavy` in every repo that vendors the adr-lint
  pack (`relais/amont.conf:80`, `amont-agent/amont.conf:9`).
- `dispatch::enter` (`crates/amont-runtime/src/dispatch.rs:1107-1113`) calls
  `host_slots::never_waits()` for every Heavy check whose severity is `warn`.
  Then `host_slots.rs:261-264` gives it `Defer { slot_wait: ZERO }` **whatever
  the busy policy is**. It was added by #395 (2026-11-01) so that a verdict
  that stops nothing never holds a commit, or a push and its open connection
  to the remote.
- A rehearsal already asks every gate to wait: it spawns its pre-push with
  `AMONT_ON_BUSY=wait` (`rehearsal.rs:1020-1023`), because "a rehearsal whose
  gates all deferred would report passed having run nothing" (ADR-0013). The
  zero-wait override bypasses that, so inside a rehearsal the warn gate still
  defers.
- From amont 2.0, `amont land` runs its gates in a rehearsal pre-stage
  (`land.rs:847-859`, `rehearse --wait`), and refuses when an owed gate left
  no evidence (`dispatch::land_refusal`, `dispatch.rs:1246`). A deferred gate
  leaves none, so a gate that cannot block the push blocks it.

The person's decision (2026-11-07): the gate should wait for a slot rather
than defer. A rehearsal has no open connection to the remote and no commit
waiting on it, so the reason #395 gave for never waiting does not apply
there. Offloading heavy gates to the cluster is a separate, already-built
programme (homelab-offload, amont ADR-0016/0017) that is not enabled yet and
does not cover Rust gates (t23). It is out of scope here.

## Approach

### Phase 1: amont, one release

1. **The rule.** In `dispatch::enter`, call `host_slots::never_waits()` only
   when the check is Heavy, its severity is `warn`, **and**
   `crate::rehearsal::in_snapshot()` is false. Inside a rehearsal snapshot a
   heavy warn check then follows the busy policy the rehearsal sets (`wait`):
   it waits for a slot up to `amont.timeout` (default 3600 s, `0` meaning
   3600, `host_slots.rs:268-271`), then runs unqueued with a note, as every
   `wait` check does. `in_snapshot()` is the existing seam (`rehearsal.rs:142`),
   primed on the main thread before any check runs and already read by
   `dispatch.rs:1298,1380`.
2. **Unchanged paths.** A plain `git push` (with `amont.pushGates inline`)
   and a commit keep #395's behaviour: a heavy warn gate takes a free slot or
   defers at once, because there the push holds an idle connection or the
   commit waits. `amont run --stamp <gate>` is not touched: on 2026-11-07 it
   already queued and ran the probe.
   - **Cost.** Land's pre-stage can now wait up to `amont.timeout` (3600 s)
     for a warn gate, where today it defers at once. The probe held pushes
     for up to 30 minutes before #395 (`busy_defers.rs:668`), so a land on a
     busy host may take that long. The person chose waiting over deferring
     on 2026-11-07. The wait shows in the rehearsal's `(waited … for a host
     slot` line.
   - **The heartbeat.** Inside a rehearsal snapshot, the push stage's
     heartbeat (`live.rs:163`, `on_push`) must not print the "git opened its
     connection to the remote" note (`live.rs:964-977`). No connection is
     open there, and a check now queues past the first beat. Set `on_push`
     false when `crate::rehearsal::in_snapshot()`, with a unit test on
     `beat_line`.
3. **Docs.**
   - Rewrite the doc comments on `host_slots::never_waits` (`host_slots.rs:206-210`)
     and `dispatch::enter` (`dispatch.rs:1100-1106`), and the
     `Current::cannot_block` field doc (`host_slots.rs:147-150`), to say
     "except inside a rehearsal".
   - Add a dated **Amendment** to `adr/0013-a-busy-host-defers-to-ci.md`
     stating both rules, since neither is written there today: a heavy `warn`
     check never waits on a commit or a push (from #395); every gate waits for
     a slot inside a rehearsal, `warn` included, so land's pre-stage leaves the
     evidence it owes (this change, #431).
   - Add a CHANGELOG `### Fixed` bullet under Unreleased that names #431.
   - `announce_deferred` (`dispatch.rs:2043-2057`): for a heavy `warn` gate,
     print `run now: amont run <gate>` and drop `stop deferring: set onBusy
     wait`, which cannot stop a `warn` gate deferring on a commit or a push.
4. **Test.** Add a new integration test in `crates/amont/tests/it/busy_defers.rs`,
   beside `a_warn_gate_never_waits_for_a_slot` (`:670`), reusing its fixture:
   a `warn heavy` pre-push gate, `AMONT_HOST_SLOTS=1`, and one slot held by
   `Holder::hold` (`:17-20`). The gate runs as the rehearsal runs it: pre-push
   with `AMONT_REHEARSAL` set and `AMONT_ON_BUSY=wait`. The holder is dropped
   after 3 s from a thread. The gate is `sh -c 'touch <scratch>/gate-ran'`
   (an absolute path) instead of `sh -c true`, so that it leaves a trace.
   The test asserts that the hook:
   - took at least 3 s and under 60 s, so it ran when the slot freed, not at
     the 600 s ceiling;
   - printed `waited ` and `for a host slot`;
   - did not print `deferred to CI`;
   - left `gate-ran`;
   - exited 0.

   The existing
   `a_warn_gate_never_waits_for_a_slot` must stay green unchanged, as the
   proof that a plain push still never waits.
5. **Release and the workaround.**
   - Land through a PR and release a patch, 2.2.1, with `tag-release`.
   - Install it.
   - Then remove the person-approved skip of `pre-push-adr-lint-cli-probe`,
     which #431 put there as its workaround.
     1. Read `git -C /Users/fredericrous/Developer/Perso/amont config --local
        --get-all hook.skip`. amont splits each value on commas
        (`lib.rs:948-953`).
     2. If the probe is a value of its own, run `git -C <that path> config
        --local --unset hook.skip '^pre-push-adr-lint-cli-probe$'`. It works
        with one value or several.
     3. If it shares a comma-joined value, rewrite that value with
        `--replace-all`, keeping the other names.
     4. Check that the command exited 0 (git gives 5 when nothing matched),
        then read `--get-all` again and confirm the probe is gone.
   - Close #431 with a link to the PR.

## Non-goals

- Turning on cluster execution (homelab-offload; its enable phase waits for
  ticket t13, about 2026-11-08, under its own plans).
- Changing `onBusy defer`, `slotWait`, or what a plain push or a commit does
  with a busy host.
- Making land accept a deferred warn gate as evidence (the other option in
  #431, not chosen).
- The probe's severity or its `heavy` flag (adr-lint's pack decides those).

## Code review

- amont: backend, lang:rust, unix, tui

## Verification

- `cargo test -p amont --test it busy_defers` (through `amont slot run`): the
  new test passes, and `a_warn_gate_never_waits_for_a_slot` still passes
  unchanged.
- `git commit` on the branch: the commit hook exits 0 and prints no blocking verdict.
- Live, on the amont repo with the branch binary:
  0. Work in a fresh `git clone` of the branch, not amont's live checkout. The live checkout keeps `hook.skip pre-push-adr-lint-cli-probe` until step 5, and the snapshot reads that config (`lib.rs:955-959`), so there the probe would be skipped. Install the branch's hooks into the clone with `--hooks-dir` only.
  1. Hold the only slot with `AMONT_HOST_SLOTS=1 amont slot run -- sleep 20`
     in the background.
  2. Run `AMONT_HOST_SLOTS=1 amont rehearse --wait` on a tree that touches
     `adr-lint.toml`, so the probe is owed.
  3. Expected: it prints `(waited … for a host slot` and a row for `pre-push-adr-lint-cli-probe`, and it never prints `deferred to
     CI` or `skipped` for that gate. Then `git notes --ref amont-gate show HEAD` lists a
     `pre-push-adr-lint-cli-probe` stamp for the tree, and `amont slots`
     shows no slot still held once both commands have exited.
- End to end, after the 2.2.1 install with the `hook.skip` removed: the next
  `amont land` of an amont branch passes with no `evidence-missing`, even
  while `amont slots` shows another holder.
- Seven days after the install: count `amont-land: evidence-missing` in the
  Claude Code transcripts under `~/.claude/projects` modified since the
  install (land output is logged nowhere else). Expected 0, against 9 in
  the two days before. Note each `(waited … for a host slot` value from land
  pre-stages in the same transcripts, with the median and the maximum.
- Rollback: revert the PR; the behaviour returns to #395's, and the
  `hook.skip` workaround can be set again.

## Verification record

Input → expected → actual, observed before the push.

- **Tests on the branch**, through `amont slot run`:
  - `busy_defers` and `land` integration tests (31, then the 19 they cover after the review fixes) → pass → pass.
  - `amont-runtime` unit tests on `deferral_advice`, `holds_a_push_connection` and `live::` (27) → pass → pass.
  - `cargo check -p amont --tests --target x86_64-pc-windows-msvc` → exit 0 → exit 0 (the blocking review finding).
  - The commit gate passed on every commit (27 checks).
- **Live, first attempt** (a fresh clone, `amont trust`, a private one-slot directory, a 20 s holder): the probe ran and stamped, with no `deferred to CI`. No `waited` line appeared, because the gate takes its slot at its first tool spawn and the snapshot setup outlasted the holder, so this attempt proved the run but not the wait.
- **Live, second attempt** (the same with a 180 s holder) → waits, then runs and stamps → `(waited 2m48s for a host slot, amont.hostSlots 1)`, then `✓ stamped pre-push-adr-lint-cli-probe`. `git notes --ref amont-gate show HEAD` lists the probe, and the slot was released afterwards. The heartbeats at 1 and 2 minutes read `queued … for a host slot`, with no "git opened its connection" note, and the silence budget did not kill the queued gate.
- **Ctrl-C** (SIGINT to the rehearsal's process group while its warn gate queued):
  - The rehearsal stopped at once.
  - The dead waiter's ticket stayed in the queue, and `amont slots` counted it as `waiting: 1`. That count is read-only and existed before this change.
  - The next waiter queued only behind the live holder, ran, and removed the dead ticket (`live_tickets`, `crates/amont-slots/src/lib.rs:586-610`). The queue was left empty.
- **After the release:** the end-to-end land with `hook.skip` removed, and the 7-day `evidence-missing` count.

## Implementation review

- implementation-review: approve-with-changes, then approve (49k, 30 s; 52k, 11 s). Fixed: the vacuous heartbeat test, an untested mixed advice. Candidates ruled fine (the roll-up `println!`, `in_snapshot` as data, the real waits in `release_after`); one `r` naming violation, removed with its test.
- backend: approve-with-changes, then approve (65k, 105 s; 74k, 22 s). Fixed: one `--raw` command per deferred gate, the hint scoped to block checks.
- lang:rust: approve-with-changes, then approve (68k, 79 s; 72k, 11 s). Fixed: the Windows test build (blocking), a test that reaches `holds_a_push_connection`, an `Option` in place of the `Cell`, an exhaustive `match`. Deliberate: the shared test helpers stay in `busy_defers`, now behind `cfg(unix)`.
- unix: approve twice (67k, 63 s; 74k, 19 s). Deliberate: the roll-up stays on stdout, a pre-existing follow-up for another PR.
- tui: approve-with-changes, then approve (66k, 71 s; 72k, 12 s). Fixed: mixed-list advice, `--raw` for block gates, the hint on its own line, and the doc comment and CHANGELOG wording.

## Decision log

- 2026-11-07: another session (`fail-fast-precommit-checks`) had staged the other #431 fix, where land accepts a `deferred-under-warn` completion, in `amont-wt-warn-deferral`. The person chose this plan ("Mine goes ahead, theirs stops"). That session stopped, pushed nothing, and left one local commit for the person to discard.
- Departures from the plan, all reviewed:
  - The test gate is declared `touch <absolute path>`, not `sh -c 'touch …'`, because the manifest splits a command on whitespace.
  - The heartbeat is pinned through the extracted `holds_a_push_connection(names, in_snapshot)`, not a `beat_line` call with a constant.
  - Every deferred gate, `block` too, is advised `AMONT_ON_BUSY=wait amont run --raw <gate>`. The `onBusy wait` hint is shown only for block checks, on its own line.
- From the panel, still open after the release: the 7-day measure. A pre-stage with several heavy warn gates waits up to `amont.timeout` for each of them, and on-commit rehearsals now wait too. Both go into the measure.

<!-- panel: repos=amont adds= reviewers=backend,language,tui,unix,codex body-sha=262e2cad37b5 -->
