---
id: ADR-0019
status: accepted
decisions:
  - key: hooks.pair-covered-by-tip
    scope: pre-push
    choice: "A pushed ref of a declared pair WITHOUT the `files` marker is covered when its tip's own stamp key — the tree note, its declared-input fingerprints and its canonical tree, as `gate_stamp::stamps_for` reads them — holds the commit-time pair's token, checked after the coverage guards; a covered ref adds no unstamped or deferred commit, and a second line names the tip when an older commit was unstamped. Otherwise every scoped commit in the range must carry the stamp, as before"
    first: true
    reason: "The per-commit rule ran the push suite whenever ONE commit in the range lacked a stamp, even when the tip's whole tree had passed: relais's hookless commits, rebases and busy-host deferrals re-ran 19–50 min suites on code already verified (decisions programme proportional-ship, t00 and t09: about 209 of 516 pre-push minutes over 10 PRs)"
---
# 0019 — a stamped tip covers its ref

Plan: [`docs/plans/2026-11-06-a-push-trusts-a-passed-commit.md`](../docs/plans/2026-11-06-a-push-trusts-a-passed-commit.md).
Narrows the per-commit pairing of the gate-pair contract, and keeps
[ADR-0015](0015-a-runner-earns-the-stamp.md)'s promise that a pushed exact
tree skips the suite when older commits in the range carry no record.

## Context

`pair_verdict` skipped a push-side twin only when every pushed commit that
touched the pair's scope carried the commit-time stamp. One unstamped commit
— made by relais without hooks, rewritten by a rebase, or deferred on a busy
host — re-ran the whole suite on the tip, even when the tip's own tree had
passed. It never asked the tip.

Not every commit-time gate judges the tree: a `files` declaration is handed
only its commit's matched paths, and its stamp vouches for those.
[ADR-0016](0016-a-gate-may-run-on-the-cluster.md) already reads a gate
without `files` as judging the whole tree.

## Decision

**The tip's own stamp key decides first.** For a pair without `files`, a ref
whose tip's tokens (tree note, declared-input fingerprints per
[ADR-0011](0011-stamps-follow-declared-inputs.md), canonical tree per
[ADR-0012](0012-stamps-follow-the-canonical-tree.md)) include the pair token
is covered. No new key is invented: a declared input, a repository-owned gate
script or `amont.conf` that changed since the passing commit gives the tip a
different key, and it vouches for nothing.

**After the coverage guards.** A ref whose changes the pair's scope does not
cover stays `NotPaired`, however well stamped its tip.

**Otherwise nothing changes.** The per-commit rule, the deferral excuse and the
`Unstamped(n)` count apply as before.

**Say what the skip rests on.** Only when a ref had an unstamped commit and
its tip covered it, a second line follows the gated one:
`the code at tip <short sha> passed at commit`, every such tip named. "The
code", because a plans-only tip never ran the gate itself; its canonical tree
did. A fully stamped push prints the plain line alone, which keeps meaning
"every pushed commit carries its stamp".

## Consequences

- relais's hookless commits and deferred commits below a verified tip no
  longer re-run the push suite.
- A rebased tip has a new tree and no stamp, so a rebase still re-runs the
  suite; re-running only what changed is a later decision (programme t02).
- A `files` pair, or a command that picks staged paths itself, keeps
  per-commit coverage; `docs/custom-checks.md` says when to mark it.
- The npm `GATE` pairing keeps its own per-commit rule for now.
