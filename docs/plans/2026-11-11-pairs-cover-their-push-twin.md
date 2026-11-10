---
status: active
branch: fix/pairs-cover-their-twin
repos: [amont]
adrs: [decisions:ADR-0009, decisions:ADR-0019]
---
# Pairs cover their push twin

## Review panel

👉 **Decide:** (1) Pair lines on stdout via `say!`, as today, or move every pair line to stderr in one change? (2) On a widened run, does `$AMONT_FILES` carry the twin's matched paths (as planned) or only the declared match?
📍 amont · plan reviewed, #450 (release gap) merged · next: land the plan, Phase 1. Panel: architect, backend, lang:rust, po, tui, unix (all opus-5-5); codex: not installed.
**Changed by review:** stamps are counted against the widened scope, so an unstamped `Cargo.lock` commit is never `Gated`; a missed resolve falls back to the narrow scope. **Changed by you:** JS roots are declared (`set rootRunsMembers`), with a hint when a repeat may be happening, instead of a runner list.
**Verdicts:** all 6 approve; the last delta (your change) was backend approve and rust approve-with-changes, 3 small items carried. Findings carried into implementation are listed in Full reviews.
📄 Full reviews: [pairs-cover-their-push-twin.reviews](2026-11-11-pairs-cover-their-push-twin.reviews.md)

## Picture

```
 today: a commit that changes only Cargo.lock
 commit --> pre-commit cargo-test (*.rs) : not touched, no stamp
   push --> pair_verdict: *.rs misses Cargo.lock --> NotPaired (silent)
        --> pre-push-cargo-test runs the whole suite, on the push

 after this plan
 commit --> pre-commit cargo-test (*.rs + its twin's files) : runs, stamp
   push --> pair_verdict: covered, stamped --> gated at commit instead
 (a files pair, not widened, prints why before the push runs it)

 JS: root script declared in amont.conf --> members covered, run once
 JS: not declared --> the push runs both and says how to declare it
```

A commit that changes only a file the push gate reads, like `Cargo.lock`,
used to slip past its commit-time pair and make the push rerun the suite
silently; after this plan the pair fires at commit on its twin's files, and
the push skips the suite or says why it cannot.

## Goal

A push never repeats a suite that the commits already passed, in any
language, and when it does run one, it says why. Two causes remain after
3.0.1 (which fixed cargo's once-per-member runs):

1. **Scope gap.** A commit-time declaration that stands in for a push gate
   (same short name: `pre-commit cargo-test *.rs` for the built-in
   `pre-push-cargo-test`) fires only on its own patterns. The twin fires on
   more (`RUST_PATHS`: `.rs`, `Cargo.toml`, `Cargo.lock`, `rust-toolchain*`,
   `rustfmt.toml`, `clippy.toml`). A commit changing only `Cargo.lock` earns
   no stamp, `pair_verdict` sees `!pair.scope.covers_all(&relevant)`
   (`crates/greengate-runtime/src/hooks/run_tests.rs:251`) and returns
   `NotPaired` **silently**. The push then runs the whole suite. The same
   shape holds for go-test (`go.sum`), pytest, run-tests-js
   (`*.ts` declared vs `.js .jsx .ts .tsx .vue` + `package.json`) and every
   declared pre-push twin. This is what made the 13:08 greengate land of
   2026-11-10 run the suite again.
2. **JS recursive roots.** `run-tests-js` runs a gate script in the root
   package and in each changed member (`packages_to_test`,
   `run_tests.rs:526`). When the root's script is itself recursive
   (website-builder: `test: pnpm -r test`, 30 packages; duro:
   `typecheck: pnpm -r run typecheck`), every changed member runs twice.
   greengate cannot tell that a root script is recursive without a list of
   runners to maintain, so the repository declares it, and a push that may be
   repeating says how.

ADR-0009 (`ci.test-gate-stage` = pre-commit) is why widening goes to the
commit side: the suite belongs at commit, never on the open push connection.

## Non-goals

- Go and pytest per-root runs: `go test ./...` stops at a nested `go.mod`,
  and pytest runs once from the root, so neither repeats (checked
  2026-11-10).
- Guessing what a JS script does, from a list of runners or from an LLM:
  the repository declares which root scripts run every member, and anything
  undeclared runs as today.
- Changing the stamp token or its format (`gate_identity::external_token`
  does not hold the scope; `pair_verdict` reads the scope from the current
  manifest, so no migration is needed).
- This repository's `amont.conf`: after Phase 1 its `*.rs` line covers
  `Cargo.lock` through its twin, with no edit.

## Code review

- amont: backend, lang:rust, unix, tui, architect

## Behaviour

**Pairs fire on their twin's files (the person's choice, 2026-11-10).**
A blocking pre-commit declaration that pairs with a push gate fires at
commit on the union of its own scope and its twin's: the built-in
`pre-push-<name>` from the registry, a declared `pre-push <name>`, or, for
the npm vocabulary (`gated_at_commit`'s `GATE` scripts), run-tests-js's
scope. Its opt-in stays its own. A commit changing only `Cargo.lock` in this
repository runs `cargo test --locked` at commit, earns the stamp, and the
push prints `✓ cargo-test gated at commit instead — not repeating it here`.
When the commit fires only through its twin's files, it says so first, on one line of 80 columns or less: `cargo-test: Cargo.lock is read by pre-push-cargo-test — testing at commit`.

- A `files` declaration is **not** widened: its command receives the matched
  paths as argv, and handing a linter `Cargo.lock` is wrong. Its pair stays
  as narrow as written.
- `greengate list` shows a widened row's effective scope in a new trailing field and leaves the existing columns unchanged, so parsers keep working, e.g.
  `*.rs (+ Cargo.toml, Cargo.lock, … from pre-push-cargo-test)`; on a TTY the row is cut at the terminal width, and piped output lists every path with no `…`.

**A push says why a pair did not vouch.** `PairVerdict::NotPaired` gains a
reason when a same-named commit declaration exists. The push prints the
reason through the same `say!` channel as the sibling pair lines (`gated at
commit instead`, `declared at commit time, but…`). Exit codes are unchanged.
Then it runs the gate.

- `⚠ cargo-test at commit covers *.rs, not Cargo.lock — running it here`
  - The first line is at most 80 columns and opens with `warning_sign()`,
    like dispatch.rs:1729.
  - Then an indented line:
    `a files declaration is not widened: add Cargo.lock to amont.conf line <n>`.
  - Past 3 uncovered paths, it names the first 3 and `and N more`.
- `… is a warn check, which vouches for nothing — running it here`
- `… is skipped here (hook.skip) — running it here`
- `… is not opted in here (needs <marker>) — running it here`
- `… cannot be identified (its program was not found) — running it here`

These replace today's silence. With no same-named declaration, there is no
line, as before. run-tests-js's `candidates` filter (`run_tests.rs:674`) uses
the same reasons and wording.

**JS: a root that runs its members is declared, and a possible repeat is
announced (the person's choice, 2026-11-11).** greengate keeps no list of
runners or flags. The repository says what its root scripts do.

- **The declaration.** `set rootRunsMembers test,typecheck` in `amont.conf`
  names the root scripts that already run every workspace member; whoever
  wrote `pnpm -r test` knows. It is policy like `onBusy`: effective only
  from a trusted manifest, strict, set once. The value is a comma-separated
  list of the gate's script names, or `none` (the root runs no members).
- **What it skips.** For a declared script `S`, a changed member is not
  run again when the root package runs `S` in this push and the member is
  inside the root's workspace globs: `package.json` `workspaces` (an array
  or `{packages}`), or `pnpm-workspace.yaml` `packages` with `!` negations
  (a pnpm root reads only that file). A tracked fixture outside them still
  runs. An unreadable or unparsable workspace file, or a member path with a
  newline or non-UTF-8 byte, counts as not covered, so the member runs.
- **What it says.** One summary line for all covered members:
  `3 packages covered by the root's test (declared in amont.conf) — not repeating them`.
- **The hint (the person's addition).** When `S` ran in the root and in at
  least one changed member in this push, and `rootRunsMembers` is not set
  at all, the push prints two lines after the runs, once per script:
  - `⚠ test ran at the root and in 2 packages — a repeat if the root runs them`
    (at most 80 columns, opening with `warning_sign()`);
  - then, indented: `declare it in amont.conf: set rootRunsMembers test   (or: none)`.

  `set rootRunsMembers none` silences it for a repository whose root does
  not run its members. The hint cannot tell whether the repeat is real, so
  it says "if". It is the only place the push guesses, and it guesses out
  loud.
- **A wrong declaration** skips tests that ran nowhere. The summary line
  says `declared`, so the person can find it, and Verification 7's rollback
  rule covers it.
- Each package that still runs is announced
  (`npm run test: 2 of 3, in packages/a`), as cargo's runs are since 3.0.1,
  even when only one does.

## Phases

- [ ] Phase 1 — pairs fire on their twin's scope.
  - `crates/greengate-runtime/src/manifest.rs`:
    - **Who gets a trigger.** Each non-`files` pre-commit Runnable external
      with a twin gets `trigger: Option<Scope>`, the union leaked once like
      declared scopes.
    - **When it is computed.** In a second pass, after every declaration
      has parsed, so a declared `pre-push` twin later in the file is found.
    - **When it applies.** `load` has no settings, so it keeps the union
      in a private `pending_trigger`. `Manifest::resolve_triggers(&settings)`
      moves it to `trigger` only for declarations that are effectively
      Block. Effective severity comes from the existing function that
      computes it for `blocking_commit_decls`: settings, overrides and
      `hook.skip`, in its order, with no second copy.
    - **Where it is called.** At every `manifest::load` caller that builds
      checks, after settings are read and before the policy window that
      `setup.rs:62` requires. The callers are setup.rs:71, lib.rs:781,
      rehearsal.rs:765, land.rs:761, tree_lint.rs, and greengate main.rs.
      A caller that builds no check says so in a comment.
    - **What a missed call does.** Until the resolve runs, `scope()`
      returns the declaration's own scope, and `blocking_commit_decls`
      sees no trigger. A caller that forgets the call behaves as 3.0.1:
      narrow, and the push runs the suite. It is never widened, and it
      never vouches.
    - **Tests.**
      - A warn declaration loaded with no resolve call keeps its own scope.
      - Through pre-commit, pre-push, `list` and `run`: a warn line
        overridden to block widens, and a block line overridden to warn
        does not.
    - **Who reads it.** The External's `Check::scope()` returns the
      trigger, so `files_to_judge`, `would_judge`, the waves, the heavy
      count and run_stamp all agree.
    - **What keeps the declaration's own scope.** `matched` for argv, and
      the `list` label.
    - **`$AMONT_FILES`.** It carries the trigger's matched paths, so a
      wrapper that filters it does not see an empty list on a widened run.
    - **Tests.** A declared `pre-push` twin after the commit line, and a
      warn line overridden to block.
  - `crates/greengate-runtime/src/registry.rs`: a lookup of a built-in
    pre-push check's scope by short name.
  - `crates/greengate-runtime/src/hooks/run_tests.rs`: `GateDecl.scope` is the trigger scope (own opt-in), so `pair_verdict` and the run-tests-js `candidates` filter use it for `covers_all`, the per-commit `touches` (run_tests.rs:271, :690) and `repo_opted_in`. Test: an unstamped `Cargo.lock`-only commit makes the push run.
  - `crates/greengate-runtime/src/check.rs`: `Scope::union`.
  - `crates/greengate-runtime/src/lib.rs`: `list` shows the widened scope in
    a new trailing field.
  - Tests in `crates/greengate/tests/it/gate_pairs.rs`:
    - a `Cargo.lock`-only commit is stamped, and the push is gated;
    - the same for a declared `pre-push` twin with a wider scope;
    - a `files` pair is not widened;
    - a warn pair is not widened;
    - an unstamped `Cargo.lock`-only commit (made before the upgrade, or
      with `--no-verify`) makes the push run the suite (`Unstamped`),
      never `Gated`;
    - `greengate list` shows the widened scope with its source.
- [ ] Phase 2 — a push says why a pair did not vouch.
  - `crates/greengate-runtime/src/hooks/run_tests.rs`: a `NotPaired` reason
    enum.
  - `crates/greengate-runtime/src/dispatch.rs`: the reason lines.
  - `crates/greengate/tests/it/gate_pairs.rs`:
    `a_stamped_tip_cannot_cover_files_outside_the_pairs_scope` also asserts
    its line, plus one test per reason.
  - `crates/greengate/tests/it/run_tests_js.rs`: the JS wording.
- [ ] Phase 3 — a declared root covers its members, and a possible repeat
  is announced.
  - `crates/greengate-runtime/src/manifest.rs`: `rootRunsMembers` joins the
    `set` allowlist: policy, strict, once, like `onBusy`; a comma-separated
    list of the gate's script names, or `none`.
  - `crates/greengate-runtime/src/hooks/js_workspace.rs` (new): the
    workspace globs and member coverage, with no list of runners or flags.
    - std-only hand-written parsers, because the crate has no dependencies;
    - any form they cannot read counts as not covered;
    - a pnpm root reads `pnpm-workspace.yaml` only.
  - `crates/greengate-runtime/src/hooks/run_tests.rs`: member coverage in
    `run`, the summary line, the hint and the per-run announcement.
  - Unit tests in `js_workspace.rs`: the glob negations, an unreadable
    workspace file, and a member path with a newline.
  - Tests in `crates/greengate/tests/it/run_tests_js.rs`:
    - declared root: each member's test logs once, and the summary prints;
    - undeclared root: the members run and the hint prints once;
    - `set rootRunsMembers none`: the members run and no hint prints;
    - a declared script the root does not define: nothing is skipped;
    - a fixture outside the globs runs.
- [ ] Phase 4 — docs and release 3.1.0 after Phases 1–2, a minor version
  because commit gates fire on more files; Phase 3 ships as 3.2.0.
  - Docs: `docs/checks.md` ("Moving a gate entry earlier") and `CHANGELOG.md`.
  - Version bump: `Cargo.toml`, `Cargo.lock`, `greenharness.toml`.
  - Release: tag it with `tag-release` and verify the published version.

## Decision log

- 2026-11-10 — ADR-0019 is unchanged: the tip's stamp still gates a covered ref. This plan only makes more tips stamped.
- 2026-11-10 — Phase 3 is separable from Phases 1-2 and ships as 3.2.0, after 3.1.0.
- 2026-11-10 — Widen a pair's commit trigger to its twin's scope, rather
  than warn and leave each repository to widen its own line, because the
  person chose it and ADR-0009 puts the suite at commit.
- 2026-11-10 — Recognise recursive JS root runners by their command form,
  rather than only announcing repeats, because the person chose it.
  Unrecognised or filtered forms keep today's behaviour. (Superseded
  2026-11-11, below.)
- 2026-11-11 — A repository declares which root scripts run every member
  (`set rootRunsMembers`), and a push that may be repeating says so and
  names the line to add, instead of greengate recognising runner command
  forms, because the person asked for no list to maintain. An LLM at push
  time was considered and rejected: its answer is not deterministic, it
  needs the network in an offline, dependency-free hook, and a wrong
  "covered" skips tests that ran nowhere. An LLM may propose the
  declaration when a repository is set up, for the person to review.
- 2026-11-10 — Name why a pair did not vouch, reversing the deliberate
  silence at run_tests.rs:252-254, because the line says "running it here",
  not partial coverage, so it cannot read as reassurance.

## Verification

Each check below runs the branch binary on a real input. It names the
failure it would show.

1. **Rust scope gap, on a scratch copy of this repository.** Scratch copy:
   `git worktree add --detach`, outside the live checkout.
   - Input: commit a `Cargo.lock`-only change through the hooks, then
     `greengate land --dry-run`, or `rehearse` and then `run pre-push`.
   - Expected: the commit runs `cargo test --locked`, and the push prints
     `gated at commit instead`.
   - Failure it shows: the push runs `pre-push-cargo-test`.
2. **Go scope gap, on a fixture Go module.**
   - Input: `pre-commit go-test *.go block go test ./...`, then commit a
     `go.sum`-only change and push.
   - Expected: the commit runs `go test` and the push is gated.
   - Failure it shows: the suite runs at push.
3. **Reason line.**
   - Input: a `files` pair whose push changes a file outside its scope.
   - Expected: the push prints `⚠ … covers <scope>, not <path> — running it
     here` in at most 80 columns, then the indented line naming the
     `amont.conf` line, and then runs.
   - Failure it shows: a silent run.
4. **JS declared root, on a scratch copy of website-builder**
   (`git worktree add --detach`), with `set rootRunsMembers test,typecheck`
   added and trusted.
   - Input: touch one file in a member package, then run the branch
     binary's `run pre-push`.
   - Expected: the root runs `pnpm -r test` once, one summary line says
     `1 package covered by the root's test (declared in amont.conf)`, and
     the member's own `npm run test` does not run.
   - Failure it shows: the member's `npm run test` runs after the root's.
5. **JS undeclared root, the same copy without the line.**
   - Input: the same touch.
   - Expected: the root and the member both run, and the hint prints once
     with the line to add, its first line at most 80 columns. With
     `set rootRunsMembers none` the hint is gone.
   - Failure it shows: a silent repeat, or a hint on every push after it
     was silenced.
6. **Commit cost.**
   - Input: a `Cargo.lock`-only commit here, and a `package.json`-only commit
     in website-builder, timed with 3.0.1 and with the branch.
   - Expected: commit time grows by the suite's time (written down),
     and push time shrinks by the same amount.
   - Failure it shows: the suite runs at both commit and push.
7. **After release, for 7 days.**
   - Count pushes in amont and website-builder that print `gated at commit
     instead`, against those that run the suite. Target: 0 silent reruns.
   - Measure the median pre-commit time on lockfile-only commits.
   - Roll back (release the previous behaviour as a patch) if that time is
     over 2x the median `cargo test --locked`, or if a skipped JS member's
     tests ran nowhere.
8. **Release.**
   - Expected: crates.io, the GitHub assets and the tap at 3.1.0, and
     `brew upgrade greengate` installs 3.1.0; the same again at 3.2.0 after
     Phase 3.

## Outcome

<!-- panel: repos=amont adds=contract reviewers=architect,backend,lang:rust,po,tui,unix body-sha=7fa7466e087c -->
