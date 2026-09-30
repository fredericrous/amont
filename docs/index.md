# amont

**Catch the bad commit before it exists — and take the whole thing back out in
one command.**

One Rust binary, no runtime, no config file to write. Install it, run
`amont install` in a repository, and your next commit is checked by thirty-nine
language-aware checks that already know when to stay out of the way.

![amont catching a commit and letting the fixed one through](assets/amont-demo.gif)

## Why this one

- **Useful in the first minute.** Other hook managers install empty and wait
  for you to write YAML. amont ships thirty-nine checks — commit-message
  conventions, merge-conflict markers, linters and formatters for the languages
  your repository actually uses, branch rules, your test suite — and each one
  fires only where the repository has opted into its tool. `amont list`
  shows you what runs here, and why the rest will not.
- **Nothing on the commit path but `std`.** The hook binary links no external
  crates, and CI fails any build that changes that. What runs on every commit,
  with your credentials, has the smallest supply chain this project could
  arrange: none.
- **A cloned repository cannot run code on your machine.** Repositories declare
  their own checks in a committed `amont.conf` — and those declarations are
  inert until you review them and say `amont trust`. No other hook manager
  puts a review gate between `git clone` and running the repository's commands.
  [The trust model](trust.md).
- **Your uncommitted work is never collateral.** Checks run against exactly
  what you staged; unstaged work is held aside without `git stash` and restored
  even if a check panics. The design that makes that true is the most carefully
  argued part of the codebase.
- **Leaving is one command.** `amont uninstall` removes exactly the six
  shims install wrote — a hook you or another tool put there is named and left
  alone. A gate you cannot exit cleanly is a gate you were right not to enter.

How that stacks up against pre-commit, lefthook and husky, feature by feature:
[how it compares](similar-projects.md).

## Why you can let this near your commits

A prompt theme is cosmetic. This blocks commits and pushes, reads every staged
file, and runs with your credentials while nobody is watching — so the claim it
has to earn is not "delightful", it is "harmless".

- **The commit path links no external crates.** `amont` and `amont-runtime`
  are std-only, and `scripts/check-no-deps.sh` fails a build that changes
  that — fails _closed_, so a cargo error or an unreachable registry is a
  failure rather than a reassuring green tick.
- **No telemetry, no update checks.** With the commit path std-only there is
  not even an HTTP client linked to phone home with. The network a push does
  touch is git's own and the tools you opted into: `pull-rebase` runs
  `git ls-remote`/`git fetch` against your upstream (off with
  `git config amont.autoRebase false`, see
  [configuration](configuration.md)), and the `audit-*` checks call
  `cargo audit`, `npm audit`, `pip-audit` and `govulncheck`.
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

Threat model and private reporting:
[SECURITY.md](https://github.com/fredericrous/amont/blob/main/SECURITY.md).

## The repositories around it

Three companions, each its own repository because it runs somewhere amont
deliberately does not:

- [**amont-agent**](https://github.com/fredericrous/amont-agent) — a Claude
  Code `PreToolUse` hook for the mistake no git hook can reach, because it
  lives in the command string itself: `git push … | tail -5` reports tail's
  exit status, so a rejected push reads as success. The guard judges the
  pipeline before it runs. Independent by design — no shared code, and
  neither needs the other; they meet in one optional place, where its
  session notice asks `amont agents-md --check` whether the guidance block
  an agent is about to believe has gone stale.
- [**attest**](https://github.com/fredericrous/attest) — the CI half of
  `amont.attest`: when every pre-push block gate passed locally, amont
  leaves a **signed** note on the tree it tested, and this single-purpose
  verifier lets CI skip work provably already done. Fail-open by
  construction, and separate precisely so that amont itself never runs in
  CI — [the reasoning](ci.md).
- [**amont-pack-java**](https://github.com/fredericrous/amont-pack-java) —
  the worked example of [a pack](custom-checks.md#shipping-a-check--packs):
  how checks amont deliberately does not build in get shipped anyway.

## Start here

1. **[Installing and activating](install.md)** — get the binary, turn hooks on
   in one repository (or every repository you ever clone), and turn them off
   again.
2. **[The checks](checks.md)** — what the thirty-nine built-ins do, and what each
   one needs before it fires.
3. **[Opting out](opting-out.md)** — skip one check, downgrade a whole trigger,
   bypass a single commit, or remove the hooks entirely.

Then, as you need them: [where the hooks fit in your flow](coding-flow.md) ·
[commit and branch conventions](commit-convention.md) ·
[configuration](configuration.md) · [custom checks](custom-checks.md) ·
[the trust model](trust.md).

## How the documentation is organised

The pages under **Using amont** are for anybody who has installed it or is
deciding whether to.

The pages under **Design records** are for maintainers: the arguments behind
the current behaviour, kept because "why is it like this" is a question that
comes back. Do not start there.
