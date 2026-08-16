# amont

**Catch the bad commit before it exists — and take the whole thing back out in one command.**

[![CI](https://github.com/fredericrous/amont/actions/workflows/ci.yaml/badge.svg)](https://github.com/fredericrous/amont/actions/workflows/ci.yaml)
[![Release](https://img.shields.io/github/v/release/fredericrous/amont?label=release)](https://github.com/fredericrous/amont/releases/latest)
[![License](https://img.shields.io/github/license/fredericrous/amont)](LICENSE)

A single Rust binary that checks `git commit` and `git push` — no YAML to
write, no runtime to install, nothing to configure before it is useful.

- **Useful in the first minute.** Twenty-four built-in checks — commit-message
  conventions, merge-conflict markers, the linters and formatters for the
  languages your repository actually uses, branch rules, your test suite —
  and each one fires only where the repository has opted into its tool.
- **A cloned repository cannot run code on your machine.** Checks a repository
  declares for itself are inert until you review them and say `amont trust`
  — a gate pre-commit, lefthook and husky do not have.
- **Nothing on the commit path but `std`.** The hook binary links no external
  crates, and CI fails any build that changes that.
- **Leaving is one command.** `amont uninstall` removes exactly the five
  shims install wrote; a hook you or another tool put there is named and left
  alone.

![amont catching a commit and letting the fixed one through](docs/assets/amont-demo.gif)

How that stacks up against pre-commit, lefthook and husky, feature by feature:
[the full comparison](docs/similar-projects.md).

## Install

**Linux and macOS**

```sh
curl -fsSL https://raw.githubusercontent.com/fredericrous/amont/main/install/install.sh | sh
```

**Windows** (PowerShell — the line above is POSIX `sh` and only reaches
Windows through Git Bash)

```powershell
irm https://raw.githubusercontent.com/fredericrous/amont/main/install/install.ps1 | iex
```

Either one downloads a release binary, verifies it against the published
`SHA256SUMS`, and puts it where the hooks already look — `~/.local/bin`, or
`%USERPROFILE%\.local\bin`. Both **enable nothing**: hooks are turned on per
repository, by you, afterwards.

```sh
cd <your-repo> && amont install   # this repository only
amont list                        # what would run here, and why not
```

Prefer not to pipe a script into a shell? Download a binary and its checksum
from [Releases](https://github.com/fredericrous/amont/releases/latest) —
prebuilt for Linux (gnu/musl, x86_64 and aarch64), macOS (Intel and Apple
silicon) and Windows. Or build from source: `cargo build --release`.

From [crates.io](https://crates.io/crates/amont):

```sh
cargo install amont
```

Or with Homebrew:

```sh
brew tap fredericrous/tap
brew trust fredericrous/tap    # Homebrew asks this of every third-party tap
brew install amont
```

**In a JavaScript project**, the binary can travel with the repository rather
than with the machine, so a teammate who clones it needs no install step at all:

```sh
npm i -D amont                            # or: pnpm add -D amont
npm pkg set scripts.prepare="amont init"
npm install                               # the hooks appear
```

Six prebuilt platform packages are declared as `optionalDependencies`, so npm
installs exactly one and runs no install scripts at all — this survives
`npm ci --ignore-scripts`. `amont init` wires up that one repository and
nothing else: no `~/.local/bin`, no template directory, no prompts.

If the project also installs **without** its dev dependencies anywhere — `npm ci
--omit=dev`, the usual second stage of a Dockerfile — write
`"prepare": "amont init || true"` there, since `prepare` still runs and `amont`
will not be installed. [The details](docs/install.md#if-anything-ever-installs-without-your-dev-dependencies).

## Uninstall

```sh
amont uninstall              # this repository
amont uninstall --binary     # …and remove ~/.local/bin/amont too
```

Uninstall is listed second on purpose. These hooks can block a commit, so the
honest question to answer first is how you get out — and the answer is that
`uninstall` removes our five shims and **nothing else**. A hook you wrote
yourself is left where it is and named in the output, whatever it is; a hook it
cannot even read is named too rather than passed over in silence. Your
`hook.skip` and `amont.severity` settings are never touched, because those
are your statements about your repository.

This is also why the README does not tell you to run
`rm $(git rev-parse --git-dir)/hooks/*`. That glob deletes every hook in the
directory — including ones other tools installed and ones you wrote — to remove
five files that belong to us.

To bypass a single run rather than uninstall: `git commit --no-verify`.
To turn off one check permanently, see [Turning a check off, or down](#turning-a-check-off-or-down).

## What actually runs

`amont list` answers that for the repository you are standing in, and it is
the honest answer rather than the catalogue: most checks are **inert** in most
repositories, because a repo with no `ruff.toml` never needs ruff.

```
pre-commit
  ● ban-terms
  ○ cargo-fmt         inert here — needs .rs + Cargo.toml
  ● lint-json-yaml
  ● merge-conflict
  ○ prettier          inert here — needs .prettierrc | .prettierrc.json | …
  ● usual-name
pre-push
  ● branch-protect
  ● branch-pattern
  ● pull-rebase
  ○ cargo-test        inert here — needs .rs + Cargo.toml

  ● runs here   ○ inert   ⊘ skipped via hook.skip   ✗ declaration unusable
```

Twenty-four built-in checks across five git hooks, plus any your repository declares
itself. The full list, with what each one needs before it fires, is in
[the checks reference](docs/checks.md).

## Two ways to turn hooks on

**Per repository** — the default, and nothing runs anywhere you did not ask:

```sh
cd <your-repo> && amont install
amont-fleet install --root ~/Developer   # or in bulk, across many repos
```

`amont install --force` replaces a hook the installer would otherwise
refuse — one carrying no marker of ours, or a symlink. Even `--force` never
writes a tracked file or a directory. The full semantics:
[what `--force` will and will not do](docs/install.md#--force-and-what-it-will-not-do).

**Everywhere, forever** — an opt-in, and a real one:

```sh
git config --global init.templateDir ~/.config/git/git-templates/templates
```

Git copies that directory into `.git/hooks` on every `init` **and every
clone**, so from then on every repository you clone runs these hooks without
being asked again. That is the convenience, and it is worth having.

It is also a standing grant, so it is worth stating what you granted. A cloned
repository can declare its own checks in `amont.conf`, and with this key
set those are one `amont trust` away from running on your first commit in a
repository you may have cloned only to read. If you set this, **trust
deliberately** rather than letting installation be the moment you decided.

## One view across every repo

`amont-fleet` — installed separately, on purpose — answers the questions a
directory full of repositories accumulates: which repos are covered, which
shims went stale after an upgrade, what every check is doing where, and which
repository is quietly carrying a `hook.skip` somebody forgot.

![the amont-fleet dashboard scanning a fleet of repositories](docs/assets/fleet-demo.gif)

```sh
amont-fleet install --root ~/Developer   # shims into every repo at once
amont-fleet                              # report the fleet
amont-fleet tui                          # the dashboard above
```

Design record: [the fleet dashboard](docs/fleet-dashboard.md).

## Trust: a repository you clone cannot run its own checks

`amont.conf` is committed — that is the point, a team shares a check by
committing it. The consequence is that cloning a repository and committing to
it would otherwise run commands that repository chose, and neither of those
acts is one anybody performs *as a decision about trust*.

So a cloned repository's declared checks are **inert until you say otherwise**:

```sh
amont trust          # show what this repo declares, and accept it
amont trust --show   # what is trusted here
amont trust --revoke
```

The record is keyed on the file's **content**, not its path, so a `git pull`
that adds a command does not inherit the consent given to the file before it.
The fingerprint is `git hash-object --no-filters`, and the `--no-filters` is
not decoration: plain `hash-object` applies the clean filter and eol conversion
that the repository's own committed `.gitattributes` asks for, which would let
a repository choose the transform its own consent is taken through.

Full reasoning: [the trust model](docs/trust.md).

## Turning a check off, or down

Every check has an id, `<trigger>-<name>` — `pre-commit-clippy`. Three things
name it, and both config surfaces read all three the same way:

```sh
git config --add hook.skip pre-commit-clippy   # that one check
git config --add hook.skip clippy              # that check, on either trigger
git config --add hook.skip pre-commit          # every pre-commit check
```

`amont.severity.<key>` takes the same three, and keeps the signal — the
check still runs and still reports, it just stops failing the commit:

```sh
git config amont.severity.clippy warn
git config amont.severity.pre-commit warn
```

Where several keys reach one check the most specific wins: full id, then short
name, then trigger. Nothing matches by substring — `hook.skip e` reaches
nothing at all, and skipping `lint-js` leaves `lint-json-yaml` alone.

A skipped check is announced on every commit, so a config line nobody remembers
writing cannot go on silently disabling things. More in
[opting out](docs/opting-out.md) and [configuration](docs/configuration.md).

## Making the commit convention yours

`commit-msg` is the one hook `hook.skip` and `amont.severity` cannot reach, so its
opinions are adjustable in themselves:

```sh
amont setup     # four questions, current values as the defaults
```

It asks where the type's gitmoji goes — `none` (the default: your subject,
untouched), `prefix`, `suffix` or `replace` — and for the subject limit, the
description budget and the body wrap column. Then it prints the exact
`git config` lines it wrote, so you can paste them into your dotfiles or hand
them to a teammate.

```sh
git config amont.commit.gitmoji suffix        # feat: add a cart ✨
git config amont.commit.descriptionMax 68     # still fits a 72-col subject
git config amont.commit.bodyWrap 0            # leave my stack traces alone
```

The limits measure what you wrote, so a gitmoji never eats your budget, and
re-running over an amended message changes nothing. Full reference:
[commit conventions](docs/commit-convention.md#if-the-defaults-do-not-fit).

## Custom checks

A repository can declare checks of its own in a committed `amont.conf`:

```
# stage       name        scope   severity  command
pre-commit    shellcheck  *.sh    block     scripts/lint-shell.sh
pre-push      smoke       *       warn      make smoke
```

They run alongside the built-ins, obey the same `hook.skip` and
`amont.severity` controls, and are inert until trusted. Full reference:
[custom checks](docs/custom-checks.md).

## Running the checks yourself

```sh
amont run                    # would my commit pass? (the staged set)
amont run --all-files        # does my working tree pass? (git ls-files)
amont run pre-commit-prettier
amont list                   # what would run here, and why not
```

The two questions are different on purpose. `--all-files` on a dirty tree
reports on content that is not committed and may never be — which is what you
want when adopting a check into an existing repository, where `git add .` is
not an acceptable way to measure the mess.

## What a push actually tests

By default `pre-push` runs your suite against the **working tree**, and says
so. That is fast and usually what you want, but it is not what you are pushing:
an uncommitted fix makes a broken commit look green.

```sh
git config amont.testPushedTree true
```

turns on the accurate answer — the suite runs in a throwaway checkout of the
commits being pushed, and your tree is not touched. It costs a second checkout
and a build that cannot reuse your `target/` cache, which is why it is opt-in.

## For coding agents

`amont list --json` is the same answer as `amont list`, machine-readable:
every check's declared and effective severity, whether it fires here and why
not, and its command if it is a declared external. `--stage` filters to one
trigger; `--pushed` scopes to what your *next push* would carry (`@{u}..HEAD`)
rather than the whole tracked tree.

```sh
amont list --json --stage pre-push --pushed
amont agents-md          # write a self-verifying pointer into AGENTS.md
amont agents-md --check  # exit non-zero if it has drifted
```

`agents-md` only ever touches a `<!-- amont:start -->` / `<!-- amont:end -->`
span, so the rest of the file stays yours.

The block it writes also warns the agent that `git commit` and `git push`
run their checks first — pre-commit can mean clippy building a workspace,
pre-push a whole test suite — and an agent whose shell tool defaults to a
two-minute timeout will kill the command mid-check and read its own
impatience as a failure. Ten minutes is the safe floor, for both.

## Why you can let this near your commits

A prompt theme is cosmetic. This blocks commits and pushes, reads every staged
file, and runs with your credentials while nobody is watching — so the claim it
has to earn is not "delightful", it is "harmless".

- **The commit path links no external crates.** `amont` and
  `amont-runtime` are std-only, and `scripts/check-no-deps.sh` fails a build
  that changes that — fails *closed*, so a cargo error or an unreachable
  registry is a failure rather than a reassuring green tick. `amont-fleet`
  takes dependencies quite happily; it is installed separately and runs when
  asked.
- **No network, ever.** The binary phones nothing home — no telemetry, no
  update checks, no fetches. With the commit path std-only, there is not even
  an HTTP client linked to do it with.
- **Over six hundred tests**, run on Linux, macOS and Windows, alongside `cargo fmt
  --check`, `clippy -D warnings`, an MSRV floor of 1.74 compiled for the commit
  path, and `cargo-audit`.
- **v1.0.0 followed a full security review**, and each finding landed with a
  committed reproduction: a drive-by RCE via a relative path in the shim; a
  held-store format that let a repository delete a tracked file and plant a
  symlink outside the worktree; a trust prompt a repository could conceal
  declarations from; guards that wrote through symlinks onto tracked source; a
  commit subject shaped like a trailer destroying commit trailers;
  `pre-commit-pyright` blocking every commit it ran on.
- **Your uncommitted work is the thing that must never be lost.** The release
  profile deliberately omits `panic = "abort"` so that the `Drop` that restores
  unstaged work still runs when a check panics — with a test asserting on the
  manifest, because cargo ignores that setting for test targets and no
  behavioural test could catch the regression.

Threat model and private reporting: [SECURITY.md](SECURITY.md).

## Windows

Everything works, and the PowerShell one-liner in [Install](#install) is all
the setup most machines need. One difference: there is no symlink. To build
from source instead — Git for Windows ships `bash` and coreutils but not
`make`:

```sh
cargo build --release
./target/release/amont install
```

On macOS/Linux `~/.config/git/git-templates` is usually a symlink to the
checkout, so `init.templateDir` can point at a stable XDG path. Windows does
not create symlinks without Developer Mode or elevation, so point git straight
at the checkout instead:

```sh
git config --global init.templateDir 'C:/path/to/amont/templates'
```

Nothing else changes. The shims never need the symlink: they resolve the binary
at runtime, trying `$GIT_HOOKS_BIN`, the baked path, `~/.local/bin/amont`
and `~/.local/bin/amont.exe`, then `PATH`.

## Requirements

- **Git 2.31+** — `git rev-parse --path-format=absolute` landed there, and
  three places depend on it. On an older git those return nothing rather than
  failing loudly, which is the worst shape for a version floor: the tool
  appears to work and quietly resolves the wrong paths.

The hooks are a single binary with no runtime dependencies. Each check brings
its own tool requirement only where you have opted into that check.

## Documentation

The full documentation is in [`docs/`](docs/), versioned with the code and
published as a book:

- [Installing and activating](docs/install.md)
- [The checks](docs/checks.md) · [Configuration](docs/configuration.md) ·
  [Opting out](docs/opting-out.md)
- [The trust model](docs/trust.md) · [Custom checks](docs/custom-checks.md)
- [Where the hooks fit in your flow](docs/coding-flow.md) ·
  [Commit conventions](docs/commit-convention.md) ·
  [How it compares](docs/similar-projects.md) ·
  [Ideas, not a roadmap](docs/ideas.md)
- Decision records for maintainers: [hook architecture](docs/hook-architecture.md),
  [index fidelity and run modes](docs/index-fidelity-and-run-modes.md),
  [skip management](docs/hook-skip-management.md),
  [the fleet dashboard](docs/fleet-dashboard.md),
  [the Rust migration](docs/rust-migration.md)

## Contributing

Everything is Rust, in `crates/`:

```
crates/amont-runtime/   the checks, registry and dispatchers. std only.
crates/amont/           the hook binary. Runs on every commit. std only.
crates/amont-fleet/     the dashboard and the fleet fixer. Opt-in.
```

`make check` is the CI-parity target — run it before you push. Setup, the
zero-dependency rule and when reopening it is legitimate, the house test style
and the commit convention are all in [CONTRIBUTING.md](CONTRIBUTING.md).

Questions, "does this work with X", ideas for checks:
[Discussions](https://github.com/fredericrous/amont/discussions). Bugs:
[issues](https://github.com/fredericrous/amont/issues).

By participating you agree to the [Code of Conduct](CODE_OF_CONDUCT.md).

## License

[MIT](LICENSE).
