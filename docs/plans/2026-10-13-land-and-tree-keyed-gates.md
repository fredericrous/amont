---
status: done
branch: feat/land-verified-push
repos: [amont]
adrs: [ci.skip-needs-the-same-command]
---
# Plan: tree-keyed gate cache gaps + `amont land` (verified landing)

## Review panel

👉 **Decide:** none — approve if closing the cache holes plus a verified `amont land` in `amont` alone (skills and amont-agent as follow-ups) is the right size.
📍 amont · plan reviewed twice, rejected once and revised · next: worktree, Part A commit then Part B. Panel: backend (opus-5-5), rust (opus-5-5), tui (opus-5-5), unix (opus-5-5).
**Changed by review:** namespaced tokens with a defined identity per gate (unknown identity is a miss); exit 4 means "expected tip not observed"; the effective push URL is resolved and verified, multiple push URLs rejected.
**Verdicts:** 5 approve-with-changes, 1 rework (tui, resolved); 13 findings carried into implementation, listed below. The person's delta (6 items) was applied and re-reviewed.

**Carried into implementation (override the body where they differ):**
- Use one name `<rb>` for the remote branch (upstream's name, else `<b>`) in the refspec, lease, tracking ref and upstream; update `refs/remotes/<remote>/<rb>` only on exit 0, in step 5, not in step 1.
- Retry semantics: the hook only fast-forwards a strictly-behind branch, so the retried HEAD is the remote tip and lands nothing new; message is `retrying once: pre-push hook fast-forwarded HEAD <old7> -> <new7>`, not "rebased trees are new". Test that gates do not re-run on the stamped tree.
- Fixtures: first landing (tracking ref and `@{u}` set, refused landing leaves the tracking ref alone); rename `feat` with upstream `origin/feature-x`.
- Unreachable stderr markers with a unit test; TTY push can block (ADR notes it); `--json` status asserted per exit code; verdict line plain text.
- 📄 Full reviews: [2026-10-13-land-and-tree-keyed-gates.reviews.md](2026-10-13-land-and-tree-keyed-gates.reviews.md)

## Context

The article "Redesigning source control at Anthropic" (Scion) rests on two ideas that fit our failures:
a ref move is a compare-and-swap that is checked afterwards, and results are keyed by content, not by history.

Exploration of `amont` / `amont-agent` found that **half of this already exists**, so the plan is smaller than the idea:

- Gate results are already cached by git tree id as notes under `refs/notes/amont-gate` (`crates/amont-runtime/src/gate_stamp.rs`). A reword, `reset --soft` or forge squash with an identical tree reuses the stamp. What is missing are three holes (below).
- There is **no landing command**. A push is a bare `git push` behind the pre-push hook. Nothing in amont re-reads the remote afterwards; the only check is the advisory amont-agent assertion `push_landed` and the by-hand recipe in the `relais-verified-push` skill. This is the "Forgejo accepted the push, the ref did not move" and "`| tail` hid a dead push" class of failure.

Outcome: a pre-push gate that already passed for a tree is never re-run because of an amend/rebase/commit-note quirk or a changed tool, and one command pushes a branch and proves the remote ref moved.

## Scope

One repo (`amont`), one PR, two commit groups. Release follows (`tag-release`, minor bump, ADR-0029).
Out of scope, listed as follow-ups: amont-agent changes, the `relais-verified-push` / `worktree-task` skill edits (they live outside the repo), CAS on `main` (branch-protect still forbids it). Caveat: the lost-ref failures seen so far were on `main` (#886), which `land` refuses, so exit 4 is proven by a shim first and on a real branch push only opportunistically.

## Part A: close the three cache holes (`gate_stamp.rs`, ADR-0024 alignment)

1. **Merge, don't prefer.** `stamps_for` (`gate_stamp.rs:630-637`) reads the commit note and only falls back to the tree note when the commit has none. After an *amend with an unchanged tree* post-commit writes a commit note holding only the commit-time scripts, which hides the tree note's push-time tokens, so the push gate re-runs. Change to the union of both notes. Sound because both are written only for that tree (`bind_to_head` checks `HEAD^{tree}`).
2. **Put the command and tool in the key.** Today the key is the tree alone. Each stamped gate gets an *identity*, and a token is written as `<script>@<identity-hash>`:
   - **Declared external checks** (`manifest.rs:180`, program + args, no `TreeTool`): the hash covers the full command line, the resolved program path, the output of its version probe (`<program> --version`, bounded), and the lockfile/config blobs it declares via `inputs=`.
   - **Built-in gates** (test gates etc.): the hash covers the gate name, amont's own version and the built-in's declared inputs.
   - **Unknown identity** (version probe fails, times out, or the program is missing) means no token is written and none is honored: a cache miss, never a skip.
   - The hash is computed by a new `tree_cache::namespace_for(tree, identity)` that reads `ls-tree -r <tree>` (the existing `namespace` reads the index, `tree_cache.rs:157`, which can differ from the pushed commit), with `hash` made `pub(crate)`, material normalized to `<mode> <oid>\t<path>` for both sources, computed once per tree per push, plus a unit test that `namespace_for(write-tree)` == `namespace` on a clean index. Per ADR-0024 `ci.skip-needs-the-same-command`.
   - **Matching:** every reader that authorizes a skip (`pair_verdict` in `hooks/run_tests.rs:237`, `vouched_for_staged_tree`, `stamps_for` consumers, rehearsal) compares the *complete* namespaced token. The pre-commit marker writer (`gate_stamp.rs:404`) and `bind_to_head` write namespaced tokens too; the namespace is stripped before `bypass` accounting. Bare names are used only for display and accounting. A legacy bare token is not honored by the new binary (cache miss, one re-run).
   - **Compatibility, stated narrowly:** `FORMAT` stays `amont-gate-v1` so `parse` keeps notes written by either version (a bump makes `parse` drop the other version's note, `gate_stamp.rs:271-272`). An older binary preserves the namespaced tokens but still matches legacy bare tokens in the same note by its own rules; that is documented in the ADR, not claimed as "re-runs". Tests mix bare and namespaced tokens in one note.
   Land this as its own commit so A.1 can ship without it.
3. **`amont run` for pre-commit checks.** `run_all` / `run_named` write no marker. ADR-0003 `hooks.fix-outcome` covers fixing/restaging, not stamping, so it does not justify the gap. Decide by reading the code: either stamp from `amont run` on a clean tree with the same rules as the hook path, or record in the new ADR an explicit non-goal with its own reason. Either way a test pins the choice.

Files: `gate_stamp.rs`, `tree_cache.rs`, `dispatch.rs` (`pre_push` 1013-1084, `stamp_tips`), `manifest.rs:707-722`, `hooks/run_tests.rs:221-238`, `rehearsal.rs:509-534`, `docs/configuration.md`, CHANGELOG.

## Part B: `amont land`

A new verb (the verb table is `crates/amont/src/main.rs:178-195`; implementation in a new `crates/amont-runtime/src/land.rs`).

Behaviour:
1. Resolve branch and remote (upstream, else `origin` with the remote branch named like the local one; after every verified landing run `git update-ref refs/remotes/<remote>/<b> <sha>` (a URL push updates no tracking ref; the `attest::sync_mirror` update-ref pattern) and, on the first one, `git branch --set-upstream-to=<remote>/<b>`, since a `<sha>:` refspec cannot take `-u`). Refuse `main`/`master` (branch-protect already forbids; fail early with the same message) and a dirty tracked tree, each naming the fix (`git switch -c <b>`; `git stash` or `gwt-stash-save`).
2. **Resolve the effective push endpoint** with `git remote get-url --push --all <remote>` (honours `pushurl` and URL rewriting). More than one push destination is rejected in v1 (exit 1). Read the remote tip from *that* URL with a new stdout-capturing `git::probe_remote_stdout -> (Probe, String)` (same `remote_env` and deadline loop as `probe_remote`, `git.rs:204-322`, which discards stdout; exit 2 = no such ref, 128 = unreachable kept apart; unit-tested) and record it as `expected_old` (empty when the ref is absent, so a lease means "must not exist"). Step 5 verifies against the same URL.
3. Run `git push <push-url> <sha>:refs/heads/<upstream-branch>` with the pre-push hooks enabled, so the sha that is verified is the sha that was pushed even if HEAD moves. The child's stdout (hook `say!` lines) goes to amont's stderr, and both streams are scanned for classification: stderr is streamed line by line with a copy kept in memory, stdout is forwarded and scanned for the retry marker (never `--quiet`, never `| tail`). When stdin is not a TTY the push runs with `git::remote_env`. **Bound, stated narrowly:** the push has no `land` deadline, because its pre-push hook runs the full gates (minutes). `remote_env` sets HTTP low-speed and SSH connect limits only; custom SSH commands, credential helpers or an established SSH session can still stall, and `land` does not claim otherwise. The 3 s probe budget applies to the probes (steps 2 and 5), and Ctrl-C exits 128+signal. Only the `--json` document is written to stdout. A non-fast-forward push is sent with `--force-with-lease=refs/heads/<b>:<expected_old>` only when the caller passed `--force`; otherwise a plain push, since the server already rejects non-ff.
4. **Retry**, once, only when all three hold: the hook printed the "push again" marker (a `pub(crate) const` shared by `hooks/pull_rebase.rs:278` and `land`, matched on either stream since git may route hook stdout to stderr), HEAD changed during the push, and the new HEAD descends from `expected_old`. HEAD movement alone never authorizes it. `pull_rebase` does not rebase a diverged branch (`pull_rebase.rs:207-232`), so a diverged branch is a refusal with no retry. Print `retrying once: HEAD rebased <old7> -> <new7>; rebased trees are new, so the gates run in full`. Re-read the remote tip into `expected_old` and re-resolve the sha first (the old lease is stale by construction). A second failure exits 1, no third try.
5. Verify: `ls-remote` the same push URL for the branch and compare with the pushed sha, re-reading at most 3 times, 2 s apart (at most 6 s, spent only on a mismatch). Extract the Raced-vs-Refused markers from `attest::push_notes` (`attest.rs:1053-1066`) into `pub(crate) fn classify_push_stderr(&str)`, and add an Unreachable class (`Could not resolve host`, `Connection refused`, `Connection timed out`, `unable to access`, `Could not read from remote repository`) with a unit test over each string; an unclassified failure exits 1 and prints git's stderr verbatim.
6. Exit codes: `0` landed (remote ref == pushed sha), `1` refused (hook, server, main/master, dirty tree, multiple push URLs, failed retry), `2` usage (unchanged, `main.rs:343-346`), `3` unreachable or timed out (probe `TimedOut`/`Failed`, or git said unreachable; also accepted-but-unverifiable, `status:"unverified"`), `4` **accepted, expected tip not observed**: equality proves the tip was observed, inequality does not prove a lost update (another writer may have advanced or replaced the ref), so the observed sha is preserved in `remote_after` and no unconditional retry is advised, least of all after `--force`. On SIGINT/SIGTERM: exit 128+signal, no retry, no "landed" line. Codes are listed in USAGE and `docs/configuration.md`.
7. Output: before the verdict, any recovery commands are printed in full, one per line, with no width limit. The last stderr line is a plain-text verdict of 80 columns or less (no colour or glyphs, unaffected by `NO_COLOR` or a non-TTY): outcome word, `<before7>..<after7>`, and the branch name middle-truncated to fit; hook output is never suppressed. `--json` prints only `{branch, remote, push_url, pushed, remote_before, remote_after, retried, status}` on stdout, where status is one of `landed|refused|unreachable|unverified|not-observed|interrupted|usage`; tests assert the status for each exit code, including the usage error (`--bogus`).

Not a re-implementation of gates: `land` only wraps the push and the proof, so stamps, rehearsal wait and attest keep working unchanged.

Docs and records: new ADR in `adr/` (decision keys for `hooks.gate-stamp-key` and `cli.land-verb`; check `aval keys` first for collisions), `docs/configuration.md`, README verb list, CHANGELOG.

## Reuse (do not rewrite)

`git::probe_remote` / `remote_env` / `network_probe_budget`, `attest::push_notes` markers (extracted), `attest::sync_mirror` update-ref pattern, `pushrefs::synthetic_from_upstream`, `gate_stamp::Note::{parse, render, add_tokens}`, `tree_cache::hash`.

## Verification

Observed on commit 9d96787 (workspace `cargo test --locked` and `cargo clippy --locked --workspace --all-targets -- -D warnings` green; the commit gate, 28 checks, passed on every commit).

- **Part A, `push_stamps.rs`.** (a) amend with unchanged tree, commit note and tree note both present, push: expected 0 gate runs, actual 1 total across both pushes (`an_amend_with_its_own_commit_note_still_reads_the_trees_push_stamp`); the same test fails on the old `stamps_for`. (b) the tool behind a declared command replaced: expected one more run, actual 2 (`a_replaced_tool_runs_the_gate_again_and_an_unchanged_one_does_not`); an unchanged tool is skipped. (c) a legacy bare-token note: expected one run, no error, a mixed note afterwards, then honoured; actual as expected (`a_legacy_bare_token_costs_one_run...`). An unidentifiable tool earns no token (`a_gate_whose_tool_cannot_be_identified_earns_no_stamp`). (d) a staged config edit: not applicable, the `namespace_for` design that read the index was dropped (Decision log). A.3: `amont_run_leaves_no_marker_but_the_pre_commit_hook_does` pins "no marker from `amont run`".
- **Part B, `land.rs`** (20 tests): landed (0), hook refusal text reaches the user (1), non-ff refused and `--force` leased (1/0), lease lost to another writer (1), closed-port remote (3), unchanged old sha via `ls-remote` shim (4, `remote_after` = old sha), concurrent third sha (4, `remote_after` = third), accepted but unreadable (3, `unverified`, no tracking ref), `pushurl` verified (0), two `pushurl`s (1), `main` and a dirty tree refused with the fix named, usage error (2, JSON `usage`), 80-column verdict for a 120-char branch, retry fixtures (behind-only: exit 0, one retry; diverged: 1, none; hook moves HEAD for another reason: 1, none; second failure: 1, exactly 2 hook runs), a gate outlasting twice the probe budget still lands, a renamed upstream (`feat` to `feature-x`) is pushed, tracked and leased under its remote name.
- **Piloted run**, real binary, scratch bare remote (the substitute for a scratch Forgejo repo, which was not used): `amont land` on `feat/demo` printed `landed -------..7ddb91a feat/demo`, exit 0; with `--json`, stdout was one document and `remote_after` equalled `git rev-parse HEAD` and `git ls-remote`; on `main` it printed the refusal and `git switch -c <name>`, exit 1.
- **Release**: not part of this branch; cut on request (`work.release-on-request`).

## Decision log

- 2026-10-14 — A.2's `tree_cache::namespace_for` (an `ls-tree -r` hash of the tree) was dropped, because the note is already keyed by the tree, which holds the manifest, lockfiles and config; hashing them again adds nothing. The identity covers only what lives outside it: the command and the program binary's resolved path, size and mtime (no `--version` run, so no repository script is executed and nothing depends on a tool supporting the flag). A relais run for A.1 ended `baseline_unrunnable` (its `make check` hit the 1200 s limit on a contended machine), so A.1 was done in the session.

- 2026-10-14 — Two promised tests were not written, on purpose: "gates do not re-run on the stamped tree after a retry" (the retried push runs the same hook, and skipping on a stamp is the behaviour `push_stamps.rs` already pins), and Part A (d), the staged config edit (it existed for the index-reading `namespace_for`, which was dropped).
- 2026-10-14 — Kept as deliberate: `classify_push_stderr` reads Unreachable from `fatal:`, `ssh:` and `error:` lines only, so a gate that prints `error: ... Connection refused` is still classed unreachable (exit 3, not 1). If it shows up, require `error:` together with git's `unable to access` or `failed to push`.
- 2026-10-14 — Ctrl-C: amont installs no signal handler (`cli.signals.ctrl-c-exits`: stop at once). The terminal signals the whole process group, so git dies with amont and the shell reports 130; the `Interrupted` status covers git killed alone.

- 2026-10-14 — PR #320's `windows` job failed to compile the `land` tests (`common::fixture_exec` is unix-only), so the shell-script fixtures were gated to unix (d8574e8). Deliberate, for now: `amont land` has no Windows integration coverage, including the five `land.rs` tests that use no hook or shim, and `a_gate_whose_tool_cannot_be_identified_earns_no_stamp` is unix-only because it shares `tooled_repo`. Opening them on Windows cannot be verified from this machine; the follow-up is to split the fixture helpers and watch them run on a Windows runner.

## Implementation review

approve-with-changes (round 1), then approve-with-changes on the Delta with every finding resolved or named deliberate above.
Fixed: results recorded in Verification; seven missing tests added; A.3 reason and pin; swallowed errors now warn; Unreachable limited to git's own lines; `Mode` enum and split `track`; stale comment; typo.
88k + 98k tokens, about 2 and 1 minutes; reviewed tree 195fddc.
After the Windows CI fix, a fresh round on tree 74e7fb5: approve-with-changes, gating confirmed complete, two lows kept as deliberate (Decision log). The closing commit changes only `docs/plans`.

<!-- panel: repos=amont adds= reviewers=backend,rust,tui,unix body-sha=2ed23773fa21 -->
