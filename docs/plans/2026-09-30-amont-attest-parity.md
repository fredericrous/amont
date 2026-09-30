---
status: done
branch: fix/attest-mirror
repos: [amont, attest]
adrs: []
---
# amont + attest: 1.4.0 parity and remote-call hardening

## Review panel

👉 **Decide:** none — approve if amont's `covered` may delete a local notes ref that origin no longer has, and judge nothing when origin cannot answer.
📍 amont + attest · done: amont 1.45.0 (#285, #286), attest 1.4.1 (#21, #22). Panel: backend, language:rust, tui, unix.
**Changed by review:** attest joins the plan (stale lock + askpass also hit 1.4.0); fetch into a throwaway ref with CAS, judged on a failed swap only if it equals the fetched oid; every non-answer says why, copyable undo lines.
📄 Full reviews: [2026-09-30-amont-attest-parity.reviews.md](2026-09-30-amont-attest-parity.reviews.md)
**Verdicts:** round 1 — 4 approve-with-changes (merged); round 2 — backend approve after two re-binds. Carried: `kill` under LC_ALL=C and gone only on "no such process" (done in code); the origin-unchanged stale-lock test also covers the concurrent-writer branch.

## Context

attest 1.4.0 (released 2026-09-30, tag `v1.4.0` = `ac32aa2`) changed two
contracts that amont also implements in `crates/amont-runtime/src/attest.rs`.
The reference is pinned read-only at `../attest-ref-v1.4.0` (detached at
`ac32aa2`; the live `attest` checkout is stale at 1.3.1 and must not be read
as the contract): SPEC.md:257 (optional paths), SPEC.md:334 ("Which refs are
read"), src/attest.rs:516 `PathTok`, :550 `path_error`, :273 `sync_mirrors`,
:690 `fingerprint`, src/git.rs:106 `remote`, verify.sh:180–266.

1. **Optional `?path`**: amont's `parse_spec` rejects `?` as a wildcard, so on
   a repo whose spec uses it (attest itself: 5 gates) the pre-push producer
   writes no fingerprints — tree-keyed only. Fails safe, loses skips.
2. **Mirror contract**: `amont attest covered` fetches best-effort
   (attest.rs:652, result ignored) and judges whatever the local ref holds, so
   a ref revoked on origin keeps covering on a persistent clone, an
   unreachable origin silently judges a stale copy, and the fetch may prompt
   or hang.

The panel found two defects that attest 1.4.0 ALSO has, so this plan fixes
them in both repositories:

3. **A killed fetch can wedge the ref.** The deadline kills git with SIGKILL;
   a fetch writing straight into `refs/notes/amont-attest` can leave
   `amont-attest.lock` behind, and every later fetch and delete then fails —
   coverage lost for good on that clone, silently.
4. **`GIT_TERMINAL_PROMPT=0` does not stop askpass.** git's `git_prompt()`
   runs `GIT_ASKPASS` / `core.askPass` / `SSH_ASKPASS` before consulting it,
   and an interactive credential manager (GCM) ignores it too: under VS Code
   `covered` can open a GUI prompt and sit out the deadline.

amont's producer `publish()` already builds each push from origin's ref plus
its new blocks (attest.rs:570–611) — unchanged.

## Shared design (both repositories)

- **Fetch into a throwaway ref OUTSIDE `refs/notes/`** —
  `refs/amont-tmp/<pid>/amont-attest` (attest: `refs/attest-tmp/<pid>/<r>`) —
  so no `refs/notes/*` push or pruning fetch ever touches it. Then a LOCAL
  `update-ref refs/notes/<r> <new> <old>` (compare-and-swap against the oid
  read before the fetch; empty old = must not exist); the temp ref deleted on
  every path. At the start of each run leftovers of runs whose PID is dead
  (`kill -0` says no such process) are removed; a live PID's is left alone,
  and where liveness cannot be asked (non-unix) nothing is swept — a
  leftover outside `refs/notes/` is inert. Only the throwaway can be
  half-written by a kill.
- **Failed compare-and-swap**: judge ONLY if the re-read local ref equals
  exactly the oid just fetched (a concurrent fetch or publish of origin's
  content got there first, or origin is unchanged under a lock); anything
  else — a stale lock pinning an old copy, a ref nobody refreshed — is not
  origin's: skip, with the reason. A temp ref that vanished (nothing
  fetched) is a skip too.
- **Stale lock on the main ref** (left by an older version's killed fetch):
  reported with the exact command, `rm <git-path of refs/notes/<r>.lock>`,
  printed by `git rev-parse --git-path`.
- **Remote-call environment** (a pure function, unit-tested both ways):
  `GIT_TERMINAL_PROMPT=0`, `GIT_ASKPASS=` (present and EMPTY: git then runs
  no askpass and does not fall back to `core.askPass`/`SSH_ASKPASS`),
  `GCM_INTERACTIVE=never`, `-c credential.interactive=never`,
  `-c http.lowSpeedLimit=1 -c http.lowSpeedTime=10`, stdin null, and
  `GIT_SSH_COMMAND='ssh -o BatchMode=yes -o ConnectTimeout=10'` unless
  `GIT_SSH_COMMAND`, `GIT_SSH` or `core.sshCommand` is set. 15 s deadline per
  call. Credential helpers themselves are left in place (dropping them would
  lose a laptop's stored credentials).
- **Call budget.** A fetch that TIMES OUT skips `ls-remote` (origin already
  did not answer); a fetch that fails fast asks `ls-remote`. Worst case
  therefore 30 s (fast fail + slow ls-remote), 15 s for a silent host.
  Known: on a kill only the direct `git` child dies; `git-remote-https` /
  `ssh` grandchildren end on their own low-speed / ConnectTimeout limits. A
  user's OWN ssh command runs without BatchMode and may prompt on the tty;
  the deadline bounds that wait, it does not prevent it.
- **Delete on absent.** Only when the local ref exists; compare-and-delete
  with the oid read before the fetch; announced on stderr (not suppressed)
  as two copyable lines, no ellipsis:
  `<tool>: origin has no refs/notes/amont-attest; deleted the local mirror (was <oid>)`
  `<tool>:   undo locally: git update-ref refs/notes/amont-attest <oid>   (undo the revocation: git push origin <oid>:refs/notes/amont-attest)`
  The oid stays restorable until `git gc` prunes it: deleting the ref deletes
  its reflog too (`core.logAllRefUpdates` does log notes refs), so the oid is
  unreachable and `gc.pruneExpire` (default two weeks) sets the window.
- **Every non-answer says why** on stderr: timed out, unreachable (exit N),
  delete failed, stale lock. Stdout and exit code unchanged.

## Changes — amont (one PR)

1. `feat(attest): optional ?paths in the input spec` — `PathTok { path,
   optional }` from `parse_spec`; `bad_path` strips one `?`, a lone `?` is
   rejected explicitly (parity with attest :554); `fingerprint` checks only
   REQUIRED paths with `cat-file --batch-check` and compares the answer count
   against that number; stripped paths feed `implicit_inputs` and `ls-tree`.
2. `fix(attest): covered follows origin, never prompts` — `git.rs` gains
   `remote_env` (the pure env/args function), `probe_env` (generic: `probe`
   with extra env) and `probe_remote` (`probe_env` with `remote_env`);
   `covered` delegates to `covered_within(…, budget)` for tests and
   implements the shared design for the main ref.
3. `docs: attest 1.4.0 parity` — CHANGELOG `## Unreleased`; docs/ci.md
   `amont attest covered` section.

## Changes — attest (one PR, 1.4.1)

4. `fix: fetch into a throwaway ref, never prompt` — the shared design's
   first two bullets and the lock report in verify.sh `remote_git` /
   `sync_mirrors` and src/git.rs `remote` / src/attest.rs `sync_mirrors`;
   the timeout-skips-ls-remote rule; SPEC "Which refs are read" and
   CHANGELOG `## 1.4.1`. Same byte-identical stderr across both
   implementations, as 1.4.0 fixtures assert.

Non-goals: `--include-local` in amont (a failed publish leaves nothing
local); multi-block parsing in `covered` (it reads block 1, documented since
attest 1.2.0).

## Verification (input → expected)

amont (`cargo test -p amont-runtime attest::`), each new test first shown
red on `origin/main` unless marked guard:
- spec `g src ?nope` → `fingerprint` == hash of a hand-built `ls-tree -z`
  listing (`.github/attest-inputs` + `src/…`); then `?nope` created →
  a different fingerprint.
- grammar: `?`, `??x`, `?/x`, `x?` rejected; `?a/b` accepted, stripped.
- bare origin, ref revoked (deleted) after a push → `covered` = None,
  `git rev-parse --verify refs/notes/amont-attest` fails, stderr has the
  "deleted the local mirror (was <oid>)" line; running the printed undo
  command restores the oid.
- origin set to `file:///nonexistent` with a valid local note → None and an
  "unreachable" stderr line (red on main: main judges the stale copy).
- `GIT_SSH_COMMAND` = a script sleeping 60 s, ssh URL, budget 2 s → None in
  < 4 s, no `refs/notes/amont-attest.lock` left, a later good fetch works.
- env function: with and without a user ssh override → exact env/argv lists,
  `GIT_ASKPASS` present and empty.
- askpass marker: an in-test `TcpListener` answering every request with
  `401` + `WWW-Authenticate: Basic`; repo `core.askPass` = a script that
  creates a marker file; `probe_remote(ls-remote …)` → marker NOT created;
  the same call through plain `probe` (no remote env) → marker created
  (negative control, so the test cannot pass vacuously).
- stale lock planted on the main ref: origin unchanged → judged (the copy
  IS origin's); origin's note rewritten → None and the `rm` line on stderr;
  lock removed → covered again.
- a leftover `refs/amont-tmp/<dead-pid>/amont-attest` → gone after one run.
- no origin → local judged (guard: main already does this).
- cross-implementation: attest v1.4.0 `sign.sh --no-push` and the built amont
  over attest's own tree → identical `input` lines for all 5 gates.

attest (`make check`): new fixtures — a killed fetch leaves no lock on the
main ref and the next run fetches; `GIT_ASKPASS` marker script never runs
(shell and Rust); a timed-out fetch makes no ls-remote call (fault-git `hang`
mode counts calls: 1). Existing 152/144/117×2 stay green.

CI: both PRs `conclusion=success`; attest's push-to-main run skips; release
runs green (amont: GH/npm/crates/tap per its recipe; attest: assets +
SHA256SUMS, `v1` → tag); released binaries report the new versions.

## Land

amont: worktree `../amont-wt-attest-mirror` (branch `fix/attest-mirror`),
PR → merge when green → release (bump both version strings, `cargo update
--workspace`, rename `## Unreleased`, PR, squash-merge, tag the merge sha
from a worktree at that commit, verify every channel). attest: worktree →
PR → merge when green → release PR 1.4.1 → tag → verify. Remove worktrees,
including `../attest-ref-v1.4.0`.

## Decision log

- 2026-09-30 — The silent-origin test allows < 6 s, timed inside the shared
  working-directory lock, not < 4 s: process spawn and the 50 ms kill poll
  under the parallel suite measured up to ~4.5 s for a 2 s budget; 6 s still
  proves the bound against the 60 s the remote would take.
- 2026-09-30 — stderr lines go through `say`, which a test can capture, so
  the exact wording and the printed undo command are asserted and run.
- 2026-09-30 — A fetch that fails while `ls-remote` exits 0 (origin has the
  ref) gets its own reason instead of "exit 0; no credentials?".
- 2026-09-30 — `remote_env` takes `Ssh::{User, Batch}`, not a bool.
- 2026-09-30 — implementation-review → approve-with-changes; its findings are
  the commit "fix(attest): assert what covered says".
- 2026-09-30 — Two body lines are superseded by what was built: the silent
  origin's bound is < 6 s (entry above), and the askpass negative control is
  a plain `git ls-remote` with `GIT_ASKPASS`/`SSH_ASKPASS` removed rather
  than `probe`, so the repo's `core.askPass` itself is what would run.
  Delta implementation-review (tree 68d8342) → approve-with-changes, low
  items only: these two and naming the gate row's commit (c5229b9).

## Verification record (amont; input → expected → actual)

| check | expected | actual |
|---|---|---|
| `cargo test -p amont-runtime`, gate on c5229b9 | green | 501 passed |
| `a_ref_revoked_on_origin_stops_covering_and_the_mirror_goes` | None, mirror gone, exact line, printed undo restores | as expected |
| `an_unreachable_origin_covers_nothing_and_keeps_the_mirror` | None, "cannot fetch … (exit" line, mirror kept | as expected |
| `a_silent_origin_is_cut_off_and_leaves_nothing_behind` | None within budget, no lock, next fetch works | as expected (< 6 s) |
| `a_stale_lock_on_the_mirror_means_nothing_is_judged` | unchanged origin judged; rewritten → None + `rm` line | as expected |
| `no_askpass_runs_against_an_origin_that_wants_credentials` | marker absent; plain git creates it | as expected |
| `leftover_sync_refs_are_swept` | dead-PID ref gone | as expected |
| `an_absent_optional_path_is_bound_by_its_absence` | fp == hand-built hash; changes when path appears | as expected |
| cross-implementation, attest v1.4.0 tree, 5 gates | identical `input` lines | identical: ci-fmt 68fd66be…, ci-clippy 58f0ee64…, ci-shellcheck 41d8c14e…, pre-push-cargo-test 58f0ee64…, ci-conformance 7f27d2fe… (throwaway `#[ignore]` test vs `sign.sh --no-push`, not committed) |
| PR #285 CI | green on every platform | failed on Windows first (fixture identity, sweep test); fixed; then success incl. windows |
| release 1.45.0 (#286, tag on e171676) | every job, every channel | release run success; GitHub 7 assets, crates.io, tap 1.45.0, npm `latest` 1.45.0 (after registry lag); local brew upgraded |
| attest 1.4.1 (change 4: #21, #22) | green, released | CI success on all legs; release success, 6 binaries + SHA256SUMS, `v1` → 1.4.1, binary verified |

<!-- panel: repos=amont,attest reviewers=backend,language:rust,tui,unix body-sha=ccb097b53ce4 -->
