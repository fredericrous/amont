---
status: done
branch: feat/attested-plan-commits
repos: [amont]
adrs: [ADR-0022, ADR-0024]
---
# Plan-only commits ride declared-input attestations, in CI and locally

## Review panel

👉 **Decide:** this plan does not sign commits as you asked; it declares attest input fingerprints, which skips the same work in CI and in the local hooks. Approve if that is acceptable.
📍 amont · plan revised after your six points, CI and local hooks in one PR · next: worktree, then the shared fingerprint module (commit 1). Panel: backend (delta, two passes on the final body).
**Changed by review:** `K(gate, fp)` keeps attest's non-object notes keys (blocker withdrawn); `stamp_tree` and `tree_tokens` join the dual write/read; scope and settings fold into the token identity or the declared inputs.
**Verdicts:** 1 approve-with-changes on the final body, 0 rework; 4 findings carried into implementation, listed below. Earlier rounds reviewed the superseded designs.
📄 Full reviews: [2026-10-14-plan-only-commits-ride-attestations.reviews.md](2026-10-14-plan-only-commits-ride-attestations.reviews.md)

## Context

Every PR has two plan commits (ADR-0022): one opens the branch, a closing commit
records verification. The closing commit changes only `docs/plans/*.md`, yet CI
reruns the whole matrix and the local hooks rerun their gates, because every
cache is keyed by the whole tree, which the closing commit changes.

The person asked for amont to sign the first and last commits. A signed commit
could only say "this touches docs/plans", which `git diff` already proves; what
must be proven is that the gates passed on everything else. Attest's declared
inputs do that (SPEC "Input fingerprints"): a repository lists each gate's input
files in `.github/attest-inputs`, and a result is reused on any tree whose
declared inputs are byte-identical. ADR-0024 allows it once input completeness
is proven, which is the guard in Part A. `amont` declares none today.

Outcome, stated narrowly: on the closing commit, CI skips `rust`, `msrv`,
`deps`, both `test` legs and the Windows `cargo test` step, and the local
pre-commit and pre-push do not rerun any gate whose inputs are declared and
unchanged. Still run on purpose: `adr`, `excerpt-fixtures`, `syntax`, `audit`,
and the Windows installer and end-to-end steps (they install the latest
published release, an external input no fingerprint can capture).

👉 This departs from the literal request (sign the commits); the outcome is the
same and no new signing concept is introduced.

## Scope

`amont` only: one PR, reviewable commits in this order. Forgejo repositories'
rollout is separate (other repositories, deployment configuration).

1. **Share the fingerprint logic (no behaviour change).** Move `parse_spec`,
   `spec_at`, `implicit_inputs`, `fingerprint`, `input_key` and their limits out
   of `crates/amont-runtime/src/attest.rs` into `attest_inputs.rs`, used by
   `attest.rs` and (later) `gate_stamp.rs`. The existing attest tests pass
   unchanged, including `the_spec_grammar_is_attests`.
2. **`amont attest check-inputs` (Part A).** Offline validation and coverage,
   built on that module, no dependency on attest's `verify.sh` (which returns 0
   before parsing when there is no allowed-signers file, `verify.sh:143`, so
   silence proves nothing).
3. **Local reuse (Part C).** Declared gates reuse stamps across trees.
4. **Declare the inputs and wire CI (Parts B and D).**

## Part A: the coverage guard

`amont attest check-inputs --require <gate>...` (the gates come from one
`ATTEST_GATES` variable in the `Makefile`, passed by `make check` and by CI).

- Reads the spec from `HEAD` with `cat-file` (never the working copy); both
  locations present, missing or invalid spec: exit 2.
- Every required gate must be declared; a missing gate: exit 1.
- **Coverage is per gate and by real ancestry.** The tracked files come from
  `git ls-tree -r -z --name-only HEAD`. A file F is covered for gate g only if
  g declares a path D (optional `?` included) with F == D or F under `D/`. So
  `crates/foo` never stands for `crates`, and a path omitted from one gate is
  not rescued by another gate (no union). The only exemption is `docs/plans/`.
  Uncovered files are listed per gate, exit 1.
- Fingerprint limits are checked here too (at most 64 paths per gate); a gate
  that cannot be fingerprinted on `HEAD` (a required path missing) fails.
- Output: `check-attest-inputs: ok` on stdout; diagnostics on stderr as
  `check-attest-inputs: <gate>: not covered:` then the paths, indented, then
  both fixes (declare it for each gate; or, only for paths no gate reads, ask
  for an allowlist entry in code review). Exit 0 / 1 / 2.
- Tests (fixtures in `mktemp -d`, git config isolated): `crates/foo` declared
  without the rest of `crates` fails; a path omitted from one of two gates fails
  while the other covers it; a missing gate declaration fails; `docs/plans`
  exempt; undeclared `docs/` child fails; non-ASCII name covered only by an
  ASCII ancestor; `?` optional path; invalid, ambiguous and missing spec each
  exit 2; the exit-0-before-parsing case (no allowed-signers file) still reports
  an invalid spec.
- `make check` gets it from the workspace build (`cargo run -p amont -- attest
  check-inputs`); CI runs it in the `rust` job, which already builds.

## Part B: the CI signing lifecycle

CI runs `push` only on `main`; a branch push runs through `pull_request`. So
signing happens on `pull_request` (same-repo only) and on `push` to `main`;
a fork PR only verifies. "Trusted" means exactly: the head repository equals
this repository, so the author can already push branches here. A PR author can
edit `ci.yaml` and `allowed_signers`; that is the same trust as write access
(`main` has no branch protection) and the docs say so. The merge to `main`
re-verifies with `allowed_signers` from `main`'s tree, so a signature made with
a key added in a PR covers nothing there.

1. Each heavy job (`rust`, `msrv`, `deps`, `test` legs, `windows` cargo test)
   verifies with `fredericrous/attest@v1` (not on `schedule`), guards its
   expensive steps with the `gates` output, and, instead of signing, writes a
   result file `{gate, platform, tree, sha, outcome}` where `tree` is its own
   `HEAD^{tree}` and `platform` is the runner's, uploaded as artifact
   `attest-result-<gate>` (one unique name per gate and matrix leg, so nothing
   is overwritten or confused).
2. One `sign` job, `needs:` all of them, `if: always()`, `contents: write` in its
   own `permissions:` block, runs no PR code: it checks out the same `github.sha`,
   downloads every result, and signs a gate only when its result says success,
   its `tree` equals the sign job's own `HEAD^{tree}`, and its `platform` is the
   one the workflow declares for that gate.
3. **Platform-bound blocks.** The payload carries one platform, so the job calls
   `fredericrous/attest/sign@v1` once per platform with that platform's
   validated gates and an explicit `platform:` input (a stable `producer` per
   platform, so a rerun recognises its block). One writer, sequential pushes
   (`attempts` retry), no concurrent writers on the notes ref.
4. Gates: `ci-rust`, `ci-msrv`, `ci-deps`, `ci-hooks-ubuntu`, `ci-hooks-macos`,
   `ci-windows-test`. `ci-hooks-<os>` is reported success only after the existing
   reporter confirms no tool-gated case was skipped. The locally signed
   `pre-push-cargo-test` stays declared as well.
5. After each verify step, an `if: always()` step prints one plain line per gate
   to the log and `$GITHUB_STEP_SUMMARY`: `<gate>: skipped, a signature covers
   this tree's declared inputs (ADR-0024)` or `<gate>: running, no valid
   signature`. It names no commit: the verify action exports covered gates and
   note status, and signed payloads identify trees, not commits. Extending
   attest's output contract is a non-goal.

## Part C: local reuse for declared gates

Today `gate_stamp.rs` stamps pass results by tree in `refs/notes/amont-gate`
(tokens `<id>@<identity>`); `stamps_for` (`gate_stamp.rs:582`) feeds
`pair_verdict` (`hooks/run_tests.rs:156`), `vouched_for_staged_tree`
(`gate_stamp.rs:437`), the pre-push path (`dispatch.rs:992-1023`) and the
rehearsal (`rehearsal.rs:509-525`).

- For a gate the spec declares, every writer (`record`/`bind_to_head`,
  `stamp_push`, `stamp_tree`) also files the full namespaced token under `K(gate, fp)` in a
  second ref, `refs/notes/amont-gate-inputs`, with `fp` from the shared module.
  Every reader (`stamps_for`, `vouched_for_staged_tree`, `tree_tokens`, rehearsal) honours a
  token if the tree note has it or the inputs ref has it under that tree's
  fingerprint. The identity in the token still has to match, so a changed
  command or tool is a miss. Gate scope and opt-in settings (`amont.conf`, manifest) are folded into the identity or declared as inputs of every locally stamped gate, so a scope-only change misses (tested). Undeclared gates keep whole-tree behaviour; an
  invalid or ambiguous spec means no fingerprint route at all.
- **Which tree.** Pre-commit evaluates the staged tree (`git write-tree`), so
  partial staging is judged on what is committed, not the working copy.
  Pre-push evaluates each pushed tip's tree, and keeps today's rule that every
  relevant commit must carry the stamp.
- Never `git notes prune` the new ref (its keys are not objects); never merge it
  with `cat_sort_uniq`; a gate is stamped only when it passed.
- Gate names declared in the spec are the bare ids the stamps use; the
  enumeration of the repository's local gates (from `amont.conf` and the manifest)
  is done in the implementation and reported, together with any gate left
  undeclared and therefore still run on a plan-only commit.
- Tests: plan-only commit reuses; a changed input, a changed declaration, an
  edited spec blob, or a changed command identity each miss; partial staging;
  a failed gate leaves no stamp; invalid and ambiguous specs fall back to
  whole-tree behaviour; an undeclared gate is unaffected; the fingerprint used
  for a stamp equals the one in an attest payload `input` line.
- ADR: the rule "local gate reuse is keyed by declared-input fingerprints" is a
  lasting decision; an ADR extending ADR-0024's idea to local stamps is offered
  in this PR (`work.lasting-decisions-become-adrs`).

## Part D: declare the inputs

`.github/attest-inputs` lists, per gate, every tracked top-level path
(README.md, CHANGELOG.md, AGENTS.md included: tests may read them),
`rust-toolchain.toml`, `Cargo.lock`, `.cargo`, `templates`, `.github`
(so `ci.yaml` changes invalidate tool installs), and each `docs/` child except
`plans`. The Part A guard is the proof of completeness. A new top-level path or
`docs/` child fails `make check` until it is declared.

## Soundness

- The only readers of `docs/plans` are the `adr` job and a comment in
  `crates/amont-runtime/src/excerpt.rs`; neither is an attested gate.
- A fingerprint binds committed bytes, modes and attributes, not tool versions;
  the gate name, platform and (locally) the identity in the token carry that.
  Runner-image drift is the residual CI risk, as for `windows` today.
- Fail-open everywhere: any error means the gate runs.
- `release.yaml` never skips; `audit` (moving advisory data) stays unattested.

## Non-goals

- No commit signing; no change to attest's format, `verify.sh` or output contract.
- No Forgejo repository changes.

## Verification

- `make check` passes, including Part A's and Part C's tests and the guard.
- Local end to end, on this branch: after the code commits, the closing commit
  is made and pushed; record which local gates ran (expect none of the declared
  ones) and which were still run and why.
- CI end to end on the PR: the first run's notes ref holds a block per platform
  for all six gates (a missing block means the closing run cannot skip: fail
  the check); the closing commit's run shows those steps skipped (per-step
  `conclusion`), the reason line in the log, and `adr` plus the Windows
  installer steps still ran. Record runner-minutes against a baseline from the
  last five full runs on `main`.
- Negative: a push changing `crates/`, `templates/hooks/`, `rust-toolchain.toml`,
  README.md or a `docs/` file outside `plans` runs everything; a fork PR and a
  PR that edits `allowed_signers` get `running, no valid signature` on `main`'s
  re-verify; an undeclared top-level path fails the guard.
- `merge-when-green` merges a head whose heavy steps were skipped.
- Rollback: deleting `.github/attest-inputs` returns every cache to whole-tree
  keys; stale notes are inert.

## Decision log

- 2026-10-14: the plan departs from the literal request (sign the commits) on purpose; declared-input fingerprints give the same outcome in CI and locally without a new signing concept.
- Part B step 1 said each heavy job verifies. Shipped as one cheap `verify` job (ubuntu, asks the attest verifier once per platform) because a job-level `if` cannot see the matrix and the `test` job has about thirty conditional install steps; the `test` matrix is built from `verify`'s output. The guard (`make attest-inputs`) runs in `verify`, never skipped: an undeclared new file changes no fingerprint.
- The relais run for commit 1 ended `blocked (baseline_unrunnable)` (cold `make check` over the 3600 s limit at load 13 to 25); the refactor was done directly and verified by the pre-commit gate. A seeded `target/` setup step is the structural fix, outside this plan.
- `CHANGELOG.md` is untouched: the section is written by the release step, and no release was asked for.
- Part B.2 said `sign` "runs no PR code". Superseded: it runs the scripts of the ref under test, so the trust is write access (plan Part B intro), not isolation. What the split gives is separation of duties: a test job holds no key. ci.yaml and docs/ci.md say so.
- The guard reads the staged tree (`git write-tree`) rather than `HEAD`, so a file added in a commit is caught before it is committed; in CI the checkout is clean and that is `HEAD`'s tree.
- An ADR extending ADR-0024 to local stamps keyed by declared inputs (`work.lasting-decisions-become-adrs`) is not written here; whether to add one is the person's call.
- CI cannot skip until two prerequisites exist, both a person's step (Part B.4): the repository secret `ATTEST_SIGNING_KEY` (`gh secret list` shows only the three release secrets) and its public half in `.github/allowed_signers` (only the laptop principal today). Without them `sign` reports `no-key` and every run executes everything, which is the fail-open direction.

## Outcome (observed before the push)

- Guard: `amont attest check-inputs` passes on the real tree for all eight gates; staging an undeclared top-level file fails it for every gate with the path listed (checked, then removed).
- Tests added and passing: 12 in `attest_check_inputs`, 14 in `ci_attest_scripts`, 14 in `gate_stamp` (declared-input reuse), and the 31 existing attest tests unchanged by the move into `attest_inputs.rs`. `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check` clean.
- Local reuse already seen at commit time: `cargo-test`, `adr` and `traits` printed "passed on this exact tree earlier" on a recommit.
- `amont rehearse --wait` with this branch's binary passed (32 min cold, load 22) and stamped `pre-push-cargo-test`; `refs/notes/amont-gate-inputs` holds the fingerprint-keyed note.
- Local end to end: a plan-only commit on top of that tip (tree changed, no declared input changed). Commit time: `adr` and `traits` skipped (documentation only), `cargo-test` not triggered (no `.rs` staged), 26 checks passed. Push time: `amont rehearse --wait` printed "pre-push-cargo-test already stamped on this tree — nothing to rehearse" in 2 s against 32 min for the code tip; the plan-only tree's own note holds only `cargo-test@…`, so the reuse came through the fingerprint route.
- Local gates left undeclared, so still whole-tree on a plan-only commit: `adr`, `traits` (they validate the plans and the decisions, and skip on docs-only commits anyway), `audit-rust` (moving advisory data) and the built-in checks (`agents-md`, `ban-terms`, `secrets`, and the rest of the 17 pre-commit checks), which run per staged file and are cheap.
- Pre-commit failed once on `timing::a_silent_busy_check_answers_to_the_ceiling_not_the_silence_budget` (a CPU-sampling timing test, killed at 4 s while the suite took 454 s at load about 50); unrelated to the diff, and the same commit passed at load 5.

## Implementation review

approve-with-changes, three passes (238k tokens, about 3.5 min); the last two on the fix commits only.
Fixed: the manifest must be a declared input or the route is refused; an identity test that could not fail was replaced; unreadable verifier output is reported as such; a result must sit in the artifact named for its gate.
Deliberate: `sign` is not isolated from the pull request (a script from the base ref would not help; the workflow itself is editable); the ADR is left to the person; `pair_verdict` is not driven end to end in tests (the readers are unchanged).
Open caveat, stated in ci.yaml and docs/ci.md: any job running the ref's code can choose an artifact name; a forged earlier upload makes the real gate's upload fail, visibly.

<!-- panel: repos=amont reviewers=backend body-sha=2fcd028f579a -->
