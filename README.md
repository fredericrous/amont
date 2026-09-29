# amont

**Catch the bad commit before it exists — and take the whole thing back out in one command.**

[![CI](https://github.com/fredericrous/amont/actions/workflows/ci.yaml/badge.svg)](https://github.com/fredericrous/amont/actions/workflows/ci.yaml)
[![Release](https://img.shields.io/github/v/release/fredericrous/amont?label=release)](https://github.com/fredericrous/amont/releases/latest)
[![License](https://img.shields.io/github/license/fredericrous/amont)](LICENSE)

A single Rust binary that checks `git commit` and `git push`. It ships
thirty-eight built-in checks — commit-message conventions, conflict markers,
secrets, the linters and test suites of the languages your repository actually
uses — and each one fires only where the repository has opted into its tool, so
there is no YAML to write before it is useful. The commit path links no
external crates, a cloned repository's own checks stay inert until you
`amont trust` them, and `amont uninstall` removes exactly the six shims install
wrote. [How it compares](docs/similar-projects.md) to pre-commit, lefthook and
husky.

![amont catching a commit and letting the fixed one through](docs/assets/amont-demo.gif)

`amont list` says what runs in the repository you are standing in — here, a
Python one:

```
pre-commit
  ● ban-terms
  ● ruff
  ● secrets
  …
pre-push
  ● audit-python
  …
  15 active here.  23 inert (Go, JavaScript, Kubernetes, Python, Rust) — amont list --all
```

## Install

```sh
# Linux and macOS
curl -fsSL https://raw.githubusercontent.com/fredericrous/amont/main/install/install.sh | sh
```

```powershell
# Windows
irm https://raw.githubusercontent.com/fredericrous/amont/main/install/install.ps1 | iex
```

Either one downloads a release binary, checks it against the published
`SHA256SUMS`, and **enables nothing**. Homebrew, crates.io, npm, prebuilt
archives and building from source: [installing](docs/install.md). Needs git
2.31 or later.

## Turning hooks on

```sh
cd <your-repo> && amont install          # this repository only — the default
amont-fleet install --root ~/Developer   # many repositories at once
amont enroll --conventions declared      # every future clone on this machine
```

`enroll` is a standing grant; read [rolling out to a team](docs/team-rollout.md)
before you use it. `amont uninstall` removes the shims again —
[opting out](docs/opting-out.md) covers that and everything short of it.

## Day to day

```sh
amont run                  # would my commit pass? (the staged set)
amont run --all-files      # does my working tree pass?
amont check src/main.rs    # these files, in the format editors already parse
amont rehearse --wait      # the push gate on a snapshot of HEAD, before you push
amont trust                # review what this repository's amont.conf declares
git config amont.severity.clippy warn   # runs, reports, does not block
git config hook.skip clippy             # does not run at all
```

## Documentation

Versioned with the code in [`docs/`](docs/) and published as a
[book](https://fredericrous.github.io/amont/):

- [Installing and activating](docs/install.md) ·
  [Rolling out to a team](docs/team-rollout.md) · [The CI backstop](docs/ci.md)
- [The checks](docs/checks.md) · [Configuration](docs/configuration.md) ·
  [Opting out](docs/opting-out.md) · [Commit conventions](docs/commit-convention.md)
- [Where the hooks fit in your flow](docs/coding-flow.md) ·
  [The trust model](docs/trust.md) · [Custom checks and packs](docs/custom-checks.md)
- [Why you can let this near your commits](docs/index.md#why-you-can-let-this-near-your-commits) ·
  [The repositories around it](docs/index.md#the-repositories-around-it) ·
  [SECURITY.md](SECURITY.md) · [design records](docs/SUMMARY.md#design-records)

## Contributing

`make check` is the CI-parity target; setup, the zero-dependency rule and the
house style are in [CONTRIBUTING.md](CONTRIBUTING.md). Questions in
[Discussions](https://github.com/fredericrous/amont/discussions), bugs in
[issues](https://github.com/fredericrous/amont/issues). By participating you
agree to the [Code of Conduct](CODE_OF_CONDUCT.md).

## License

[MIT](LICENSE).
