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
that leaves a heartbeat and states where the checkout stands against the
remote. Without the heartbeat, `doctor` cannot tell "nothing fired this week"
from "the guard has been dead since Tuesday".

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
| `stale-base` | `advise` | a branch or worktree started from a checkout the remote has moved past |

Only `pipe-to-tail` blocks, and only because seven consecutive weeks of
measurement showed no downward trend while every other habit halved.
`stale-base` advises from the start because it refuses nothing, speaks only
after measuring a real gap, and names a failure no correcting loop can see —
nothing fails when you build on stale code.

## The session-opening notice

There is a mistake no command-level rule can catch, because no command is
wrong: a session opens in a checkout last pulled on Tuesday, the model reads
the tree it is given, and builds a feature that landed on `origin/main` on
Wednesday. The work is correct against the code it can see.

So at `SessionStart` the guard does the one thing the model cannot do for
itself. It refreshes `origin/main` — one branch, no tags, killed at five
seconds, skipped when `FETCH_HEAD` is under ten minutes old so a burst of
sessions shares one round-trip — and if `HEAD` is behind, says so:

```
amont-agent/stale-base: this checkout of amont (branch main) is 8 commits
behind origin/main; newest there: d3b2ed5 chore(release): 1.16.0 (3 days
ago). Work that seems missing here may already exist on origin/main —
`git log HEAD..origin/main --oneline` lists it — and a branch or worktree
started from HEAD inherits the gap; one started from origin/main does not.
```

It never pulls. `git pull` rewrites the working tree under whoever is using
it, and a per-task worktree exists precisely so that nobody does that. Moving
`refs/remotes/origin/*` is safe in every worktree at once; moving `HEAD` is
not. If the fetch fails or times out, the notice is computed against the last
successful fetch and says so; when it is up to date, or not in a repository,
or there is no remote, it says nothing.

The `stale-base` rule is the same fact at the moment it is about to be
inherited: `git worktree add`, `git checkout -b` or `git switch -c` from
`HEAD` or a local branch, while that start point is behind. The remote form
(`… -b feat/x origin/main`) is the remedy and never fires. Both the notice and
the rule answer to one stance key, and the fetch has its own switch:

```sh
git config --global amont.agent.stale-base.stance observe   # measure, say nothing
git config --global amont.agent.fetch false                 # never touch the network
git config checkout.defaultRemote forgejo                   # measure against another remote
```

`checkout.defaultRemote` is git's own key for "which remote is the remote",
and a repository mid-migration — `origin` a mirror going stale, a second
remote carrying the truth — sets it once for both git and the guard. With two
remotes and no preference the guard says nothing rather than guess.

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
