---
id: ADR-0003
status: accepted
decisions:
  - key: hooks.install-mechanism
    choice: Five copied dispatcher files, never core.hooksPath
    first: true
    reason: core.hooksPath is all-or-nothing per repository, and an install deleted one
  - key: hooks.index-fidelity
    choice: The staged content, by stashing the worktree
    first: true
    reason: Judging the file on disk judges content the commit will not contain
  - key: hooks.index-fidelity
    scope: pre-push
    choice: Not stashed; the same problem reached by a different route
    first: true
    overrides: ADR-0003
    reason: A push has no staging area to disagree with, and its own fidelity gap
  - key: hooks.fix-outcome
    choice: A check may rewrite what it judges, and re-stages the result
    first: true
  - key: hooks.fix-outcome
    scope: pre-push
    choice: Invalid, and refused when the declaration is parsed
    first: true
    overrides: ADR-0003
    reason: There is nothing to re-stage at push, and parse time is the cheap place to say so
  - key: hooks.failure-collection
    scope: pre-commit
    choice: Run concurrently and report every failure
    first: true
  - key: hooks.failure-collection
    scope: pre-push
    choice: Run serially and stop at the first
    first: true
  - key: cli.all-files-semantics
    choice: Implies no stash
    first: true
  - key: cli.check-command
    choice: A read, not a rehearsal
    first: true
    reason: Its findings carry positions, which a pass/fail rehearsal does not need
---
# 0003 — checks read the index, and what that means per stage

Detail is in
[`docs/index-fidelity-and-run-modes.md`](../docs/index-fidelity-and-run-modes.md).
That document notes about itself that it said "nothing here is built" for six
sections after five of them had landed, which is the kind of thing this corpus
exists to stop happening silently.

## The rule

A check reads the **staged** content. The worktree is stashed first, so what is
judged is what the commit will contain, rather than whatever happens to be on
disk at the time.

## Why the stage axis is real here, and not an exception list

Three decisions genuinely differ by stage. Recording them as one rule with
exceptions would be a fiction; recording them as scoped entries says which
stage answers which way, and `overrides` keeps the default's head where it is.

- **`hooks.index-fidelity`** — `pre-push` is not stashed. It is not exempt from
  the problem either: it has its own fidelity gap by another route, which the
  source document deliberately files as its own item rather than as a footnote
  to this one.
- **`hooks.fix-outcome`** — a fixing check makes no sense at `pre-push`, where
  there is nothing to re-stage. It is refused **when the declaration is
  parsed**, not when the push happens. Same fact, discovered earlier, by the
  person who wrote it, rather than later and by everyone.
- **`hooks.failure-collection`** — `pre-commit` runs concurrently and reports
  everything, because a commit is cheap to retry and a list of four problems
  beats four rounds. `pre-push` runs serially and stops at the first, because
  it is the expensive gate and there is no value in continuing past a verdict.

## `amont check` reads

`amont check <paths…>` reports findings with positions. It is not a rehearsal
of the hook — `amont run` is that. The distinction matters because a rehearsal
only has to decide pass or fail, while a read has to say where, and building
one to do the other's job makes both worse.

## Why not `core.hooksPath`

This is the one that has an incident behind it, so it is recorded rather than
left as taste.

`core.hooksPath` is all-or-nothing per repository. "Managed by amont" and
"managed by something else" stop being separately expressible, a repository
with hooks of its own loses them silently, and a colleague can no longer read the
dispatcher in the repository's own hooks directory to find out what will run.

Eleven repositories in this fleet had `core.hooksPath` pointed at husky's
directory. An install wrote the dispatchers there, `npm install` deleted them,
and a push to a protected branch went through unchecked. Five copied files
cannot be removed by another tool's install step.
