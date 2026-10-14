---
status: active
branch: feat/land-dry-run-receipt
repos: [amont, amont-agent, dotfiles, relais]
adrs: [cli.land-verb, work.implementation-review, work.preview-approval]
---
# Plan: agents land through `amont land`, with every push guard intact

## Review panel

👉 **Decide:** none — approve if a four-repo chain (amont 1.53 dry-run → amont-agent release → skills) is worth doing before agents use `amont land`.
📍 amont, amont-agent, dotfiles, relais · plan reviewed, no code yet · next: Phase 1 in amont. Panel: backend (opus-5-5).
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
- **Retry escapes the judgement** (land.rs:354-378: a pre-push hook that fast-forwards HEAD makes land push a new sha): PreToolUse records the judged sha for every land clause, before `record_before`'s `!gated(&repo)` early return (preview.rs:1683), with its start time. A PostToolUse assertion, run on PostToolUse and on PostToolUseFailure alike (assertions get only PostToolUse today, assertions/mod.rs:20-24: the dispatcher routes PostToolUseFailure to this assertion and hook registration adds the event; its `skipped` line is a stated exception to assertions/mod.rs:42), reads the land receipt from the dry-run's `receipt_dir` for that branch started after the recorded time (exactly one, since the command holds one land): `pushed` ≠ the judged sha with `landed` → journaled as an unreviewed push; with `unverified` or `not-observed` (push accepted, land.rs:407-421) → journaled as a possibly unreviewed push; `refused`, `unreachable`, `interrupted` → nothing journaled; no receipt, more than one, or no recorded judgement (an empty `tool_use_id`, preview.rs:1663) → journaled as `retry-check skipped: <why>`, never a silent pass. The Bash exit code is not used; what Claude Code's PostToolUse and PostToolUseFailure payloads carry is checked against the installed version first.
- Corpus lines (`tests/corpus/*.cases`) for the shapes, end-to-end tests in the style of `tests/preview.rs` (`World`, `pre_bash_in`) with a real amont on PATH (built from the pinned release, or skipped with a message when absent): review held for an unreviewed land under deny and passed after a review; preview held for a UI land; land on main not held; held as unresolvable: `git commit -m x && amont land`, `git fetch --set-upstream origin x && amont land`, `amont land; true`, `amont land && false`, `amont land | cat`, `amont land > out`, `GIT_DIR=/other/.git GIT_WORK_TREE=/other amont land`, `env A=b amont land`, `sudo amont land`, `timeout 60 amont land`, `amont land` with `run_in_background: true`; supported: `amont land --json`, `cd /x && amont land`. Retry: a pre-push hook that commits and prints the push-again marker, with the tracking ref locked → the journal records an unreviewed push for the retried sha; a refused land → nothing journaled; a retried land ending unverified → journaled as possibly unreviewed; empty `tool_use_id` → `retry-check skipped`.

## Phase 3, skills

- **dotfiles** `dot_claude/skills/worktree-task/SKILL.md` F6 (and the F7/F10 references): `amont rehearse --wait`, then `amont land --json` (exit 0 is the proof; on 4 read `remote_after` before anything else). Fallback line: where `amont --version` < 1.53 or amont-agent < the Phase 2 version, the current `git push -u origin <branch>` + `git ls-remote` stays.
- **relais** `crates/relais/src/install/mod.rs` `skill_relais_verified_push`: the same switch with the same fallback; update its pinning test. Release relais; the plugin reinstall is the person's step (memory: `relais install --claude --user --write`).

## Out of scope

CAS on `main` (branch-protect forbids pushes there); `push_landed` for plain git pushes stays as is.

## Verification

- Phase 1: `amont land --dry-run --json` on branch `x` with no upstream → `status:would-land`, `dst:refs/heads/x`, exit 0, remote ref and `refs/remotes/origin/x` unchanged; on main → `refused`, exit 1; `amont land --dry-run --json` piloted in a scratch repo matches the `pushed`/`dst` a real `amont land` then uses; Windows CI runs the dry-run tests; a scratch land writes one receipt with `status:landed` and `pushed` = `git ls-remote` tip, and with `refs/remotes/origin/x.lock` present still exits 0 with the retried sha in the receipt.
- Phase 2: hook input `git commit -m x && amont land` → deny with the unresolvable shape; `amont land` on main → no hold; PreToolUse time for one land < 5 s (measured); `amont land; true` → deny naming the supported shape; scratch pre-push hook that commits and prints the push-again marker → journal shows an unreviewed push for the retried sha; then live: in a scratch branch of a gated repo, with deny on, an unreviewed `amont land` is held with the review message, and after the review it passes and lands.
- Phase 3: a real worktree-task run on a small change uses `amont land` and the journal shows the review gate judged it.
- Each release: `tag-release` (workflow conclusion, artifacts, brew upgrade, `which -a` shows one version).

<!-- panel: repos=amont,amont-agent,dotfiles,relais adds= reviewers=backend body-sha=286061f64194 -->
