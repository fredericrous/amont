# Configuration

Everything is `git config`. There is no config file of ours, no `.amontrc`,
nothing to keep in sync — the settings live where a git user already looks for
settings, and `--local`/`--global` mean what they always mean.

To bypass or disable rather than tune, see [opting out](opting-out.md).

## Naming a check

Every check has an id, `<trigger>-<name>` — `pre-commit-clippy`. **Three things
name it**, and every config surface reads all three the same way:

| key | reaches |
|---|---|
| `pre-commit-clippy` | that one check |
| `clippy` | that check, on either trigger |
| `pre-commit` | every check on that trigger |

Where several keys reach one check, **the most specific wins**: full id, then
short name, then trigger. So you can downgrade a whole trigger and then exempt
one check from that downgrade.

**Nothing matches by substring.** `hook.skip e` reaches nothing at all, and
skipping `lint-js` leaves `lint-json-yaml` alone — a skip can never silently
couple two checks whose names happen to share a prefix.

## `hook.skip` — do not run it

Multi-valued; add as many as you need.

```sh
git config --add hook.skip pre-commit-clippy   # that one check
git config --add hook.skip clippy              # on either trigger
git config --add hook.skip pre-commit          # the whole trigger
git config --unset-all hook.skip               # start over
git config --get-all hook.skip                 # what is set here
```

A skipped check is **announced on every commit**. A config line nobody
remembers writing cannot go on silently disabling things — that silence is how
a repository ends up with a check everyone believes is running.

For one commit only, without touching config:

```sh
git -c hook.skip=clippy commit -m "fix: …"
```

## `amont.severity.<key>` — run it, but do not block

Takes the same three spellings, and keeps the signal: the check still runs and
still reports, it just stops failing the commit.

```sh
git config amont.severity.clippy warn       # runs, reports, does not block
git config amont.severity.pre-commit warn   # the whole trigger
git config amont.severity.clippy block      # back to blocking
```

`warn` is usually the right first move when adopting a check into an existing
repository: you get the report immediately and pay down the backlog on your own
schedule, rather than choosing between a blocked commit and a `hook.skip` you
will forget to remove.

## `amont.commit.*` — what a commit message must look like

Four keys, and `amont setup` walks you through all of them:

| key | default | means |
|---|---|---|
| `amont.commit.gitmoji` | `none` | where the type's emoji goes |
| `amont.commit.subjectMax` | `72` | longest the whole subject may be |
| `amont.commit.descriptionMax` | `50` | longest the part after `type: ` may be |
| `amont.commit.bodyWrap` | `72` | column the body is hard-wrapped at; `0` never wraps |

These matter more than they look, because `commit-msg` is the one hook
`hook.skip` and `amont.severity` do **not** reach, and git exempts it from
`--no-verify`. Without these keys the only answers to "I do not want a gitmoji
in every subject" were to comply or to uninstall.

### The four placements

```sh
git config amont.commit.gitmoji prefix
```

| | stored as | |
|---|---|---|
| `none` | `feat: add a cart` | the default — your subject, untouched |
| `prefix` | `✨  feat: add a cart` | |
| `suffix` | `feat: add a cart ✨` | commitlint and changelog tools still see the type |
| `replace` | `✨  add a cart` | the emoji stands in for the type word |

You always *write* `feat: add a cart`, and it is always validated as that —
the placement only decides what gets stored. `replace` costs you interop, and
that is the trade it is: an emoji is not a type any conventional-commit tool
knows how to read.

Two things hold whatever you choose. The limits measure **what you wrote**, so
the emoji never eats your description budget. And running the hook again over
its own output — an amend, a rebase reword — changes nothing.

### The limits

```sh
git config amont.commit.descriptionMax 68
git config amont.commit.bodyWrap 0
```

`68` is the useful number if 50 feels tight: it still fits a 72-column subject
with a short type and no scope. `bodyWrap 0` leaves the body exactly as
written, which is what keeps a pasted stack trace or a fenced code block
intact.

A value git cannot parse, or one outside `1..=1000`, takes the shipped default
**and says so on the commit it happened on** — because a limit you believe you
raised and did not is the whole failure mode this project refuses to be quiet
about. A pairing that cannot do anything (a description budget the subject
limit can never accommodate) is reported by `amont list`, not by the hook:
the commit path says what is in effect, and the config-reading commands say
what makes no sense.

## `amont.autoRebase` — whether pre-push may sync a behind branch for you

```sh
git config amont.autoRebase false   # default true
```

On (the default, and the behaviour every install so far has had):
`pre-push-pull-rebase` rebases a clean, behind, non-diverged branch onto its
own upstream — then **stops the push and asks for a second one**, because the
refs git handed the hook predate the rebase; the suite would otherwise judge
commits git is no longer pushing, and the server refuses the stale objects
regardless.

Off: the check becomes a pure advisor. It performs no network I/O at all (no
`ls-remote`, no advisory `fetch` — behind is judged from your last fetch) and
never runs a rebase you did not type; a behind branch stops the push with the
command to run. A hook that rewrites your branch is a bigger claim than most
teams want a "check" to make — this is the key that unmakes it.

## `amont.timeout` — the budget one check's command may run for

```sh
git config amont.timeout 60      # seconds; default 600, 0 disables
```

Every command a check spawns — a linter, a formatter, a declared
`amont.conf` line, the push gate's test suite — is killed at this deadline
and the check **fails**, loudly, naming this key. Without it, one hung tool
blocked the commit forever while your unstaged changes sat parked out of the
tree — and the learned response to that is `--no-verify`, permanently. The
default is ten minutes, the same budget the generated agent guidance already
tells tooling to allow a whole commit or push.

The kill reaches the command itself; a grandchild it detached may survive,
orphaned, but the commit is no longer hostage to it.
## `amont.knownIdentity` — identities usual-name has vouched for

Written by the tool, not by you: when `pre-commit-usual-name` finds your
`user.name <user.email>` in history once, it records the identity here
(local config, never committed) and never walks the full history for it
again — `git shortlog --all` on every commit is milliseconds today and a
scale cliff on a long history. Multi-valued; `amont uninstall` removes it
with the rest of amont's bookkeeping. Delete a value to make the check walk
again.

## `amont.fix` — let a check repair what it finds

```sh
git config amont.fix true
```

Off unless you ask. A hook that edits your files without being asked is a
larger surprise than one that complains. See
[custom checks](custom-checks.md#letting-a-check-fix-what-it-finds).

## `amont.testPushedTree` — test what you are pushing

```sh
git config amont.testPushedTree true
```

By default `pre-push` runs your suite against the **working tree**, and says
so. That is fast and usually what you want, but it is not what you are pushing:
an uncommitted fix makes a broken commit look green.

With this set, the suite runs in a throwaway checkout of the commits being
pushed, and your tree is not touched. It costs a second checkout and a build
that cannot reuse your `target/` cache, which is why it is opt-in rather than
the default.

## `amont.trusted`

Set by `amont trust`, read by everything that decides whether a declared
external may run. `--local` only, never committed. Do not set it by hand — see
[the trust model](trust.md).

## `commit.template`

Not ours, but worth setting: it puts the footer scaffold in front of you when
you write a commit.

```sh
git config --global commit.template ~/.config/git/git-templates/message
```

## Environment variables

| variable | effect |
|---|---|
| `GIT_HOOKS_BIN` | Absolute path to the binary a shim should use. First candidate in the shim's resolution order. |
| `AMONT_BIN_DIR` | Where `amont install` and the installer script put binaries. Default `~/.local/bin`. |
| `AMONT_VERSION` | Pins the version the installer script fetches. |
| `NO_COLOR` | Honoured, as is a non-tty stdout. |

## Repository-declared checks

A repository can add checks of its own without anybody forking anything, in a
committed `amont.conf`. They obey every control on this page, addressed the
same three ways, and they are inert until trusted.

Full reference: [custom checks](custom-checks.md) ·
[the trust model](trust.md).

## Seeing the result

```sh
amont list              # what would run here, and why not
amont list --json       # the same, machine-readable
amont setup             # walk the commit-style keys, with the current values
amont-fleet             # the same, across every repository
```

`amont list` ends with the commit style in effect, and names the key and the
scope of anything you set:

```
commit style
  gitmoji            suffix     amont.commit.gitmoji (global)
  subject max        72
  description max    68         amont.commit.descriptionMax (global)
  body wrap          off        amont.commit.bodyWrap (local)

  `amont setup` to change any of these
```

`amont list` reports the **effective** severity, after overrides — so a
check you downgraded three months ago is visible as downgraded rather than
having to be inferred from config. Across a fleet, `amont-fleet` shows
skips and severities per repository, with `TRIGGER` as its own column.
