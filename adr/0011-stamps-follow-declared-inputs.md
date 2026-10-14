---
id: ADR-0011
status: accepted
decisions:
  - key: hooks.gate-stamp-key
    scope: pre-push
    choice: A stamped gate result is keyed by the git tree and, per token, by the gate's identity (the full command and the resolved program binary's path, size and modification time; for a built-in, its name and amont's version), and for a gate whose inputs the repository declares in attest-inputs it is also filed under the fingerprint of those inputs, offered only to a gate that declares the tracked manifest; only a complete namespaced token authorizes a skip, and an identity or fingerprint that cannot be computed is a cache miss
    replaces: [ADR-0010]
    reason: Keyed by the whole tree, the closing plan commit of every branch missed every stamp and re-ran a 32 minute suite for content it had already judged; the declared inputs were unchanged
  - key: hooks.gate-stamp-key
    scope: pre-commit
    choice: At commit time the same rule applies to the staged tree, so partial staging is judged on what will be committed, never on the working copy
    first: true
    reason: ADR-0010 decided the key at pre-push only, and the commit-time reader is the first to see a plan-only commit
---
# 0011 — a stamp follows the inputs its gate declares

Plan: [`docs/plans/2026-10-14-plan-only-commits-ride-attestations.md`](../docs/plans/2026-10-14-plan-only-commits-ride-attestations.md).
Fleet record: `decisions:ADR-0033`, which allows the same widening for a CI skip
and states the completeness rule this record applies locally.

## Context

ADR-0010 keyed a gate stamp by the git tree and the gate's identity, and said
why the tree was enough: the tree holds the manifest, the lockfiles and every
config the check reads, so hashing them again would add nothing.

That is true and it is also the cost. Every branch ends with a commit that
changes only `docs/plans/*.md`. It changes the tree and nothing a gate reads, so
under a tree key it missed every stamp: the local pre-push gate re-ran
`cargo test` (32 min cold) for content it had already judged.

Attest already gives a repository a way to say what a gate reads
(`.github/attest-inputs`) and a fingerprint over exactly those bytes. CI uses
it (`decisions:ADR-0033`). The local stamps did not.

## Decision

**A declared gate is also keyed by its inputs.**

- For a gate the repository's input declaration names, every writer of a stamp
  (`record`/`bind_to_head`, `stamp_push`, `stamp_tree`) also files the full
  token under the gate's fingerprint, in `refs/notes/amont-gate-inputs`. The
  tree note is still written. Every reader (`stamps_for`,
  `vouched_for_staged_tree`, `tree_tokens`, the rehearsal) honours a token
  found in either place.
- The token is unchanged: `<name>@<identity>`. The identity still has to
  match, so a changed command, tool or amont version is a miss. A token filed
  under a gate's key is honoured only for that gate's name.
- **The manifest is an input.** ADR-0010 relied on the tree to carry the
  manifest. Under a fingerprint it does not, so a tree that tracks `amont.conf`
  offers the route only to a gate whose declared inputs cover it. Otherwise the
  gate keeps the tree key and a scope-only edit misses, as before.
- **At commit time the key is the staged tree** (`git write-tree`). At push time
  it is each pushed tip's tree, and every relevant commit must still carry the
  stamp.
- **Fail open.** No declaration, an invalid or ambiguous one (both
  `.forgejo/` and `.github/`), a gate with no fingerprint (a required path is
  missing), git failing: the route contributes nothing and the gate runs. A gate
  the declaration does not name keeps the tree key unchanged.
- **Completeness is checked, not trusted.** `amont attest check-inputs` fails
  unless each named gate declares every tracked file, per gate and by ancestry,
  with `docs/plans/` the only exemption. `make check` runs it with the gates in
  `ATTEST_GATES`. That is the rule `decisions:ADR-0033` makes a condition.
- The ref's keys are not objects: never `git notes prune` it and never merge it
  with `cat_sort_uniq`. `FORMAT` stays `amont-gate-v1`.

## Why

- **What a gate reads is the only honest key.** The tree over-approximates it,
  which is safe and expensive. The declared inputs are exact, which is cheap and
  only as safe as the declaration is complete, so the completeness check is part
  of the decision, not an extra.
- **One fingerprint, two readers.** CI and the local stamps compute it from the
  same module, so "identical inputs" means the same thing in both places.
- **Fail open costs a run, never a skip.** Every doubt falls back to the
  behaviour ADR-0010 decided.

## Consequences

- A plan-only commit reuses the stamps of the code commit before it: measured,
  a pre-push rehearsal of 2 s against 32 min.
- A repository that declares nothing sees no change.
- Settings that live outside tracked files are in no fingerprint. They were not
  in the tree key either.
- A new top-level path or documentation page fails `make check` until it is
  declared for every gate in `ATTEST_GATES`.
- An older amont ignores the new ref, finds no stamp and runs the gate: slower,
  never wrong. A stamp's identity includes amont's version, so a stamp written
  by one release is not honoured by another.
- ADR-0010's decision on `cli.land-verb` is unchanged.
