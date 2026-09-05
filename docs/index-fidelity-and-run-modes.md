# Index fidelity, fixes, and run modes

Status: **mostly shipped**. This began as a specification and said so — "Nothing
here is built" — and stayed saying it for six sections after five of them landed.
README links here twice as the authority on trust and on run modes, so a reader
was being sent to a design document for an answer about behaviour they already
had.

| § | what | status | where it lives |
|---|---|---|---|
| 0 | activation, `uninstall`, bulk install | **shipped** | `install.rs`, `amont-fleet install/uninstall` |
| 0b | manifest trust | **shipped** | `trust.rs`, `amont trust [--show]` |
| 1 | index fidelity (staged-only) | **shipped** | `staged_only.rs`, `dispatch::pre_commit` |
| 2 | `stage_fixed` | **shipped** as `Fix::Rewrite` + `Outcome::Fixed` | `check.rs`, `hooks/common.rs::restage` |
| 3 | `not_during` git-state conditions | **shipped** | `check.rs::GitState`, `registry.rs::MID_OPERATION` |
| 4 | `amont run [--all-files]` | **shipped** | `main.rs`, `dispatch::run_named` |
| 4b | `amont check <paths…>` | **shipped** | `content.rs`, `finding.rs` |
| 5 | shebang detection | **not built** | — |

§5 is the only one still a proposal. Everything below the numbered sections —
*What we are not taking* and *What this does not solve* — is unchanged and still
current: those are refusals and known gaps, not a backlog.

The prose in each section is kept in its original tense, because the argument
for a thing is worth more than a description of it, and rewriting the reasoning
into the past tense would lose why each decision went the way it did. Where the
implementation ended up somewhere other than where the design pointed, that is
called out in the section itself rather than quietly edited away.

Read against `pre-commit`, `lefthook` and `husky`, then re-read as somebody who
would have to get this through an adoption review. Four of their ideas are worth
taking, one is worth refusing on the record, the first is not a missing feature
at all — it is a correctness gap we have been describing as a trade-off — and
before any of them there is a trust problem that has nothing to do with the
three tools and everything to do with a decision we already shipped.

The last two sections are the ones an adoption review reads first: *What we are
not taking* and *What this does not solve*. The second is a list of honest
noes — pinned tool versions, CI enforcement, DCO — and it is deliberately not a
backlog.

---

## 0. A cloned repository can run its own commands

> **Shipped.** Per-repository activation is the default; `amont install`
> / `uninstall` and `amont-fleet install` / `uninstall` are all real
> verbs. `install.rs` carries the routine, and `crates/amont/tests/
> install.rs` and `crates/amont-fleet/tests/uninstall.rs` carry the
> guards. `uninstall` also removes the shims from the template directory
> and says so loudly if `init.templateDir` is still set.

`amont.conf` is committed, which is the point: a team shares a check by
committing it. The consequence had not been written down.

`git clone` seeds `.git/hooks` from `init.templateDir`, so a fresh clone arrives
with our shims already installed. The manifest is then read from that repository
and its commands are executed. No prompt, no trust decision:

```
$ git clone hostile victim && cd victim
  hooks present after clone: commit-msg post-commit pre-commit pre-push prepare-commit-msg
$ git commit -m "feat: an innocent commit"
  >>> arbitrary code from the cloned repo <<<
```

Cloning a repository and committing to it is not an act of trust that anyone
performs deliberately. Reviewing a diff before running it is; nothing here asks
for that.

`pre-commit` has the same property. That is not a defence — it is a decade-old
known quantity with an ecosystem that has argued about it in public, and ours is
undocumented. `docs/custom-checks.md` presents externals purely as a
convenience. §2 of this document then proposes `stage_fixed`, which upgrades the
primitive from *run a command* to *run a command that rewrites my files and
stages the result*, and that must not ship into an untrusted manifest.

### Where the exposure actually comes from

Not from the manifest, and not from the shims. From one line in our own README:

```sh
git config --global init.templatedir ~/.config/git/git-templates/templates
```

We never set that key in code — `amont install` writes files and touches no
config. The README asks the user to make *every future clone on the machine*
managed, and that ambient grant is what turns a committed manifest into a
drive-by.

It also quietly undermines the fleet's own model. `managed` vs `unmanaged` is
supposed to be a decision the dashboard reports; with `init.templateDir` set,
everything cloned since is managed and "unmanaged" means "cloned before I
configured this". A category that records the date you ran a `git config`
command is not a category.

### The design: activation is the boundary, and `templateDir` opts out of it

**Two modes, both supported, and the difference is what you granted.**

*Per repository* is the default. Hooks run where somebody put them and nowhere
else:

```
amont install              # this repo
amont uninstall            # this repo — remove shims, leave the binary
amont-fleet install        # every managed-eligible repo under a root
amont-fleet uninstall
```

`amont install` already does the per-repo half. `amont-fleet` has `scan`,
`fix` and `tui`, with `--apply` behind `fix` — bulk activation exists but is
named after repair rather than intent, which is why nobody reaches for it when
they mean "set this up".

A clone is then inert until asked. The drive-by case is gone, not mitigated:
there is no hook to run.

*Everywhere* is `init.templateDir`, and it stays supported as a **deliberate
opt-in** rather than being removed. Git copies the template into `.git/hooks` on
every `init` and every clone, so hooks are never forgotten and the fleet never
shows an uncovered repository. That is a real benefit and people who want it
should be able to say so.

What matters is that setting it is a **standing grant, made once, for every
repository you will ever clone** — and it therefore opts out of activation being
the trust boundary. It cannot be otherwise: the whole point of the key is that
nobody is asked again.

So the honest statement is a conditional, and the README now carries it:

| mode | who decides a repo runs hooks | what closes the drive-by case |
|---|---|---|
| per repository | you, per repository | activation itself |
| `init.templateDir` | you, once, for all future clones | **only manifest trust** |

That is not an argument against the key. It is an argument that §0b is not a
second layer of defence for the people most likely to set it — it is the only
one — which raises its priority rather than lowering it.

### It is necessary and not sufficient

Worth being precise, because it is tempting to stop here.

Explicit installation removes the case where you clone something to read it.
It does not remove the case that matters most in open source: you clone a
stranger's repository **because you intend to contribute**, you run
`amont install` because you want your own checks while you work, and their
`amont.conf` runs on your first commit.

`amont install` means *I want my hooks here*. It does not mean *I have read
this repository's committed commands and accept them*. Those are two different
grants and only one of them was made.

### So: one prompt, at the moment of the deliberate act

> **Shipped** — this is §0b. `crates/amont-runtime/src/trust.rs`, surfaced as
> `amont trust` and `amont trust --show`, with `amont install` offering
> it interactively. The record is keyed on the manifest's CONTENT (via
> `git hash-object`, because the binary links no crates and `std`'s only hash is
> a fixed-key SipHash a crafted manifest could collide), so a `git pull` that
> adds a command does not inherit consent given to the file before it.
> `crates/amont/tests/trust_display.rs`.

Which is where activation-as-the-boundary improves on the `direnv` design rather
than replacing it. direnv must prompt lazily, on `cd`, because there is no
install step to hang the question from. We have one:

```
$ amont install
  ✓ installed /Users/me/.local/bin/amont
  ✓ baked 4 shims into .git/hooks

  ⚠ amont.conf declares 2 checks that would run on your commits:
      shellcheck  pre-commit  *.sh  block  scripts/lint-shell.sh
      smoke       pre-push    *     warn   make smoke
    Trust them? [y/N]
```

One question, asked once, at a moment the user is already thinking about this
repository. Declining still installs the built-ins — the manifest simply stays
untrusted, and reports as `Unavailable` with a reason rather than being silently
skipped:

```
⚠ amont.conf declares 2 checks and is not trusted here — `amont trust`
⚠ 2 check(s) could not run: shellcheck, smoke
```

Trust records a hash of the file in git config, so a later edit re-arms it —
`manifest changed since you trusted it` — and a `git pull` that adds a command
cannot inherit the consent given to the file before it.

### What none of this fixes

A built-in check still runs tool binaries the repository can influence:
`resolve_tool` prefers `<root>/node_modules/.bin/<tool>`, so a hostile
`node_modules` is executed by prettier or eslint with no manifest involved and
no trust prompt to decline. That is inherent to running a repository's own
toolchain — the same exposure `npm install` already carries — but it means both
halves above are a floor, not a ceiling, and the README should say so rather
than implying the manifest was the only door.

### The cost of the per-repository mode, and why the fleet absorbs it

Not setting `init.templateDir` means a fresh clone has **no checks at all** until
somebody installs them. For a codebase whose whole argument is *do not look
protected when you are not*, that deserves stating rather than burying: the
failure mode moves from "a hostile repo ran code" to "my repo was never
covered", and the second is quieter.

It is also the failure the fleet dashboard already exists to catch — and this is
what makes the `unmanaged` column earn its place. With `init.templateDir` set it
is close to noise, because everything cloned since is managed and "unmanaged"
records the date you ran a `git config` command. Without it, the column is the
point of the tool: which of your ninety-six repositories are not covered, with
`amont-fleet install` as the fix.

Both modes are legitimate. They trade a quiet failure for a loud grant, and the
tool should let you pick which one you would rather explain.

### `uninstall`, which is missing regardless

We can disable a check (`hook.skip`), downgrade one (`amont.severity`) and
install everything. There is no supported way to take it off — a user who wants
out deletes five files by hand and leaves a stale binary in `~/.local/bin`.

`uninstall` at both levels, and it must be honest about what it removes: shims
yes, the binary only when asked, `hook.skip`/severity config never, since those
are the user's statements about their own repository and not our artefacts.

### Ordering

This lands **before** `stage_fixed`, and arguably before anything else here. It
is the only item on the list that is a security property rather than a
correctness or ergonomics one.

The two halves can ship separately and in this order: activation and
`uninstall` first, which is a README change plus two verbs and closes the
drive-by case on its own; then the trust prompt, which needs the install flow to
hang from and is much smaller once it exists.

---

## 1. We name the staged files and then read the unstaged ones

> **Shipped**, as `crates/amont-runtime/src/staged_only.rs`, wrapped
> around the whole `pre-commit` check stage in `dispatch::pre_commit`.
> The mechanism is NOT the one designed below — see *The design* and *The
> danger* for what changed and why. `crates/amont/tests/
> index_fidelity.rs` is its suite, including the Ctrl-C case.

`staged_files()` asks the index for the path list, which is right:

```rust
git::stdout(&["diff", "--diff-filter=d", "--cached", "--name-only"])
```

Those paths are then handed to a tool that opens them **from the working tree**:

```rust
pub fn run(root: &str, argv: &[String], extra: &[String]) -> bool
// …cmd.args(extra).current_dir(root)   ← `extra` is the path list; the tool reads the file
```

So a partially-staged file is judged by content that is not being committed.
`git add -p` half of `a.js`, commit, and prettier reads the whole working-tree
file: it fails on lines you did not stage, or passes on lines you did.

**This is systemic, not local.** Of the fifteen pre-commit checks:

| | checks | how |
|---|---|---|
| reads the **tree** — affected | 11 | prettier, lint-js, lint-json-yaml, yamllint, ruff, pyright, argo-lint, kube-linter, kubeconform, cargo-fmt, clippy |
| reads the **index** — correct | 2 | `merge-conflict` (`git grep --cached`), `ban-terms` (`git show :<file>`) |
| reads no file content | 2 | `package-lock` (path names only), `usual-name` |

**We already have the technique.** `ban_terms` selects candidates with
`git diff --cached` and then reads each one with `git show :<file>` — the index
blob, never the tree. The two checks that get this right are the two that were
ported most carefully, which is a hint about the other eleven rather than a
coincidence.

That suggests a second possible fix, and it is worth saying why it is not the
one to take. `git show :<file>` is enough when a check only needs CONTENT, which
is why it works for `ban-terms`. It is not enough for a tool invoked on a path:
prettier resolves its config by walking up from the file, kubeconform needs the
kustomization directory around it, and ruff needs the file to sit where its
`pyproject.toml` can be found. Feeding those a temp file changes their answer.
The stash puts the right content at the right path, which is the only fix that
serves all eleven.

### pre-push has the same bug, from the other end

`pre-push` has no index at all, so this looks like a pre-commit problem. It is
not. `rust_tools::test` and `run_tests::run` compute the changed file set from
the **pushed refs** — correct, that is what is being pushed — and then run the
suite with `current_dir(dir)`, i.e. **against the working tree**:

```rust
let changed = crate::pushrefs::changed_files(refs);   // what you are pushing
let roots = cargo_roots(&root, changed.iter()…);      // where to run
each_root(&roots, None, &["test", …])                 // runs in the WORKING TREE
```

So the suite can pass on an uncommitted fix, or fail on an uncommitted
experiment, and in neither case has it tested the commits being pushed.

The fix is not the same one. Stashing is wrong here: a push is not a staging
operation, and the honest question is "does the pushed tree pass", which means
running against the pushed commit — a worktree or `git archive` of the tip
rather than the developer's tree. That is more expensive and wants its own
decision, which is why it is named here and scheduled separately rather than
folded into the stash work.

`rust_tools.rs:149` calls it out and then accepts it:

> Note this inspects the WORKING TREE, not the index, so a partially-staged file
> is judged by its unstaged form too. Same trade-off cargo fmt gives everyone;
> scoping it to staged paths would need the edition resolved by hand.

The first sentence is true of all eleven. The second is the mistake:
it is not a trade-off cargo fmt gives everyone, it is one `pre-commit` removes
for everyone. Their wording is worth quoting because it names both failure
directions:

> Running hooks on unstaged changes can lead to both false-positives and
> false-negatives during committing. pre-commit only runs on the staged contents
> of files by temporarily stashing the unstaged changes while running hooks.

### The design

A guard around the whole pre-commit stage, not per check. Per-check stashing is
wrong: twenty checks run concurrently and would fight over one working tree. It
belongs around the whole fan-out.

That much shipped unchanged. **The mechanism did not**, and the difference is the
most important thing on this page, because the design below said `git stash
--keep-index` and the implementation refused it — twice.

```rust
// WHAT WAS SPECIFIED, and is not what runs:
struct StagedOnly { stash: Option<StashRef> }   // `git stash --keep-index`
```

```rust
// WHAT SHIPS — crates/amont-runtime/src/staged_only.rs
pub struct StagedOnly { held: bool }
impl StagedOnly {
    pub fn enter() -> Result<StagedOnly, String>;
}
impl Drop for StagedOnly { /* restore, ALWAYS */ }
```

Saving is the easy half; restoring is the whole problem, and both stash-shaped
mechanisms failed it:

- **`git stash --keep-index`** was tried first. `stash pop` MERGES into a tree
  that already holds the staged content, so it writes conflict markers into the
  author's file. Measured, not predicted.
- **`git diff` + `git apply`** was tried second — deterministic on Unix, and what
  `pre-commit` itself does. But it applies PATCH semantics to text, and Git for
  Windows converts line endings by default. Every restore test failed on Windows
  and passed everywhere else, which is the worst possible shape for the one
  routine here that can lose somebody's work.

So: **byte-exact copies**. Read the file, put it back. No patch to apply, no
newline policy to agree about, and binary files need no special case. It costs a
temporary copy of only the files that have unstaged changes.

The store is `$GIT_DIR/amont-held/`, and its layout is itself the result of an
incident:

```
$GIT_DIR/amont-held/
  index              NUL-delimited, one record per parked path:
                     format tag, then kind + path + (mode | symlink target)
  files/<rel>        the payloads, byte for byte
```

Metadata used to be encoded in the payload FILENAMES —
`<name>.amont-absent`, `<name>.amont-symlink` beside the copies — and a
repository is allowed to contain files with those names. A repo tracking both
`notes` and a modified `notes.amont-absent` had `notes` DELETED from the
working tree on restore, because a suffix strip turned one file's payload into a
statement about another. The symlink form was worse: the repo chose both the
link name and an arbitrary absolute target, so committing in it planted a
symlink pointing anywhere on the machine. Escaping cannot fix it, because
`amont restore` runs in a later process with only the filenames to go on. So
the metadata moved out of band into `index`, and everything repo-controlled
moved under `files/`, where it cannot collide with `index` whatever it is
called. The `index` carries each entry's KIND (modified / absent / symlink) and,
for a modified file, its working-tree FILE MODE — without which an unstaged
`chmod +x deploy.sh` came back non-executable, invisible to every content-based
assertion.

### The danger, stated plainly

**A stash that is taken and not restored loses uncommitted work.** That is a
worse failure than any this repository has had, including the two that
overwrote tracked files, because there is nothing on disk to recover from.

Rules, all of which wanted tests and all of which now have them, in
`crates/amont/tests/index_fidelity.rs`:

- **Nothing unstaged → do nothing**, and silently. The common case must not
  touch the tree, and it is not a degraded run — there is no unstaged content
  for a check to be confused by.
- **Restore in `Drop`**, so a panicking check (which we catch — #64) and an
  early return both restore. `Drop` runs on unwind.
- **`Drop` does not run on a signal, and that is the likely case.** Ctrl-C
  during a slow pre-commit — eslint over a large tree, a cold `cargo fmt` — kills
  the process without unwinding, and the parked work is orphaned. An interrupt
  is the most probable route to losing work, not the least, so `StagedOnly`
  installs a `SIGINT`/`SIGTERM` handler that restores and then dies BY the
  signal. Restoring from a thread of its own rather than from the interrupted
  call stack introduced a race with `enter()`, which `ENTER_LOCK` closes; the
  test that found it is
  `ctrl_c_mid_run_still_restores_and_dies_by_the_signal`. Windows has the
  same net via `SetConsoleCtrlHandler` (Ctrl-C, Ctrl-Break, console
  closed) — the handler already runs on its own thread there, restores
  under the same lock, and then lets the default action terminate.
- **A recovery path for when even that fails**: `amont restore` puts back
  what this tool parked. Belt and braces, because the handler can itself be
  interrupted.
- **Restore failure is fatal and loud**: print the STORE'S PATH, do not swallow
  it, block the commit. Not `git stash list` — nothing here is a stash ref, so
  the work is findable as files on disk under `$GIT_DIR/amont-held/`.
- **Never park when the tree is already mid-operation** — merge, rebase,
  cherry-pick. §3's `GitState` predicate is what answers this, which is why it
  landed first.
- **Conflicted paths abort the stage** rather than being worked around. **This
  is now true, and it was not for a while.** `StagedOnly::enter` tests for
  unmerged paths FIRST and returns an `Err` that `dispatch::pre_commit` turns
  into a printed message and `Verdict::Block`; the conflict test comes before
  the mid-operation test deliberately, because the other order made an ordinary
  conflicted merge take the mid-operation branch and warn instead of aborting.
  It is safe by construction: git itself refuses a commit with unmerged entries,
  so nothing that would have succeeded now fails.

### Reproduced

```
$ git show :x.json      # staged:      {"a": 2}       ← valid
$ cat x.json            # working tree: { THIS IS NOT JSON
$ amont pre-commit
  ✗ Invalid JSON: x.json
  🚨 Error raised by: pre-commit-lint-json-yaml
```

The commit that was about to be made is valid. The hook blocked it anyway. This
is now a test rather than a transcript.

### Decision

`cargo fmt` joins staged-only mode. Its scope is a crate, not a file list, but
that is exactly why the stage-level guard is the right fix: once unstaged
changes are held aside, `cargo fmt --check` sees staged content at the normal crate
paths and still resolves the manifest, edition and rustfmt config the same way
it does today. The misleading comment in `rust_tools.rs` should be deleted when
this lands; the trade-off was an implementation gap, not an inherent cargo
constraint.

*Done.* `rust_tools.rs` now records the correction in place of the claim.

---

### Writes that land while the checks run

Two kinds of process write to the tree during the hold, and they are treated
differently:

- **An editor save** is work. The restore compares each held file against the
  content the checkout put there; a file that changed mid-run is KEPT, and the
  held (pre-commit unstaged) version is parked in `$GIT_DIR/amont-preserved/`
  with a printed pointer. Before this guard existed, the restore overwrote the
  save silently — the one way this module could destroy something.
- **A fixer** (`amont.fix true`) rewrites held files as its job, and telling
  its writes apart from an editor's is not possible from inside the restore.
  With fixing on, the guard stands down and the documented contract holds
  unchanged: the tree returns to your unstaged version, the fix lives in the
  index. `amont.fix` is an explicit opt-in; the guard protects everyone else.

The checkout that starts the hold is also scoped to exactly the held paths
(spelled `:(literal)` — a file named `*.rs` is a name, not a glob), so a
dirty commit costs git a walk of the changed files, not of the whole tree.


## 2. `stage_fixed` — a formatter that fixes should re-stage

> **Shipped**, under different names: the declaration is `Fix::Rewrite` on
> a check in `registry.rs`, the opt-in is `git config amont.fix true`,
> the re-staging is `hooks::common::restage`, and the result is
> `Outcome::Fixed`. Three checks declare it — prettier, ruff and cargo-fmt
> — and all three now genuinely repair; two of them declared the fix for a
> while without having any fixing code, which `amont list --json`
> reported to agents as a capability. `crates/amont/tests/fixing.rs`.

From lefthook's job options: *"automatically add modified files back to git
staging"*.

Three of our checks currently print an instruction and stop:

```
✗ Prettier found unformatted files. Run prettier --write on:
```

You then run the command yourself and commit again. lefthook's users run
`prettier --write` in the hook and get the result staged.

**This depends on §0 and §1, and must not ship before either.** An untrusted
manifest that can rewrite files and stage the result is a worse primitive than
one that can only run a command, so the trust model is a hard precondition, not
an ordering preference.

On §1: Without the stash, "re-stage
what the formatter touched" re-stages unstaged work the author deliberately kept
back. With the stash in place, the tree contains exactly the staged content, so
anything the formatter changed is by definition part of this commit.

### The design

Opt-in per check, declared, not global:

```rust
pub enum Fix {
    /// Reports only. Every check today.
    None,
    /// A command that rewrites files, and whose result should be staged.
    Rewrite { argv: fn(&Ctx) -> Vec<String> },
}
```

Enabled by config, off by default — `git config amont.fix true` — because a
hook that edits your files without being asked is a bigger surprise than one
that complains. Reported as a new `Outcome::Fixed`, which is neither `Passed`
(something happened) nor `Failed` (the commit proceeds).

Candidates: prettier (`--write`), ruff (`format` + `check --fix`), cargo fmt.
Not eslint `--fix`: its fixes are semantic and occasionally wrong.

---

## 3. Declared skip conditions, replacing one ad-hoc guard

> **Shipped.** `check::GitState` with the five states, `Scope::not_during`,
> and `registry::MID_OPERATION` as the shared set applied to the checks
> that need it. `lib.rs::git_states_in_progress` does the detection and
> `dispatch.rs` consults it for BOTH stages, closing the pre-push gap the
> section names. `crates/amont/tests/git_state.rs`.

lefthook:

```yml
pre-commit:
  commands:
    lint:
      skip: [merge, rebase]
```

and `skip: {ref: main}`, and `skip: {run: test "$NO_HOOK" -eq 1}`.

We have exactly one of these, hard-coded, in one dispatcher:

```rust
// dispatch.rs
if cherry_pick_in_progress(ctx.hooks_dir) { return Verdict::Proceed; }
// …and, twelve lines down:
// NB: no CHERRY_PICK_HEAD check here — the zsh pre-push had none either.
```

That comment is an admission: pre-push has no such guard because the shell
version had none, which is history rather than a decision.

### The design

`Scope` already declares *when a check applies to files*. This is the missing
half — when it applies to **repository state**:

```rust
pub struct Scope {
    pub files: &'static [&'static str],
    pub opt_in: &'static [&'static str],
    /// Git operations during which this check does not run.
    pub not_during: &'static [GitState],   // Merge | Rebase | CherryPick | Revert | Bisect
}
```

Detected from the files git writes into `$GIT_DIR`: `MERGE_HEAD`,
`rebase-merge/`/`rebase-apply/`, `CHERRY_PICK_HEAD`, `REVERT_HEAD`,
`BISECT_LOG`. Not `REBASE_HEAD`: git leaves that one behind after
`rebase --continue` finishes, so it says a rebase happened rather than that
one is happening, and reading it paused every push gate forever in a
worktree that had once hit a conflict. `cherry_pick_in_progress` becomes one arm of that, and its hard-won
comment about `parent()` being lexical while `join("..")` is not moves with it.

**Only `not_during`, not lefthook's full set.** `ref:` conditions duplicate
`hook.skip`, which is already per-repo and already visible in the dashboard;
`run:` conditions are a shell escape hatch in a design that has deliberately
refused shells (see `amont.conf`). Taking the useful third is not a failure
to copy the other two.

---

## 4. `amont run [--all-files]`

> **Shipped**, exactly as specified, plus `--hooks-dir`.
> `amont run`, `amont run --all-files`, `amont run <check>`.
> `crates/amont/tests/run_mode.rs`.

`pre-commit run --all-files` runs every hook over the whole repository rather
than the staged set. Two uses, both of which we currently cannot serve:

- **Adopting a check in an existing repo** — you want to know how big the mess
  is before you turn it on, and `git add .` is not an acceptable way to find out.
- **CI parity** — running the same checks in CI over the whole tree.

We have `amont list` (would it run here?) and `amont <check>` (run one,
staged). We have no "run everything, over everything".

`Scope::matches` already answers against an arbitrary path list, so the file
selection is done. The work is:

```
amont run                 # every applicable check, staged files (what a commit does)
amont run --all-files     # …over `git ls-files` instead
amont run <check>         # one check, either way
```

`--all-files` skips the §1 stash: there is no staged/unstaged distinction to
protect when the answer is "all of it".

---

## 4b. `amont check <paths…>` — the read that is not a rehearsal

> **Shipped.** `crates/amont/tests/check_verb.rs`,
> `amont-runtime/src/content.rs`, `amont-runtime/src/finding.rs`.

`run` answers *"is this commit ready?"* — a question about the **index**. It is
entitled to everything in §1 and §2: the staged-only hold, the stash, the
re-staging of fixes, and `restore` as the way back.

An editor asks a different question — *"what is wrong with this buffer?"* — and
the buffer is not staged, may not match HEAD, and via `--stdin-filename` may
never have been written to disk at all. Serving that from `run` would drag
index fidelity into what is a read-only lookup, and an editor asking about a
buffer would inherit a stash. So it is a separate verb with a separate
contract:

```
amont check src/app.js                          # a path
amont check src/*.ts --format json              # several, structured
amont check --stdin-filename src/app.js < buf   # a buffer never saved
```

**It is a read.** No index, no staging, no stash, no writes —
`checking_never_touches_the_index_or_the_worktree` asserts the repository is
byte-identical afterwards, index included.

### Findings, and why positions had nowhere to live

`Outcome` has five variants and no payload — correct for git, which needs
proceed-or-block, and the reason a report could only name the file:

```text
✗ Unwanted terms found
  The following files contains 'debugger' in them:
  - app.js
```

The line was always known. `ban_terms` blanks comments and strings *preserving
length and line count* precisely so offsets stay valid, and `secrets::scan` has
always returned line numbers — there was simply nowhere to put them. `Finding`
is that place, and the hooks improved on the way past:

```text
✗ Unwanted terms found
  app.js:7:3 — 'debugger' is a banned term here
```

`file:line:col: severity: message [check]` is the output format because every
editor's error parser already reads it and every modern terminal makes it
clickable — which is what lets `efm-langserver`, `nvim-lint`, a VS Code
`problemMatcher` or `flycheck` consume amont with **no editor-side code in this
repository**. `--format json` (`amont-check-v1`) is there for anything that
would rather not parse a line.

### What is deliberately not in it

Only the checks that are about a file's **content**: `ban-terms`, `secrets`,
`merge-conflict`, `large-files`. `branch-pattern`, `branch-protect` and
`pull-rebase` are not about files; `package-lock` is about a relationship
between two; and `clippy`, `ruff` and `eslint` already have editor
integrations of their own that are better than anything proxied through here.

What remains is exactly the set `docs/ci.md` says CI deliberately does not
reproduce — the checks only amont has are the ones only amont can surface
early, which is the whole argument for the verb.

Positions are a **reporting** concern and never a decision input: a check
decides pass or fail exactly as it did before, and a finding says where. Where
a position cannot be pinned down — `large-files` is about the file, not a place
in it — `line` is `None` and every renderer degrades to naming the file, which
is what it did for everything until now.

---

## 5. Shebang detection

> **The one section still unbuilt.** `Scope::files` is suffix-only today —
> `matches()` in `check.rs` calls `path.ends_with(ext)` and nothing reads a
> file head. Nothing is waiting on it; it is here because it is the gap
> `amont.conf` inherits from `Scope`, not because it is next.

pre-commit classifies files with `identify`, which reads shebangs, so an
extensionless `scripts/deploy` starting `#!/bin/sh` is a shell file.

Our `Scope.files` is suffix-only, and `amont.conf`'s `*.sh` inherits that —
a repository whose scripts have no extension cannot scope an external check onto
them at all.

Smallest useful version: `Scope.files` accepts a `#!` pattern.

```rust
Scope::files(&[".sh", "#!/bin/sh", "#!/usr/bin/env bash"])
```

Reading file heads costs an open per extensionless staged file, so it happens
only when a scope actually asks for a shebang, and only for files with no
matching suffix.

**Lowest value on this list.** Listed because it is the gap our own manifest
format inherits, not because anything is waiting on it.

---

## What we are not taking, and why

**`core.hooksPath` (husky).** Husky sets one config key and ships no per-repo
hook files. Our entire drift model — `amont-fleet apply`, `BakeState`, the
`SHIMS` column, `recover_baked` — exists because we copy five files into
ninety-six repositories. A global `core.hooksPath` deletes that problem class
outright.

Refused, and the reason matters more than the refusal: `core.hooksPath` is
all-or-nothing per repository. "Managed vs unmanaged", which the fleet view is
built around, becomes unexpressible; a repository with hooks of its own silently
loses them; and a colleague who has never heard of this tool can read
`.git/hooks/pre-commit` and see what runs. That legibility is worth five files.

**And the failure is not hypothetical — we were on the receiving end of it.**
Eleven repositories on the author's machine ran husky, so `core.hooksPath` was
`.husky/_`, so `git rev-parse --git-path hooks` answered `.husky/_` and
`install` cheerfully baked four shims into a directory husky's own `prepare`
regenerates. They were gone by the next `npm install`. Every one of those
repositories reported as merely "drifted" in the fleet view while running no
checks at all, and a direct push to a protected branch went through
unchallenged for as long as it lasted. That is exactly "a repository with hooks
of its own silently loses them", arrived at from the other direction, and it is
now [refused by name](install.md#when-another-tool-already-owns-the-hooks).

None of which the npm packaging contradicts. `amont init` writes the same five
files to the same `.git/hooks`; what a `prepare` script changes is *who types
the command*, not where the hooks live or whether they can be read.

**repo + rev pinning, per-hook language isolation, `autoupdate` (pre-commit).**
These solve distributing hooks to strangers. We compile checks in and distribute
one binary through the fleet — the same problem, already solved differently.
Adopting the mechanism would mean adopting the problem.

**`remotes:` (lefthook).** The fleet's job.

**`piped:`, `priority:` (lefthook).** pre-commit runs concurrently and reports
every failure; pre-push runs serially and stops at the first. Those two shapes
are load-bearing and documented as such. A configurable ordering invites a third
shape nobody has asked for.

---

## What this does not solve

Named because an adoption review asks these first, and an honest "no" is worth
more than silence.

**Tool versions are not pinned.** §1 fixes *which content* is checked and leaves
*which tool* open. `resolve_tool` prefers `<root>/node_modules/.bin/<tool>` and
falls back to `PATH`, so two developers and CI can run three prettier versions
and disagree about the same commit. `pre-commit` solves this with `rev` pinning
per hook repository, which is dismissed above as "distribution to strangers" —
that is its mechanism, not its value. Its value is determinism, and we do not
have that. Fixing content fidelity while leaving tool fidelity open is half a
reproducibility story, and the half we have is the less visible one.

**Nothing here enforces anything.** Hooks are advisory by construction:
`--no-verify` and `hook.skip` are each one command away, deliberately. So the
question "what stops an unformatted commit reaching the default branch" has no
answer in this document. An earlier draft of this paragraph proposed making
`amont run --all-files` CI-grade — exit-code contract, SARIF, JUnit. The
decision went the other way: amont deliberately does not run in CI at all.
CI wants the real tools, called directly, with the platform's own caching
and attribution — see [the CI backstop](ci.md), which ships copyable
workflow templates saying exactly that.

**No DCO / `Signed-off-by` check.** `commit-msg` enforces a gitmoji prefix and
length rules, which are house style. Any project that requires a Developer
Certificate of Origin needs a different check, and today it would have to be an
external — which lands it squarely in §0. It is a good candidate for a built-in
precisely because it is a policy many organisations cannot adopt the tool
without.

**musl is untested.** CI covers ubuntu, macOS and Windows. A glibc-dynamic
binary does not start in the Alpine containers a lot of pipelines use. Probably
a one-line target addition; worth knowing before somebody finds out from a
pipeline rather than from here.

## Order

**All of this shipped, in this order.** It is left as written rather than
converted to a changelog: the argument for each ordering — why trust had to
precede `stage_fixed`, why `not_during` had to precede index fidelity — is the
part worth keeping, and it reads as advice only in the future tense.

**PR 0a — activation and `uninstall` (§0).** Add `uninstall` at both levels and
name bulk activation `amont-fleet install` rather than hiding it behind
`fix --apply`. The README presents per-repository activation as the default and
`init.templateDir` as a stated opt-in with its consequence spelled out. Closes
the drive-by case for the default mode and makes the fleet's `unmanaged` column
mean something for anyone in it.

**PR 0b — the trust prompt (§0).** Small once 0a exists, because it hangs off
the install flow. Must precede `stage_fixed`, which cannot ship into an
untrusted manifest — and it is the **only** thing standing between a cloned
repository and your shell for anyone who set `init.templateDir`, which is the
convenient mode and therefore the popular one.

**PR 1 — `amont run [--all-files]`.** Small, useful immediately, no risk, and
it gives the later work a way to be exercised over a whole repository.

**PR 2 — `not_during` git-state conditions.** Generalises the `CHERRY_PICK_HEAD`
special case and closes the pre-push gap. Independently useful, and §1 needs its
predicate.

**PR 3 — index fidelity.** The correctness gap. Alone, no fixing.

**PR 4 — `stage_fixed`.** Only after 3 is proven in the fleet for a while: this
is the first feature that would write to someone's index, and it should not be
the change that also introduces the stash.

**PR 5 — pre-push runs against the pushed commits**, not the working tree. Its
own decision: a worktree or `git archive` of the tip is more expensive than
anything else here, and the cost is the whole question.

Shipped as `pushed_tree.rs`, and the cost is what made it OPT-IN:
`git config amont.testPushedTree true`. The instrument is
`git worktree add --detach <tip>`, not the stash — a push is not a staging
operation, so the difference that matters is tree-versus-the-commit-you-are-
sending, which includes staged-but-uncommitted work too. Holding all of that
aside for the length of a test suite would leave the developer looking at a tree
that is not theirs for minutes at a time.

Shebang detection is unscheduled. So is everything under *What this does not
solve*, which is a list of known gaps rather than a backlog.

## Decisions

1. `--all-files` implies no stash. There is no staged/unstaged distinction to
   protect when the input set is `git ls-files`, so taking a stash would be
   surprising extra mutation with no correctness upside. If a future explicit
   `--no-stash` flag exists for diagnostics, `amont run --all-files
   --no-stash` should be accepted as redundant rather than rejected.

   **Corollary, stated because it is the inverse of §1**: on a dirty tree,
   `--all-files` reports on content that is not committed and may never be. That
   is correct — the question it answers is "does my working tree pass", not
   "would my commit pass" — but §1 spends a page arguing that judging unstaged
   content is a bug, and a reader who meets this without warning is entitled to
   think one of the two is wrong. They are different questions; the mode's help
   text should say which one it answers.
2. `Outcome::Fixed` is invalid in `pre-push`. A pre-push hook must not modify
   the worktree or index: silently proceeding after a write would make the
   pushed commit differ from the tree the developer is now looking at.

   **Refused where every other bad declaration is refused**, rather than at push
   time. A `pre-push` line declaring a fix is a `ParseError`, alongside
   `NameTaken` and `Duplicate` — reported on every commit, named, located, and
   visible in the dashboard's `DECL` column. A "hook contract violation" raised
   at push time would be the same fact discovered later, by fewer people, in the
   one place where blocking is most expensive.

   A built-in cannot express it at all: `Fix::Rewrite` is reachable only from a
   `Stage::PreCommit` declaration, which the compiler enforces.
3. The **stash** applies to the `pre-commit` check stage only. `commit-msg`
   reads and rewrites the message file Git passes as `$1`; `prepare-commit-msg`
   appends to that same message file based on the branch name and commit
   source. Neither hook selects paths from the index or asks tools to read
   repository files, so wrapping them in `StagedOnly` would add stash risk
   without fixing a real fidelity problem.

   **`pre-push` is excluded from the stash and NOT from the problem.** It has
   the same bug by a different route (§1) — the pushed refs choose the files and
   the working tree supplies the content. Stashing is the wrong instrument
   there, so it gets its own item rather than an exemption. Saying "pre-commit
   only" and stopping is precisely the move §3 criticises: pre-push has no
   cherry-pick guard today because the zsh version had none, and nobody wrote
   down that it was a choice.
