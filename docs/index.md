# amont

**Catch the bad commit before it exists — and take the whole thing back out in
one command.**

One Rust binary, no runtime, no config file to write. Install it, run
`amont install` in a repository, and your next commit is checked by thirty-four
language-aware checks that already know when to stay out of the way.

![amont catching a commit and letting the fixed one through](assets/amont-demo.gif)

## Why this one

- **Useful in the first minute.** Other hook managers install empty and wait
  for you to write YAML. amont ships thirty-four checks — commit-message
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
- **Leaving is one command.** `amont uninstall` removes exactly the five
  shims install wrote — a hook you or another tool put there is named and left
  alone. A gate you cannot exit cleanly is a gate you were right not to enter.

How that stacks up against pre-commit, lefthook and husky, feature by feature:
[how it compares](similar-projects.md).

## Start here

1. **[Installing and activating](install.md)** — get the binary, turn hooks on
   in one repository (or every repository you ever clone), and turn them off
   again.
2. **[The checks](checks.md)** — what the thirty-four built-ins do, and what each
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
