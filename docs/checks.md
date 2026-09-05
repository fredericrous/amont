# The checks

Five git hooks are installed — `pre-commit`, `commit-msg`,
`prepare-commit-msg`, `post-commit`, `pre-push` — and behind them thirty-seven
named checks, plus
any your repository declares in [`amont.conf`](custom-checks.md).

This page is the catalogue. **It is not the answer to "what will run in my
repository"** — for that, ask:

```sh
amont list                       # here, and why not
amont list --stage pre-push      # one trigger
amont list --all                 # the inert ones too, one row each
amont list --json                # the same, machine-readable
```

Most checks are **inert** in most repositories, by design. A check fires only
when the commit touches files it understands *and* the repository carries the
configuration that opts into that tool. A JavaScript repository never invokes
cargo; a repository with no `ruff.toml` never needs ruff.

That is why `amont list` leads with what actually runs and gives the rest a
line:

```text
  12 active here.  22 inert (Go, JavaScript, Kubernetes, Python) — amont list --all
```

Even in a repository amont serves well, about half the checks are inert; in one
built on a stack it does not cover yet, two thirds. Printing all of them
interleaved meant a Terraform shop read thirty rows naming Rust, Go, Python and
JavaScript — which says "this tool is for other people" when the truth is the
opposite: the checks that *were* running are `secrets`, `large-files` and
`merge-conflict`, the ones that prevent incidents in any repository at all.

`--all` prints the condition each inert check is waiting on, which is still the
answer to "why is clippy not running here". **Skipped (`⊘`) and unusable (`✗`)
checks are never collapsed** — somebody silenced the first and broke the second,
and a count is the wrong shape for either.

## Reading the table

- **id** — `<trigger>-<name>`. Any of the three spellings (full id, short name,
  trigger) can address it in `hook.skip` and `amont.severity`; see
  [configuration](configuration.md).
- **fires when** — the scope. `always` means it has no file condition.
- **fixes** — the check can rewrite the file rather than only complain. Those
  rewrites are staged; see [run modes](index-fidelity-and-run-modes.md).
- Linters with a warning class run at **zero warnings**: eslint gets
  `--max-warnings 0`, yamllint `--strict`, pyright `--warnings`, and clippy
  has always run with `-D warnings`. A warning that exits 0 is a list nobody
  is forced to read — a human scrolls past it, an agent reads "passed" and
  moves on — so a finding either blocks or it does not exist. A repository
  that wants the old behaviour downgrades the check:
  `git config amont.severity.lint-js warn`.
- Checks marked **soft** warn and skip when their tool is missing, rather than
  blocking a commit, because CI is the hard gate and not every developer has
  every toolchain installed.

## `--json`, the machine contract

`amont list --json` is what an agent or a script reads instead of parsing the
human table, so its field names are a contract rather than an implementation
detail. Every document it prints declares which contract it is:

```json
{"format": "amont-list-v1", "stage_filter": null, "pushed": false, "checks": [...]}
```

Assert that `format` before reading anything else, the same way this tool
refuses a gate stamp or an attestation whose version it does not know. The
version changes when a field's MEANING changes or a field is removed; adding
a field does not change it, which is why the top level is an object rather
than a bare array.

**Envelope**: `format`, `stage_filter`, `pushed`, `checks`, `commit_style`,
`branch_style`, `bypasses`, `downgrades`, `conventions_apply`.

**Each entry of `checks`**:

| field | what it says |
| --- | --- |
| `id` | `<trigger>-<name>`, the full spelling |
| `short_name` | the name without its trigger |
| `stage` | which trigger runs it |
| `source` | `builtin`, or `declared` for an `amont.conf` check |
| `declared_severity` | what the check ships as |
| `effective_severity` | what it is here, after overrides |
| `severity_overridden` | whether those two differ |
| `severity_source` | `config` or `policy` when overridden, else `null` |
| `fix` | whether it can rewrite the file |
| `status` | `ready`, `inert`, `skipped`, `unavailable` |
| `reason` | why, in the words the text view uses |
| `scope_files` | extensions it fires on (`[]` means always) |
| `scope_opt_in` | files whose presence opts the repository in |
| `command` | the command a declared check runs, else `null` |

A field named here and absent there — or the reverse — is a bug this page's
own test fails on, because a reader who guesses a field name gets `null`
rather than an error, and `null` reads as a perfectly plausible answer.

## `commit-msg`

Validates the summary line and reformats the message. `--no-verify` skips it
(git's rule); no `hook.skip` or severity override names it.

**Validates:** a subject is present and at most 72 characters; it carries a
[conventional type prefix](commit-convention.md); a description follows the
prefix; the description is at most 50 characters. Messages git itself writes
(`Merge …`, `Revert "…"`, `fixup!`/`squash!`/`amend!`) pass through unjudged.

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

## The safety net vs the conventions

With `git config --global amont.conventions declared` — the mode
[`amont enroll`](team-rollout.md) offers for machines that also clone other
people's projects — the checks split in two. Five are the **safety net**
and run in every repository, declared or not: `merge-conflict`,
`large-files`, both `secrets` checks, and `ban-terms` — findings that are
mistakes in any codebase, with near-zero false positives. Everything else,
the `commit-msg`/`prepare-commit-msg` hooks included, is a **convention**
and runs only where the repository commits an `amont.conf`. The default
mode, `everywhere`, keeps the distinction inert.

## `pre-commit`

All twenty-two run **concurrently**, and a panic in one is isolated so the
other twenty-one still report.

| id | fires when | what it does |
|---|---|---|
| `pre-commit-agents-md` | `AGENTS.md`/`CLAUDE.md` carry the amont markers | The generated guidance block is behind the amont that would generate it now — an agent reading it follows last release's instructions. Warns with the `amont agents-md` fix; with `amont.fix true` regenerates and re-stages. Silent without the markers (opt-in), and during merge/rebase/cherry-pick. **Never blocks.** **fixes** |
| `pre-commit-argo-lint` | `.yaml` `.yml` + `kustomization.yaml`/`.yml` | Argo CD app lint. **soft** |
| `pre-commit-ban-terms` | `.js` `.jsx` `.ts` `.tsx` `.vue` `.rs` `.py` | Refuses focused/debug leftovers in staged sources — `describe.only`, `fit(`, `debugger` in JS/TS, `dbg!(` in Rust, `breakpoint()` and `pdb.set_trace()` in Python. Scoped to what this commit touches, and re-checked against staged content with each language's comments and string literals blanked (a term named in prose is discussion, not code — and an f-string interpolation or template substitution is code, not prose). |
| `pre-commit-branch-pattern` | always | Says at the **first commit** what [`pre-push-branch-pattern`](#pre-push) will refuse at push time, with the `git branch -m` fix — while renaming costs nothing. Quiet on a detached head, in a remoteless repository, and on any branch a remote already has. **Never blocks.** |
| `pre-commit-branch-protect` | always | Says at **commit time** that a commit landing on `main`/`master` will be refused by [`pre-push-branch-protect`](#pre-push), with the `git switch -c` fix — while moving it costs one command and nothing is stacked on it. Quiet on a detached head and in a remoteless repository. **Never blocks.** |
| `pre-commit-cargo-fmt` | `.rs` + `Cargo.toml` | `cargo fmt`. **fixes** |
| `pre-commit-clippy` | `.rs` + `Cargo.toml` | `cargo clippy` |
| `pre-commit-go-vet` | `.go` + `go.mod` | `go vet ./...`, per touched module. |
| `pre-commit-gofmt` | `.go` + `go.mod` | `gofmt`, handed exactly the staged files. **fixes** |
| `pre-commit-kube-linter` | `.yaml` `.yml` + `.kube-linter*.yaml`/`.yml` | kube-linter. **soft** |
| `pre-commit-kubeconform` | `.yaml` `.yml` + `kustomization.yaml`/`.yml` | Schema-validates rendered manifests. **soft** |
| `pre-commit-lint-js` | `.js` `.jsx` `.ts` `.tsx` `.vue` + `package.json` | ESLint at zero warnings (`--max-warnings 0`), only in repos that carry an eslint config. |
| `pre-commit-lint-json-yaml` | `.json` `.yaml` `.yml` | Parses staged JSON/YAML so a syntax error never reaches the repo. **soft** |
| `pre-commit-merge-conflict` | always | Refuses staged files still carrying conflict markers. |
| `pre-commit-package-lock` | `package.json` | Keeps `package.json` and its lockfile in step, scoped per directory — one project's lockfile does not satisfy another's in a monorepo, and a `package.json` with no lockfile beside it never demands one. |
| `pre-commit-prettier` | a prettier config is present | Format check. **fixes** |
| `pre-commit-pyright` | `.py` `.pyi` + `pyrightconfig.json`/`.jsonc`/`pyproject.toml` | Type check. |
| `pre-commit-ruff` | `.py` `.pyi` + `ruff.toml`/`.ruff.toml`/`pyproject.toml` | Lint and format. **fixes** |
| `pre-commit-usual-name` | always | Warns the first time you commit under a given name/email, so a misconfigured `user.name` is noticed at commit one rather than commit twenty. **Never blocks.** |
| `pre-commit-hadolint` | `Dockerfile` | Dockerfile lint. Matches that basename **exactly** — `Dockerfile.dev` and `Dockerfile.prod` do not, because scope name tokens are exact basenames. |
| `pre-commit-helm-lint` | `.yaml` `.yml` `.tpl` + `Chart.yaml` | `helm lint`, once per chart directory the commit touched — resolved by walking up to the nearest `Chart.yaml`, not once per file. |
| `pre-commit-shellcheck` | `.sh` `.bash` | Shell lint. No opt-in file: shellcheck's defaults are the reason to run it, unlike yamllint's. A script with a shebang and no extension is not matched. |
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
| `pre-push-pull-rebase` | always | Rebases the branch onto **its own** upstream before pushing (then asks for a second push — the first one's refs predate the rebase), and warns — never acts — when the default branch has moved ahead. Never touches a dirty tree, aborts cleanly on conflict, and `amont.autoRebase false` makes it a pure, networkless advisor. |
| `pre-push-run-tests-js` | `.js` `.jsx` `.ts` `.tsx` `.vue` + `package.json` | Runs each touched JS package's gate: `typecheck`, `test:unit`, `test`, whichever it defines, cheapest first. Skips any of those a `pre-commit` declaration already covers — see below. |
| `pre-push-cargo-test` | `.rs` + `Cargo.toml` | `cargo test`. |
| `pre-push-go-test` | `.go` + `go.mod` | `go test ./...`, per touched module, against the pushed tree. |

`pull-rebase`'s constraints are load-bearing: rebasing onto the *default*
branch instead of the branch's own upstream, or autostashing a dirty tree to
do it, are exactly the ways a pre-push hook loses somebody's work — so it
does neither, ever.

### The large-file guard — `large-files`

Git history never forgets a megabyte: an accidentally committed dataset or
bundle is paid for by every clone forever, even after deletion — deleting
adds a commit, it does not remove the bytes. At `pre-commit`, a staged file
over `amont.largeFileWarn` MB (default 10) gets a named warning — a large
asset can be deliberate, and this is the moment to decide — and one over
`amont.largeFileBlock` MB (default 100, GitHub's own refusal line) blocks
with the remedy named: git-lfs, or keep it out of history.

### The Python test gate — `pytest`

`cargo-test`'s contract for the third ecosystem: a repository declaring a
pytest setup (a `pytest.ini` or a `conftest.py` — a bare `pyproject.toml`
is not a promise to test) runs its suite at `pre-push` against the PUSHED
tree, per ref, for pushes that change Python. Missing pytest or an
unanswering git is `Unavailable` — loud, never green.

### The secrets check — `secrets`, at both stages

A staged credential is a ten-second fix: unstage it. A PUSHED credential is
not a history problem, it is an incident — the secret is compromised the
moment it leaves the machine, and the remedy stops being `git commit
--amend` and becomes rotation. So this check exists twice:

- **`pre-commit-secrets`** scans the staged content and blocks — private
  key headers, cloud access key ids, the well-known API token prefixes
  (GitHub, Slack, Google, Stripe live keys, npm, OpenAI/Anthropic).
- **`pre-push-secrets`** scans every line every pushed commit ADDS —
  including commits made with `--no-verify`, from other tools, or three
  commits ago, and including a secret added and removed *within* the pushed
  range, because the history being published still carries it. The push is
  the last moment a secret is recoverable at all.

Detection is curated token shapes, not entropy — entropy heuristics are
where secret scanners get noisy, and a noisy blocker is a blocker people
learn to delete. A legitimate fixture opts out per line with the pragma
`amont:allow-secret` on the same line: visible in review, greppable, and
narrower than skipping the whole check. Binary files and files over 2 MB
are skipped.

Findings are **redacted**: the report names the kind and the place
(`a private key at config/deploy.pem:1`), never the matched text — a hook
that echoes a secret into scrollback and CI logs has widened the leak it
exists to prevent.

### The dependency audits — `audit-rust`, `audit-js`, `audit-python`, `audit-go`

At `pre-push`, one vulnerability audit per ecosystem the repository uses:
`cargo audit` (opted in by a `Cargo.lock`), `npm audit` (`package-lock.json`),
`pip-audit -r requirements.txt` (`requirements.txt`), and `govulncheck ./...`
(`go.sum`). No lockfile, no check — an audit without a resolved tree audits
a guess.

The severity is the push's, not the finding's:

- **a branch push** with known vulnerabilities gets a named warning — the
  advisory is information, tomorrow's retry is free, and it tells you now
  that it *will* block a release;
- **a push carrying a `v*` tag** (a `v` followed by a digit — `v1.2.3`,
  `v2`; a tag merely starting with the letter v does not count) is a release
  leaving the building, and known vulnerabilities refuse it, with the tool's
  full report reprinted;
- **warning-class advisories** (unmaintained, unsound) are named and never
  block, anywhere — a gate nothing can pass is a gate people learn to
  delete;
- **a tool that is missing or cannot reach its advisory database** says so
  loudly and never blocks: a hook may be offline, and a push gate that fails
  on a captive portal teaches `--no-verify`. If your releases must not ship
  unchecked, enforce that in CI, where the network is never in question —
  this repository's own release workflow does exactly that.

The tools' output decides, never the exit code alone: every one of these
tools conflates "found vulnerabilities" with "could not fetch the database"
in its exit status, and those mean opposite things.

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

**And it is not npm-specific.** A gate is a NAME declared at both stages —
the vocabulary is yours, not package.json's. Declare the same name at
`pre-commit` (severity `block`) and at `pre-push`, and the commit-time side
earns per-commit stamps the push-time side defers to:

```text
# stage       name   scope   severity  command
pre-commit    test   *.rs    block     cargo test
pre-push      test   *.rs    block     cargo test
```

**A BUILT-IN gate needs only the commit-time half.** amont already owns the
push side of `cargo-test`, `pytest`, `go-test` and `run-tests-js`, so declare
the commit-time twin under the built-in's own short name and stop:

```text
# stage       name        scope   severity  command
pre-commit    cargo-test  *.rs    block     cargo test
```

`pre-push-cargo-test` then defers to its stamps — no second declaration, no
`hook.skip`, and no repeating a command amont already knows how to run. The
name must be the SHORT one (`cargo-test`, not `pre-push-cargo-test`): it is
matched against what you wrote here, and a full id matches nothing.

This is worth reaching for when a suite is fast enough to run on every
commit. When it is not — a four-minute suite is not a per-commit cost anyone
accepts — leave it at push time and **rehearse the push** instead; see the
next section. The window that matters is the same either way: git opens its
connection to the remote *before* calling `pre-push` and holds it idle until
the gate finishes, and a remote may close it first (ssh keepalive does not
prevent that).

### Rehearsing the push gate

A push-time gate that passes stamps the tips it vouched for — the same
`refs/notes/amont-gate` record the commit-time gates use, keyed by tree, so
it survives a reword or a rebase that keeps the content. The next push of
that content skips the gate and says so:

```text
✓ pre-push-run-tests-js passed on this exact tree earlier — not repeating it here
```

Two things fall out of one record:

- **A retry after a dropped connection is instant.** The gate passed, the
  remote closed the idle session while it ran, the push died; `git push`
  again sends the same tips, finds their stamps, and is on the wire in
  seconds.
- **The suite can run before git connects at all.** `amont run pre-push`
  drives the same dispatcher with no push in flight — on a branch that has
  never been pushed it measures against `origin/HEAD`, `origin/main` or
  `origin/master` — and stamps `HEAD` when every block gate passes. Then
  `git push` holds its connection open for the seconds the transport takes,
  not the minutes the suite does. An agent that runs `amont run pre-push`
  before every `git push` never meets the idle timeout; `amont-agent`'s
  `push-preflight` rule says so when a push is about to run without one.

Only scoped gates — test suites, whose verdict is a function of the tree —
are stamped or skipped; `branch-protect`, `secrets` and the other unscoped
checks ask questions about the *push* and always run. A stamp is written
only for content the suite actually tested:

- with `amont.testPushedTree`, that is the tip itself — **unless the
  snapshot could not be made.** A `worktree add` that fails, or an
  `amont.snapshotPrepare` that exits non-zero, falls back to the working
  tree and says so; that tip is then stamped by nothing, because the suite
  that passed never saw its content;
- in the default working-tree mode it is the tip only when `HEAD` *is* the
  tip and the tree was clean **when the gate started** — nothing modified
  and nothing untracked. An untracked file is not in the tree the stamp
  would vouch for, but it was there while the suite ran, and a new test
  file or a local `.env` is exactly the sort of thing that changes a
  suite's answer. Two things deliberately do not count: ignored files
  (`target/`, `node_modules/`), and anything the gate itself writes — the
  state is captured before any check runs, so a suite that leaves a log or
  a coverage directory does not disqualify its own stamp;
- inside a rehearsal the checkout *is* the commit, because git made it, so
  whatever `amont.snapshotPrepare` added to make it runnable is not a
  reason to distrust it.

`git config amont.pushStamps false` turns both the writing and the
honouring off.

### Rehearsing in the background

`amont run pre-push` is still a wait somebody has to remember to start.
`amont rehearse` starts it for you and gets out of the way:

```text
$ git commit -m "feat: the thing"
rehearsing the push gate in the background (`amont rehearse --status`)
$ git push
✓ pre-push-run-tests-js passed on this exact tree earlier — not repeating it here
```

With `git config amont.rehearseOnCommit true`, `post-commit` spawns a
detached worker — its own process group, output on
`$GIT_DIR/amont-rehearsal.log`, nothing for git to wait for. The worker
checks out `HEAD` into a throwaway worktree (the `amont.testPushedTree`
machinery) and runs the ordinary pre-push dispatcher there, with the branch
and its upstream as the ref line. The snapshot is what makes this safe while
you keep editing: the suite reads a tree nobody is touching, and the stamp
it earns is for exactly that tree. Only the test gates run — `branch-protect`,
`secrets` and the auto-rebase ask about a push that is not happening, and
run when it is. A checkout that needs a step before a suite can start (a
pnpm monorepo has no `node_modules` in a fresh worktree) names it in
`amont.snapshotPrepare`.

A push that arrives mid-rehearsal waits for it rather than starting the
suite over — but not forever. `amont.rehearsalWait` (default 300s, `0` for
no limit) bounds that wait, because it happens *after* git has opened its
connection to the remote: holding it open for a wedged worker is the same
failure the rehearsal exists to prevent. When the budget expires the gate
runs here, and the rehearsal is left alone to finish and stamp the tree for
next time.

The stamp is the whole hand-off; there is no second record to keep in step.
What the state file beside the log adds is *whether someone is still working
on it*, so a push can choose:

- **stamped** — the push skips the suite, as above;
- **still running** — the push waits for the verdict rather than starting
  over (`⚠ a background rehearsal of this tree started 2m ago is still
  running — waiting for it rather than starting the suite over`): less
  remaining work than a fresh run, and no extra CPU;
- **failed**, or died without a verdict — said, with the log's path, and the
  gate runs again in the terminal you are looking at. A failed rehearsal is
  never honoured.

Latest wins. A worker that finds another one running on a *different* tree
kills it — the whole process group, suite included — and removes its
snapshot; a rebase replaying ten commits does not queue ten suites. One on
the same tree is left alone. Nothing starts during a rebase, merge or
cherry-pick: the commit being made is not the one that will be pushed.

`amont rehearse` by hand starts one for `HEAD` in the background;
`--wait` follows the running one, or runs it in the foreground if none is
(the shape an agent wants before `git push`); `--status` says which tree the
last one was for and how it ended; `--stop` cancels. Every path that goes
wrong ends in the gate running at push time exactly as it would have
without any of this: the background run can only *remove* work from the
push, never let it skip work nobody did.

The detached worker is Unix-only for now. On Windows a child process
inherits its parent's pipes, so a worker started from a hook whose output
is captured would hold the commit until the suite ended; there `amont
rehearse` says so, and `--wait` runs the rehearsal in the foreground.

A push whose commits all carry the `test` stamp skips the pre-push line with
the same `✓ test gated at commit instead` message; a `--no-verify` commit
brings it back with the same warning; and the dodge lands in the bypass
ledger under its own name. `cargo test`, `pytest`, `go test` — the contract
is identical because the machinery never looks at the command, only at the
name, the severity, the scope, and the stamps.

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

**The missing stamp is also counted.** Every commit that a gate declaration
covered but that carries no record its gate ran appends one line per dodged
script to `$(git rev-parse --git-common-dir)/amont-bypasses` — a plain local
file, never a ref, never pushed, never sent anywhere; the no-telemetry
promise applies in full. `--no-verify` is only the commonest cause: a
blocked attempt retried with it, or a gate whose tool was missing, count the
same way, which is why `amont list` labels the tally **unverified commits**
rather than guessing at intent. A rising count is the first symptom of a
gate people have started routing around — a slow check, a flaky one — and
until it was counted, the hooks detected that signal on every commit and
threw it away. `amont uninstall` deletes the file;
`git config amont.recordBypasses false` stops the counting.

### Trying it before you impose it

The question a team lead asks before adopting anything that can block a commit
is *will this annoy my team into switching it off?* One line answers it:

```sh
git config amont.severity.pre-commit warn
```

Every blocking check still runs and still reports; nothing stops a commit. Work
normally for a fortnight, then read what happened:

```text
problems that did not block
  pre-commit-usual-name    61   last 2h ago
  pre-commit-ban-terms      4   last 1d ago
  pre-commit-secrets        1   last 3d ago
  66 events over 41 commits, since 12d ago
  66 of them would have blocked — set amont.severity.<check> to keep one
  advisory when you go back to block
```

That is a configuration worksheet: turn off the one or two checks your team
disagrees with **before** the rollout, rather than discovering them one angry
message at a time. **Events and commits are different facts** — forty commits
each tripping a check once is a check nobody agrees with, while one commit
tripping it forty times is one person losing an afternoon — so both are shown.
A check that ships as `warn` is marked `(advisory)` and excluded from the
would-have-blocked count: it was never going to block, and counting it would
inflate the only number being read.

The events land in `$(git rev-parse --git-common-dir)/amont-downgrades`, on
the same terms as the bypass ledger above: a plain local file, never a ref,
never pushed, never sent anywhere. It follows that this **cannot aggregate
across a team** — each developer's ledger is their own machine's, and a lead
runs the trial on their own checkout or asks people to paste. `amont uninstall`
deletes the file; `git config amont.recordDowngrades false` stops the counting.

A rehearsal (`amont run`, `amont run --all-files`) deliberately records
nothing: it is not a commit, and letting it count would make the number mean
something other than what it says. Neither does a check that actually blocked —
there is nothing to report about a problem that did its job.

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

### Telling CI about it

The gate stamps above stay local by design — an unsigned note is only as
honest as whoever can write the ref. Their signed successor can travel:
with `git config amont.attest true`, a push whose block gates all passed
leaves an `ssh-keygen`-signed note on each pushed tip in
`refs/notes/amont-attest` and sends that ref along, and CI may then skip
the test steps the attestation names — but only for exactly the attested
tree, and only after the signature verifies. The whole contract, including
what CI must check and why every failure mode falls back to running the
tests, lives in [the CI backstop](ci.md#skipping-what-pre-push-already-proved).

## Adding one

A check is a module plus one registry entry in
`crates/amont-runtime/src/registry.rs`; see
[hook architecture](hook-architecture.md) and
[CONTRIBUTING.md](https://github.com/fredericrous/amont/blob/main/CONTRIBUTING.md).

If the check belongs to your repository rather than to everybody's, declare it
in [`amont.conf`](custom-checks.md) instead — no fork required.
