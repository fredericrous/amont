---
status: active
branch: fix/staged-gate-sees-commit-index
repos: [amont]
adrs: [ADR-0010]
---
# Declared pre-commit gates see the index being committed

## Context
`External::execute` strips every `GIT_*` variable before a declared command
runs (`strip_git_env`), so a test suite's fixture repositories cannot write
into the real one. That strip also takes `GIT_INDEX_FILE`, the only place git
names the TEMPORARY index of `git commit -a` and `git commit <paths>`.

A declared gate that reads the index (`amont-agent suppressions --staged`,
any `git diff --cached` wrapper) therefore judged the old index. Under
`commit -a` from a subdirectory it reported "0 files", passed, and its pass
was stamped onto the tree actually committed. The suppressions-commit-gate
session found it on 2026-11-05 (relais run run-65f89214c1245-ca85). A
handwritten hook calling the same binary blocks the same content.

The person asked for the fix and left the design to me, favouring automation
and the least human interaction.

## Goal
Every declared pre-commit command can find the index being committed, without
a test suite inheriting a git variable it could misuse. The repo author has
nothing to do for a tool that reads `AMONT_INDEX_FILE`.

## Non-goals
- Not changing `strip_git_env`, or what any other caller strips.
- Not handing `GIT_INDEX_FILE` to every command: a pre-commit suite running
  `git add` in a fixture would write into the commit's index.
- Not changing pre-push, which has no index being committed.
- Not changing amont-agent: its consumer change is the suppressions-commit-gate
  session's, and amont never names it (every Green Harness part is optional).

## Code review
- amont: backend, lang:rust, unix

## Behaviour
- Every declared pre-commit command gets `AMONT_INDEX_FILE`: the absolute path
  of git's `GIT_INDEX_FILE` when the hook got one, else
  `git rev-parse --git-path index`. A relative value is resolved against the
  hook process's directory, which is where git ran the hook.
- The `index` command-column marker, beside `files`/`heavy`, also hands that
  command `GIT_INDEX_FILE` (the same absolute path). It is refused on `pre-push`
  and beside `platform=any` (a pod cannot read this machine's index).
- An inherited `AMONT_INDEX_FILE` is removed from every declared command first,
  so a pre-push command never sees one.
- Every pre-commit declared gate's token gains a version, and `index` where
  declared: passes recorded before the variable existed stop counting, so each
  gate runs once more after the upgrade (a wrapper script's binary identity is
  its interpreter, so a fixed tool behind it would not void them). Pre-push
  tokens are unchanged.
- `AMONT_INDEX_GIT_DIR` names the repository the index belongs to: a consumer
  honours `AMONT_INDEX_FILE` only where its own absolute git dir is that one,
  so a suite's fixture repository never reads the outer index.
- When amont cannot locate the index, an `index` gate is refused and others
  get a warning naming the gate.
- docs/custom-checks.md documents both.

## Phases
- [x] Phase 1 — amont: `AMONT_INDEX_FILE`, the `index` marker, the token, tests, docs

## Rollback
Revert the PR. A repository that declared `index` then reads it as the program
name, so take the marker out first.

## Decision log
- 2026-11-05 — Options put to the person: an opt-in marker, `AMONT_INDEX_FILE` for every gate, `GIT_INDEX_FILE` for every pre-commit gate, or the marker plus `AMONT_INDEX_FILE`. The person said "choose for me … automation and least human interaction". I chose the marker plus `AMONT_INDEX_FILE`: the variable needs no declaration change for a tool that reads it, and the marker covers tools that cannot learn it. Handing `GIT_INDEX_FILE` to everything was rejected for the fixture-repository risk.
- 2026-11-05 — Implementation review (implementation-review, backend, lang:rust, unix; all approve-with-changes). Fixed: the token version for every pre-commit gate (backend: a wrapper's identity is its interpreter); `AMONT_INDEX_GIT_DIR` binding (backend, unix: the variable reached fixture repositories); a failed lookup refuses an `index` gate and warns otherwise (lang:rust, unix); `RunsOn` replaces `platform_any`/`index` booleans so the forbidden pair cannot be built (lang:rust); the lookup reads bytes (unix); it runs per gate rather than through a process-wide cache, which the delta review refused (dependencies.explicit), at a few milliseconds of `git rev-parse` each; the tests check their setup, name the refusing gate and accept good content (implementation-review, lang:rust). The amont-agent consumer's precedence changes to match the git-dir binding.
- 2026-11-05 — The pre-push rehearsal failed twice on `run_stamp::busy_host::a_deferred_gate_leaves_no_token` (exit 0, not 4), which passes alone. Cause: a rehearsal runs the suite with `AMONT_ON_BUSY=wait` (ADR-0013), and the test's `amont` children inherited it, overriding the fixture's `set onBusy defer`; reproduced by setting the variable. The `run_stamp` helper and its direct hook call now remove it, as `busy_defers` already did. A test fix, unrelated to the index; on `origin/main` the same rehearsal would fail the same way.
- 2026-11-05 — Contract agreed with the suppressions-commit-gate session, which owns the amont-agent consumer: a non-empty `AMONT_INDEX_FILE` takes precedence there.

## Verification
- `crates/amont/tests/it/staged_index.rs`, each one red with the fix removed (checked by disabling it):
  - `commit -a` from a subdirectory, gate declared `index`: blocks the bad index content, accepts good → passes
  - no marker, gate reading `$AMONT_INDEX_FILE` alone: accepts good, blocks bad → passes
  - an unmarked gate sees no `GIT_*` variable, and `AMONT_INDEX_FILE` is absolute → passes
  - linked worktree, `git commit -- <path>` from a subdirectory → passes
  - a relative caller `GIT_INDEX_FILE` is handed on absolute and judged; bad refused by the gate, good accepted → passes
  - a pre-push command never inherits a caller's `AMONT_INDEX_FILE` → passes
- Unit test: `index` parses with other markers, and is refused on pre-push and beside `platform=any` → passes
- Unit test: an index that cannot be located refuses a gate declared `index` and warns any other; a located one sets `AMONT_INDEX_FILE`, `AMONT_INDEX_GIT_DIR`, and `GIT_INDEX_FILE` for `index` only → passes
- Unit test: a pre-commit token differs from the one recorded before the version, `index` changes it again, a pre-push token is unchanged → passes
- End to end (suppressions-commit-gate session, this branch's debug build with amont-agent 61a49e25): the unmarked suppressions pack blocks subdir `commit -a` in primary and linked worktrees, `commit <path>`/`--only`, and an old false-clean stamp stops counting after the binaries are replaced in place → passes (~/.claude/amont-agent/attestations/suppressions-2026-11-05/)
- clippy `-D warnings` and fmt clean on the pinned toolchain → passes
- The workspace suite → passes (623 integration, 725 unit; exit 0)

## Outcome
