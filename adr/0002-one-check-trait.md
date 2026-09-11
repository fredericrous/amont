---
id: ADR-0002
status: accepted
decisions:
  - key: arch.check-abstraction
    choice: One `Check` trait
    first: true
    reason: Four tables keyed by name need reconciliation tests to stay in step
  - key: arch.outcome-model
    choice: Three-valued — pass, fail, fixed
    first: true
    reason: Two values cannot express "it was wrong and now it is not"
  - key: arch.scope-model
    choice: A struct, read as a conjunction
    first: true
    reason: An enum of alternatives needed a `Custom` variant that swallowed the set
  - key: arch.severity
    choice: Declared on the trait, overridable per repository
    first: true
    reason: Downgrading by skipping turns "warn me" into "tell me nothing"
  - key: arch.external-ordering
    choice: Externals are appended to a stage and cannot precede a built-in
    first: true
  - key: arch.external-severity
    choice: A declared check may block; the author chooses
    first: true
---
# 0002 — one Check trait

Detail is in [`docs/hook-architecture.md`](../docs/hook-architecture.md),
marked shipped.

## One trait rather than four tables

The alternative was four tables keyed by check name — what it is called, when
it runs, what it applies to, how loud it is — kept in agreement by
reconciliation tests. That is a design whose correctness is a test rather than
a type.

## Three outcomes, because two cannot say it

A formatter that rewrites a file has neither passed nor failed. It found a
problem and removed it. `Outcome` says so; a boolean would force every caller
to guess which half of the truth to report.

## Scope is a conjunction, not a choice

The enum-of-alternatives version needed a `Custom` variant, and once that
existed most real scopes would have been `Custom` — which is another way of
saying the enum was not the shape of the problem. The struct says "these
extensions **and** this filename **and** this shebang", which is what a scope
actually is.

## Severity is declared, then owned by the repository

A check ships with an opinion about whether it should block. A repository may
disagree, and `amont.severity.<check> warn` is how it says so. The important
part is that this is *not* the same as skipping: a warned check still runs and
still reports, and a repository that has switched one off can see that it has.

## Externals are appended, and that is not configurable

A declared check cannot run before a built-in. Making the order configurable
would make the question "what already ran when my check runs" unanswerable
without reading a config, and every declared check would have to be written
defensively.
