# Gate evidence — reading the record the hooks already keep

**A gate that stopped checking anything passes faster than one that works.**
That is the failure this page is about, and nothing inside the hook can see
it: the exit code is 0, the output says what it always said, and the only
thing that changed is the clock.

A fleet audit here on 2026-09-19 found four mechanisms in that state at once
— a lockfile audit run from a directory whose lockfiles were one level down,
a `govulncheck` built with a Go too old to read the vulnerability database,
`uv` invoked with no `.venv` to resolve against, and an `npm` that timed out
without saying so. Between them they hid 99 Python and 12 Go findings. Every
one of them had been green for weeks, and every one of them had gone from
minutes to milliseconds on the day it broke.

The hooks have been recording that number all along, one gate at a time,
and nothing read it back.

## The record

Every push writes one line per gate it ran into the note it already keeps in
`refs/notes/amont-gate`, keyed by the tree the gate ran against:

```text
amont-gate-v1 pre-push-cargo-test
run 1726900000 pre-push-cargo-test pass 412391
run 1726903600 pre-push-audit-js fail 903
```

`run <epoch> <gate> <outcome> <milliseconds>`. Line one is the stamp, exactly
as it has always been — [`amont.pushStamps`](configuration.md#amontpushstamps--remember-what-the-push-gate-already-proved)
reads that line and nothing else, so every version of amont that predates
these lines reads a note that carries them and sees no change at all. The
`run` lines are additive, optional, and are never consulted by any decision
about whether a check may be skipped.

That separation is deliberate and it is the safety property of the whole
feature:

> **Evidence never gates.** A stamp is a record that a check RAN on exactly
> this content; a `run` line is a record of how it went. Forging a `run` line
> gets you a wrong row in a report. Nothing here can make a check be skipped,
> because skipping is decided by line one, which this feature does not touch.

Four things are worth knowing about the shape:

- **Failures are recorded.** The stamp deliberately has no opinion about a
  gate that failed — there is nothing to vouch for — but a dataset of only
  the pushes that succeeded could never produce a failure rate, so the
  blocked-push path records before it leaves.
- **The key is the tree, so the fingerprint is free.** Two runs filed under
  one tree read identical content. If they disagree, the content is not what
  changed.
- **It is local.** Notes in this ref are never pushed, never travel, and are
  deleted by `amont uninstall` along with the rest of amont's own
  bookkeeping. Nothing here is a statement to anybody else's system — that is
  what [`amont.attest`](configuration.md#amontattest--amontattestkey--sign-what-pre-push-proved-for-ci)
  is for, and its signed payload is a separate, versioned contract that this
  does not touch.
- **Only pre-push is recorded.** Pre-commit checks run concurrently and every
  failure is reported, so neither of the questions below has an answer there;
  and the commit path's subprocess budget is guarded at 26 git spawns, which
  a note read and write on every commit in every repository would break for a
  report nobody is blocked on. A commit-time gate's twin is recorded when the
  push side runs it.

## The report

```sh
amont-fleet gates                       # every repository under the scan root
amont-fleet gates --root . --depth 1    # just this one
amont-fleet gates --json
```

Per repository and gate: runs in the window, pass and fail counts, median and
last duration, how long ago it last ran — and the flags below. A gate that
has never run does not appear: this record knows what ran, and `amont list`
is the answer to what is *declared*.

### `no-op suspect`

Two shapes of one failure, and both are needed.

1. **The collapse.** The last run took less than **10%** of the median, for a
   gate whose median is at least **30 s**. Eleven minutes to four hundred
   milliseconds is the signature of a runner that found nothing to run.
2. **Born broken.** The last run *passed* in under **1 s** and no earlier run
   of that gate ever finished that fast. A gate misconfigured from its first
   day has no collapse to measure; what it has is a history that never once
   did real work.

Both are stated with their numbers attached, so the row can be argued with:

```text
pre-push-audit-python  NO-OP SUSPECT: last run 210 ms against a median of 41.3 s
                       (0% of it, threshold 10%)
```

### `flaky`

Two runs against **the same tree** disagreed — one passed, one failed. The
content could not have changed between them, so something outside it decided
the verdict: a port, a clock, a shared fixture, a test that depends on
another test's order.

### `stale`

It has stopped running while its neighbours kept going: **10** later trees
were judged by other gates and not by this one, or its last run is more than
**30 days** old while another gate ran more recently. A repository where
*nothing* has run is quiet, not stale, and is reported as having no record.

### Abstention

Under **5 verdicts** in the window, no flag is computed and the row says so:

```text
pre-push-cargo-test  2 runs  2 pass  0 fail  …  insufficient history (2 verdicts) — no flag computed
```

This is not politeness. A two-run history can be made to look like anything,
and a column that cries wolf on one is a column people stop reading. The same
rule covers a repository whose notes were pruned: no record is reported as no
record, never as zero problems.

### The thresholds are flags

Every number above is a default, printed at the top of every report and
overridable per invocation:

| flag | default | decides |
|---|---|---|
| `--window <days>` | `90` | how far back the report looks |
| `--min-runs <n>` | `5` | verdicts below which nothing is flagged |
| `--noop-ratio <pct>` | `10` | a last run under this share of the median |
| `--noop-median <secs>` | `30` | …for a gate whose median is at least this |
| `--fast-pass <ms>` | `1000` | a pass under this, never once seen before |
| `--stale-pushes <n>` | `10` | later trees other gates judged and it did not |
| `--stale-days <days>` | `30` | …or this long since it last ran |

`gates` reports; it does not gate. A finding is printed, not exited on — the
one non-zero exit is a scan that found no repositories at all, which is the
rest of the tool's rule.

## Evidence ordering (opt-in)

```sh
git config amont.order evidence    # default: declared
```

Pre-push runs its checks serially and stops at the first blocking failure
(see [hook architecture](hook-architecture.md)). Which means the ORDER
decides how long a push that is going to fail takes to say so — and the
registry's order is a fixed guess made once, for every repository.

With `evidence`, the push gates are ordered by this repository's own record:
the gates that have actually failed in the last 90 days are attempted first,
ordered by **failures per unit of time** — `failures / runs / median
duration` — and everything else keeps its declared order behind them. The
ratio, rather than the failure rate alone, is what minimises the time a
failing push spends before it fails: a five-second audit that catches one
push in six is worth attempting before a twenty-minute suite that catches one
in three.

What it does not do, stated because an optimisation that quietly changes
enforcement would be a much worse deal than a slow push:

- **It never skips a check.** The order is a permutation. Every gate that
  would have run still runs, and a gate is never assumed to pass because the
  record says it usually does. A prediction is not a run, and a check that
  does not run cannot be stamped or attested — the whole chain from
  [the stamps](configuration.md#amontpushstamps--remember-what-the-push-gate-already-proved)
  to [the attestation](ci.md) rests on a record of something that happened.
- **It never moves the push-shaped checks.** Only the *scoped* gates — the
  suites and audits, the ones whose verdict is a function of the content —
  are permuted, and only among the positions they already occupy.
  Branch-protect, branch-pattern, secrets and pull-rebase keep theirs
  absolutely: the registry orders them "cheapest and most decisive first",
  and discovering a protected branch after twenty minutes of tests is exactly
  the waste this feature exists to remove.
- **With no record it is the declared order**, exactly. A fresh clone, a
  pruned ref, a git that would not answer: all of them fall back, and the
  first push after turning the key on changes nothing.
- **It is reported.** When the order differs from the declared one, the push
  says so and names the order it took.

It can be set per machine (`git config amont.order evidence`) or committed
for the team (`set order evidence` in `amont.conf`, which is
[trust-gated](trust.md) like every other policy line). A local `git config`
outranks the committed value, as it does for every key.

## What this is not

- **It is not a test selector.** Nothing chooses which tests a suite runs;
  amont does not know what is inside one.
- **It is not a prediction, and it does not act on one.** Every flag above
  is a statement about runs that happened, and the only thing the ordering
  does with a prediction is decide what to try first.
- **It does not replace looking.** `no-op suspect` is a suspicion, named as
  one. Its job is to put a number in front of somebody, not to conclude. The
  2026-09-19 audit found its four mechanisms by hand, and the point of this
  page is that it should not have had to.
