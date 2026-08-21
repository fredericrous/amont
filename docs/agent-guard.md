# The agent guard

`amont` gates `git commit` and `git push`. There is a class of mistake it
cannot see, because the mistake is in the command string and never reaches a
git hook at all:

```sh
git push origin main 2>&1 | tail -5
```

A pipeline's exit status is the status of its **last** command. `tail` succeeds
at tailing an error message, so a rejected push, a push killed by a timeout, and
a push that never left the machine all report success — and the trimming
discards the error text too, so the failure is silent in both channels.

`amont-agent` is a Claude Code hook that reads a command before it runs and can
refuse it. It is a separate binary, installed separately, and it is not on the
commit path.

## Why this is a separate binary

The same reasoning that keeps `amont-fleet` separate. `amont` runs on every
commit in every repository with the developer's credentials, so it links no
external crates. `amont-agent` runs when Claude Code is driving a shell, is
opt-in, and buys features with dependencies. The dependency guard needs no
exception for it: `scripts/check-no-deps.sh` walks `cargo tree -p amont`, and
nothing reachable from that binary reaches this one.

## Installing

```sh
amont-agent install          # prints the settings block, writes nothing
amont-agent install --write  # merges it into ~/.claude/settings.json
amont-agent doctor           # is it installed, runnable, and actually firing?
```

`install` refuses to guess. It will not patch a `settings.json` it could not
parse, and it will not write one whose formatting it cannot reproduce — a diff
full of reformatting hides the one line it added — so it prints the block and
changes nothing unless `--reformat` says otherwise. `uninstall` removes exactly
what it wrote and leaves everything else byte-identical.

Two entries are written: the guard on `PreToolUse`, and a `SessionStart` entry
whose only job is to leave a heartbeat. Without the second, `doctor` cannot tell
"nothing fired this week" from "the guard has been dead since Tuesday".

## Stances

A rule does one of three things, and the middle one is the point:

| stance | effect |
|---|---|
| `observe` | records the firing and says nothing at all |
| `advise` | puts the reason into the model's context; refuses nothing |
| `deny` | refuses the tool call, with the reason and the remedy |

`observe` and `advise` are not two ways of saying "not blocking yet".
`additionalContext` enters the model's context and therefore changes its
behaviour, which contaminates the rate the observation exists to measure. A rule
that talks is intervening.

Change one at any time; it takes effect on the next command:

```sh
git config --global amont.agent.pipe-to-tail.stance observe
AMONT_AGENT_OFF=1        # or switch the whole guard off for one shell
```

## The rules

| rule | ships as | what it catches |
|---|---|---|
| `pipe-to-tail` | `deny` | a mutating command whose status is swallowed by a pipe |
| `bare-stash-pop` | `observe` | `git stash pop` with no ref, where `refs/stash` is shared across worktrees |
| `gh-pr-merge-auto` | `observe` | `--auto` on a repository with no required checks, which merges immediately |
| `no-verify` | `observe` | turning the whole commit gate off rather than one check |
| `git-add-broad` | `observe` | staging the tree instead of the change |

Only `pipe-to-tail` blocks, and only because seven consecutive weeks of
measurement showed no downward trend while every other habit halved.

## Evidence

Nothing here is asserted. `backtest` replays your own transcripts through the
rules and reports firings per 1,000 tool calls per week, so a rule's cost is a
number rather than an impression:

```sh
amont-agent backtest --since 2026-07-06
amont-agent check 'git push | tail -1'    # what would happen to this command?
```

Precision is kept as reviewed judgements rather than as a metric, because a
metric charts a regression and a test prevents one:

```sh
amont-agent explain pipe-to-tail --format cases >> tests/corpus/pipe-to-tail.cases
$EDITOR tests/corpus/pipe-to-tail.cases    # each `?` becomes match or nomatch
amont-agent corpus check                   # runs in the test suite
```

Promotion is gated on that corpus — including expected-negatives, since a
corpus of positives alone measures recall — and demotion is not gated at all:

```sh
amont-agent graduate bare-stash-pop --to advise
amont-agent demote bare-stash-pop
```

A guard that is hard to back out of is one people uninstall instead of demoting,
and uninstalling takes every rule with it.

## What it will not do

**It never emits `allow`.** That would short-circuit your own permission prompt,
so a guard approving everything it has no objection to would have switched off
the permission system it was installed beside. Silence is how it says "no
objection".

**Every failure path is silence.** An unreadable payload, an unknown event, a
command it cannot parse, a rule that panics, a journal it cannot write — all of
them exit 0 having written nothing. A hook that fails toward refusing gets in
the way of work you knew was correct, and the fix people reach for at that
moment is to delete it from `settings.json`, which switches off every rule at
once. One that fails toward silence loses a single firing.

**It does not judge what it cannot read.** Heredocs without terminators, `eval`,
`sh -c`, unbalanced quotes — all opaque, and opaque never fires.

## Knowing it is alive

Claude Code hooks fail open quietly: a command that cannot be resolved exits
127, which is a *non-blocking* status, and nothing tells you. `doctor` exits
non-zero when the guard is inert, so it can run from cron:

```
✓ installed in /Users/you/.claude/settings.json
✓ amont-agent 1.15.0 at /Users/you/.local/bin/amont-agent
✓ a refused command produces a valid decision document
✓ last ran 4m ago
✓ acting on pipe-to-tail
```

## The journal

Every firing is recorded at `~/.claude/amont-agent/journal.log`, redacted, and
never transmitted anywhere — the project's no-telemetry promise applies in full.
Like `.git/amont-bypasses`, it only counts: nothing in it may participate in a
decision.
