# Interactive `hook.skip` management

Status: **shipped**, with one section kept as history because the reasoning is
worth more than the design it produced.

Extends `docs/fleet-dashboard.md`, which listed the `s` toggle as a v2 item
without saying what it should do.

## Naming a check

A check's **id** is `<trigger>-<name>` — `pre-commit-clippy`. Exactly three
things name it, and both config surfaces resolve all three identically:

| written | means | example |
|---|---|---|
| the full id | that one check | `pre-commit-clippy` |
| a trigger | every check on that trigger | `pre-commit` |
| a short name | that check, on any trigger | `clippy` |

Three exact comparisons. No substring. So:

```sh
git config --add hook.skip pre-commit-clippy   # one check
git config --add hook.skip clippy              # that check, either trigger
git config --add hook.skip pre-commit          # all fifteen pre-commit checks
git config amont.severity.clippy warn       # same vocabulary, other surface
```

`hook.skip e` matches **nothing**. Where several severity keys reach one check,
the most specific wins — full id > short name > trigger — so you can downgrade a
whole trigger and then exempt one check from it.

Declared checks in `amont.conf` have ids too, so `hook.skip pre-commit`
covers them. See `docs/custom-checks.md`.

## History: the problem this was written to solve

Kept because the sequence — measure, ship the cheap safety net, then find the
real fix — is the reasoning, and a rewritten doc that hid it would read as though
the right answer had been obvious.

`hook.skip` used to match by **substring**: `check_name.contains(skip_value)`.
Measured against the 20 checks of the day:

| `hook.skip` value | suppressed |
|---|---|
| `pre-commit-clippy` | 1 / 20 |
| `cargo` | 2 / 20 |
| `lint` | 5 / 20 |
| `pre-commit` | 15 / 20 |
| `t` | **19 / 20** |
| `e` | **20 / 20** |

None of those are adversarial. `t` is a plausible shorthand for "tests"; `pre`
for "prettier". Either silently disabled the whole suite, in a config file nobody
reads, with no output at commit time saying so.

Worse, no value could express "this check only": `pre-commit-lint-js` is a prefix
of `pre-commit-lint-json-yaml`, so skipping the first unavoidably skipped the
second. The full id was not a safe value either, and an earlier revision of this
document was wrong to imply it was.

Two things followed, in order.

**First, the announcement**, because a UX review against usage traces changed the
priority. 1 of 96 repos had any skip at all, and its value (`run-tests-js`) was
hand-written at a terminal. Nobody reached for a dashboard, because editing
config takes ten seconds — while the consequence landed on every commit. So the
dispatcher started saying what it skipped:

```
  ! 15 checks skipped by hook.skip: pre-commit-argo-lint, pre-commit-ban-terms, …
```

Silent when nothing is skipped. It reaches **every** skip however it was created,
needs no dashboard, and turned `hook.skip = e` from invisible into unmissable
within one commit.

**Then the vocabulary**, which removed the hazard rather than reporting it. It
was prompted by a different bug: `hook.skip` matched by substring and
`amont.severity.<key>` matched exactly, on the same identifiers, so
`hook.skip clippy` worked and `amont.severity.clippy warn` silently did
nothing. Fixing the disagreement meant picking one rule, and the only rule that
serves both is exact naming.

Three parts of this document were built and are now retired by that change:

- **The typed confirmation** for a skip reaching more than one check. It existed
  because a value could silently take four when you asked for one. An id names
  exactly one check, so the dashboard's toggle can no longer over-reach and the
  gate had become unreachable code.
- **The fragment diagnosis** — recognising `run-tests-js` as a fragment and
  offering `pre-push-run-tests-js` instead. It is not a fragment; it is the short
  name, and it means what its author meant.
- **"Over-broad" as a warning.** A value reaching fifteen checks now means
  somebody wrote a trigger, which is a thing you can only do on purpose. The
  detail view says so plainly instead of raising an alarm.

What replaced them is one predicate worth alarming about: **inert**. Under
substring matching almost any string hit something, so "matches nothing" was
rare; under exact naming it is the normal shape of a typo.

## Non-goals

- **Not a replacement for `--no-verify`.** A one-off bypass is git's job.
  `hook.skip` is for a persistent decision, and the UI should not blur them.
- **Not a scheduler.** No expiry dates, no "skip until Friday". A skip that
  expires silently is a different surprise, not a smaller one.
- **Not fleet-wide bulk writes.** See *Scope* — for a fleet-wide skip the correct
  mechanism is one global config entry, not 96 local ones.

## Rules

**1. The UI writes an id, never a short name.**
`pre-commit-clippy`, not `clippy`. Both work, but the short name is a bet on
every future check name: a `clippy` check added to `pre-push` later would widen
an existing skip without anyone touching it. The id cannot widen.

**2. Lead with what protection is lost, not with a count.**
"Suppresses 19 of 20" is an aggregate, and aggregates do not move people the way
a named consequence does. It also treats every check as interchangeable, which
they are not: `pre-commit-yamllint` reformats, `pre-push-branch-protect` is what
stops a push to `main`. Name them, most consequential first; the count is a
subtitle.

**3. A skip that reaches nothing is reported as a finding.**
The UI cannot stop someone writing `hook.skip = clipy` by hand, so it surfaces
it. A value matching no check appears in the repo detail view saying exactly
that, and a value naming a trigger is shown with its expansion — informative, not
alarming:

```
 hook.skip
   pre-commit-clippy    local   -> pre-commit-clippy
   pre-commit           local   -> the whole pre-commit trigger — 15 checks
   clipy                global  -> ! matches no check
```

**4. Removal is exact, and the exit code is not evidence.**

`git config --unset hook.skip <value>` takes a value-pattern that is a **regex**,
not a literal. Measured:

- `--unset hook.skip 'pre-commit-lint.js'` removes `pre-commit-lint-js` — the
  `.` is a wildcard. Any value the UI does not escape can over-match.
- When the pattern matches **more than one** value, git prints
  `warning: hook.skip has multiple values`, **removes nothing, and exits 5**. An
  earlier revision of this document said it exits 0 and called that a silent
  no-op; that was a mis-measurement — `$?` was read after a pipe, so it reported
  the last command in the pipeline rather than git. git does signal the refusal.
- `--unset-all` with the same pattern removes **both**, which is the opposite
  surprise, and duplicates are legal so an anchored pattern can match twice.

So the UI passes an anchored, escaped pattern (`^pre-commit-clippy$`) and then
**re-reads the config to confirm**. Not because the exit code lies, but because a
status code reports what the command believes it did while a re-read reports what
is true. Config is small and the read is cheap; there is no reason to prefer the
weaker evidence.

**5. Prefer undo to confirmation.**
A toggle writes immediately and offers `u` to take it back. An undo helps when
the user was wrong, where a confirmation only interrupts when they were right
(Nielsen's third heuristic; the Undo Send precedent).

The draft required typing the check name whenever a skip reached more than one.
That gate is gone with the substring rule that made it necessary — see *History*.

The UI never refuses. Showing the consequence is the intervention; refusing
invites working around the tool by hand, which is worse and unobservable.

**6. Idempotent.**
`--add` on a value already present creates a duplicate that must then be unset
twice. The UI checks first and reports "already skipped" rather than writing.

## Scope: local, or global

Two scopes, and the distinction matters more than it looks.

- **Local** (`git config --add`, writes `.git/config`) — this repo only. The
  default, because it is the reversible one and it is where a repo-specific
  decision belongs.
- **Global** (`git config --global --add`) — every repo on the machine, including
  ones cloned later.

**What ships: the UI writes LOCAL, only.** `skips::plan` builds
`git config --add` / `--unset` with no `--global`, so every toggle lands in that
repository's `.git/config`. And it goes further than not offering global — it
REFUSES to touch an entry that is not local. Toggling a check whose skip came
from `~/.gitconfig` produces

```
that entry is global, not local — edit it where it lives
```

rather than a write. That refusal is the right default: a keystroke in a
per-repository detail pane should not silently change every repository on the
machine, and an `--unset` aimed at the wrong file is exactly the kind of write
this tool has already been burned by.

**The argument for global is still a good one, and it is a stated gap.** A
fleet-wide skip *should* be one global entry, not a loop over 96 local writes:
one entry is one thing to find and one thing to undo, while 96 entries are a
migration in their own right, and the tool has already learned what a
half-applied sweep costs. Nothing in the UI offers that today. Whoever builds it
should treat "affects every repository, present and future" as text the user
must read, not a mode they can fall into — which is why it was not bolted onto
the existing local toggle.

`configured_skips` reads via plain `git config --get-all`, so global and local
entries are already merged with no way to tell them apart at dispatch time. The
detail view shows which scope each value came from
(`git config --show-origin --get-all hook.skip`), or a developer deletes a repo's
`.git/config` line and is baffled that the check is still skipped. This part is
real: `SkipEntry` carries a `scope` field of `Local | Global | Other { origin }`,
resolved by `skips::scope_of` from `--show-origin`, and `Other` exists precisely
so a system config, an include or a worktree config is not silently relabelled
as one of the two the UI can reason about.

## Where it lives in the UI

**Repo detail (`Enter`, then `s`)** — the check list is selectable, and `s`
toggles the highlighted check for that repo. This is the common case: "not in
this repo, not right now". `u` takes it back.

**Hook view (`h`)** — the transposed matrix, one row per check, with `TRIGGER` as
its own column. Answers "where does this check actually apply?", which the CHECK
column alone could not once two checks could share a short name.

## Data model

```
SkipEntry {
  value      : String                  // as written in config
  scope      : Local | Global | Other { origin }
  suppresses : Vec<&'static str>       // resolved against the check registry
}

SkipPlan {
  check      : &'static str            // the check being toggled
  action     : Add | Remove
  command    : Vec<String>             // exactly the argv that will run
  suppresses : Vec<&'static str>       // what changes as a result
  refuse     : Option<String>          // why not, in words the user reads
}
```

`SkipPlan` carries no `repo` or `scope`: the repository is the argument to
`plan()` rather than a field of its result, and there is no scope to choose
because every plan is local — see *Scope* above. `refuse` is a plain `String`
rather than a `Refusal` enum because every refusal is shown to a human and none
is branched on: "already skipped", "already covered by a broader entry", "that
entry is global, not local".

`suppresses` is resolved through `amont_runtime::names_check`, the same
function the dispatcher uses. Reimplementing the match differently is how a UI
comes to claim a check is active while the dispatcher skips it.

## Tests

The important ones are about honesty, not mechanics:

- A value naming no check is reported as reaching nothing — not silently dropped.
- A trigger value is expanded to the checks it covers, by name.
- The written value is always an id — falsified by writing a short name and
  asserting the test fails.
- Adding an existing skip is refused, not duplicated.
- Removal uses an anchored pattern and is verified by re-reading config.
- A global skip is labelled as global in the detail view, with its origin.
- The resolver agrees with `dispatch::selected` for the same inputs.

## How we would know it worked

- **Zero inert skips.** Measurable from `greengate-fleet --json` at any time. A
  skip that names nothing is a developer who believes a check is off when it is
  not.
- **Time-to-discover a skip.** The announcement drives this to one commit; before
  it, the honest answer was "possibly never".

## Rendering

Inherits the dashboard's constraints and must not quietly drop them: the preview
is legible under `NO_COLOR`, degrades below 100 and 60 columns rather than
scrolling sideways, and encodes nothing by colour alone.

## Open questions

1. ~~**Should the UI ever refuse a write?**~~ RESOLVED: no. Show the consequence.
   Refusing protects against a mis-keystroke but invites working around the tool
   by hand, which is both worse and unobservable.
2. ~~**Should `skips` in the fleet table show a count or the values?**~~
   RESOLVED: a count in the table, the values with their reach in the detail
   view. A count alone hid an over-broad entry back when one could happen by
   accident; the detail view now carries the expansion.
3. **Is `--show-origin` fast enough per repo at fleet scale?** It is one extra
   `git config` invocation across ~96 repos. Measure before adding it to the scan
   rather than the detail view.
