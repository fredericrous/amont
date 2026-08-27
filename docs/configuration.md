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

**Where committed policy sits.** A trusted `amont.conf` can carry
[`severity`, `skip`, and `set` lines](custom-checks.md#repo-policy--severity-skip-and-set-lines).
For one key, the ladder is: built-in default < system < global < **policy** <
local < worktree < command — the team's committed decision beats your global
preferences, and your local config in that repository beats the team. Between
different keys naming the same check, specificity (full id > short name >
trigger) decides regardless of source. Skips union across every source. On a
git too old for `--show-scope` (< 2.26) this degrades fail-safe to "all git
config beats policy". A `set` line reaches an allowlist of the keys on this
page — the thresholds, commit style, `autoRebase`, `timeout`,
`testPushedTree`, `minVersion` — and never `fix`, `trusted`,
`conventions`, or the observability opt-outs, which stay per-machine.

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

## `amont.conventions`

```console
$ git config --global amont.conventions declared
```

`everywhere` (the default) or `declared`. In `declared` mode the house
rules — commit-message shape, branch patterns, lint/format gates, test
suites, audits, auto-rebase — run only in repositories that commit an
`amont.conf` (an empty one declares), while the safety net (merge-conflict,
secrets, large-files, ban-terms) keeps running everywhere. This is what
makes a machine-wide standing grant (`amont enroll`, `init.templateDir`)
safe on a machine that also clones other people's projects. A held-back
stage announces itself in one line; `amont list` reports the state, and
`--json` carries it as `"conventions_apply"`. An unrecognised value falls
back to `everywhere`, loudly.

## `amont.largeFileWarn` / `amont.largeFileBlock`

```console
$ git config amont.largeFileWarn 25
$ git config amont.largeFileBlock 500
```

The two thresholds of [`pre-commit-large-files`](checks.md#pre-commit), in
megabytes: a staged file over the first is named (default 10), one over the
second blocks (default 100 — GitHub's own refusal line).

## `amont.recordBypasses` — whether a dodged gate is tallied

```sh
git config amont.recordBypasses false   # default true
```

When a commit that a commit-time gate declaration covered lands without that
gate having run — `git commit --no-verify`, a blocked attempt retried with
it, a gate whose tool was missing — `post-commit` silently appends one line
per dodged script to `$(git rev-parse --git-common-dir)/amont-bypasses`.
`amont list` shows the tally as "unverified commits". The file is local:
never a ref, never pushed, never sent anywhere.

`false` stops the counting from now on. The switch exists because "my tool
counts my bypasses" can reasonably read as surveillance, and the answer to
that reading should be a documented off-switch rather than an argument —
though the count is also the first place a slow or flaky check becomes
visible as the thing people route around.

## `amont.recordDowngrades` — whether a warning is tallied

```sh
git config amont.recordDowngrades false   # default true
```

The companion to the switch above, for the other half of the signal. When a
check FAILS but the severity that applies says `warn`, the hook silently
appends one line to `$(git rev-parse --git-common-dir)/amont-downgrades`, and
`amont list` shows the tally as "problems that did not block".

That file is what makes a **trial** readable: set
`amont.severity.pre-commit warn`, work for a fortnight, then read which checks
your team would actually have fought. See
[Trying it before you impose it](checks.md#trying-it-before-you-impose-it).

Local on the same terms as the bypass ledger — never a ref, never pushed,
never sent anywhere — and `false` stops the counting from now on, for the same
reason its sibling has an off-switch.

Nothing is recorded by a rehearsal (`amont run`), and nothing by a check that
actually blocked: the first is not a commit, and the second has nothing to
report.

## `amont.progress` — one check, one block

```sh
git config amont.progress false   # default true
```

On (the default): everything a pre-commit check says — its own lines and its
tools' captured output — is buffered and emitted as ONE contiguous block when
the check finishes, so twenty concurrent checks stop shuffling their failure
output together. Blocks arrive in completion order. And when stderr is a
real terminal, a live region under the blocks shows one line per running
check — spinner, name, elapsed — so a slow `cargo test` is a ticking clock
instead of a frozen prompt. The region only ever paints on an interactive
terminal; piped or redirected output, `TERM=dumb`, and CI logs never see a
control code.

Off: raw streaming, exactly as before — every line lands the moment it is
written, interleaved across whatever else is running. The honest cost of the
default is that a long-running tool's output arrives when the check ends
rather than as it happens; this key is the way back if you want to watch a
test suite scroll.

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

## `amont.idleTimeout` — how long a check may be silent

```sh
git config amont.idleTimeout 300  # seconds; default 120, 0 disables
```

A hung tool is silent — a captive portal, a deadlocked lock file, a prompt
nobody will answer. A slow tool talks: `cargo test` prints a line per test.
So the clock that decides "stuck" counts **silence**: a command that writes
nothing for this long is killed and the check **fails**, saying so and naming
this key. Two minutes catches a real hang faster than the old ten-minute wall
clock did, and lets a chatty twenty-five-minute suite finish.

Only commands whose output amont observes answer to this clock — every
check runs its tools that way by default. With `amont.progress false` the
tool inherits your terminal, nobody sees the bytes, and only the ceiling
below applies.

## `amont.timeout` — the ceiling one check's command may run for

```sh
git config amont.timeout 900     # seconds; default 3600, 0 disables
```

The backstop behind `idleTimeout`: a tool that keeps printing and never
finishes is killed here, and the message says whether it was still printing
(slow — raise this) or had gone quiet (stuck — look at the tool). The default
was ten minutes when this was the only clock and had to catch hangs; with
silence doing that job, the ceiling can afford an hour.

The kill reaches the command itself; a grandchild it detached may survive,
orphaned, but the commit is no longer hostage to it.

While a stage runs, a terminal shows a live line per check with its elapsed
time, a `· quiet 45s/2m` note once a check has been silent for half a minute,
and `· 50m/1h` once it is within 80% of the ceiling — the cliff, shown before
the fall. Piped (an agent, CI), the same information arrives as one plain
line a minute per running check: elapsed, time since its last output, and,
the first time, both budgets.

The same clock bounds the push path's own network verbs: `pull-rebase`'s
sync runs under the full budget, and the reachability *probes*
(`ls-remote` before a sync, the initial-push check) under the smaller of
this and 30 seconds — a probe answers in a second or two when the network
is there at all, and a captive portal must not get ten minutes to say
nothing. `0` disables these deadlines too.

## `amont.minVersion` — the amont this repository means

```sh
git config amont.minVersion 1.11.0
```

Rarely set by hand — the committable spelling is `set minVersion 1.11.0`
in a trusted [`amont.conf`](custom-checks.md#repo-policy--severity-skip-and-set-lines),
which is the point: a binary one release behind answers every hook name
and simply *lacks* a check, silently, and nothing in the repository could
say which amont the team meant. A binary older than the floor gets one
warning line per stage naming both versions. Warn-only, deliberately:
blocking commits for being out of date teaches `--no-verify`, and a
binary too old to know this key cannot honour it anyway.
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

## `amont.attest` / `amont.attestKey` — sign what pre-push proved, for CI

```sh
git config amont.attest true
```

Off by default. With it on, a push whose pre-push block gates all passed
leaves an `ssh-keygen`-signed note on each pushed tip in
`refs/notes/amont-attest` — binding the pushed **tree**, the names of the
gates that passed, and the amont version — and pushes that ref to the same
remote, so CI can verify the note and skip the test steps it names.
`amont.attestKey` points at the signing key; unset, it means
`~/.ssh/amont-attest`. The verifying side, the trust statement being made,
and why every failure falls back to CI running the tests are in
[the CI backstop](ci.md#skipping-what-pre-push-already-proved).

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
| `AMONT_ATTEST_PUSH` | Set by amont itself on the notes push [`amont.attest`](#amontattest--amontattestkey--sign-what-pre-push-proved-for-ci) makes, so the recursive pre-push stands down. Not for humans. |
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
