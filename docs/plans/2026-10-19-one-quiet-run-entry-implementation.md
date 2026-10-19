---
status: active
branch: feat/amont-quiet
repos: [amont, amont-agent, relais]
adrs: [ADR-0039, ADR-0037, ADR-0020]
---
# Plan: one implementation of the quiet-run active entry

## Review panel

👉 **Decide:** none. Approve if one std-only `amont-quiet` crate, which amont publishes and amont, amont-agent and relais call, is worth one crate dependency in each tool and two releases.
📍 amont, amont-agent, relais · reviewed, no code yet · next: Phase 1, the crate in amont. Panel: backend (opus-5-5), codex (gpt), the delta panel `plan-panel` computed.
**Changed by review:**
- The writer creates its temp file exclusively, so a planted symlink is never followed.
- amont-agent's caller is adapted explicitly, not one call for one call.
- Old temp files of both forms are swept.
**Verdicts:** backend approve-with-changes in round 1 and round 2, and on the Codex edits. Codex approve-with-changes. 4 findings carried into implementation.

📄 Full reviews: [2026-10-19-one-quiet-run-entry-implementation.reviews.md](2026-10-19-one-quiet-run-entry-implementation.reviews.md)

## Context

ADR-0039 (`harness.shared-code`) says that knowledge whose outputs must agree across tools has one implementation. It names one known breach. The quiet-run active entry, a JSON file per running check that relais's pane reads, is written by two copies of the same code:

- amont: `crates/amont-runtime/src/active.rs`, 380 lines with tests, used only by `capture.rs:77,140`;
- amont-agent: `src/quiet.rs:261-449`, used only by `execute` in the same file.

The two copies already disagree in five ways, listed in the `holds-until:` comment at amont-agent `src/quiet.rs:268-280`. relais carries a third copy of the directory rule, `active_dir` at `crates/relais/src/quiet.rs:47`, which it uses to find the files.

The person agreed that amont and amont-agent should share this code (2026-10-19). Today they share only a document, amont `docs/quiet-runs.md`, and nothing keeps the copies in step.

Depending on `amont-runtime` itself was ruled out: it is 48 modules, versioned in lockstep with amont, to buy one file. The fleet's own precedent is ADR-0037's `git-attest`, a small std-only crate on crates.io. amont depends on it, and `scripts/check-no-deps.sh` allows it by name while its own resolved tree stays empty. This plan does the same.

## Decision

A new crate, **`amont-quiet`**, in amont's workspace (`crates/amont-quiet`). It is std-only with no dependencies, its MSRV is 1.74 (amont's commit path), it is a workspace member published to crates.io with amont's release, and it uses the workspace version.

Dependents:

- amont-runtime (the commit path);
- amont-agent;
- relais, for the directory rule only.

**API**, with a pure core and the I/O at the edge, following relais's testable `active_dir(get)`:

- `pub fn dir(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf>`, and `pub fn dir_from_env()`. Both call a private pure `dir_for(env, windows: bool)`, so the tests exercise both platform branches on any host.
- `pub struct Entry`, created by `Entry::start(dir, tool, check, repo, log, session) -> Option<Entry>`. `entry.end(exit, final_log)` ends it, and `Drop` ends it too. The tool name is a parameter, so one writer serves `amont` and `amont-agent`. `start` returns `None` for an empty `session` before calling `repo` (a lazy `FnOnce`, as at active.rs:81); each caller passes `CLAUDE_CODE_SESSION_ID` unfiltered. `dir`'s `cfg!(test)` guard (active.rs:37) no longer covers amont-runtime's tests; `.cargo/config.toml:31-32` does, and the move keeps it.
- `pub fn sweep(dir, tool, now_ms)` and `pub fn alive(pid: i64) -> bool`.
- A private minimal JSON writer, and a tolerant field reader for the three fields the sweep reads. Each is a leaf utility the crate owns. Copying one is allowed by ADR-0039.

**One behaviour for each divergence.** The rule is that a writer never deletes a live run's entry and never writes outside the state directory.

The shared atomic writer creates its temp with `OpenOptions::create_new(true)` and Unix mode 0600; an existing temp, including a symlink, causes a silent write failure rather than truncation or following it. Rename occurs only after a successful complete write. Add a Unix regression test planting the expected temp name as a symlink to a sentinel outside the state directory: attempting the write must leave the sentinel's bytes and permissions unchanged.

| Divergence | Chosen | Why |
|---|---|---|
| empty `HOME` | no directory, no entry (amont-agent, relais) | amont wrote a relative `.local/state` under the working directory, inside someone's repository |
| Windows | `%LOCALAPPDATA%\quiet\active` (amont, relais) | amont-agent wrote nothing there |
| liveness | gone only on `ESRCH` (amont-agent) | `kill(pid, 0)` answers `EPERM` for a live process; any other doubt keeps the entry, because a reader removes a stale one after 1 h |
| pid ≤ 0 or > `i32::MAX` | not a process, so dead and sweepable, and `kill` is never called with it (amont) | `kill(0)` and `kill(-1)` signal process groups; amont-agent kept such entries forever |
| temp file | `.<id>.json.tmp`. Once older than 1 h, the sweep removes that form and both legacy ones: amont's `<id>.json.tmp` and amont-agent's `.amont-agent-tmp-*.json` (amont-agent `src/atomic.rs:45`) | a leading dot is ignored by every reader rule (relais `quiet.rs:313`); amont's `<id>.json.tmp` was never swept, and files already on disk must not outlive the change |

The entry stays at `"v": 1`. No field changes, so readers are unaffected.

## Changes

### Phase 1: amont (one PR, then a release)

- Add `crates/amont-quiet` (`Cargo.toml` with `rust-version = "1.74"` and empty `[dependencies]`; `src/lib.rs`), moved from `active.rs` and generalised as above. Its unit tests cover:
  - every `dir` case through the env getter (`QUIET_ACTIVE_DIR`, `XDG_STATE_HOME`, `HOME` set, empty and unset, Windows);
  - `sweep` on a temporary directory: an ended entry older than 60 s, its own dead pid, another tool's dead pid kept, a stale dot-temp file, a stale legacy `<id>.json.tmp` and `.amont-agent-tmp-1-2.json`, a fresh one of each kept;
  - `alive` for pids ≤ 0 and wider than an i32;
  - JSON escaping of a path with quotes and backslashes, the case `active.rs:261` already tests.
- In `crates/amont-runtime`:
  - `Cargo.toml` gains `amont-quiet = { path = "../amont-quiet", version = "…" }`, with the argument written beside it, as `git-attest`'s is (ADR-0020);
  - `capture.rs` calls `amont_quiet` with `tool = "amont"`;
  - `active.rs` and `pub mod active` are removed, since `capture.rs` is the only caller.
- `scripts/check-no-deps.sh` allows `amont-quiet` by name. The script fails if `cargo tree -p amont-quiet` shows anything besides itself, the same rule `git-attest` has.
- `.github/workflows/release.yaml:613` publishes `amont-quiet` first, before `amont-runtime`.
- `.github/attest-inputs` declares `crates/amont-quiet`, which is already covered by `crates`, so the check confirms it.
- `docs/quiet-runs.md`:
  - the empty-`HOME` paragraph becomes the rule, not a divergence;
  - it names the temp-file rule and the liveness rule;
  - it says the contract has one implementation, `amont-quiet`.
- Before tagging, confirm that `amont-quiet` is unclaimed on crates.io and that `CARGO_REGISTRY_TOKEN` is allowed to publish a new crate. Release amont (minor) with `tag-release`, and verify that `amont-quiet` resolves on crates.io before Phase 2.

### Phase 2: amont-agent (one PR, then a release)

- `Cargo.toml` gains `amont-quiet = "<released>"`, with its argument written beside it.
- `src/quiet.rs:261-449` is replaced by calls to `amont_quiet` with `tool = "amont-agent"`. That removes `ENDED_KEEP_MS`, `active_dir`, `pid_alive`, `sweep_active` and `ActiveEntry`, and the `holds-until:` comment with them.
  - Adapt `execute` to `Option<amont_quiet::Entry>` explicitly. Preserve the no-log guard and absolute-path conversion before start; pass repository discovery lazily. Early error returns call `end(Some(code), None)` when an entry exists. On normal completion, retain the exit code locally until the log disposition is known, then call `end` exactly once, passing the absolute final log path only when the rename succeeded. Do not translate the assignment at `:712` into an early `end`: subsequent calls are ignored. Keep Drop for unwinding and otherwise unfinished paths. Extend integration coverage for failed log creation and a relative config directory, checking absolute started/ended log paths, final output drainage, and unchanged command exit codes.
  - `crate::atomic` stays for its other callers. Check with LSP findReferences before removing anything.
- `tests/quiet.rs` stays as the behaviour test. It gains one test: with `HOME=""` and no override, `amont-agent quiet -- true` writes nothing.
- Release amont-agent (minor).

### Phase 3: relais (one PR, no release needed)

- `Cargo.toml` gains `amont-quiet` with its argument. `crates/relais/src/quiet.rs:47` `active_dir` delegates to `amont_quiet::dir`, which already has the same rule.
- relais's reader-side liveness check keeps its own Windows `OpenProcess` path and is not shared. It is the reader's protocol (ADR-0039), and the crate's `alive` is the writer's.

Order: Phase 1 and its release, then Phases 2 and 3 in parallel. Each PR goes through `worktree-task`, then `merge-when-green`; releases go through `tag-release`.

## Verification

- `amont-quiet` unit tests pass. `cargo +1.74 build -p amont --locked` passes. `scripts/check-no-deps.sh` passes. A planted extra dependency in `amont-quiet` makes it fail.
- On this machine, run `env -u XDG_STATE_HOME HOME= QUIET_ACTIVE_DIR= CLAUDE_CODE_SESSION_ID=s-verify amont run <check>` in an empty directory. Expected: no `.local/` appears there. Before the change, `.local/state/quiet/active/*.json` appears. The same holds for `amont-agent quiet -- true`.
- For `amont run <check>` and `amont-agent quiet -- sleep 2`, each with `QUIET_ACTIVE_DIR` set to a scratch directory, the two entry files have the same key set and order, apart from `tool` and the values. `relais quiet watch --protocol` emits `started` and `ended` for both.
- After Phase 2, `git grep -n 'holds-until' -- src/quiet.rs` in amont-agent returns nothing, and `git grep -n 'fn active_dir\|fn pid_alive\|fn sweep_active'` returns nothing in amont-agent or amont-runtime.
- relais's existing `quiet.rs` tests for `active_dir` pass unchanged against the delegated rule.
- After the releases, `cargo search amont-quiet` shows the released version, and `amont --version` and `amont-agent --version` on this machine show the new versions.

<!-- panel: repos=amont,amont-agent,relais reviewers=backend,codex body-sha=60127b65f789 -->
