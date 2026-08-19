# How it compares

The field is good. [pre-commit](https://github.com/pre-commit/pre-commit),
[lefthook](https://github.com/evilmartians/lefthook) and
[husky](https://github.com/typicode/husky) are all mature, widely used and
worth your time. This page is what you get by choosing amont instead — and
what you give up.

## At a glance

| | amont | pre-commit | lefthook | husky |
|---|---|---|---|---|
| Runtime it needs | none — one binary | Python (hooks bring their own environments) | none — one binary | Node.js — already present in the projects it targets |
| Useful before you write any config | **32 built-in checks**, scoped to what the repo uses | starts empty | starts empty | starts empty |
| On the commit path | **zero external crates**, CI-enforced | Python + a managed environment per hook | Go binary | Node + `node_modules` |
| A cloned repo's committed config runs code… | **only after you review it and `amont trust`** | after `pre-commit install`, unreviewed | after `lefthook install` — often automatic via a package `postinstall` | after `npm install` — the `prepare` script activates it |
| Your unstaged work during a run | held aside without `git stash`, restored even if a check panics | `git stash` around the run | untouched — checks see the worktree, not the staged set | your problem — hooks are your scripts |
| Uninstall | removes exactly the five files it wrote, names everything else | `pre-commit uninstall` | `lefthook uninstall` | delete `.husky/`, unset `core.hooksPath` |
| Commit-message conventions | built in — validated, wrapped, limits and gitmoji configurable | via a separate hook | via commitlint etc. | via commitlint etc. |
| One view across all your repos | **`amont-fleet`** — bulk install, report, dashboard | per repo | per repo | per repo |
| Machine-readable state for coding agents | **`amont list --json`, `amont agents-md`** | no | no | no |

## The three arguments

**It works before you configure it.** Every other manager on this list installs
a framework and hands you an empty file: nothing runs until you have decided
what a good commit looks like and written it down, per repository, in that
tool's YAML. amont ships the decision — conventional commit subjects, no
merge-conflict markers, no `describe.only` leftovers, the linters and
formatters for the languages your repository actually uses, a test-suite gate
on push — and scopes it automatically: a repository with no `ruff.toml` never
needs ruff, a JavaScript repository never invokes cargo. What is left for you
to configure is the exception, not the baseline, and every knob is plain
`git config`.

**The commit path is the smallest attack surface in the field.** A hook
manager runs on every commit, with your credentials, reading every staged
file. husky puts `node_modules` on that path; pre-commit puts a tree of
cloned hook repositories with their managed language environments there. The
amont binary links no external crates — `std` only, with a CI script that
fails any build that changes it — and the installer verifies its download
against published checksums before placing the binary. And where the others
run a cloned repository's committed config as a side effect of routine setup —
`npm install`, a `postinstall`, an unreviewed `pre-commit install` — amont
holds a repository's declared checks **inert until you have read them and said
[`amont trust`](trust.md)**. Nobody else on this list has that gate.

**It is built for the failure cases.** What happens to your unstaged work when
a formatter rewrites files mid-commit — and when it panics mid-rewrite? What
happens when a check's tool is not installed, a config line is malformed, or a
`hook.skip` was set three years ago and forgotten? amont has a designed
answer to each: unstaged work is held aside without `git stash` and restored
even on panic; a check that cannot run is reported as a gap rather than passed
or silently dropped; a skipped check is announced on every commit. These cases
decide whether a hook manager is trusted or worked around, and most of this
project's engineering lives there.

## What the others do better

Stated plainly, because a comparison that finds no trade-offs is an
advertisement:

- **pre-commit's ecosystem is enormous.** Hundreds of ready-made hooks, and it
  bootstraps each hook's language environment for you. amont deliberately
  refuses to be a package manager — a check that cannot find its tool warns or
  fails loudly, and the tool stays your problem. For turnkey environments
  around many exotic tools, pre-commit is the right call.
- **lefthook's config is very expressive** — glob routing, piped commands,
  scripts, tags, per-OS overrides — where [`amont.conf`](custom-checks.md)
  is deliberately five columns and no shell.
- **husky is nearly nothing**, which is a real virtue: two lines of shell in a
  committed file and you are done. An all-Node team that reviews everything may
  never fall into the gaps husky leaves open.
- **The built-ins carry opinions** — conventional commits, branch naming, a
  set of commit types. Every check can be [downgraded or
  skipped](opting-out.md) with exact, announced config, and the commit-message
  rules are themselves [tunable](commit-convention.md#if-the-defaults-do-not-fit)
  — but a blank slate is the one thing this tool is not.

## Credit where due

Ideas taken from the others, with the full record in
[index fidelity and run modes](index-fidelity-and-run-modes.md):

- **From pre-commit:** a repository declaring its own checks in a committed
  file, so a team shares a check by committing it rather than by each member
  installing it by hand — taken as [`amont.conf`](custom-checks.md), with
  a [trust gate](trust.md) added in front, because a committed manifest is a
  committed *command* and cloning is not consent.
- **From all three:** the observation that filename-prefixed
  `.git/hooks/pre-commit-*` scripts can never be shared, because `.git/hooks`
  is not committed.

Also in the neighbourhood:
[overcommit](https://github.com/sds/overcommit) [Ruby],
[lint-staged](https://github.com/okonet/lint-staged),
[commitlint](https://github.com/conventional-changelog/commitlint),
[devmoji](https://github.com/folke/devmoji),
[git-fancy-message-prefix](https://github.com/negokaz/git-fancy-message-prefix).
