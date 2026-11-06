---
status: active
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
  (so `NO_COLOR` and non-TTY fall back) and a count:
  `<sign> 2 check(s) skipped, a blocking check failed: pre-commit-clippy, cargo-test`
  then, as `announce_deferred` does, the next step on its own line:
  `  fix the failures above and commit again · run them now: amont run pre-commit-clippy · amont run cargo-test` (one `amont run <name>` per skipped check, since `amont run` takes one name). It is printed in
  quiet mode too (as `could not run` and `deferred` are) and before the
  final `Error raised by:` block.
  The live progress view closes their slots with the same outcome.
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
  `true`; a malformed value is reported through `complain` and the default
  applies. Documented in `docs/configuration.md`, and named in the ADR and in
  `docs/checks.md` next to the skip line. Live and plain output say the word `skipped`, never a
  glyph alone.
- **The pass-path cost.** A commit that passes starts its Heavy checks once
  the slowest Light check has finished, which was about 4 s in #394. The
  live view shows the Heavy slots as `waiting for quick checks`, and plain
  mode prints one line when the Heavy wave starts. The 14-day measure below
  checks the delay.
- **Stamps unchanged.** Only `Passed` and `Fixed` declarations are stamped
  (`dispatch.rs:471-496`). A skipped one earns nothing, so the corrected
  commit runs it.
- The stale `Skip::GatedAtCommit` doc comment from 2.0.1
  (`dispatch.rs:1001-1003`) is corrected in passing.

## Phases
- [ ] Phase 1 — implementation:
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
- [ ] Phase 2 — release amont 2.1.x or the next free version after the open-issues session's 2.1.0, coordinated with that session; install and verify.

## Decision log
- 2026-11-06 — waves by weight, not cancellation, because the check
  runner kills only the direct child and would orphan test binaries.
  Cancellation stays a non-goal until data shows heavy-on-heavy failures
  matter.

## Verification
- **Tests** (a scratch repo per test, as in `gate_pairs.rs`, with a Heavy
  declared gate that logs each run):
  - a blocking Light check fails → the commit is refused, the Heavy log is
    empty, and the `↷ skipped … fix the failures above first` line names it;
  - the Light checks pass → the Heavy gate runs once and is stamped;
  - a Light check fails at `warn` → the Heavy gate runs;
  - `amont.failFast false` and a blocking Light failure → the Heavy gate runs
    (today's behaviour);
  - `AMONT_FAIL_FAST=false` with `set failFast true` in `amont.conf` and a
    blocking Light failure → the Heavy gate runs;
  - a skipped Heavy declaration earns no stamp (`gate_stamp` marker empty);
  - a Light fixer returns `Fixed` and a Heavy gate then runs once, on the staged tree, with no stamp;
  - `Report::verdict()` is `Proceed` for `[Skipped]`, and the live view closes skipped slots with `amont.progress` on and off;
  - `amont run` (staged) with a blocking Light failure → the Heavy log is empty and the skip line is printed, as at commit;
  - revert-red: the skip tests fail with the wave split removed.
- **Real input** (#394's shape): a scratch `cargo new` crate with
  `cargo-test` declared at pre-commit (moved, so Heavy) and a `cargo` shim
  counting runs. Commit an unformatted `src/lib.rs`. Expected: the built-in
  `cargo-fmt` blocks, there is no `test` line in `runs.log`, and the skip
  line names clippy and cargo-test. The installed 2.0.1 on the same commit
  runs `cargo test`.
- **14 days after install:** from the pre-commit wave ledger on the laptop, count the
  heavy runs skipped and estimate the minutes saved. Measure the extra start
  delay on passing commits (the slowest Light check per commit). A median
  over 30 s is a reason to revisit.

## Outcome

<!-- panel: repos=amont adds= reviewers=backend,language,tui,unix body-sha=b6f5751c93dd -->
