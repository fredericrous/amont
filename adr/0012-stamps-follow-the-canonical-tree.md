---
id: ADR-0012
status: accepted
decisions:
  - key: hooks.gate-stamp-key
    scope: pre-push
    choice: A stamped gate result is keyed by the git tree and, per token, by the gate's identity (the full command and the resolved program binary's path, size and modification time; for a built-in, its name and amont's version); for a gate whose inputs the repository declares in attest-inputs it is also filed under the fingerprint of those inputs, offered only to a gate that declares the tracked manifest; and with no attest-inputs file at all it is also filed under the canonical fingerprint of the tree (the tree with the content of the top-level docs/plans/ left out and its existence kept), unless the repository sets planExempt off; only a complete namespaced token authorizes a skip, and an identity, a fingerprint or an opt-out that cannot be read is a cache miss
    replaces: [ADR-0011]
    reason: The declared route needed a file, a completeness check and a trust step in every repository; the canonical tree covers the same closing plan commit with nothing declared, and an invalid, ambiguous or unlistable spec still means the gate runs
  - key: hooks.gate-stamp-key
    scope: pre-commit
    choice: At commit time the same rule applies to the staged tree, so partial staging is judged on what will be committed, never on the working copy
    replaces: [ADR-0011]
    reason: The commit-time reader is the first to see a plan-only commit and must follow the same key as the push
---
# 0012 — a stamp follows the canonical tree when nothing is declared

Plan: [`docs/plans/2026-10-15-plan-only-commits-skip-no-declaration.md`](../docs/plans/2026-10-15-plan-only-commits-skip-no-declaration.md)
(a pointer; the plan lives in the attest repository).
Fleet record: `decisions:ADR-0035`, which allows the same widening for a CI skip.

## Context

ADR-0011 keyed a declared gate by its inputs. That worked here, and in every
other repository it needed a declaration, a completeness check, a manifest guard
and a trust prompt for what is, nearly everywhere, the same sentence: everything
tracked except `docs/plans/`.

## Decision

**A repository that declares nothing is keyed by the canonical tree.**

- The push adds `canon <fp>` to the signed block and files the block under the
  canonical key in attest's inputs ref, whether or not a valid spec exists.
- The local stamps of a repository with no spec file at all (a successful
  `ls-tree` that lists neither location) are also filed under the canonical
  fingerprint, in `refs/notes/amont-gate-inputs`, and honoured at pre-commit
  (on the staged tree) and pre-push. A valid spec keeps its declared route; an
  invalid, ambiguous or unlistable one gets neither, and the gate runs.
- `amont.conf` is part of the canonical tree, so no manifest rule is needed.
- `set planExempt off` opts a repository out. It is read from the committed
  bytes, never through the trust-gated settings, by one tolerant reading shared
  with attest, and fails narrow: a failing `ls-tree` or an unreadable
  `amont.conf` is off. The line itself is strict in amont, exactly `on` or
  `off`, once, plain blanks, so a shape attest would read as "on" is a broken
  line.

## Consequences

- No file, check or trust step per repository; an existing declaration keeps
  working.
- A gate that reads the content of a plan must opt out or ignore `docs/plans`
  (`decisions:ci.skipped-gates-do-not-read-plans`). A 27-repository audit found
  none that is skipped and reads plan content.
- About eight git spawns per pushed commit on the canonical route, uncached; the
  cost is stated, not hidden.
