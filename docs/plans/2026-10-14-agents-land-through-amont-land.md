---
status: done
branch: feat/land-dry-run-receipt
repos: [amont, amont-agent, dotfiles, relais]
adrs: [cli.land-verb, work.implementation-review, work.preview-approval]
---
# Plan: agents land through `amont land`, with every push guard intact

## Review panel

👉 **Decide:** none. Done: all three phases shipped; the only step left is the person's relais plugin reinstall.
📍 amont 1.54.0, amont-agent 2.41.0, dotfiles #40, relais 0.22.0 · closed 2026-10-15. Panel: backend (opus-5-5).
**Changed by review:** the land retry is checked after the fact (a per-invocation landing receipt vs the judged sha, not the tracking ref or exit code); the dry-run runs once per hook with a 3 s timeout; only `[cd <path> &&] amont land [--json] [--force]` in the foreground is resolvable.
**Verdicts:** backend approve after 6 rounds (approve-with-changes ×4); your four P1s (receipt, one supported shape, env and wrappers unresolvable, Bash result) applied and re-reviewed; 5 findings carried into implementation, listed below.

**Carried into implementation:**
- `dry_run` is a flag beside `Mode`, not a third `Mode` value, so `--dry-run --force` stays expressible.
- Verification adds the retry check end to end: a pre-push hook that commits and prints the push-again marker → land retries → the journal records an unreviewed push for the new sha; a refused land journals nothing.
- An empty `tool_use_id` loses the land record: the retry check says it skipped, never passes silently.
- A land whose dry-run refused: the `refused` receipt wins (nothing journaled), never `retry-check skipped`; the Phase 1 `.lock` check names the commit-and-push-again pre-push hook that causes the retry.
- Confirm what Claude Code's PostToolUse and PostToolUseFailure payloads carry for a Bash land on the installed version before the retry check relies on them.
- 📄 Full reviews: [2026-10-14-agents-land-through-amont-land.reviews.md](2026-10-14-agents-land-through-amont-land.reviews.md)

## Context

amont 1.52.0 shipped `amont land` (push a branch, then prove the remote ref moved). The follow-up is for agents to use it: `worktree-task` F6 and `relais-verified-push` still teach `git push` + `git ls-remote`.

That switch cannot come first. Every amont-agent push guard finds a push as the program `git` with the subcommand `push` (`src/push_target.rs:394-401` `find`, and three rules with their own matchers). `amont land` runs `git push` as a child process, so a Claude Code PreToolUse hook never sees it: the implementation-review gate (`work.implementation-review`, a constraint), the preview gate (`work.preview-approval`, ADR-0028) and push-preflight would all be skipped. Switching the skills first would silently turn the review gate off.

The hard part is that land's target is not on its command line: it is computed at run time from git config and HEAD (`amont/crates/amont-runtime/src/land.rs` `land`, steps 1-2). Re-implementing that in amont-agent would drift. So amont answers the question itself: `amont land --dry-run --json` resolves exactly what a land would push, without pushing or touching the network.

Outcome: an agent lands with `amont land`, and every guard that held a `git push` holds the land the same way.

## Order (each step needs the previous one released)

1. **amont** `land --dry-run` → release 1.53.0.
2. **amont-agent** recognises `amont land` → release (minor), brew upgrade.
3. **dotfiles** `worktree-task` F6 and **relais** `relais-verified-push` switch to `amont land` where the installed amont and amont-agent are new enough; relais release, plugin reinstall is the person's step.

## Phase 1, amont: `amont land --dry-run`

- `land.rs`: a `dry_run` flag beside `Mode` (Mode stays Plain/Force, so `--dry-run --force` stays expressible). Runs steps 1-2 of `land` (branch, upstream `<rb>`, the refusals for detached HEAD / main / master / dirty tree / not exactly one push URL) and stops before reading the remote: no network, no push, no ref written.
- Output: `--json` prints `{branch, remote, push_url, pushed (HEAD sha), dst ("refs/heads/<rb>"), retried:false, status, receipt_dir}` with status `would-land` (exit 0) or `refused` (exit 1, reason on stderr). Same verdict line on stderr. `--dry-run` with `--force` reports what would be leased (`lease: true`).
- **Landing receipt**: every non-dry-run land writes one receipt, `$GIT_DIR/amont/land/<started-ns>-<pid>.json` = `{started, ended, branch, remote, push_url, dst, pushed, retried, status}`, written with the final status before exit (also on refusal, unverified and not-observed; a receipt that cannot be written is a warning that names the path, and land still exits by its verdict). It is the evidence of what was pushed, independent of the tracking ref, whose `update-ref` failure land treats as a warning (land.rs:430). Receipts older than 7 days are pruned by the next land.
- Allowlist the flag in `main.rs` `known_flags(Sub::Land)` and in `land.rs` `command`'s argument check (land.rs:213-218, which rejects anything but --force/--json); USAGE and `docs/configuration.md` updated; CHANGELOG.
- Tests in `crates/amont/tests/land.rs`: a receipt per land with the pushed sha and status (landed, refused, not-observed); a retry whose tracking-ref update fails (ref locked with a `.lock` file) still exits 0 and its receipt names the retried sha; dry-run writes no receipt. Cross-platform, no shell, where possible: would-land on a branch with no upstream and with a renamed upstream (`dst` = remote name), refused on main, dirty tree, two push URLs; the remote is untouched and no tracking ref moved.

## Phase 2, amont-agent: `amont land` is a push

- `src/push_target.rs`: `find` returns a git push OR a land clause (`program()=="amont" && subcommand()=="land"`, through `wrapped_by` like git). `resolve` gets a land arm that runs `amont land --dry-run --json` in `cwd_at(clause)` once per hook invocation, memoised for every caller (resolve is called at implementation_review.rs:275, preview.rs:476 and 1680), with a 3 s timeout against the ~19 s hook budget (measured p95 recorded in the PR) and maps it: `would-land` → `Push::Resolved{repo: toplevel, targets: [Target{remote, src: pushed, dst, Branch}]}`; `refused` → a new `Push::Refused(reason)` that publishes nothing (land will refuse it itself, so no guard holds it); amont missing, older than 1.53 (unknown flag → exit 2), or unparseable JSON → `Unresolvable` (held under deny, advised otherwise, as today).
- **One supported shape, everything else `Unresolvable`**: the whole command is `amont land [--json] [--force]`, optionally preceded by exactly one `cd <literal path> &&`. No other clause before or after (`;`, `&&`, `||`), no pipeline, no redirection, no background `&` and no `run_in_background: true` in the tool input (payload.rs:41: PostToolUse would fire before land ends), no subshell or substitution, no env assignment and no wrapper (`GIT_DIR=… amont land`, `env …`, `sudo …`, `timeout …`, `nice …`; the parser strips these at shell.rs:839, so the land arm reads the raw clause). Why: with nothing before it, nothing can move HEAD or the upstream between PreToolUse and the push (no `git commit`, no `git fetch --set-upstream`); with nothing after it, the Bash result is the land's own result and nothing can rewrite its evidence before PostToolUse; with no env or wrapper, the dry-run in `cwd_at(clause)` resolves exactly the repository and identity the land will use. An unsupported shape is held under deny with a message naming the supported one.
- Callers switch to the new `find`: `push_preview` and `implementation_review` (`examine` fires on every land; `confirm` declines when `dst` is main/master, which land refuses anyway), `preview::needs_preview` and `record_before`, `implementation_review::judge_push`, `hook::attributed_repo`, `unbounded_background_push`, `push_preflight::detect`.
- `pipe_to_tail::MUTATING` gains `("amont", ["land"])`.
- `push_landed` and `push_published` skip land: land verifies the effective push URL itself and exits 4/3 when the ref did not move; their `ls-remote` on the remote name would read the fetch URL.
- **Retry escapes the judgement** (land.rs:354-378: a pre-push hook that fast-forwards HEAD makes land push a new sha): PreToolUse records the judged sha for every land clause, before `record_before`'s `!gated(&repo)` early return (preview.rs:1683), with its start time. A PostToolUse assertion, run on PostToolUse and on PostToolUseFailure alike (assertions get only PostToolUse today, assertions/mod.rs:20-24: the dispatcher routes PostToolUseFailure to this assertion and hook registration adds the event; its `skipped` line is a stated exception to assertions/mod.rs:42), reads the land receipt from the dry-run's `receipt_dir` for that branch started after the recorded time (exactly one, since the command holds one land): `pushed` ≠ the judged sha with `landed` → journaled as an unreviewed push; with `unverified` or `not-observed` (push accepted, land.rs:407-421) → journaled as a possibly unreviewed push; `refused`, `unreachable` → nothing journaled (a land killed by a signal writes no receipt, so it falls under the no-receipt rule below); no receipt, more than one, or no recorded judgement (an empty `tool_use_id`, preview.rs:1663) → journaled as `retry-check skipped: <why>`, never a silent pass. The Bash exit code is not used; what Claude Code's PostToolUse and PostToolUseFailure payloads carry is checked against the installed version first.
- Corpus lines (`tests/corpus/*.cases`) for the shapes, end-to-end tests in the style of `tests/preview.rs` (`World`, `pre_bash_in`) with a real amont on PATH (built from the pinned release, or skipped with a message when absent): review held for an unreviewed land under deny and passed after a review; preview held for a UI land; land on main not held; held as unresolvable: `git commit -m x && amont land`, `git fetch --set-upstream origin x && amont land`, `amont land; true`, `amont land && false`, `amont land | cat`, `amont land > out`, `GIT_DIR=/other/.git GIT_WORK_TREE=/other amont land`, `env A=b amont land`, `sudo amont land`, `timeout 60 amont land`, `amont land` with `run_in_background: true`; supported: `amont land --json`, `cd /x && amont land`. Retry: a pre-push hook that commits and prints the push-again marker, with the tracking ref locked → the journal records an unreviewed push for the retried sha; a refused land → nothing journaled; a retried land ending unverified → journaled as possibly unreviewed; empty `tool_use_id` → `retry-check skipped`.

## Phase 3, skills

- **dotfiles** `dot_claude/skills/worktree-task/SKILL.md` F6 (and the F7/F10 references): `amont rehearse --wait`, then `amont land --json` (exit 0 is the proof; on 4 read `remote_after` before anything else). Fallback line: where `amont --version` < 1.53 or amont-agent < the Phase 2 version, the current `git push -u origin <branch>` + `git ls-remote` stays.
- **relais** `crates/relais/src/install/mod.rs` `skill_relais_verified_push`: the same switch with the same fallback; update its pinning test. Release relais; the plugin reinstall is the person's step (memory: `relais install --claude --user --write`).

## Out of scope

CAS on `main` (branch-protect forbids pushes there); `push_landed` for plain git pushes stays as is.

## Verification

- Phase 1, observed on 41b024c (amont repo). `cargo test -p amont --test land`: 29/29, nine new. Dry-run: no upstream → `would-land`, `dst refs/heads/<b>`, exit 0, remote and tracking ref unchanged, no receipt dir; renamed upstream → `dst` names the remote branch; main, dirty tree, two push URLs → `refused`, exit 1; `--dry-run --force` → `lease:true`. Receipts: landed, refused and not-observed lands each write one with that status; a retry with `refs/remotes/origin/<b>.lock` present exits 0, prints the update-ref warning, and its receipt names the retried sha. A branch tracking `main` (the `worktree add -b <b> origin/main` shape) → dry-run `dst refs/heads/<b>`, land writes `<b>`, `main` untouched, `@{u}` = `origin/<b>` (this test was red on 78345a9). Pilot with the built binary in a scratch repo: dry-run `pushed 83fa8e7`, `dst refs/heads/feat/p`; the real land pushed `83fa8e7`, receipt `status:landed`, `git ls-remote` = `83fa8e7`. In a linked worktree `receipt_dir` is that worktree's own git dir. Workspace clippy and Windows-target clippy on `tests/land.rs`: clean. Windows CI on PR #328: `windows` passed (the shell-free `land` tests run there since #321).
- Phase 2, observed (amont-agent #113, released 2.41.0; details in its pointer plan): `make check` green, 18 unit and 6 end-to-end tests (a fake `amont` answering `--dry-run` as 1.54.0 does, since CI has no amont), falsified (with `find` not matching a land the review test fails). Real dry-run latency p50 96 ms, p95 112 ms against the 3 s budget. Live with the released hook in this session: an unreviewed land held under deny; a land `amont` refuses not held; a land amont refused (exit 1) fired `PostToolUseFailure` and `land-retry` journalled `retry-check skipped: no judgement was recorded before the land`, which confirms Claude Code routes a failing Bash call there and the reply shape works; a UI land held under the preview gate's deny with the push message. A scripted run with real amont and a fast-forwarding pre-push hook, tracking ref locked: judged `aef4c44`, land retried to `232e53a`, PostToolUse said `land-retry: This land pushed 232e53a … guards judged aef4c44`.
- Phase 3, observed: dotfiles #40 (`worktree-task` F6) merged and applied; relais #249 (`/relais-verified-push`, and the worker deny floor now refuses `amont land`) released as 0.22.0. Both branches, and the relais release branch, were pushed with `amont land --json` from their worktrees: each landed (`remote_after` = `pushed`), and the journal shows `implementation-review … reviewed`, `push-preview` declining (not a UI repo) and `land-retry … held`. The dotfiles branch tracked `main` and landed under its own name with its upstream moved there.
- Each release, observed: amont 1.54.0 (tag on 2911295; release workflow success on every job; 7 GitHub assets, crates.io and tap at 1.54.0; npm accepted all seven packages; `which -a amont` shows one 1.54.0 build after replacing the `~/.local/bin` copy). amont-agent 2.41.0 (tag on 3f5127e; workflow success; 7 assets, crates.io and tap at 2.41.0; `amont-agent install --write` added only the `PostToolUseFailure` Bash block). relais 0.22.0 (tag on 0fb212c; workflow success; 7 assets, tap at 0.22.0; `relais --version` 0.22.0).

## Decision log

- 2026-10-14 — Phase 1 was written by relais (run-65dce89eb6f7c-f500). The run ended blocked on a relais fault (`admission_refused`, run not registered) after its candidate `make check` was cut off by the wall limit under load; the base was already red on an unrelated test (`index_fidelity::ctrl_c_mid_run_still_restores_and_dies_by_the_signal`). The candidate was salvaged unchanged, acceptance re-run in the session, and the salvage recorded with relais.
- 2026-10-14 — Running the dry-run in this branch's own worktree showed every `worktree-task` branch tracks `main` (`git worktree add -b <b> origin/main`), so land refused them as pushes to `main`; Phase 3 could not have worked. Fixed in Phase 1 (41b024c): a main/master upstream under another name is the branch's base, and it lands under its own name, as `git push -u` would.
- 2026-10-14 — Another session released 1.53.0 (#327, comma-joined `hook.skip`) while Phase 1 was in review, so Phase 1 ships as **1.54.0**. Read every "1.53" in the body as 1.54: the release Phase 2 waits for, and the version amont-agent and the skills' fallback check against. The rebase onto it conflicted only in CHANGELOG.md; the branch's first push happened mid-rebase and was replaced with `--force-with-lease` on the stale tip, as the person chose.
- 2026-10-14 — PR #328's macOS job failed on `an_accepted_push_that_cannot_be_read_back_is_unverified` (from #320): the runner image moved to git 2.56.0, which updates a remote's tracking ref itself on a push to a URL that matches it (RelNotes 2.56.0). The test now asserts the ref absent below 2.56 and, from 2.56, equal to the pushed sha if present (14090a9). This also confirms the plan's choice of the receipt over the tracking ref as evidence: on git 2.56 the tracking ref moves on every accepted push.
- 2026-10-14 — A land killed by a signal before it returns writes no receipt (the plan expected an `interrupted` one). Phase 2 reads a missing receipt as `retry-check skipped`, never a silent pass, so this stays.
- 2026-10-15 — Closed. Remaining for the person: reinstall the relais plugin so sessions get the new `/relais-verified-push` text (relais 0.22.0 is installed; the plugin copy is the person's step). Seen on the way, outside this plan: `amont rehearse --wait` said "nothing to rehearse" for relais's release branch while the push gate then ran the full `cargo-test`.

## Implementation review

Phase 1 (amont): approve-with-changes, then approve-with-changes on the Delta (41b024c, no new blocker).
Fixed: verification actuals and the receipt-on-kill gap recorded here; deliberate: the reworded usage error, no `dst` in a real land's `--json`.
55k + 58k tokens, about 30 s each; reviewed tree dfdc698. Further Deltas after the rebase onto 1.53.0 (range-diff: CHANGELOG context only) and the git 2.56 test fix, last on tree bf65542: approve-with-changes, nothing left but this record. Phases 2 and 3 are reviewed in their own repositories.

<!-- panel: repos=amont,amont-agent,dotfiles,relais adds= reviewers=backend body-sha=286061f64194 -->
