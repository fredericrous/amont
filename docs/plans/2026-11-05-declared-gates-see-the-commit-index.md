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
- `index` is part of the gate's token only where declared: a marked gate's
  earlier passes stop counting, every other gate keeps its token. A tool that
  learns `AMONT_INDEX_FILE` changes its binary identity when it is installed,
  which voids its earlier passes the same way.
- docs/custom-checks.md documents both.

## Phases
- [x] Phase 1 — amont: `AMONT_INDEX_FILE`, the `index` marker, the token, tests, docs

## Rollback
Revert the PR. A repository that declared `index` then reads it as the program
name, so take the marker out first.

## Decision log
- 2026-11-05 — Options put to the person: an opt-in marker, `AMONT_INDEX_FILE` for every gate, `GIT_INDEX_FILE` for every pre-commit gate, or the marker plus `AMONT_INDEX_FILE`. The person said "choose for me … automation and least human interaction". I chose the marker plus `AMONT_INDEX_FILE`: the variable needs no declaration change for a tool that reads it, and the marker covers tools that cannot learn it. Handing `GIT_INDEX_FILE` to everything was rejected for the fixture-repository risk.
- 2026-11-05 — Contract agreed with the suppressions-commit-gate session, which owns the amont-agent consumer: a non-empty `AMONT_INDEX_FILE` takes precedence there.

## Verification
- `crates/amont/tests/it/staged_index.rs`, each one red with the fix removed (checked by disabling it):
  - `commit -a` from a subdirectory, gate declared `index`: blocks the bad index content, accepts good → passes
  - no marker, gate reading `$AMONT_INDEX_FILE` alone: accepts good, blocks bad → passes
  - an unmarked gate sees no `GIT_*` variable, and `AMONT_INDEX_FILE` is absolute → passes
  - linked worktree, `git commit -- <path>` from a subdirectory → passes
  - a relative caller `GIT_INDEX_FILE` is handed on absolute and judged → passes
  - a pre-push command never inherits a caller's `AMONT_INDEX_FILE` → passes
- Unit test: `index` parses with other markers, and is refused on pre-push and beside `platform=any` → passes
- clippy `-D warnings` and fmt clean on the pinned toolchain → passes
- The workspace suite → passes (623 integration, 725 unit; exit 0)

## Outcome
