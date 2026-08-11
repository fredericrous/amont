# The checks

Five git hooks are installed — `pre-commit`, `commit-msg`,
`prepare-commit-msg`, `post-commit`, `pre-push` — and behind them twenty-one
named checks, plus
any your repository declares in [`amont.conf`](custom-checks.md).

This page is the catalogue. **It is not the answer to "what will run in my
repository"** — for that, ask:

```sh
amont list                       # here, and why not
amont list --stage pre-push      # one trigger
amont list --json                # the same, machine-readable
```

Most checks are **inert** in most repositories, by design. A check fires only
when the commit touches files it understands *and* the repository carries the
configuration that opts into that tool. A JavaScript repository never invokes
cargo; a repository with no `ruff.toml` never needs ruff. `amont list`
prints the condition each inert check is waiting on.

## Reading the table

- **id** — `<trigger>-<name>`. Any of the three spellings (full id, short name,
  trigger) can address it in `hook.skip` and `amont.severity`; see
  [configuration](configuration.md).
- **fires when** — the scope. `always` means it has no file condition.
- **fixes** — the check can rewrite the file rather than only complain. Those
  rewrites are staged; see [run modes](index-fidelity-and-run-modes.md).
- Checks marked **soft** warn and skip when their tool is missing, rather than
  blocking a commit, because CI is the hard gate and not every developer has
  every toolchain installed.

## `commit-msg`

Validates the summary line and reformats the message. Cannot be bypassed with
`--no-verify`.

**Validates:** a subject is present and at most 72 characters; it carries a
[conventional type prefix](commit-convention.md); a description follows the
prefix; the description is at most 50 characters.

**Formats:** hard-wraps the body at 72 columns, groups the trailing footers
with one blank line before them, and places the type's gitmoji wherever you
asked for it — nowhere, by default.

Every number above and the gitmoji placement are `amont.commit.*` settings;
`amont setup` walks them. See
[if the defaults do not fit](commit-convention.md#if-the-defaults-do-not-fit).

## `prepare-commit-msg`

Appends the issue id found in the branch name to the footer: JIRA first
(`ABC-1234`), else a bare Kanbanize id (`1234`).

Only for a commit you are authoring. `-m`, `-t`, a merge, a squash and
`--amend` all pass a source in `$2` and are left alone.

## `pre-commit`

All fifteen run **concurrently**, and a panic in one is isolated so the other
fourteen still report.

| id | fires when | what it does |
|---|---|---|
| `pre-commit-argo-lint` | `.yaml` `.yml` + `kustomization.yaml`/`.yml` | Argo CD app lint. **soft** |
| `pre-commit-ban-terms` | `.js` `.jsx` `.ts` `.tsx` `.vue` | Refuses focused/debug leftovers (`describe.only`, `debugger`, …) in staged JS/TS. Scoped to what this commit touches, and re-checked against staged content with comments and string literals blanked. |
| `pre-commit-branch-pattern` | always | Says at the **first commit** what [`pre-push-branch-pattern`](#pre-push) will refuse at push time, with the `git branch -m` fix — while renaming costs nothing. Quiet on a detached head, in a remoteless repository, and on any branch a remote already has. **Never blocks.** |
| `pre-commit-cargo-fmt` | `.rs` + `Cargo.toml` | `cargo fmt`. **fixes** |
| `pre-commit-clippy` | `.rs` + `Cargo.toml` | `cargo clippy` |
| `pre-commit-kube-linter` | `.yaml` `.yml` + `.kube-linter*.yaml`/`.yml` | kube-linter. **soft** |
| `pre-commit-kubeconform` | `.yaml` `.yml` + `kustomization.yaml`/`.yml` | Schema-validates rendered manifests. **soft** |
| `pre-commit-lint-js` | `.js` `.jsx` `.ts` `.tsx` `.vue` + `package.json` | ESLint, only in repos that carry an eslint config. |
| `pre-commit-lint-json-yaml` | `.json` `.yaml` `.yml` | Parses staged JSON/YAML so a syntax error never reaches the repo. **soft** |
| `pre-commit-merge-conflict` | always | Refuses staged files still carrying conflict markers. |
| `pre-commit-package-lock` | `package.json` | Keeps `package.json` and its lockfile in step, scoped per directory — one project's lockfile does not satisfy another's in a monorepo, and a `package.json` with no lockfile beside it never demands one. |
| `pre-commit-prettier` | a prettier config is present | Format check. **fixes** |
| `pre-commit-pyright` | `.py` `.pyi` + `pyrightconfig.json`/`.jsonc`/`pyproject.toml` | Type check. |
| `pre-commit-ruff` | `.py` `.pyi` + `ruff.toml`/`.ruff.toml`/`pyproject.toml` | Lint and format. **fixes** |
| `pre-commit-usual-name` | always | Warns the first time you commit under a given name/email, so a misconfigured `user.name` is noticed at commit one rather than commit twenty. **Never blocks.** |
| `pre-commit-yamllint` | `.yaml` `.yml` + `.yamllint`/`.yamllint.yaml`/`.yml` | Strict YAML lint, where a repo has opted in. |

Both Python checks prefer the repository's **pinned** tool over an ambient
latest, in this order: `uv run --no-sync` (the lockfile-pinned one CI runs) →
the worktree's `.venv` → the *main* worktree's `.venv` (a linked worktree has
none of its own) → `PATH` → `uvx`, which is unpinned latest and therefore warns,
because it flags issues the CI-pinned version does not.

### Checks that are paused mid-operation

Most content checks do not run during a merge, rebase, cherry-pick or revert:
half the tree is somebody else's work and you cannot fix it from inside the
operation anyway.

`merge-conflict` and `ban-terms` are deliberately **not** paused. Those are
exactly the checks you want during a resolution commit — leaving a conflict
marker in the commit that *resolves* a merge is the bug, and importing a banned
term from the other branch is the other one.

## `pre-push`

These run **in sequence**, cheapest and most decisive first: refuse a forbidden
push before validating a name, and validate everything structural before paying
for a test suite.

| id | fires when | what it does |
|---|---|---|
| `pre-push-branch-protect` | always | Refuses a direct push to `main` or `master`. |
| `pre-push-branch-pattern` | always | Requires `prefix/branch-name` (e.g. `feat/3002-image-crop`), unless the branch already exists on the remote. |
| `pre-push-pull-rebase` | always | Rebases the branch onto **its own** upstream before pushing, and warns — never acts — when the default branch has moved ahead. Never touches a dirty tree, and aborts cleanly on conflict rather than leaving a half-rebased state. |
| `pre-push-run-tests-js` | `.js` `.jsx` `.ts` `.tsx` `.vue` + `package.json` | Runs each touched JS package's gate: `typecheck`, `test:unit`, `test`, whichever it defines, cheapest first. Skips any of those a `pre-commit` declaration already covers — see below. |
| `pre-push-cargo-test` | `.rs` + `Cargo.toml` | `cargo test`. |

`pull-rebase`'s constraints are load-bearing: rebasing onto the *default*
branch instead of the branch's own upstream, or autostashing a dirty tree to
do it, are exactly the ways a pre-push hook loses somebody's work — so it
does neither, ever.

### Moving a gate entry earlier

`typecheck` sits in the push gate because nothing checks it sooner. For some
repositories that is too late — a type error is cheapest to hear about at the
commit that caused it, not an hour later when you go to push.

Move it by declaring it in [`amont.conf`](custom-checks.md) **under the name of
the script**:

```text
# stage       name       scope       severity  command
pre-commit    typecheck  *.ts,*.tsx  block     npm run typecheck
```

`pre-push-run-tests-js` then drops `typecheck` from its gate and says so:

```text
✓ typecheck gated at commit instead — not repeating it here
```

This is the argument that already keeps `lint` out of the gate — pre-commit
lints staged files, so repeating it on push costs time and catches nothing —
applied to whatever a repository decides to move. It is not `typecheck`-specific:
`test:unit` and `test` work the same way.

**Only a declaration that would actually run, and actually cover the push,
counts.** All of these leave the push gate exactly as it was:

- an **untrusted** manifest, an **unusable** line, a `hook.skip`, or a
  declaration on the `pre-push` stage — a declaration that never runs is not a
  check;
- an effective severity of **`warn`**, whether declared or arrived at through
  an `amont.severity.*` override — a check that lets a failing commit through
  cannot stand in for one that blocks a failing push;
- a push whose JS changes fall even partly **outside the declaration's
  scope** — `*.ts,*.tsx` above says nothing about a `.js` change, so a push
  carrying one runs the full gate for that ref;
- every package but the **repo root** — the declared command runs at the root,
  so a monorepo sub-package's gate is never skipped on its account;
- a pushed commit with no **stamp** — see below. A declaration that qualifies
  on all the points above is still only a promise; the stamp is the proof it
  was kept.

The failure being avoided is the one worth stating plainly: a repository that
declared `pre-commit typecheck`, never trusted it, and had types checked at
*neither* end while both ends reported green.

**The stamp is how the push gate trusts the event rather than the paper.**
When the moved check runs at commit time, the `post-commit` hook records that
fact against the commit — a local notes ref, `refs/notes/amont-gate`, never
pushed, invisible to `git log`, garbage-collected with the commits it
annotates, and removed by `amont uninstall`. At push, the gate skips a script
only when every pushed commit inside the declaration's scope carries its
stamp. A commit created with `git commit --no-verify`, made by a client that
runs no hooks, made on a machine without amont, or whose hash a rebase or
amend rewrote has no stamp — and the push says so and runs the script itself:

```text
⚠ typecheck is declared at commit time, but 1 pushed commit carries no record of it — running it here
```

Merge commits and cherry-picks never run `post-commit`, so they re-run the
gate the same way. Every failure mode points in one direction: a missing
stamp can only cost a redundant run, never skip a check that did not happen.
The residual trade of moving a gate entry earlier is therefore latency, not
safety — an unchecked commit makes the push slower, not greener.

### What a push actually tests

By default `pre-push` runs your suite against the **working tree**, and says
so. That is fast and usually what you want, but it is not what you are pushing:
an uncommitted fix makes a broken commit look green.

```sh
git config amont.testPushedTree true
```

turns on the accurate answer — the suite runs in a throwaway checkout of the
commits being pushed, and your tree is not touched. It costs a second checkout
and a build that cannot reuse your `target/` cache, which is why it is opt-in.

## Adding one

A check is a module plus one registry entry in
`crates/amont-runtime/src/registry.rs`; see
[hook architecture](hook-architecture.md) and
[CONTRIBUTING.md](https://github.com/fredericrous/amont/blob/main/CONTRIBUTING.md).

If the check belongs to your repository rather than to everybody's, declare it
in [`amont.conf`](custom-checks.md) instead — no fork required.
