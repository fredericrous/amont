# amont

**Catch the bad commit before it exists — and take the whole thing back out in one command.**

[![CI](https://github.com/fredericrous/amont/actions/workflows/ci.yaml/badge.svg)](https://github.com/fredericrous/amont/actions/workflows/ci.yaml)
[![Release](https://img.shields.io/github/v/release/fredericrous/amont?label=release)](https://github.com/fredericrous/amont/releases/latest)
[![License](https://img.shields.io/github/license/fredericrous/amont)](LICENSE)

A single Rust binary that checks `git commit` and `git push` — no YAML to
write, no runtime to install, nothing to configure before it is useful.

- **Useful in the first minute.** Thirty-seven built-in checks — commit-message
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

```sh
# Linux and macOS
curl -fsSL https://raw.githubusercontent.com/fredericrous/amont/main/install/install.sh | sh
```

```powershell
# Windows (the line above is POSIX sh and only reaches Windows through Git Bash)
irm https://raw.githubusercontent.com/fredericrous/amont/main/install/install.ps1 | iex
```

Either one downloads a release binary, verifies it against the published
`SHA256SUMS`, and puts it where the hooks already look. Both **enable
nothing** — hooks are turned on per repository, by you, afterwards:

```sh
cd <your-repo> && amont install   # this repository only
amont list                        # what would run here, and why not
```

Homebrew, crates.io, npm, prebuilt binaries for six targets, building from
source, and what `--force` will and will not do:
[installing and activating](docs/install.md).

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

Thirty-seven built-in checks across five git hooks, plus any your repository
declares itself. What each one needs before it fires:
[the checks reference](docs/checks.md).

## Turning hooks on

**Per repository** is the default, and nothing runs anywhere you did not ask:

```sh
cd <your-repo> && amont install
amont-fleet install --root ~/Developer   # or in bulk, across many repos
```

**Everywhere, forever** is an opt-in, and a real one:

```sh
amont enroll --conventions declared
```

One command per machine: every future `git clone` and `git init` arrives with
the hooks. `--conventions declared` keeps the house rules — commit shapes,
branch names, gates — scoped to repositories that commit an `amont.conf`,
while the safety net of conflict markers, secrets, oversized files and debug
leftovers runs everywhere.

That split is what makes the grant safe on a machine that also clones other
people's projects. It is still a standing grant, so it is worth stating what
you granted: a cloned repository's own checks are then one `amont trust` away
from running on your first commit in a repository you may have cloned only to
read. **Trust deliberately** rather than letting installation be the moment
you decided. The full reasoning, and rolling this out to a team:
[team rollout](docs/team-rollout.md).

## Trust: a repository you clone cannot run its own checks

`amont.conf` is committed — that is the point, a team shares a check by
committing a line rather than by everybody installing something. So a
repository you cloned can _declare_ checks, and running them is a decision you
make, not one `git clone` makes for you.

```sh
amont trust          # show what it declares, then record it
amont trust --show   # what it declares, and whether it is trusted
amont trust --revoke
```

Until then the declarations are **reported, not dropped** — you can see there
is a decision waiting. Acceptance is recorded against the file's _content_, so
a `git pull` that adds a command does not inherit the trust you gave the file
before it. [The trust model](docs/trust.md).

## Custom checks, and packs

A repository declares checks of its own in a committed `amont.conf` — five
whitespace-separated fields, no shell:

```
# stage       name        scope                severity  command
pre-commit    lint-shell  *.sh                 block     scripts/lint-shell.sh
pre-commit    rubocop     *.rb+.rubocop.yml    block     rubocop
pre-push      smoke       *                    warn      make smoke
```

They run alongside the built-ins, obey the same `hook.skip` and
`amont.severity` controls, and are inert until trusted.

The `+` in the scope column splits _what the change touches_ from _what the
repository carries_: `*.rb+.rubocop.yml` reads "a staged `.rb`, **and** this
repository has a `.rubocop.yml`". That is what lets a check be safe to hand to
somebody else.

### Shipping one — packs

A **pack** is any git repository with an `amont.pack` at its root, written in
that same syntax. `amont add` vendors its rows into your `amont.conf`:

```sh
amont add github:fredericrous/amont-pack-java@v1
```

What ships is **text, not execution**. The rows land between markers with the
pack's commit id beside them, the manifest's fingerprint changes, and every
declared check is inert until you `amont trust` it. Adding a pack is never the
moment anything becomes runnable.

Want to write one? [`fredericrous/amont-pack-java`](https://github.com/fredericrous/amont-pack-java)
is a complete worked example — two rows, one for Maven and one for Gradle, so
half of it is visibly inert wherever you install it. Its README is the
long-form authoring guide: gating your rows, what a pack may not carry, and
how to publish and tag one.

Full reference for both: [custom checks](docs/custom-checks.md).

## Day to day

```sh
amont run                      # would my commit pass? (the staged set)
amont run --all-files          # does my working tree pass? (git ls-files)
amont check src/main.rs        # what is wrong with these FILES — no index, no
                               # staging; `file:line: message` an editor can parse
amont list                     # what would run here, and why not
amont restore                  # bring back unstaged work a killed hook parked
```

`amont run --all-files` on a dirty tree reports on content that is not
committed and may never be — which is what you want when adopting a check into
an existing repository, where `git add .` is not an acceptable way to measure
the mess.

Turning one off, or down:

```sh
git config amont.severity.clippy warn   # runs, reports, does not block
git config hook.skip clippy             # does not run at all
```

Prefer the downgrade: it keeps the check working and keeps you looking at what
it finds. [Opting out](docs/opting-out.md) · [configuration](docs/configuration.md)
· [commit conventions](docs/commit-convention.md).

**What a push actually tests.** By default `pre-push` runs your suite against
the _working tree_, and says so — fast, and usually what you want, but not what
you are pushing. `git config amont.testPushedTree true` runs it in a throwaway
checkout of the commits being pushed instead.

**For coding agents.** `amont list --json` is the same answer as `amont list`,
machine-readable: declared and effective severity, whether each check fires
here and why not, and the command if it is a declared external. `--stage`
filters to one trigger, `--pushed` scopes to what your next push would carry.
`amont agents-md` writes the guidance block into `AGENTS.md`.
[Where the hooks fit in your flow](docs/coding-flow.md).

## One view across every repo

`amont-fleet` — installed separately, on purpose — answers the questions a
directory full of repositories accumulates: which repos are covered, which
shims went stale after an upgrade, and which repository is quietly carrying a
`hook.skip` somebody forgot.

![the amont-fleet dashboard scanning a fleet of repositories](docs/assets/fleet-demo.gif)

```sh
amont-fleet install --root ~/Developer   # shims into every repo at once
amont-fleet                              # report the fleet
amont-fleet tui                          # the dashboard above
amont-fleet fix --root ~/Developer       # what drifted (dry run)
```

Design record: [the fleet dashboard](docs/fleet-dashboard.md).

## Why you can let this near your commits

A prompt theme is cosmetic. This blocks commits and pushes, reads every staged
file, and runs with your credentials while nobody is watching — so the claim it
has to earn is not "delightful", it is "harmless".

- **The commit path links no external crates.** `amont` and `amont-runtime`
  are std-only, and `scripts/check-no-deps.sh` fails a build that changes
  that — fails _closed_, so a cargo error or an unreachable registry is a
  failure rather than a reassuring green tick.
- **No network, ever.** No telemetry, no update checks, no fetches. With the
  commit path std-only there is not even an HTTP client linked to do it with.
- **Over a thousand tests**, run on Linux, macOS and Windows, alongside
  `cargo fmt --check`, `clippy -D warnings`, an MSRV floor of 1.74 compiled
  for the commit path, and `cargo-audit`.
- **v1.0.0 followed a full security review**, and each finding landed with a
  committed reproduction — a drive-by RCE via a relative path in the shim, a
  held-store format that let a repository plant a symlink outside the
  worktree, a trust prompt a repository could conceal declarations from.
- **Your uncommitted work is the thing that must never be lost.** The release
  profile deliberately omits `panic = "abort"` so the `Drop` that restores
  unstaged work still runs when a check panics — with a test asserting on the
  manifest, because no behavioural test could catch that regression.

Threat model and private reporting: [SECURITY.md](SECURITY.md).

## Requirements

**Git 2.31+** — `git rev-parse --path-format=absolute` landed there, and three
places depend on it. On an older git those return nothing rather than failing
loudly, which is the worst shape for a version floor.

The hooks are a single binary with no runtime dependencies. Each check brings
its own tool requirement only where you have opted into that check.

Everything works on Windows; the one difference is that there is no symlink,
so `init.templateDir` points straight at the checkout. [Details](docs/install.md).

## Documentation

The full documentation is in [`docs/`](docs/), versioned with the code and
published as a book:

- [Installing and activating](docs/install.md)
- [The checks](docs/checks.md) · [Configuration](docs/configuration.md) ·
  [Opting out](docs/opting-out.md)
- [The trust model](docs/trust.md) · [Custom checks and packs](docs/custom-checks.md)
- [Where the hooks fit in your flow](docs/coding-flow.md) ·
  [Commit conventions](docs/commit-convention.md) ·
  [CI, and why amont does not run in it](docs/ci.md) ·
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
