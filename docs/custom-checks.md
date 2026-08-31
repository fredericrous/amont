# Custom checks — `amont.conf`

A repository can declare checks of its own. Put a `amont.conf` at the
repository root, **commit it**, and the hooks run it alongside the built-ins.

```
# stage       name        scope         severity  command
pre-commit    lint-shell  *.sh          block     scripts/lint-shell.sh
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
`pre-commit  lint-shell  …` declares `pre-commit-lint-shell`.

That id is what `amont list` and the dashboard show, and either the id or the
short name addresses it in `hook.skip` and in a severity override — the same
three-way vocabulary the built-ins take:

```sh
git config hook.skip pre-commit-lint-shell   # that check
git config hook.skip lint-shell              # that check, on either stage
git config hook.skip pre-commit              # every pre-commit check, declared ones included
```

The **same name on both stages is two checks**, and that is allowed:
`show-unicorn` on `pre-commit` and on `pre-push` gives you
`pre-commit-show-unicorn` and `pre-push-show-unicorn`, each separately skippable
and separately downgradable. See _What a repository cannot do_ for the limits.

**scope** — `*` for every change, or a comma-separated list mixing `*.<ext>`
extensions and bare **filenames**: `*.ts,package.json,.prettierrc`. A bare
token matches the path's basename exactly, anywhere in the tree — an
extension list cannot say `package.json` without also matching
`not-package.json`, which is why this is its own kind of token. Directories
are not expressible. Evaluated against the files staged for a commit, or
against the range being pushed. This gate is real: a `*.sh` check does not
run on a commit that touches no shell.

A `+` adds the second half — what the **repository** must carry:

```
pre-commit  rubocop  *.rb+.rubocop.yml  block  rubocop
```

_"a staged `.rb`, **and** this repository has a `.rubocop.yml`."_ Both sides are
comma-separated and each is an OR — any trigger, any one of the opt-ins —
while the two halves are an AND. With nothing on the left, `+Gemfile` reads
"any change, in a repository that carries a Gemfile".

The separator is the one `amont list` already prints between the halves:

```text
○ rubocop (declared)   inert here — needs .rb + .rubocop.yml
```

What you read in the listing is what you write in the file.

**Why it exists.** Every builtin has this: `clippy` stays inert without a
`Cargo.toml`, `yamllint` without a `.yamllint.yaml`. Declarations did not,
because the manifest _was_ the opt-in — you typed the line, so you wanted the
check. [`amont add`](#shipping-a-check--packs) ended that: a vendored line is
in your file because you took a whole pack, and without a condition a packaged
`rubocop` fires on every `.rb` in a repository that never wanted it and just
errors.

A `+` that names no file (`*.rb+`) is refused rather than read as "no
condition" — that is the opposite of what it was reaching for. A filename
containing `+` is not expressible, the same class of limit as directories.

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
pre-commit  lint-shell  *.sh  block  files scripts/lint-shell.sh --strict
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
⚠ amont.conf: lint-shell could not run scripts/lint-shel.sh — No such file
⚠ 1 check(s) could not run: lint-shell
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
`commit.bodyWrap`, `autoRebase`, `timeout`, `testPushedTree`, and
`minVersion` — the last being the team's version floor: a binary older
than `set minVersion 1.11.0` says so once per stage, warn-only, instead
of silently lacking the checks the team added since. Any other
key is refused with its line number — most deliberately `amont.fix`,
because a committed file must not change what already-trusted commands
may DO to your working tree (see below). One caveat worth reading before
committing it: `set timeout` accepts the full configured range, including
`86400` and `0` (which disables the per-check deadline entirely) — a
review of that line is a review of how long a hook may hang everyone.

## What a repository cannot do

**Take a built-in's id.** `pre-push  branch-protect  …` is refused: it would
either shadow `pre-push-branch-protect` or silently lose to it, and a text file
should not be able to do either. The same name on the _other_ stage is fine —
`pre-commit  branch-protect  …` is a different check and shadows nothing.

**Write the stage into the name.** `pre-commit  pre-commit-clippy  …` is refused.
It would declare a check whose short name is another check's full id, so a single
`hook.skip pre-commit-clippy` would silence both and no rule could pick between
them. So is a name that simply _is_ a stage. The stage column already says which
one this is.

**Declare the same id twice.** The second is refused: it could not be addressed
by `hook.skip` or by a severity override, so it would run anonymously. Two lines
with the same name on _different_ stages are two ids, and both run.

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
⚠ amont.conf: lint-shell — declared in an untrusted amont.conf …
⚠ 1 check(s) could not run: lint-shell
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
git config hook.skip lint-shell                 # do not run it
git config amont.severity.lint-shell warn    # run it, do not let it block
```

Both surfaces read the same three names, and **nothing matches by substring**:
the full id (`pre-commit-lint-shell`), the short name (`lint-shell`, on either
trigger), and the trigger (`pre-commit`, meaning all of them). Three exact
comparisons, in `runtime::names_check` — `hook.skip = e` reaches nothing at all,
and skipping `lint-js` leaves `lint-json-yaml` alone.

**Prefer the severity downgrade anyway.** The two do different things:

|                             | runs | reports                       | blocks |
| --------------------------- | ---- | ----------------------------- | ------ |
| `amont.severity.<key> warn` | yes  | yes                           | no     |
| `hook.skip <key>`           | no   | no (only that it was skipped) | no     |

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
  ● lint-shell (declared)
  ✗ oops (declared)                 amont.conf line 3: severity "LOUD" …
pre-push
  ● pre-push-branch-protect
  ● smoke (declared)

  ● runs here   ○ inert   ⊘ skipped via hook.skip   ✗ declaration unusable
```

Across the fleet, `amont-fleet` has a `DECL` column — `2` for two declared
checks, `2!1` when one of them cannot run — and lists them per repository in the
detail pane.

## Shipping a check — packs

A check you wrote could always be run; until `amont add` it could not be
**shared**, except by pasting a line into somebody's manifest or upstreaming it
into amont itself.

A **pack** is any git repository with an `amont.pack` at its root, written in
exactly the syntax above:

```text
# amont-pack-java — Spotless for a JVM repository, Maven or Gradle
pre-commit  spotless-maven   *.java+pom.xml                        block  mvn -q spotless:check
pre-commit  spotless-gradle  *.java+build.gradle,build.gradle.kts  block  ./gradlew -q spotlessCheck
```

```console
$ amont add github:fredericrous/amont-pack-java@v1
github:fredericrous/amont-pack-java @ 2f5dbd9 declares:
    pre-commit  spotless-maven   *.java+pom.xml                        block  mvn -q spotless:check
    pre-commit  spotless-gradle  *.java+build.gradle,build.gradle.kts  block  ./gradlew -q spotlessCheck

amont.conf changed — these commands cannot run until you review them:
    amont trust
```

`<source>` is `github:owner/repo`, `forgejo:host/owner/repo`, or any git URL —
including a local path, which is all a test fixture needs. `--dry-run` shows
without writing.

That pack is real, and it is the worked example for everything below:
[fredericrous/amont-pack-java](https://github.com/fredericrous/amont-pack-java).
Its README is the long-form version of this section.

### What ships is text, not execution

This is the whole design, and it is what separates it from the ecosystem
model pre-commit built. pre-commit **clones a repository and executes it**,
building an isolated environment per hook. `amont add` copies rows into _your_
`amont.conf`, and stops:

- the rows land between `# amont:pack:start` / `# amont:pack:end` markers with
  the pack's **commit id**, so what you got is recorded next to what you run;
- the manifest's content changed, so its fingerprint no longer matches and
  **every** declared check — the pack's and your own — is inert until you
  [trust](trust.md) it;
- editing a vendored row by hand revokes consent exactly like editing any other
  line. A pack's rows are not privileged.

Adding a pack is therefore never the moment anything becomes runnable. It is a
way to avoid typing.

### Pinning, and what "verify" means here

`@v2` is a tag, and a tag moves. `amont add` resolves it with `git ls-remote`
**before** fetching, then refuses whatever arrives unless it is that commit —
so a ref that moves between the two steps is an error, not a surprise. The
commit id, never the tag, is what gets written into your manifest.

That is also why the transport is git and not HTTP. amont links no crates and
has no TLS stack; git is already a hard dependency, it is content-addressed, and
it brings SSH, private repositories and self-hosted forges with it for free.

### Making a packaged check well-behaved

A pack's rows land in somebody else's repository, so gate them on that
repository actually using the tool:

```text
pre-commit  rubocop        *.rb+.rubocop.yml  block  rubocop
pre-commit  terraform-fmt  *.tf               block  terraform fmt -check
```

The first is inert in a Ruby repository with no rubocop config; the second
needs no opt-in because a `.tf` file in the diff already says everything.
Without that condition a pack is a promise to run somebody's linter on every
matching file whether or not they configured it — which is how a useful pack
becomes an uninstalled one.

### What a pack may not carry

Checks, and nothing else. `tool` pins, `severity`, `skip` and `set` lines are
refused, and the whole pack with them.

Those lines are policy about the repository _installing_ the pack — see [What a
repository cannot do](#what-a-repository-cannot-do). A `skip` could silence your
secrets scan; a `set` could raise your large-file ceiling. A third party
proposing commands you will read is one thing; a third party quietly changing
what your existing checks do is another, and the second is not on offer.

A pack is refused **whole**: one bad row and nothing is written, because a
half-applied pack is a manifest neither side asked for.

### Publishing one

A pack repository is the `amont.pack` and nothing else — no schema to satisfy,
no registry to join, no build step. The example above is three files, and two
of them are the README and the licence:

```text
amont-pack-java/
├── amont.pack
├── README.md
└── LICENSE
```

Test it before it goes anywhere. A local path is a source, so the whole loop
is:

```sh
amont add ../my-pack --dry-run
```

Then tag it, and let people pin the tag:

```sh
git tag v1.0.0 && git push origin v1.0.0
git tag -f v1 && git push -f origin v1     # moving major alias, optional
```

A moving alias is safe here in a way it is not for a CI action, because what
lands in a consumer's manifest is the **commit id** and never the tag: `@v1`
resolves once, at install time, and then stops moving. The cost of that is
that a consumer does not get your fixes by pulling — they get them by running
`amont add` again, which is also a fresh `amont trust`. That is the trade the
whole design makes, and it is the right way round: nothing you publish later
can start running on somebody's machine without them reading it first.

### Updating and removing

`amont add` the same source again and its block is **replaced** — not appended,
which would declare the same id twice and break the manifest. To remove a pack,
delete its block; it is plain text, and the markers say where it ends.

## Why this format and not TOML

TOML would be nicer to write and costs a dependency tree that would then run on
every commit in ninety-six repositories. For four fields and a command, twenty
lines of `std` parsing wins. See `scripts/check-no-deps.sh` for the reasoning
behind that default; it is a judgement about the commit path's supply chain, not
a prohibition, and a genuinely rich format would be worth reopening it for.
