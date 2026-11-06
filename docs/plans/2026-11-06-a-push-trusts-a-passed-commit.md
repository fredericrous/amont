---
status: active
branch: fix/tip-tree-verdict
repos: [amont]
adrs: [ADR-0019, ADR-0015, ADR-0016]
---
# A push trusts a passed commit with the same code

## Review panel
👉 **Decide:** none — approve if a tip whose own stamp key holds the pair token may skip the push suite, older unstamped commits notwithstanding.
📍 amont · plan ready, nothing built · next: Phase 1 in `amont-wt-tip-tree-verdict`. Panel: backend (opus-5-5), lang:rust (opus-5-5), tui (opus-5-5), unix (opus-5-5), codex.
**Changed by review:** Codex replaced the scope diff with the tip's existing stamp key (declared inputs, gate scripts); placement after the `covers_all` guard; the tip SHA in the gated line.
📄 Full reviews: [reviews](2026-11-06-a-push-trusts-a-passed-commit.reviews.md)
**Verdicts:** 4 approve-with-changes in round 1; backend approve-with-changes on rounds 2–4; Codex approve-with-changes; 2 carried into implementation (the ~209 min claim overstates, since rebased tips do not qualify; the suffix names the tip itself).

## Goal
A push skips `pre-push-cargo-test` (and any declared pair) when a commit in
the pushed range passed the commit-time gate on the same code the tip
carries, even if an older commit in the range never earned a stamp. The
programme `decisions/docs/programmes/proportional-ship` ranks this first by
time saved: its t00 audit counted ~209 of 516 min of pre-push suites spent
re-testing code that had passed, and t09 traced it to `pair_verdict`.

## Context
- `pair_verdict` (`crates/amont-runtime/src/hooks/run_tests.rs:160-283`)
  returns `Unstamped` as soon as ONE commit in the range touches the pair's
  scope without a stamp (`:256-261`, `:276`). It never asks whether the tip's
  code passed.
- Unstamped commits come from relais's hookless `commit-tree`, from a rebase
  (it runs no pre-commit), and from busy-host deferrals. relais #385 re-ran
  19–24 min per push this way. amont #409 re-ran on a stamped tip.
- Commit-time gates are not all whole-tree. A `files`-marked declaration gets
  only the commit's matched paths in its argv (`docs/custom-checks.md:108-127`,
  `manifest.rs:1002` `files_to_judge`). Its stamp vouches for those files, not
  for the tree, so per-commit coverage stays correct for it. ADR-0016 already
  treats a gate without `files` as judging the whole tree
  (`PlatformAnyWithFiles`, `manifest.rs:104-106`).

## Non-goals
- Re-running only checks whose inputs changed after a rebase (programme t02,
  a later plan).
- The fail-fast pre-commit (programme t01, a later plan).
- Any change to the push-side tokens, `Skip::Stamped`, CI attestation, or the
  deferral semantics of a range with no passing commit.
- Finding a stamp on commits outside the pushed range (a pre-rebase
  original).
- The npm `GATE` pairing (`run_tests.rs:620-657`), which keeps its own
  per-commit rule: on the same history it still prints "no record of it —
  running it here". A later plan aligns it.

## Code review
- amont: backend, lang:rust, tui (unix left out: no command or flag changes; one printed line gains a suffix)

## Behaviour
- For each pushed ref of a pair WITHOUT the `files` marker, the ref counts as
  covered when the TIP's own tokens include the pair token, as
  `crate::gate_stamp::stamps_for(&[tip])` already reads them: the tip
  tree's note, its declared-input fingerprints (ADR-0011) and its canonical
  tree, which ignores the content of plans (ADR-0012). This reuses the
  existing stamp-key contract unchanged: a declared input, a repository-owned
  gate script or `amont.conf` that changed since the passing commit gives the
  tip a different key, so it vouches for nothing and the gate runs. No new
  diff is computed, and a git that will not answer reads as "no stamp", as
  everywhere in `stamps_for`.
- A covered ref adds nothing to `unstamped` or `deferred`. When every judged
  ref is covered, the verdict is `Gated` and the push prints the existing
  "`<name>` gated at commit instead — not repeating it here", followed, when a
  ref was covered by its tip rather than by every commit's stamp, by
  "(tip `<short-sha>` passed on the same code)". `docs/checks.md` quotes the new
  form.
- Otherwise the current rule applies unchanged: per-commit stamps, deferrals
  excused under `onBusy defer`, `Unstamped(n)` naming the count.
- A `files`-marked pair keeps the per-commit rule only.
- `docs/custom-checks.md` gains one paragraph: a declaration that judges only
  `$AMONT_FILES` must carry the `files` marker, or a later passing commit on
  the same code vouches for it. The same holds for a gate that picks staged paths itself (`lint-staged`, `git diff --cached`/`--staged`). A scan of the fleet's live `amont.conf` files and `scripts/` for `AMONT_FILES|lint-staged|diff --cached|--staged` found 3 lines in 27 repositories: `pre-commit suppressions * block amont-agent suppressions --staged` in amont-agent, decisions and grh. None has a `pre-push suppressions` twin, so `pair_verdict` never judges them today; the paragraph names this case.
- `ADR-0019` records the rule (key `hooks.pair-covered-by-content`), citing
  ADR-0015's promise that a pushed exact tree skips the suite, and ADR-0016's
  whole-tree reading of an unmarked gate.

## Phases
- [x] Phase 1 — `GateDecl` gains `files: bool`, filled from
  `Kind::Runnable { files, .. }` in `blocking_commit_decls`; `pair_verdict`
  checks the tip's tokens per ref inside `for r in push.get()`, after the
  `relevant.is_empty()` and `covers_all` guards (`run_tests.rs:229-243`) and
  before `commits_and_files_for` (`:244`);
  `PairVerdict::Gated` carries the vouching tip's short SHA, matched at
  `dispatch.rs:1364` and printed at `:1519`; integration tests in
  `crates/amont/tests/it/gate_pairs.rs`; `docs/custom-checks.md` and
  `docs/checks.md`; `adr/0019-…` plus `aval heads --write`.
- [ ] Phase 2 — release amont 2.0.1 (`tag-release`; push the tag with `-c amont.pushGates=inline` until #204), install it on the laptop
  and re-bake the hooks.

## Decision log
- 2026-11-06 — Codex's final review: comparing only the trigger scope
  between a passing commit and the tip ignored declared inputs and
  repository-owned gate scripts. The rule now asks the tip's own stamp key,
  which already covers plans-only differences, instead of diffing.
- 2026-11-06 — implementation: the decision key is `hooks.pair-covered-by-tip`
  (registered in `.adr.yaml`, scope pre-push), not `-by-content`, since the
  rule became the tip's own key after Codex's review.
- 2026-11-06 — implementation: the `attest-inputs` variant of the
  changed-input test is not written. The repository-owned gate-script case
  exercises the same fallback, and declared-input keys are `stamps_for`'s own,
  already tested in `attest_check_inputs.rs`.
- 2026-11-06 — the tip rule applies only to pairs without `files`,
  because a `files` gate's stamp vouches only for the files of its own commit.

## Verification
- **Tests in `gate_pairs.rs`:**
  - a stamped tip over an unstamped `.txt` commit → gated, and the gate runs
    once (commit time only). This replaces the second half of
    `an_opt_in_marker_does_not_hide_an_unstamped_commit`, which asserted the
    old rule. That test's first half (an unstamped tip still runs) stays.
  - a tip that only modifies an existing `docs/plans/` file, over a stamped
    commit, with an unstamped scoped commit below and no `attest-inputs` spec
    (the canonical route) → gated.
  - a stamped commit followed by an unstamped commit that touches the scope →
    runs, naming `no record of it`.
  - a `files`-marked pair, stamped tip over an unstamped commit → runs.
  - a stamped tip whose changed files include one the push-side scope covers
    and the pair's scope does not → runs, with no "gated" line (`NotPaired`).
  - an unstamped scoped commit, then a stamped commit, then a tip changing
    only a repository-owned gate script outside the trigger scope → runs: the
    tip's key differs, so the per-commit rule decides (Codex).
  - the same with the tip changing only a declared input of an
    `attest-inputs` spec → runs.
  - under `onBusy defer`, a stamped tip over a deferred commit → gated, with no deferral reported.
  - two refs, one covered and one with an unstamped scoped tip → runs,
    `Unstamped(1)`.
  - revert-red: the gated tests (stamped tip, plans-only tip, deferral) fail
    with the tip check removed; the `files` test fails with the `files` guard
    removed. The tests that expect a run pass either way; they guard the
    fallback.
- **Real input** (relais #385's shape, with real cargo): in a scratch
  `cargo new` crate under `$TMPDIR`, declare
  `pre-commit cargo-test *.rs block cargo test`, with a `cargo` shim first on `PATH` that appends a line to `runs.log` and runs the real cargo (counting the commit side and the built-in push side alike), and run `amont trust` and
  `amont init` with the branch binary (`init` copies no binary). Then:
  - add a relais-style hookless commit, made with `git commit-tree`, that
    edits `src/lib.rs`;
  - add a hooked commit on top that edits `src/lib.rs`, which passes and stamps (`amont stamps --tree HEAD` shows `cargo-test` stamped);
  - add a commit that only modifies an existing `docs/plans/probe.md`,
    committed in the baseline before the range;
  - feed the branch binary's `pre-push` dispatcher the ref line.

  Expected: `cargo-test gated at commit instead`, and `runs.log` holds 1 test
  run. The installed 2.0.0 binary, on the same repository, runs it again: 2
  test runs.
**Observed (Phase 1, 2026-11-06, branch `fix/tip-tree-verdict`):**
- `cargo test -p amont --test it -- gate_pairs busy_defers`: 12 + 14 pass. The
  new tests are the stamped tip over an unstamped commit (the replaced half),
  the plans-only tip, the unstamped scoped tip, the `files` pair, the scope
  guard (`NotPaired`), the changed gate script, the two refs, and the stamped
  tip over a deferred commit.
- Revert-red, with the tip check disabled: 4 fail
  (`an_opt_in_marker_does_not_hide_an_unstamped_commit`,
  `a_plan_only_tip_over_a_stamped_commit_is_gated_despite_an_unstamped_one`,
  `two_refs_one_covered_one_not_runs_for_the_one`,
  `a_stamped_tip_over_a_deferred_commit_is_gated`), and the run-expecting
  tests pass, as stated.
- Real input, `/tmp/claude-501/tipprobe.sh`: real cargo through a counting
  shim, scope `*.rs,*.toml,Cargo.lock` as in relais. The sequence was a
  hookless `commit-tree`, then a verified commit, then a plan edit.
  - Branch binary at push: `✓ cargo-test gated at commit instead — not
    repeating it here (tip e5d6dda4 passed on the same code)`, with no
    `cargo test` at push.
  - Installed 2.0.0, same history: `! cargo-test is declared at commit time,
    but 1 pushed commit carries no record of it — running it here`, one more
    `cargo test` (3 runs in all against 2).
  - Both probes also ran the push gates once in the background after the
    verified commit (clippy `--workspace`, `test`, audit), which is why the
    commit-side count is 2 and not the planned 1. It happens on both
    versions; it is noted for the programme, not fixed here.
  - A first probe with scope `*.rs` stayed `NotPaired`: the commit added
    `Cargo.lock`, which that scope does not cover. That is the guard working.
- **After implementation review (round 1 fixes):**
  - The tip line prints only when a ref had an unstamped commit. It is a
    second indented line, `the code at tip <sha> passed at commit`, and names
    every such tip. A fully stamped range prints the plain line alone
    (asserted in `a_builtin_push_gate_is_gated_by_its_commit_time_namesake`).
  - New test: `a_changed_declared_input_at_the_tip_does_not_vouch`, the
    `attest-inputs` route. The decision-log entry that skipped it is
    superseded.
  - `gate_pairs` + `busy_defers`: 27 pass.
  - Revert-red, tip check off: the same 4 fail.
  - Revert-red, `!pair.files` removed:
    `a_files_pair_never_lets_its_tip_vouch_for_the_range` fails.
- **After Phase 2:** `amont --version` prints 2.0.1, and a push of a hookless
  commit below a stamped tip prints "gated at commit instead" with the
  vouching commit's short SHA.
- **14 days after install:** re-run the t00 audit. Expected: the pre-push
  suite minutes spent re-testing passed code fall from the baseline of ~209
  of 516, with the numbers reported.

## Implementation review
- implementation-review (opus): approve-with-changes, then Delta approve-with-changes. Fixed: tip line only with an unstamped commit, `files` revert-red recorded, `attest-inputs` test. 4 candidates fine. 57k+68k tokens, 56+16 s.
- backend (opus): approve-with-changes, then Delta approve. Fixed: `attest-inputs` test, gate-script test reason and counts, `holds-until` at the `files` guard. 54k+69k tokens, 117+21 s.
- lang:rust (opus): approve-with-changes, then Delta approve-with-changes. Fixed: `Coverage` enum and struct variant, shared `gate_evidence::short`, one stamps read. 59k+71k tokens, 40+15 s.
- tui (opus): approve-with-changes, then Delta approve. Fixed: the second line within 80 columns, "the code at tip", every tip named. 58k+65k tokens, 72+12 s.
- deliberate: the stale `Skip::GatedAtCommit` doc comment (implementation-review and lang:rust Delta, low) is fixed in the next amont change, the fail-fast pre-commit (programme t01), which edits `dispatch.rs`. Re-running the 16-min gate and the review round for a comment is the cost this programme cuts.

## Outcome

<!-- panel: repos=amont adds= reviewers=backend,language,tui,unix,codex body-sha=be41429325d9 -->
