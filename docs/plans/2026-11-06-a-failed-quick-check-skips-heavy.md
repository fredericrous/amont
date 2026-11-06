---
status: done
branch: fix/pre-commit-fail-fast
repos: [amont]
adrs: [ADR-0020, ADR-0013]
---
# A failed quick check skips the heavy ones

## Review panel
👉 **Decide:** none — approve if a passing commit may wait for its quick checks (~4 s in #394) before clippy and the test suite start, so that a failed quick check skips them.
📍 amont · plan ready, nothing built · next: Phase 1 in a new worktree. Panel: backend (opus-5-5), lang:rust (opus-5-5), tui (opus-5-5), unix (opus-5-5). Codex skipped: no risk trigger (programme t03/t10).
**Changed by review:** the roll-up line in house style (sign, count, a run-now hint per check, quiet mode); `AMONT_FAIL_FAST` → git config → `amont.conf` precedence; a pre-commit ledger so the 14-day measure has data.
📄 Full reviews: [reviews](2026-11-06-a-failed-quick-check-skips-heavy.reviews.md)
**Verdicts:** 4 approve-with-changes in round 1; backend approve-with-changes in round 2, then approve; 2 rejected (lang:rust#6, `Warned` exists; backend#3, a duplicate); none carried.

## Goal
When a quick pre-commit check blocks the commit (`cargo fmt`, a module
rule, a lint), amont no longer runs the heavy ones (clippy, the test
suite) for a commit that is already refused. In relais #394 a format check
failed after 4 s and the module check after 2 s, but `cargo test` ran its
full 13 minutes anyway. Then the corrected commit ran it again (programme
`decisions/docs/programmes/proportional-ship`, t01).

## Context
- `run_stage_traced` (`crates/amont-runtime/src/dispatch.rs:597-658`) runs
  every selected check at once through `run_concurrently` (`:304-335`) and
  waits for all of them. Nothing stops a heavy check once another check
  has failed. pre-push is serial and fail-fast already
  (`registry.rs:1071`).
- Checks already carry a weight. Built-ins in `host_slots::HEAVY`
  (`host_slots.rs:24-32`: clippy, go vet, pyright, the test suites) are
  Heavy. A declared check is Heavy when it says `heavy` or is a moved gate
  entry (`manifest.rs:786-793`). Everything else is Light.
- Stopping a running heavy check was considered and is not this plan.
  `wait_within` (`hooks/common.rs:1775`) kills only the direct child
  (`child.kill()`), and check commands do not get their own process group,
  so `cargo test`'s test binaries would keep running orphaned. Starting the
  heavy checks only after the light ones have passed needs no kill at all.

## Non-goals
- Cancelling a heavy check that is already running when another heavy check
  fails (clippy fails while `cargo test` runs). That needs process groups
  and signal forwarding; it waits for data on how often it happens.
- pre-push, which is already fail-fast.
- Re-running only what a fix changed (programme t02).

## Code review
- amont: backend, lang:rust, tui, unix (a new config key and new printed lines)

## Behaviour
- **Two waves at pre-commit.** `run_stage_traced` runs the Light checks
  first, concurrently, as today. If one of them `Failed` at a blocking
  severity, the Heavy checks do not run. Otherwise the Heavy checks run
  concurrently, as today, and each still takes its host slot.
- **The skipped checks are said.** A new `Outcome::Skipped` means not run,
  because a check before it blocked. It is never stamped or attested, never
  blocks by itself (the light failure already blocks), and is printed once
  after the failures, in the shape of the other roll-ups, with a `ui.rs` sign
  (so `NO_COLOR` and non-TTY fall back) and a count, naming the failing check
  when there is one:
  `<sign> 2 check(s) skipped because pre-commit-cargo-fmt failed: pre-commit-clippy, pre-commit-cargo-test`
  then `  fix the failures above and commit again; to run one now:` and one
  indented `amont run <name>` per skipped check, since `amont run` takes one
  name. It is printed in
  quiet mode too (as `could not run` and `deferred` are) and before the
  final `Error raised by:` block.
  The live progress view does not draw them: a slot appears only once its
  check starts (`live.rs`), so a skipped one is never shown.
- **What does not trigger it:**
  - a Light check that fails at `warn`, or `Warned`;
  - `Unavailable`, `Inert`, `Deferred`;
  - a fixer's `Fixed`.
  In all of these the Heavy wave runs. A skipped Heavy check is never also
  reported as deferred or unavailable. A Light check whose thread died counts
  as `Failed` (`dispatch.rs:651`) and so skips the Heavy wave: a dead check
  has not passed.
- **A fixer's rewrite.** A Light fixer that returns `Fixed` has already
  restaged its repair when wave 2 starts, so the Heavy checks judge the
  repaired index. That is the tree to be committed, and better than today's
  race between a fixer and a concurrent heavy check. Stamping is unchanged:
  any `Fixed` withholds every stamp (`dispatch.rs:498-499`).
- **`amont run` too.** `run_all` shares `run_stage_traced`, so the
  rehearsal runs the same waves under the same setting, matching the commit.
- **Off switch.** `git config amont.failFast false` runs one wave, as
  before. Precedence, stated once: `AMONT_FAIL_FAST` (one run), then git
  config (any scope), then `set failFast` in `amont.conf`, then the default
  `true`; a malformed value is reported through `complain` and the
  configured value applies. Documented in `docs/configuration.md`, and named in the ADR and in
  `docs/checks.md` next to the skip line. Live and plain output say the word `skipped`, never a
  glyph alone.
- **The pass-path cost.** A commit that passes starts its Heavy checks once
  the slowest Light check has finished, which was about 4 s in #394. Nothing
  is printed for that wait (decision log). The wave ledger records it, and
  the 14-day measure below checks it.
- **Stamps unchanged.** Only `Passed` and `Fixed` declarations are stamped
  (`dispatch.rs:471-496`). A skipped one earns nothing, so the corrected
  commit runs it.
- The stale `Skip::GatedAtCommit` doc comment from 2.0.1
  (`dispatch.rs:1001-1003`) is corrected in passing.

## Phases
- [x] Phase 1 — implementation:
  - `Outcome::Skipped` in `check.rs`, with every match handled (grep each `Outcome` match across the 21 files and replace each wildcard arm with an explicit one):
    `classify`/`Report` (a `skipped` list and the `announce` line, its sign
    from a new `ui::skipped_sign()` and names through `highlight()`), `live`,
    `gate_stamp`, `run_stamp`, `manifest`, the hooks that match outcomes;
  - the two waves in `run_stage_traced`, splitting on `Check::weight()`
    and keeping `outcomes` and `durations` index-aligned with `checks`;
  - the `amont.failFast` setting in `config.rs`;
  - a best-effort ledger line per pre-commit (as `downgrade::note`): skipped check names and the Light-wave wall time in ms, the data the 14-day measure reads;
  - integration tests in `crates/amont/tests/it/`;
  - docs: `docs/checks.md` (pre-commit) and `docs/configuration.md`;
  - the doc comment fix;
  - an ADR-0020 (`hooks.pre-commit-fail-fast`) plus `aval heads --write`.
- [x] Phase 2 — release amont 2.1.x or the next free version after the open-issues session's 2.1.0, coordinated with that session; install and verify.

## Decision log
- 2026-11-06 — waves by weight, not cancellation, because the check
  runner kills only the direct child and would orphan test binaries.
  Cancellation stays a non-goal until data shows heavy-on-heavy failures
  matter.

- 2026-11-06 — implementation: the plan's "any `Fixed` withholds every stamp
  (`dispatch.rs:498-499`)" was wrong. That `stampable` governs tree gates only.
  A declared gate that `Passed` or was `Fixed` is stamped on the restaged
  index (`dispatch.rs:471-496`). With the waves that stamp is now true, since
  the Heavy gate judged the repaired index. The fixer test asserts it.
- 2026-11-06 — implementation: a Heavy check the waves hold back is `Skipped`
  only if it would have judged something: its scope touches the staged files
  and the repository opted in. Otherwise it is `Inert`, so a repository with
  no Rust never reads "skipped pre-commit-clippy". The first test run caught
  clippy, go vet and pyright named that way.
- 2026-11-06 — implementation: the live view shows no `waiting for quick
  checks` label, and plain mode prints no line when the Heavy wave starts
  (tui round 1, finding 3). A slot that has not started is simply not drawn
  (`live.rs` `running`), so nothing is wrong on screen. A line on every
  passing commit would be noise for a wait of seconds. The wave ledger
  measures the wait; the label waits for that number.

- 2026-11-06 — implementation: the first commit attempt failed
  `spawn_budget::a_small_commit_stays_inside_the_git_spawn_budget` (29 > 26).
  `fail_fast` is now read once per process, and the ledger counts a Heavy
  check only if the change touches its scope, so it spends no `rev-parse`
  on a commit no Heavy check judged. The remaining two reads, `amont.failFast`
  and the cached staged list, raise the budget to 28. The reasons are written
  in the test.

- 2026-11-06 — implementation review Delta (tui, low): the first skip
  line can pass 80 columns with long names. Deliberate: the terminal wraps it
  after the cause, so the failing check and the count stay on the first row,
  and the commands below are one per line.
- 2026-11-06 — implementation review Delta: the discard reasons in
  `waves.rs` sit on the same line (`errors.never-swallowed`), `tmp` is
  renamed `scratch_path`, and the `append` comment says a line above the
  header is unlikely, not impossible (unix).

- 2026-11-06 — implementation review (fresh round on 87608d83): the
  budget comment named a spawn that was not there. Re-measured with a
  counting git shim, the waves add one spawn (the `amont.failFast` read), so
  the budget is 27, not 28. Inside the test, with the bound lowered to
  print the count: 27 with the setting read, 26 under
  `AMONT_FAIL_FAST=false`. A manual shim outside it had counted 29 and 28. The measure also found that the ledger counted
  clippy, go vet and pyright on a `.txt`-only commit, where they return
  `Passed` having judged nothing. It now counts only Heavy checks the change
  reaches, and reads the tracked list only for those.
- 2026-11-06 — unix (low, deliberate): the `append` comment's "the reader
  skips" describes the manual 14-day read, which is the only reader.

## Verification
- **Tests** (a scratch repo per test, as in `gate_pairs.rs`, with a Heavy
  declared gate that logs each run):
  - a blocking Light check fails → the commit is refused, the Heavy log is
    empty, and the skip line names it and the failing Light check;
  - the Light checks pass → the Heavy gate runs once and is stamped;
  - a Light check fails at `warn` → the Heavy gate runs;
  - `amont.failFast false` and a blocking Light failure → the Heavy gate runs
    (today's behaviour);
  - `AMONT_FAIL_FAST=false` with `set failFast true` in `amont.conf` and a
    blocking Light failure → the Heavy gate runs;
  - a skipped Heavy declaration earns no stamp (`gate_stamp` marker empty);
  - a Light fixer returns `Fixed` and a Heavy gate then runs once, on the restaged tree, and is stamped (decision log);
  - `Report::verdict()` is `Proceed` for `[Skipped]`; with `amont.progress` on and off the stage ends without a hang and prints the skip line (stderr is not a terminal in the test, so the painted region is not exercised);
  - `amont run` (staged) with a blocking Light failure → the Heavy log is empty and the skip line is printed, as at commit;
  - revert-red: the skip tests fail with the wave split removed.
- **Real input** (#394's shape): a scratch `cargo new` crate with
  `cargo-test` declared at pre-commit (moved, so Heavy) and a `cargo` shim
  counting runs. Commit an unformatted `src/lib.rs`. Expected: the built-in
  `cargo-fmt` blocks, there is no `test` line in `runs.log`, and the skip
  line names clippy and cargo-test. The installed 2.0.1 on the same commit
  runs `cargo test`.
**Observed (Phase 1, 2026-11-06, branch `fix/pre-commit-fail-fast`):**
- `cargo test -p amont --test it fail_fast`: 11 pass (the eleven cases
  above, the fixer and ledger cases included); the `amont-runtime` dispatch
  unit tests pass (`[Skipped]` is `Proceed`; the `AMONT_FAIL_FAST` resolver).
- Real input, `/tmp/claude-501/ffprobe.sh`: a `cargo new` crate with
  `cargo-test` declared at pre-commit, an unformatted `src/lib.rs` staged,
  and a counting `cargo` shim.
  - Branch binary (re-run after the review fixes): `✗ Unformatted Rust`,
    then `↷ 2 check(s) skipped because pre-commit-cargo-fmt failed:
    pre-commit-clippy, pre-commit-cargo-test`, then `fix the failures above and
    commit again; to run one now:` and one `amont run <name>` line each, before
    `Error raised by: pre-commit-cargo-fmt`. cargo ran only `fmt` (2 calls).
  - Installed 2.0.1, same commit: the same refusal, after `clippy` (2 calls)
    and `test` (1 call) ran.
**Observed (implementation review round 1 fixes, 2026-11-06):**
- `Waves::resolve` is called once by `pre_commit` (and `run_stage`) and
  handed down as `enum Waves { One, LightThenHeavy }`. There is no static.
- The skip line names the failing check when there is one, and puts each
  `amont run <name>` on its own line within 80 columns.
- `Check::would_judge` decides skipped against inert.
- `Outcome::judged_or_skipped` matches every variant.
- The ledger writes its header together with the first line, and compacts
  by writing a temp file and renaming it, with 2 unit tests.
- Tests: `fail_fast` 11 pass, `spawn_budget` 4 pass, and the dispatch and
  waves unit tests 16 pass.
- Revert-red, with `in_waves` replaced by one `run_concurrently`: 7 of 11
  `fail_fast` tests fail, the fixer test among them, so one wave let the
  Heavy gate judge the unfixed content. The 4 that pass expect a run.
- **Real input on the final tree (042d2f90):** the same probe prints the
  skip line and one `amont run` line per check, and cargo ran only `fmt`. The
  wave ledger wrote `1153 2 pre-commit-clippy,pre-commit-cargo-test`: the
  Light wave took 1.2 s, and 2 Heavy checks were skipped.
- **Phase 2, observed 2026-11-07:**
  - v2.2.0 tagged on `da1dfdc7` (#432) with `amont land --tag`. Release
    workflow 42308324797 concluded success: 7 platform builds, GitHub
    release with 7 assets, crates.io 2.2.0, npm, homebrew tap, and "brew can
    install what was published".
  - `brew upgrade fredericrous/tap/amont` → `amont 2.2.0`. The hooks are
    baked to `/usr/local/bin/amont`.
  - Probe with the installed binary: the skip line names clippy and
    cargo-test, and cargo ran only `fmt`.
  - Pushes for #430, #432 and the tag ran with `hook.skip
    pre-push-adr-lint-cli-probe` (approved by the person; amont #431). Its
    removal from the shared config is the person's.
- **14 days after install:** from the pre-commit wave ledger on the laptop, count the
  heavy runs skipped and estimate the minutes saved. Measure the extra start
  delay on passing commits (the slowest Light check per commit). A median
  over 30 s is a reason to revisit.

## Implementation review
- implementation-review (opus): round 1 approve-with-changes (3 blocking), then Delta approve-with-changes, then round 1 on the fix approve-with-changes, then Delta, then approve on 042d2f90. Fixed: no static, reasons on the same line, `scratch_path`, ledger counting, budget 27 with in-test counts. 25/23/21 candidates ruled.
- backend (opus): approve-with-changes, then Delta approve. Fixed: atomic compaction, header written with the first row. Deliberate: an unreadable staged list reads as empty (no failure signal in `staged_files`).
- lang:rust (opus): approve-with-changes, then Delta approve. Fixed: `Waves` enum, `Check::would_judge`, exhaustive `Outcome::judged_or_skipped`, `is_heavy`, restored doc.
- tui (opus): approve-with-changes, Delta approve-with-changes, approve on 87608d83. Deliberate: no waiting label (the ledger measures the wait); the first skip line may wrap past 80 columns.
- unix (opus): approve-with-changes, Delta approve-with-changes, approve on 87608d83. Fixed: compaction by rename, accurate `append` comment. Deliberate: "the reader" is the manual 14-day read.

## Outcome
Shipped in amont 2.2.0. A commit a quick check refuses no longer runs
clippy or the test suite. The change is said, unstamped, and mirrored in
`amont run`, and a ledger measures the cost.
Surprises:
- Code review took four rounds beyond the first, each costing a ~17 min
  commit gate. Most findings were literal rule points (comment placement, a
  placeholder name). Two were real: the ledger counted clippy on a `.txt`
  commit, and a manual spawn count disagreed with the test's own.
- The push hit amont #431: a warn-only Heavy gate never waits for a slot,
  so `land` refused for missing evidence while another session held the
  queue.
- Mid-branch, main moved by 8 PRs (another session's 2.1.0); the rebase
  was clean.
Open: the 14-day measure, due 2026-11-21; #431; removing the `hook.skip`.

<!-- panel: repos=amont adds= reviewers=backend,language,tui,unix body-sha=b6f5751c93dd -->
