# Migrating the hooks to a single Rust binary

Status: **complete — all four phases**. See [Outcome](#outcome) for what the
plan got wrong.

Phase 4 is done, not "optional and not started" as this line said for a while.
There is no `.zsh` file anywhere in the repository; the "19 zsh suites" it
described porting ARE the cargo integration tests under `crates/*/tests/`, and
Windows CI runs the same suite every other platform does rather than a smoke.
The one thing left of the shell era is the five `sh` shims, and they are shims
on purpose.

**This document is history, kept for its reasoning.** Everything below is in the
tense it was written in: the argument for a phase order, for a safety rule, for
a distribution shape, is worth more than a description of the result. Where the
plan and the code diverged, the divergence is called out where it happens.

## Why

Two requirements the current design cannot meet:

**1. No unconditional runtime dependency.** Three hooks run on *every commit in
every repo*, whatever the language:

- `commit-msg` — **node**
- `prepare-commit-msg` — **zsh**
- the `pre-commit` / `pre-push` dispatchers — **zsh**

So a pure-Python repository needs **node and zsh installed to make a commit** —
not because it uses either, but because the hook framework does. Per-hook
scoping cannot fix this: the dependency is in the entrypoint, before any scoping
runs.

**2. Windows.** 17 of 20 hooks are `#!/bin/zsh`. Git for Windows ships MSYS2
bash, not zsh, so those hooks do not execute at all. Re-shebanging is not enough
— they use zsh-only syntax throughout (23 `${0:a:h}`-style modifier expressions,
45 bare-word arrays, 5 `(N)` glob qualifiers, 2 `setopt`).

A single static binary answers both: nothing to install alongside it, and
Windows is a build target rather than a rewrite.

**What this is NOT for.** The nine linter-orchestration hooks shell out to
eslint / prettier / ruff / pyright / yamllint / kubeconform / kube-linter / argo.
Rust does not remove those, and should not: they are opt-in per repo already
(each hook no-ops unless the tool's config is present), which is exactly the
"only the tools I actually use" behaviour we want. A Python repo pulls `ruff`,
not `eslint`. That part of the design is already right.

## Scope

| group | files | lines | why it moves |
| --- | --- | --- | --- |
| entrypoints + always-on | `pre-commit`, `pre-push`, `commit-msg`, `prepare-commit-msg`, `ban-terms`, `branch-pattern`, `usual-name`, `pull-rebase`, `run-tests-js` | ~700 | runs regardless of repo language — the actual dependency |
| linter orchestration | 9 × `pre-commit-*.zsh` | ~576 | only for Windows parity; otherwise optional |
| tests | 15 × `tests/*.zsh` | ~859 | see below — these are an asset, not a cost |

## The thing that de-risks this

**The existing test suite is interface-level.** Every test invokes the hook by
file path and asserts on exit code and stdout; none reaches inside. So once a
hook file becomes a shim that `exec`s the binary, all 14 suites keep working
**unchanged** against a Rust implementation.

This only holds under a constraint that is easy to get wrong, so state it as a
contract: **every hook keeps a file at its current path, forever.** The tests
resolve `templates/hooks/<name>` from their own filename — `pre-push-branch-
pattern.test.zsh` runs `templates/hooks/pre-push-branch-pattern.zsh`,
`pre-commit-ban-terms.test.zsh` runs the `.js`. Porting a hook therefore means
**replacing its body with a shim, never deleting the file**. Delete it and the
unmodified test cannot run at all, and the safety property this whole plan rests
on evaporates.

That is not just a testing concern: the dispatcher discovers sub-hooks by
globbing `<hook-name>-*` in its own directory, and `hook.skip` filters that glob
by substring on the path. Keeping every path alive keeps both working, unchanged,
throughout the migration. It is ~20 shims, not 4.

Renaming the shims (dropping the now-misleading `.zsh` / `.js` suffixes) is a
tidy-up for *after* the migration, and costs a test edit plus a propagation
sweep. Not worth bundling into it.

That means a behavioural regression net for the whole migration, on Linux and
macOS via the CI added in #12, before writing a single line of Rust test. Port a
hook, run `make test`, and the same assertions that guarded the zsh version now
guard the Rust one. Port the tests to `cargo test` later, or never.

Treat this as a hard rule: **a hook is not ported until its existing `.zsh` test
passes against the binary, unmodified.**

## The two genuinely hard parts

### 1. Shim resolution (the part that will bite)

Git requires an executable file at each of the four entrypoints (`commit-msg`,
`pre-commit`, `pre-push`, `prepare-commit-msg`), and — per the contract above —
every sub-hook keeps its path too, so every ported hook becomes a shim of this
shape:

```sh
#!/bin/sh
exec git-hooks pre-commit "$@"
```

The trap: **git hooks do not inherit an interactive shell's PATH.** GUI clients
(VS Code, Tower, SourceTree, JetBrains) launch git with a login-ish environment
that frequently lacks `~/.local/bin` or `~/.cargo/bin`. A shim that only does
`exec git-hooks` works in the terminal and fails silently in the GUI — the worst
possible failure shape for something standing between a person and their commit.

Resolve in this order, and bake the install path in at `make install` time:

1. `$GIT_HOOKS_BIN` (escape hatch, and how the tests point at `target/debug`)
2. the absolute path written into the shim by `make install`
3. `command -v git-hooks` (PATH, last resort)
4. **fail loudly** — never silently skip a check

Windows: the `#!/bin/sh` shim runs under Git for Windows' bundled sh, which is
always present, and MSYS resolves `git-hooks` to `git-hooks.exe`. Keep the shim
rather than installing a bare `.exe` as the hook file.

### 2. Distribution

> **A PLAN NOT TAKEN.** None of the release pipeline below was built, and
> `make install` does not work the way this describes. What ships is:
> `make install` runs `cargo build --release` and then `amont install`, which
> writes the binary and bakes the shims — building from source is the ONLY path,
> so the "contributor fallback" became the whole story and the
> `make install-from-source` target this bullet list asks for never existed.
> `make install-fleet` does the same for the dashboard.
>
> The section is kept because the reasoning still applies the day somebody wants
> binary releases: five targets, musl for the static Linux build, checksums, and
> a source path for unlisted triples are all still the right list. It is a
> backlog item nobody has needed, not a description of today.

This is the real new cost, and it is permanent machinery we do not have today.

- Build 5 targets in CI: `aarch64-apple-darwin`, `x86_64-apple-darwin`,
  `x86_64-unknown-linux-musl` (static — no glibc-version coupling),
  `aarch64-unknown-linux-musl`, `x86_64-pc-windows-msvc`.
- Publish on tag, with checksums. `cargo-dist` generates most of this.
- `make install` fetches the artifact for the host triple, verifies the
  checksum, installs the binary, then writes the shims with the resolved path.
- Contributor fallback: `cargo install --path .`.
- Keep a `make install-from-source` path so a machine without network (or on an
  unlisted triple) is not stuck.

**Offsetting win:** updating today means copying N files into 96 repos (done
twice this week). After this, it is replacing **one binary** — the shims change
almost never. Net, install gets *simpler*, not harder.

## Behaviour that must be preserved

Verified against the current dispatcher — easy to lose in a rewrite:

- **The two dispatchers behave DIFFERENTLY, and both behaviours are load-bearing:**
  - `pre-commit` backgrounds every sub-hook, waits for all of them, then reports
    **every** failure as a list. Running it serially would be a visible slowdown
    on each commit; stopping at the first failure would hide the rest, so you'd
    fix one lint error, commit, and immediately meet the next.
  - `pre-push` runs sub-hooks **serially and exits on the first failure**, naming
    just that hook (`Error raised by hook <path>`). That is correct for push:
    the steps are ordered and expensive (branch-pattern, then pull-rebase, then
    the full test suite), and there is no point running tests after a rebase
    conflict.

  A Rust port must reproduce both, including the two distinct message formats,
  and should have a test for each — a single shared "run all sub-hooks" helper
  is the obvious accidental way to lose the distinction.
- **`git config --get-all hook.skip`** filters by substring match against the
  hook path. `git -c hook.skip=package-lock commit` must keep working.
- **`CHERRY_PICK_HEAD` short-circuit**: the dispatcher exits 0 during a cherry-pick.
- **`HOOKS_FORCE_GREP`** exists only to exercise the grep fallback in tests; it
  disappears with the shell hooks, along with the `rg`/`grep` split itself —
  the binary uses the `regex` crate and needs neither.

## Phases

Each phase ships and leaves the repo working. Nothing is a flag day.

**Phase 0 — scaffold, no behaviour change.**
Cargo crate, CI build job, shim mechanism, `make install` writing shims.
The Rust dispatchers do nothing themselves yet: they glob and run the existing
`pre-commit-*` / `pre-push-*` script files, honouring `hook.skip` — `pre-commit`
in parallel collecting every failure, `pre-push` serially stopping at the first,
each with its existing message format.
*Done when:* all 14 zsh suites pass unchanged with shims installed.

**Phase 1 — the always-on set.** In this order (cheapest correctness first):
`branch-pattern` → `usual-name` → `prepare-commit-msg` → `pull-rebase` →
`commit-msg` → `ban-terms` → `run-tests-js`.
Each: implement in Rust, **replace the script with a shim (do not delete it)**,
`make test` must stay green.

Phase 1 also moves the **config gating into the dispatcher**. Today every
`pre-commit-*.zsh` is spawned unconditionally and each decides for itself
whether to no-op — which means a Python repo still starts a zsh process for the
eslint hook purely to have it exit. The Rust dispatcher should evaluate the same
signals (an eslint config, `[tool.ruff]`, `pyrightconfig`, staged `.yaml`, …)
and **not spawn** what cannot apply. Fewer processes per commit, and it is what
makes the dependency claim below true rather than nearly true.

*Done when:* the **unconditional** dependency is gone — a repo whose staged
files trigger no linter hook needs neither zsh nor node to commit.

Note precisely what this does **not** yet deliver: a repo that *does* trigger a
still-zsh linter hook (a Python repo with `[tool.ruff]` reaching
`pre-commit-ruff.zsh`) continues to need zsh. Full removal is Phase 3. Requirement
1 is met for the framework; it is met for every repo only after Phase 3.

`ban-terms` earns a real parser here. Its comment/string blanker is explicitly
"not a parser" and mis-handles a regex literal containing an escaped slash; a
proper tokenizer removes that whole class of false negative.

(Written of the JS implementation. The Rust port gained a `Regex` state that
handles escaped slashes and character classes, so the escaped-slash defect was
gone by Phase 1 — this paragraph outlived it. The defect that actually
survived was a different one: see the tokenizer note below.)

**Phase 2 — Windows.** Add `windows-latest` to CI, running the suites for the
ported hooks only.

Be clear about what this proves and what it does not: a **real end-to-end
`git commit` on Windows still fails** while any applicable hook is zsh. Phase 1's
gating helps — a repo triggering no linter hook commits fine — but a Python repo
with `[tool.ruff]` reaches a zsh script that Windows cannot execute. So Phase 2
demonstrates the always-on path, and **Phase 3 is a prerequisite for Windows
being genuinely usable**, not an optional extra, if the goal is "work on Windows"
rather than "the framework is portable".

*Done when:* the ported hooks are green on `windows-latest`, and the doc says
plainly which hooks a Windows user cannot yet run.

**Phase 3 — linter orchestration.** Port the nine `pre-commit-*.zsh`. Optional
only if Windows is a "commit hygiene" goal; **mandatory** if a Windows user must
be able to commit to a repo that uses any of these linters (see Phase 2).
*Done when:* zsh is absent from the repo entirely, and requirement 1 holds for
every repo rather than for the framework.

**Phase 4 — retire the shell test suite (optional).** Port `tests/*.zsh` to
`cargo test`. Deliberately last: those tests are the migration harness, and
rewriting them early would remove the net while walking the wire.

*Done, and it stopped being optional the moment Windows mattered.* The suites
are `crates/amont/tests/*.rs` and `crates/amont-fleet/tests/*.rs`, each
`Repo` an isolated temp repository so they run in parallel — which the old
one-repo-per-suite runner could not, and which is why several old cases depended
on state left by earlier ones. Windows CI runs the full suite rather than a
smoke, because the tests no longer need an interpreter Git for Windows does not
ship.

## Rollout to existing repos

96 local repos hold copies from `git init` time. The established recipe
(compare each installed copy against every historical blob of that file, replace
only exact matches so a customised copy is never clobbered) applies unchanged to
the shims — see the propagation note in the ban-terms memory. Survey found zero
customised copies, twice.

After Phase 0 the shims are stable, so subsequent phases need **no** repo sweep:
updating the binary updates every repo at once.

## Decisions to make before starting

1. **Binary name.** `git-hooks` collides conceptually with `git hooks` (git may
   resolve `git-hooks` as a subcommand). Prefer something unambiguous.
2. **One binary or one per hook?** One, with the hook name as argv[1]. Five
   binaries would mean five downloads and five things to keep in sync.
3. **Config format.** The zsh hooks read `git config` (`hook.skip`). Keep that,
   or introduce a `.amont.toml`? Keeping `git config` avoids inventing a
   second source of truth and preserves `git -c hook.skip=… push`.
4. **MSRV and dependency budget.** `regex` and `ignore` (ripgrep's own crates)
   cover matching and file walking. Resist more.
5. **Does Windows need the linter hooks?** Determines whether Phase 3 is
   optional or mandatory — see Phase 2. If a Windows user must commit to a repo
   using ruff/eslint/prettier, Phase 3 is part of the Windows story, not a
   follow-up, and the honest total cost is ~1,500 lines rather than ~700.
6. **Do the shims keep their `.zsh` / `.js` suffixes?** RESOLVED: no, they were
   dropped after Phase 4.

   **The rest of this answer was wrong, and is corrected rather than deleted
   because somebody will otherwise rely on it.** It said "the suffix STRIPPING
   in `main.rs` stays permanently — a repo seeded before the rename still passes
   the old filename through". There is no such stripping, and there never was.
   `registry::lookup` is an exact match against `ENTRYPOINTS` and the check
   names; a repo still holding `pre-commit-ruff.zsh` hands the binary a name it
   does not know, and gets `unknown hook` and exit 2.

   That is deliberate and it is fine, because the repair path is the fleet, not
   a compatibility shim in the binary. `amont-fleet fix` / `install` is what
   moves the 96 repos, and `Repo::stale_ours` — "our files that we no longer
   ship" — is exactly the field that finds those leftovers and turns them into a
   removal. The binary and the repos DO move independently; the mechanism is a
   sweep with a plan you can read first, not silent name rewriting inside the
   hook that runs on every commit.

## Honest cost

~700 lines of shell/JS to port for Phase 1, plus a release pipeline that must be
maintained forever. Against that: the framework stops imposing node and zsh on
every commit, Windows becomes a build target, and the BSD/GNU divergence class
disappears — that class produced two of the three bugs CI found on its first two
runs (`realpath -s`, and the `\w`-vs-POSIX-class dialect split).

If Phase 1 alone lands, the framework stops imposing anything and Phase 4 stays
optional. Phases 2-3 are optional only if Windows is not a real target; if it is,
budget for Phase 3 too — roughly 1,500 lines rather than 700.

---

## Outcome

All 20 hooks are `sh` shims over one dependency-free binary (~430 KB). A repo
that triggers no linter needs **neither zsh nor node** to commit or push, which
was the requirement. Phase 2 (Windows CI) and Phase 3 (the nine linter hooks)
landed too, so zsh is gone from the hook set entirely.

### What the plan got wrong

**"All 14 zsh suites passing is Phase 0 done."** They were necessary but not
sufficient: every suite invokes a SUB-HOOK directly, so the dispatchers — the
exact code Phase 0 replaced — had **no coverage at all**. Phase 0 had to bring
its own tests.

**The safety rule contradicted itself.** "Existing tests pass unchanged" only
holds if every hook keeps a file at its original path; the plan said Phase 1
would delete the scripts. Corrected before implementation: porting replaces the
body with a shim, never deletes the file. ~20 shims, not 4.

**"Phase 1 delivers no zsh."** The dispatcher still globs and spawns
`pre-commit-*` scripts, which need zsh before they can no-op. Phase 1 removes
the UNCONDITIONAL dependency; full removal needed Phase 3.

**Phase 3 was listed as optional.** It is a prerequisite for Windows being
usable, not an extra — a real commit there fails while any *applicable* hook is
zsh.

### What the method actually caught

Seven bugs in code that had already shipped:

- four hooks used `rg` unguarded — a missing `rg` makes `! rg …` true, so
  `branch-pattern` rejected EVERY branch name (silent wrong answers, not errors);
- `make test` had never run on a clean macOS (`realpath -s` is GNU-only);
- CI could not go red (`make test | tee` without pipefail);
- `[]` is truthy in JS, so `run-tests-js` selected every package regardless of
  what changed;
- `prepare-commit-msg` appended a dangling `Issue: #id ` (tested `$?` after a
  pipeline ending in `head -n 1`);
- `pull-rebase` read the ahead-count with `head -c 1`, so 12 printed as "1";
- 54 of 96 installed `commit-msg` copies matched no known template blob and were
  silently skipped by the propagation sweep.

Plus five in the ports themselves, each caught by the existing suite before
merge, and three tests that could not fail (the dispatchers had none;
`pull-rebase`'s dirty-tree case was satisfied by an unrelated early exit; my own
Windows smoke read a commit subject without first asserting the commit existed).

**Two bugs were found only by the Windows leg, on its first two runs** — and
both were silent-wrong rather than loud. `which` missed `.exe`, so `resolve_tool`
would have skipped the repo's PINNED linter for an ambient one. And Windows has
no shebang support, so every sub-hook the dispatcher spawned failed with
`%1 is not a valid Win32 application`.

### Rules worth keeping

1. **A hook is not ported until its existing test passes untouched** — then break
   the implementation once to confirm the test can fail. Vacuous tests are the
   characteristic failure here.
2. **"Verified against the original first" is only as good as the environment it
   was verified in.** A kubeconform test asserting silence encoded the machine it
   was written on; CI has neither tool, hits the gate first, and warns.
3. **A sweep reporting "0 customised" is not proof of a clean fleet.** Check the
   DISTINCT-BLOB count per hook; consistent means 1.
4. For hooks that REWRITE (`commit-msg`) or filter (`ban-terms`), diff the old
   and new implementations over the same inputs. Both were byte-identical across
   11 and 14 cases.

### Still open

- ~~**Phase 4** — port the 19 zsh suites to `cargo test`.~~ Done. They are
  `crates/*/tests/*.rs`, and Windows runs them all.
- ~~**The `ban-terms` tokenizer.**~~ Done. The documented defect (escaped slash
  in a regex) turned out to be already fixed by the port's `Regex` state — the
  note was stale. Probing 20 hard constructs found the real survivor: template
  literal **substitutions** were blanked as string content, so any banned call
  written inside `${…}` was missed. Now handled as code, with a DEPTH STACK
  because substitutions nest.

  Differential over 39,378 real fleet files: blanking changed on 10,567 of them,
  with 0 new alarms and 0 dropped detections — the path is heavily exercised and
  the stricter rule costs nothing in false positives.
- ~~**The rename.**~~ Done. Shims carry no extension; `scripts/propagate.sh`
  swept all 96. The sharp edge was the dispatcher's `<hook>-*` GLOB: a repo left
  holding both `pre-commit-ruff.zsh` and `pre-commit-ruff` runs ruff TWICE,
  silently. Removal and installation therefore happen per repo, in that order,
  which is why the sweep is a script and not a typed loop.
- ~~**`pre-commit-pyright` in 6 of 96 repos**~~ Done — it self-scopes on a
  pyright config, so installing it everywhere changes nothing for the 90 repos
  without one. `governance-ts` had it for months and it never fired.


## Checks moved in-process (PR #36)

Git invokes exactly **four** hook names — `pre-commit`, `pre-push`, `commit-msg`,
`prepare-commit-msg`. The other 16 files in every `.git/hooks` were our own
dispatcher's business: byte-identical `sh` shims whose only job was to re-exec
the binary and tell it their own filename. One commit cost 27 processes to run
work the binary already had in a table.

Fleet-wide 1920 files → 384; steady-state pre-commit ~383 ms → ~293 ms. Most of
a commit is the linters, so 24% is the whole timing prize; the structural wins
are larger: order is **declared** rather than emerging from a lexicographic
filename glob, the Windows shebang emulation is deleted, and stdin is read once.

That last one fixed a silent bug. git feeds pre-push its ref list on stdin,
consumable **once**. As separate processes with INHERITED stdin, whichever check
ran first drained it — two repos carried a `pre-push-branch-protect.sh` sorting
before `pre-push-run-tests-js`, so the JS gate saw EOF and ran nothing, for as
long as both existed.

`branch-protect` is now built in, first in pre-push, protecting `main`/`master`
everywhere; it matches the REMOTE ref and treats a delete as a write.

## Rust checks (PR #38)

| check | dispatcher | command |
|---|---|---|
| `pre-commit-cargo-fmt` | pre-commit | `cargo fmt --all -- --check` |
| `pre-commit-clippy` | pre-commit | `cargo clippy --workspace --all-targets --all-features -- -D warnings` |
| `pre-push-cargo-test` | pre-push | `cargo test --workspace --all-features` |

Split by cost as the other languages are: fmt/clippy with ruff and pyright, test
with `run-tests-js`. Three separate checks so `hook.skip` disables them
individually. Scoping runs on the **nearest ancestor `Cargo.toml`**, not the repo
root, so a Rust component in a subdirectory works; `--workspace` covers the
members from there. `Cargo.toml`/`Cargo.lock` count as touching Rust for clippy.

**Two bugs the tests caught:**

1. Availability was probed with `cargo <sub> --version`. That works for rustfmt
   and clippy, which are separately installable components, but
   `cargo test --version` is *"unexpected argument"* — so the probe reported
   test as unavailable and the gate **silently passed**. It would never have run
   a test in any repo. Built-in subcommands need no probe; only components do.

2. **git exports `GIT_DIR`/`GIT_INDEX_FILE`/`GIT_WORK_TREE` to every hook, and
   they OVERRIDE the working directory.** Running a project's test suite from a
   hook therefore hands it the hook's repository. This repo's own suite creates
   throwaway repos and commits to them — so the first real run of the gate put a
   stray commit, authored by the test fixture, onto a live branch and pushed it.
   `strip_git_env` now clears them before spawning any tool, npm included; a
   suite must behave exactly as it does when run by hand.


## `propagate.sh` is gone (PR #47)

Replaced by `amont-fleet fix`, after a differential proved the Rust plan
removes exactly the same set on a fixture covering every actionable case. That
comparison is what earned the deletion; the expectation it produced is kept as a
golden test, because deleting the gate along with the script would have thrown
away the only evidence the rewrite was faithful.

`make propagate` now runs the Rust path. Dry-run by default, `APPLY=1` to write.


## Why the dependency guard exists (corrected)

`scripts/check-no-deps.sh` asserts that `amont` resolves to nothing outside
the workspace. Its original comment said the migration existed because a Python
repo needed node and zsh to commit, so a dependency tree "would undo it".

**That reasoning was wrong** and is corrected in the script. node and zsh were
RUNTIME requirements — they had to exist on `PATH` in every repo or the hook
failed. A Rust crate is compiled in and statically linked; adding `serde` would
make nobody install anything. The two are not the same problem.

The guard is still worth having, for reasons that survive scrutiny:

1. **Supply chain.** The binary runs on every commit in 96 repos, with the
   developer's credentials, reading every staged file. Every transitive crate is
   code executing in that position. This argument is specific to THIS binary —
   `amont-fleet` pulls ratatui and a dozen crates without concern, because it
   is opt-in.
2. **Offline reproducibility.** A std-only crate builds without a registry,
   indefinitely. Real but modest.
3. **A forcing function.** It makes each dependency in the commit path an argued
   decision rather than a default.

So it is a strong default to argue *with*, not a wall. If the commit path ever
needs a real parser, weigh that crate's tree against the code it replaces. The
answer today is still "hand-roll the twenty lines", because the config format
under discussion is trivial — not because a dependency is forbidden.
