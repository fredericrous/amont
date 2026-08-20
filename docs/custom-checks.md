# Custom checks — `amont.conf`

A repository can declare checks of its own. Put a `amont.conf` at the
repository root, **commit it**, and the hooks run it alongside the built-ins.

```
# stage       name        scope         severity  command
pre-commit    shellcheck  *.sh          block     scripts/lint-shell.sh
pre-commit    protos      *.proto,*.pb  block     buf lint
pre-push      smoke       *             warn      make smoke
```

Five whitespace-separated fields, in file order. Alignment is cosmetic; tabs and
single spaces parse identically.

## Why the file is committed

`.git/hooks` is not committed, so a hook script dropped in there can never
actually be shared: every member of the team has to install it by hand, and
nothing tells them when it changes. A committed manifest is reviewed like any
other change, arrives with a `git pull`, and is visible in the fleet
dashboard.

## The fields

**stage** — `pre-commit` or `pre-push`.

**name** — the short name. Together with the stage it forms the check's **id**,
`<stage>-<name>`, exactly as a built-in has: a line reading
`pre-commit  shellcheck  …` declares `pre-commit-shellcheck`.

That id is what `amont list` and the dashboard show, and either the id or the
short name addresses it in `hook.skip` and in a severity override — the same
three-way vocabulary the built-ins take:

```sh
git config hook.skip pre-commit-shellcheck   # that check
git config hook.skip shellcheck              # that check, on either stage
git config hook.skip pre-commit              # every pre-commit check, declared ones included
```

The **same name on both stages is two checks**, and that is allowed:
`show-unicorn` on `pre-commit` and on `pre-push` gives you
`pre-commit-show-unicorn` and `pre-push-show-unicorn`, each separately skippable
and separately downgradable. See *What a repository cannot do* for the limits.

**scope** — `*` for every change, or a comma-separated list mixing `*.<ext>`
extensions and bare **filenames**: `*.ts,package.json,.prettierrc`. A bare
token matches the path's basename exactly, anywhere in the tree — an
extension list cannot say `package.json` without also matching
`not-package.json`, which is why this is its own kind of token. Directories
are not expressible. Evaluated against the files staged for a commit, or
against the range being pushed. This gate is real: a `*.sh` check does not
run on a commit that touches no shell.

**severity** — `block` fails the stage; `warn` runs the check, prints whatever it
prints, and lets the commit through. It is your choice, per check.

**command** — the rest of the line, split on whitespace and executed directly
from the repository root, through the same program resolution the built-ins
use — so `npx` and friends work on Windows, where a bare `Command::new` cannot
start a `.cmd`.

## Your command gets the file list

Two ways, both carrying exactly the paths the scope matched — the same set the
gate above judged, which a wrapper re-running `git diff --cached` can diverge
from (`amont run --all-files` overrides the set in-process):

- **`$AMONT_FILES`**, always: the matched paths, newline-separated, relative
  to the repository root. Empty when the set would not fit in an environment
  variable (very large change sets) — treat empty as "derive it yourself".
- **the `files` marker**, opt-in: prefix the command with `files ` and the
  matched paths are appended to the argv, the way a built-in hands its tool
  the staged list:

```
pre-commit  shellcheck  *.sh  block  files shellcheck --severity=warning
```

With `files`, a commit whose matched set is empty does not run the command at
all — most linters error on an empty argv, and a commit must not be blocked
over nothing.

## There is no shell

No pipes, no redirection, no globbing, no quoting. `make smoke` works; `find . |
xargs foo` does not — put that in a script and invoke the script.

Two reasons, and the second decided it. Windows has no `sh`, and every emulation
of one this project has tried has been a source of bugs. And a manifest line that
silently gained shell semantics would be a much larger thing to have introduced
than it looks.

## Exit codes

`0` passes. Anything else fails, and whether that stops you depends on the
severity column.

A command that **cannot be started at all** — a typo'd path, a tool nobody
installed — is neither. It is reported as a gap:

```
⚠ amont.conf: shellcheck could not run scripts/lint-shel.sh — No such file
⚠ 1 check(s) could not run: shellcheck
```

It does not block, because a command that never ran has not judged anything;
reporting it as a lint failure sends someone hunting for an error that does not
exist. But it is never silent, because a check that has quietly never executed is
the one failure this whole design is arranged against.

## A line nobody can parse is not skipped

The same rule applies to the manifest itself. A malformed line still produces a
check — one that reports on every commit and says which line and what was wrong:

```
⚠ amont.conf: oops — line 3: severity "LOUD" must be `block` or `warn`
```

Silently ignoring it would mean a check somebody committed months ago has never
run once and nothing ever said so.

## Repo policy — `severity`, `skip`, and `set` lines

The manifest can also carry the TEAM's decisions about the built-ins, so
"clippy is warn-only here" is a committed, reviewed line instead of sixty
people running the same `git config` incantation:

```
severity  clippy          warn     # runs, reports, does not block — here
severity  pre-push-pytest block    # and this one always blocks
skip      yamllint                 # never runs in this repository
```

Targets use the same three-way naming as `hook.skip` — full id, short name,
or a whole trigger — and a target that names no check here is reported once
per run, with its line number, rather than silently doing nothing.

**Trust-gated, like everything else this file says.** Untrusted policy is
inert and announced (`amont.conf policy not applied: …`) — a repository you
cloned to read cannot weaken your safety net until you consent, and the
trust prompt shows the policy lines you are consenting to.

**Precedence is a specificity ladder, per key**: built-in default < system
config < global config < **policy** < local config < worktree < command
(`git -c`). Your `git config --global` preferences yield to the team's
committed decision; your LOCAL config in that repository still beats it —
the developer owns their machine, and every documented escape hatch keeps
working. Between DIFFERENT keys naming the same check, key specificity
decides regardless of source: a policy `severity pre-commit-clippy …`
outranks a local `amont.severity.pre-commit`, and vice versa a local full id
outranks a policy trigger. Skips are a union of all sources — nothing can
un-skip, so there is no conflict to order.

(On a git too old for `--show-scope`, this degrades fail-safe: ALL git
config beats policy. See [configuration](configuration.md).)

### `set` — committed thresholds and commit style

The same file can carry the numeric and style knobs the checks consult:

```
set  largeFileWarn     1      # warn above 1 MB, in this repository
set  commit.subjectMax 50     # the whole team's subject budget
```

`set <key> <value>` takes the key exactly as you would write it after
`git config amont.` — matched case-insensitively, values parsed by git
itself, so `set largeFileBlock 2k` means what `git config` would mean by
it and a bad value complains the same way. The same ladder applies: a
policy value beats your global config, and your local config in that
repository still beats the policy.

Only these keys are settable: `largeFileWarn`, `largeFileBlock`,
`commit.gitmoji`, `commit.subjectMax`, `commit.descriptionMax`,
`commit.bodyWrap`, `autoRebase`, `timeout`, `testPushedTree`. Any other
key is refused with its line number — most deliberately `amont.fix`,
because a committed file must not change what already-trusted commands
may DO to your working tree (see below). One caveat worth reading before
committing it: `set timeout` accepts the full configured range, including
`86400` and `0` (which disables the per-check deadline entirely) — a
review of that line is a review of how long a hook may hang everyone.

## What a repository cannot do

**Take a built-in's id.** `pre-push  branch-protect  …` is refused: it would
either shadow `pre-push-branch-protect` or silently lose to it, and a text file
should not be able to do either. The same name on the *other* stage is fine —
`pre-commit  branch-protect  …` is a different check and shadows nothing.

**Write the stage into the name.** `pre-commit  pre-commit-clippy  …` is refused.
It would declare a check whose short name is another check's full id, so a single
`hook.skip pre-commit-clippy` would silence both and no rule could pick between
them. So is a name that simply *is* a stage. The stage column already says which
one this is.

**Declare the same id twice.** The second is refused: it could not be addressed
by `hook.skip` or by a severity override, so it would run anonymously. Two lines
with the same name on *different* stages are two ids, and both run.

**Grant its own trust, or reach the machine-level knobs.** Policy stops at
severities, skips, and the allowlisted `set` keys: `amont.fix` (rewriting
your working tree), the trust decision itself, `amont.conventions`, and
the observability opt-outs stay per-machine — a committed file must not
change what already-approved commands are permitted to DO, which is a
different consent than "I read these commands".

**Run before the built-ins.** Externals are appended to each stage, always. A
third-party command must not be able to delay `pre-push-branch-protect`, and
appending is the only arrangement in which it cannot. On `pre-push`, which stops
at the first blocking failure, that means a built-in failure means your check
does not get a turn.

## A manifest is inert until you trust it

A repository you cloned can declare checks, and running them is a decision you
make — not one `git clone` makes for you. So nothing in `amont.conf` runs
until somebody accepts it here:

```sh
amont trust            # shows what it declares, then records it
amont trust --show     # what it declares, and whether it is trusted
amont trust --revoke
```

`amont install` asks once, with the declarations in view. Declining still
installs the built-ins.

Until then the checks are **reported, not dropped** — the point is that you can
see there is a decision waiting:

```
⚠ amont.conf: shellcheck — declared in an untrusted amont.conf …
⚠ 1 check(s) could not run: shellcheck
```

Acceptance is recorded against the file's CONTENT (`git hash-object`, which you
can run yourself), so a `git pull` that adds a command does not inherit the
trust you gave the file before it. When that happens the message says so —
`changed since it was trusted` — because "somebody edited this" is a different
thing to be told than "you have not looked at this yet".

**This is a floor, not a ceiling.** A built-in check still runs your
repository's own toolchain: `prettier` and `eslint` are taken from
`node_modules/.bin` when present, so a hostile `node_modules` needs no manifest
at all. That is the same exposure `npm install` already carries.

## Pinning tool versions

The checks run whatever `prettier`, `ruff` or `shellcheck` this machine has —
and when two machines disagree, the hook "passes here, fails in CI", which
reads as flakiness and trains people toward `--no-verify`. A `tool` line
turns that skew into a printed fact:

```
# tool  <program>  <version-substring>
tool  ruff  0.6.
tool  shellcheck  0.10
```

Once per hook run (both stages), each pinned tool's `--version` first line is
checked for the substring; a mismatch or an unrunnable tool warns, naming
both sides. **Warn-only, always** — skew never blocks a commit, because the
fix is a human decision about which side to move.

A substring, not a semver range: `0.6.` pins a minor, `0.6.3` a patch, and
the point is agreement between machines, not range arithmetic. Pins are
trust-gated like every declaration — verifying one executes
`<program> --version` for a name the repository chose, which is exactly the
consent `amont trust` collects.

## Letting a check fix what it finds

Prefix the command with `fix ` and the check may rewrite files, with whatever it
changed re-staged:

```
pre-commit  format  *.js  block  fix npx prettier --write
```

Two conditions, both deliberate:

- **Off unless you ask.** `git config amont.fix true`, per repository — and
  the consequence is worth stating in bold: **with `amont.fix` off, a
  `fix`-declared check does not run at all**, not even in check-only mode.
  It reports "not run" (a warn, never a block) on every commit, because the
  one command you declared is a rewriting command and running it would edit
  files nobody asked to have edited. A team that commits a `fix` declaration
  gets zero enforcement from every member who has not personally opted in —
  if you need the check to always judge, declare a second, non-`fix` line
  with the tool's check mode.
  A hook that edits your files without being asked is a larger surprise than
  one that complains.
- **pre-commit only.** `fix` on a `pre-push` line is a parse error, reported on
  every commit like any other bad line. A pre-push hook must not modify the
  worktree or index: the pushed commit would then differ from the tree you are
  looking at.

Re-staging is safe because the pre-commit stage holds your unstaged changes
aside first, so the tree contains what you staged and nothing else — anything a
formatter touches is by definition part of this commit. Work you deliberately
kept back is never swept in.

The built-in `prettier` check does this too, under the same `amont.fix` gate.

## Turning one off

Exactly as for a built-in:

```sh
git config hook.skip shellcheck                 # do not run it
git config amont.severity.shellcheck warn    # run it, do not let it block
```

Both surfaces read the same three names, and **nothing matches by substring**:
the full id (`pre-commit-shellcheck`), the short name (`shellcheck`, on either
trigger), and the trigger (`pre-commit`, meaning all of them). Three exact
comparisons, in `runtime::names_check` — `hook.skip = e` reaches nothing at all,
and skipping `lint-js` leaves `lint-json-yaml` alone.

**Prefer the severity downgrade anyway.** The two do different things:

| | runs | reports | blocks |
|---|---|---|---|
| `amont.severity.<key> warn` | yes | yes | no |
| `hook.skip <key>` | no | no (only that it was skipped) | no |

A downgrade keeps the check working and keeps you looking at what it finds; you
have decided the finding should not stop a commit, not that you no longer want
to know. A skip removes the signal along with the block, so the problem it was
watching for grows silently until somebody turns it back on. Reach for `skip`
when a check is genuinely inapplicable to a repository, and for `severity` when
it applies but should not be a gate.

## Seeing what you declared

```sh
amont list
```

```
pre-commit
  ● pre-commit-merge-conflict
  ○ pre-commit-clippy               inert here — needs .rs + Cargo.toml
  ● shellcheck (declared)
  ✗ oops (declared)                 amont.conf line 3: severity "LOUD" …
pre-push
  ● pre-push-branch-protect
  ● smoke (declared)

  ● runs here   ○ inert   ⊘ skipped via hook.skip   ✗ declaration unusable
```

Across the fleet, `amont-fleet` has a `DECL` column — `2` for two declared
checks, `2!1` when one of them cannot run — and lists them per repository in the
detail pane.

## Why this format and not TOML

TOML would be nicer to write and costs a dependency tree that would then run on
every commit in ninety-six repositories. For four fields and a command, twenty
lines of `std` parsing wins. See `scripts/check-no-deps.sh` for the reasoning
behind that default; it is a judgement about the commit path's supply chain, not
a prohibition, and a genuinely rich format would be worth reopening it for.
