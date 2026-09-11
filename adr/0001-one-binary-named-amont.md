---
id: ADR-0001
status: accepted
decisions:
  - key: dist.binary-shape
    choice: One binary; the hook name is argv[1]
    first: true
    reason: Five binaries would be five downloads and five points to keep in step
  - key: dist.binary-name
    choice: amont
    first: true
    reason: git may resolve an executable named `git-hooks` as a subcommand
  - key: config.source
    choice: git config, and nothing else
    first: true
    reason: A second configuration file is a second source of truth
  - key: deps.budget
    choice: regex and ignore, and resist more
    first: true
  - key: platform.windows-parity
    choice: The same cargo test suite as every other platform
    first: true
    reason: A smoke test would find what a smoke test finds
---
# 0001 — one binary, called amont

Detail and the full migration record are in
[`docs/rust-migration.md`](../docs/rust-migration.md), which is marked complete
and carries its own account of what the plan got wrong.

## Shape

One executable, selecting behaviour from `argv[1]`. Five separate binaries
would mean five artefacts to download, five version numbers, and five chances
for a repository to end up with a set that does not agree with itself.

## Name

`amont`, not `git-hooks`. An executable named `git-hooks` on `PATH` is
something git may pick up as a subcommand, which is a surprising way to
discover a naming collision.

## Configuration lives in `git config`

Not a `.amontrc`, not an `.amont.toml`. The rule this protects is that there is
no second source of truth about what runs — and it keeps `git -c hook.skip=… push`
working, which is the escape hatch people reach for under pressure.

`amont.conf` is not a counter-example: it declares *repository* checks and is
committed and reviewed (ADR-0004). What a given machine skips or downgrades
stays in git config.

## The dependency budget

`regex` and `ignore`, both ripgrep's, and resistance to more. This is a binary
that runs on the commit path, so every dependency is a thing that can fail to
build, slow a commit, or need a security response.

## Windows runs the same suite

Not a smoke test. A hook that behaves differently on one platform is a hook
that is wrong somewhere, and the cheapest time to find out is in CI.
