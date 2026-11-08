# Contributing

Thanks for looking. This document is the parts that are not obvious from the
source — the local gate, the one hard rule, the test style, and the commit
convention this repository enforces on itself.

By participating you agree to the [Code of Conduct](CODE_OF_CONDUCT.md). To
report a vulnerability, do **not** open an issue: see [SECURITY.md](SECURITY.md).
For questions and ideas that are not yet defects, use
[Discussions](https://github.com/greenharness/greengate/discussions).

## Setup

```sh
git clone https://github.com/greenharness/greengate.git
cd amont
make check
```

That is it. The toolchain is pinned in `rust-toolchain.toml`, and the commit
path's floor is `rust-version` in `Cargo.toml` (1.74), enforced by CI's `msrv`
job. Both are deliberate PRs to change: under `-D warnings` a new clippy
release is a breaking change, which is the whole reason the pin exists.

Some checks shell out to tools (`ruff`, `prettier`, `kubeconform`, …). Their
tests **skip** when the tool is absent rather than failing, and `make test`
passes `--show-output` specifically so a skip is visible — cargo hides stdout
for passing tests, and a silent skip is indistinguishable from a pass.

### The make targets

| target | what it is |
|---|---|
| `make check` | **`lint` then `test`. This is the CI-parity target, and what a bare `make` runs.** Run it before you push. |
| `make lint` | Exactly what CI's `rust` job gates on: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`. |
| `make test` | `scripts/check-no-deps.sh` plus `cargo test`. Deliberately lint-free so the inner loop stays fast. `make test RUN=<suite>` runs one. |
| `make deps` | The dependency guard on its own. |
| `make install` | Builds and runs `amont install`. |
| `make install-fleet` | The dashboard, installed separately and on purpose. |
| `make propagate` | Push the shim **set** to every repo. Dry run; `APPLY=1` writes. Only needed when a hook is added, removed or renamed. |

**There is a third clippy invocation, and it is stricter than both of the
others.** This repository runs its own `pre-commit-clippy` on itself, and that
one is `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
CI's is `--all-targets` only. So the hook can reject a commit that `make check`
and CI would both accept. That divergence is left in place on purpose —
unifying them is a decision about which is authoritative, not a quiet edit to
whichever line you happened to be looking at.

## The one hard rule: the commit path takes no dependencies

`amont` and `amont-runtime` link **no external crates**, and
`scripts/check-no-deps.sh` fails a build that changes that. CI runs it as its
own job.

The reasons, in the order they actually matter:

1. **Supply chain.** This binary runs on every commit, in every repository it
   is installed in, with the developer's credentials, reading every staged
   file, while nobody is watching. Every transitive crate is code executing in
   that position. Zero dependencies means that code is `std` and ours.
2. **Offline reproducibility.** A std-only crate builds indefinitely without a
   registry. Real, though modest.
3. **A forcing function.** The guard prevents no specific harm so much as it
   makes each dependency in the commit path an argued decision rather than a
   default.

**It is something to argue with, not a wall.** Read the script before arguing
either way — it says so itself, and it explains when reopening the rule would
be a legitimate call. If the commit path ever genuinely needs a parser, weigh
that crate's tree against the code it replaces, and make the case in the PR. A
rule nobody may question is just superstition with CI attached.

The guard checks the **resolved dependency tree**, not the manifest, because a
dependency reaches the commit path through `amont-runtime` as easily as
directly. Every step of it fails **closed**: cargo missing, registry
unreachable, run from outside the workspace — all of those are failures, not
passes. It used to send stderr to `/dev/null` and end with `|| true`, and a
job named "hook binary has no external dependencies" would have stayed green
through every one of them. A guard that cannot fail is decoration.

`amont-fleet` takes dependencies quite happily — ratatui, crossterm, serde.
It is installed separately and runs when asked. **If your feature needs a
crate, that is where it goes.**

## Layout

```
crates/amont-runtime/   the checks, registry and dispatchers. std only.
crates/amont/           the hook binary. Runs on every commit. std only.
crates/amont-fleet/     the dashboard and the fleet fixer. Opt-in.
```

A new check is **a module plus one registry entry** in
`crates/amont-runtime/src/registry.rs` — not a script. Name, stage, scope,
severity and function are declared together in one table, and a consistency
test fails if a registered name has no shim or a shim has no handler. See
[docs/hook-architecture.md](docs/hook-architecture.md).

Before adding a built-in, ask whether it belongs in everybody's binary. A check
that is right for *one* repository can be declared in that repository's
`amont.conf` today, no fork required. See
[docs/custom-checks.md](docs/custom-checks.md).

## Tests

There are over six hundred of them and they are the reason changes here can be
made at all.
Two conventions matter:

**Test the property, not the line.** Name the test after the guarantee it
defends — `the_two_vocabularies_are_reconciled`,
`the_usage_block_names_every_subcommand`, `only_chore_allows_dots` — so a
failure reads as "this promise broke", and so the test survives a refactor of
the code beneath it.

**Write the doc comment.** Every non-obvious test carries a narrative `///`
saying what would go wrong without it, and where possible naming the incident:

```rust
/// The point of this module. Every name must be shared, or declared as an
/// exception with a reason — so adding a commit type forces a decision about
/// whether it is also a branch prefix, instead of silently producing a
/// rejected push months later.
#[test]
fn the_two_vocabularies_are_reconciled() {
```

A test whose purpose is not written down gets deleted by the next person who
finds it inconvenient. That is not a hypothetical here: several tests in this
repository exist because a specific bug shipped, and the comment is the only
thing that will stop the guard being "simplified" away.

**A bug fix arrives with its reproduction.** Every finding in the v1.0.0
security review landed with a test that fails on the old code. If a fix cannot
be reproduced in a test, say so in the PR and explain why — sometimes that is
legitimate (`release_profile.rs` asserts on `Cargo.toml` because cargo ignores
the `panic` setting for test targets, so no behavioural test could ever see the
regression), and sometimes it means the fix is not understood yet.

## Commits

This repository's own hooks validate its commits, so the convention is not
advice:

- **A conventional type prefix**, from `build`, `chore`, `docs`, `feat`, `fix`,
  `perf`, `refactor`, `revert`, `style`, `test`, `add`, `remove`.
- **The description is at most 50 characters** after the prefix; the whole
  subject at most 72.
- **No emoji.** A `prepare-commit-msg`/`commit-msg` hook prepends the type's
  gitmoji for you. One you add by hand eats into the 50-character budget and
  can push you over.
- **A substantial body.** The subject says what changed; the body says why, and
  what was considered instead. This repository's history is a large part of its
  documentation — several `docs/` pages are just a commit message that grew up.

```
feat: declared checks carry a trigger id

<why, what else was considered, what broke before>
```

Branches: `prefix/name`, e.g. `feat/3002-image-crop`. Prefixes are the commit
types plus `hotfix` and `automation`. Only `chore/` allows dots, for version
bumps. See [docs/commit-convention.md](docs/commit-convention.md).

`commit-msg` cannot be bypassed with `--no-verify` — that is git's behaviour.
To get a message past it, fix the message.

## Pull requests

1. Branch from `main`.
2. `make check` locally. Match CI exactly rather than letting CI find the first
   round of failures for you.
3. If you touched anything under `docs/`, the docs workflow builds the book on
   the PR — a page missing from `docs/SUMMARY.md` fails there.
4. Say **why** in the description. A diff shows what changed; the review needs
   the alternative you rejected.
5. If an upgrader would notice the change, say so under `Unreleased` in
   [CHANGELOG.md](CHANGELOG.md) — in sentences, what they get, not what the
   diff did. The release guard refuses to tag a version whose section is
   missing, and the published notes open with it.

Documentation lives in `docs/`, versioned with the code and published as a
book. It is not a wiki, deliberately: a wiki cannot be reviewed in a pull
request, cannot be bisected, and drifts from the implementation without
anything noticing — which is exactly what happened to the pages `docs/` now
replaces.
