---
status: done
branch: fix/snapshot-init-no-rebake
repos: [amont]
adrs: [ADR-0008, ADR-0022]
---

# A fixed amont never re-bakes hooks from a push snapshot

## Review panel

👉 **Decide:** none. Approve if a marker in the snapshot's own git dir is the right guard. It cannot leak to other repositories or processes.
📍 amont · plan reviewed · next: worktree `fix/snapshot-init-no-rebake`, failing test on main first. Panel: backend, lang:rust.
**Changed by review:**
- The guard moved from env vars to a git-dir marker, because `AMONT_REHEARSAL` is stripped before gates run. `init` checks the marker before any hook-config error.
- The real-repo check now uses packed local builds, verified by the sha256 of the resolved native binary.
- After your review:
  - The scope is limited to runner and pin both fixed, with mixed-version runs recorded.
  - Detection fails closed and is tested without a process-wide current directory.
  - The isolation test was added.
- The marker write fails closed, behind a test seam.
**Verdicts:** round 1 had 2 approve-with-changes, round 2 had 2 approve-with-changes, and the final bind had 2 approve. Carried into implementation:
- In `checkout_at`, an unresolvable marker path fails closed (`None`).
- Cleanup goes through `PushedTree`'s `Drop`.
- The seam test asserts on its own `base`.
- The files live under `crates/amont-runtime/src/`.
- The `init_with` test passes an `exe` path B ≠ A, so a re-bake would show.
- Change 2's "repo_hooks()" wording means `repo_hooks_in(dir)`.
- Confirm the `0o000` test really runs, not skips, as non-root locally and on CI.

📄 Full reviews: [2026-10-05-fixed-amont-never-rebakes-hooks.reviews.md](2026-10-05-fixed-amont-never-rebakes-hooks.reviews.md)

## Context

On 2026-10-05, in duro-design-system, after a push rehearsal from a git worktree the repository's **shared** hooks (`<git-common-dir>/hooks/*`) read `BAKED="/private/var/folders/…/T/amont-push-<pid>-<hash>/node_modules/.pnpm/@amont-hooks+darwin-x64@1.47.0/…/bin/amont"`: a binary inside a temp snapshot that no longer exists.

Traced cause (amont 1.47.0, `origin/main` 3ee6aa9):

- **The snapshot shares the hooks folder.** The pushed-tree snapshot is a real linked worktree, made by `git worktree add --detach` in `crates/amont-runtime/src/pushed_tree.rs` `checkout_at`. Its common dir is therefore the repository's own.
- **Its install runs lifecycle scripts.** Snapshot dependencies install with them enabled (`crates/amont-runtime/src/snapshot_prep.rs` `install`/`manager_cmd`: `pnpm install --frozen-lockfile …` or `npm ci …`, with no `--ignore-scripts`; `GIT_*` env is stripped by `hooks/common.rs` `strip_git_env`).
- **`prepare` re-bakes from inside the snapshot.** The repo's `"prepare": "amont init"` runs there. `install::init()` (`crates/amont-runtime/src/install.rs`) asks git for `--git-path hooks`, gets the shared folder, and re-bakes every amont shim with `current_exe()`, a path inside the snapshot's `node_modules`.
- **Nothing tells `init` it is inside a snapshot.** `AMONT_REHEARSAL` is set only on the gate child after the install (`rehearsal.rs`), and `AMONT_SOURCE_WORKTREE` only on `snapshotPrepare` (`pushed_tree.rs`).

Effect: the shim's resolution order (`templates/hooks/pre-commit`: `$GIT_HOOKS_BIN`, baked path, `~/.local/bin`, `PATH`) silently falls through to whatever `amont` is on the machine. The repository's pinned version is replaced without a word, and if no other amont exists, every hook fails until someone runs `npm install` in the live checkout.

**Scope of the guarantee.** The guard has two halves:
- the runner's `checkout_at` writes the marker;
- the `init` that runs *inside* the snapshot reads it. That binary is whatever version the pushed commit's lockfile installs.

So it holds only when **both** the runner and the snapshot's pinned amont carry this change:
- A fixed runner rehearsing a commit that still pins 1.47.0 or older is re-baked by that old `init`.
- An old runner writes no marker, so a fixed `init` in its snapshot re-bakes as before.

Upgrading only the runner is not enough. The docs say so, and a mixed-version check records both cases (Verification).

## Non-goals

- `--ignore-scripts` on snapshot installs: it would also skip dependencies' own install scripts (esbuild, prisma), which real snapshots need.
- Changing how `init` bakes in an ordinary linked worktree. `init_from_a_linked_worktree_bakes_into_the_shared_dir` (`crates/amont/tests/init.rs`) stays exactly as it is.
- Repairing hooks already baked to a dead path. The person re-runs `npm install` / `pnpm install` in the live checkout. Follow-up issue, not this PR: `amont doctor` warns when `BAKED` names a missing file, since the shim falls through silently.
- Protecting against old pinned binaries. That needs isolation on the runner side, for example saving the shared amont shims before the snapshot install and restoring any whose `BAKED` now names a path under the snapshot. That is a separate mechanism, a follow-up issue rather than this PR.
- A release: on request only.

## Changes

1. **The snapshot marks itself in its own git dir, not the environment.**
   - `checkout_at` (`pushed_tree.rs`) writes an empty `amont-snapshot` file right after `git worktree add`, at `git -C <snapshot> rev-parse --git-path amont-snapshot`: the worktree's private admin dir, `<common>/worktrees/<name>/`. `init` resolves the same path the same way.
   - **Fail closed:** if `mark_snapshot` fails, return `None`, as a failed add does. An unmarked snapshot would bring the bug back silently.
     - Cleanup runs `git worktree remove --force` (git has already registered `<common>/worktrees/<name>`), not just the existing `remove_dir_all(&base)` (`pushed_tree.rs:141-145`).
     - The marker writer is a parameter: `checkout_at_with(…, mark: impl Fn(&Path) -> io::Result<()>)`, and `checkout_at` passes `mark_snapshot`. Never a global flag, which would race parallel unit tests.
   - That dir is the snapshot's alone and is deleted with the worktree. So the marker covers every process run inside the snapshot: the dependency install, `snapshotPrepare`, and an install a gate runs itself.
   - It cannot reach another repository: amont's own suite runs `amont init` in fixture repos during its rehearsal, and those resolve their own git dir.
   - Why not an env var as the guard: `AMONT_REHEARSAL` is removed by `rehearsal::in_snapshot()` before any gate spawns (`rehearsal.rs:128-150`, `main.rs:329`), and an exported variable would leak into gate children (the trap `rehearsal.rs:130-133` records).
2. **`init` stands down inside a snapshot.** In `install::init()` (`install.rs:634-651`), bind `let rh = repo_hooks_in(dir);` inside `init_with`. Return `Ok(())` on `Nowhere`, then run the marker check, then `match rh`. A snapshot install then never fails `prepare` over hook config (a hostile redirect or `Unanswerable`):
   - **Detection is three-valued.** `in_push_snapshot(dir) -> io::Result<bool>`:
     - It runs `git rev-parse --git-path amont-snapshot` (`snapshot_marker_path`) and resolves a relative answer against the queried directory.
     - It probes with `probe_marker(path: &Path) -> io::Result<bool>`, a split-out `fs::symlink_metadata` match: `Ok` is true, `NotFound` is false, and any other error is `Err`. `Path::exists()` is not used, because it hides errors.
     - A failed git query is also `Err`, since `repo_hooks()` has already established that this is a repository.
     - When `init` gets `Err`, it returns an error **without writing anything**: it fails closed, never re-bakes on doubt. `checkout_at` uses `snapshot_marker_path` for its write, and a resolution failure there is a mark failure (`None`).
   - When the check returns true, write nothing and return `Ok(())`.
   - **The probe and the directory are parameters for tests.**
     - `pub fn init()` keeps `current_exe()` in its own body (the npm_packaging scan) and calls a private `init_with(exe, dir, probe)` with the current directory. Production passes `in_push_snapshot`.
     - `repo_hooks()` becomes a wrapper over `repo_hooks_in(dir)`, which runs git with `-C dir`.
     - Tests never change the process's current directory or environment, so parallel tests cannot race.
   - Print one stderr line, always (`init` takes no flags, `main.rs:363`): `amont init: inside an amont push snapshot — the repository's hooks are left as they are`. It is captured by `bounded_success`, so it shows in the rehearsal log on a failed install only. It is a trace, not a notice.
   - Written as an indented `if` block, so the line scan in `tests/npm_packaging.rs:234` still reaches `current_exe()`. `on_path_already` is not introduced.
3. **Tell scripts too.** Set `AMONT_SNAPSHOT=1` on `manager_cmd` (`snapshot_prep.rs:440`; it covers install, reuse and the `pnpm ls` at :622, which is harmless) and on the `snapshotPrepare` command. This is information for a custom `prepare` script. It is not the guard.
4. **Help and docs.**
   - The `init` help line in `crates/amont/src/main.rs` gains "never re-bakes from inside a push snapshot".
   - `docs/` gets one paragraph beside `snapshotDeps`. It names the marker and `AMONT_SNAPSHOT`, and states the scope: both the runner and the pushed commit's pin must be a fixed amont, and upgrading either alone is not enough.

## Verification

- **Unit / integration (`cargo test`):**
  - **`crates/amont/tests/init.rs`, new test.**
    - Setup: a linked worktree whose admin dir holds `amont-snapshot`, in a repo whose hooks are baked to a sentinel path A.
    - Expected: `init` there leaves every shim byte for byte identical, exits 0, and prints the one line.
    - A second test: a marked worktree whose `core.hooksPath` is a hostile redirect. Expected: `init` exits 0 and the shims are untouched.
    - **Fail-closed detection, two unit tests in `install.rs`:**
      - `init_with(exe, <fixture dir>, probe)` with a probe returning `Err`, after `repo_hooks_in` has found a repository. Expected: `Err`, and the shims byte-identical to sentinel A.
      - `probe_marker` tested directly on `<tmp>/locked/amont-snapshot`, with `locked` set to mode `0o000` (no search bit). Expected: `Err(PermissionDenied)`.
        - It is skipped, with a message, when the effective user is root.
        - A drop guard restores the mode, so the tempdir can be cleaned up.
        - It is not built on a worktree's admin dir, because git would fail first and the test would hit the wrong branch.
      - Two mutation checks, each run once and recorded. Each must turn exactly its own test red:
        - `probe_marker`'s `Err` arm turned into `Ok(false)` turns the direct probe test red;
        - `init_with` treating a probe `Err` as `false` (`unwrap_or(false)`) turns the `init_with` test red.
    - **Isolation:** while a snapshot worktree's marker exists, `init` run with cwd in a *separate* fixture repository, and with `AMONT_SNAPSHOT=1` in its environment, still bakes that repository's hooks normally. This is the reason for a marker over an inherited variable, tested directly.
    - The existing `init_from_a_linked_worktree_bakes_into_the_shared_dir` and `init_is_idempotent_and_rebakes` pass unchanged; a plain linked worktree still bakes.
  - **`crates/amont/tests/snapshot_deps.rs`, new test with the existing `STUB` manager.**
    - Setup: the shared hooks are seeded with sentinel `BAKED` A. On `op=install` the stub copies the built amont into `<snapshot>/node_modules/.bin/` and runs that copy's `init`. This mirrors the real bug, where `current_exe()` lies under the snapshot.
    - Expected after a rehearsal: `BAKED` is still A and never names a path under `amont-push-*`.
    - This test must fail on `origin/main`: run it once there and record the failing `BAKED`.
  - **Extend `a_prepare_command_owns_the_dependencies` (`snapshot_deps.rs:321`).** Its prepare script records `$AMONT_SNAPSHOT` and runs `amont init`; the hooks must stay unchanged. This covers the `snapshotPrepare` path.
  - The marker write is one helper, `mark_snapshot(path) -> io::Result<()>`, and `checkout_at` calls it. A unit test calls `checkout_at_with` with a failing `mark`. (A read-only admin dir would fail `git worktree add` first, so it cannot test this.) It asserts:
    - `None` is returned;
    - `git worktree list` holds no `amont-push-*` entry;
    - `$TMPDIR` holds no leftover `amont-push-*` directory.
  - `cargo test -p amont --test npm_packaging` stays green.
- **Real repo: duro-design-system, on a throwaway local branch never pushed.**
  - Setup: pack BOTH the wrapper and the `@amont-hooks/darwin-x64` platform package (the locally built binary) with `npm pack`. Pin both by **absolute** `file:` paths (`pnpm add -D` plus a `pnpm.overrides` entry for the platform package), with the lockfile committed on that branch. A relative path would not resolve inside `$TMPDIR/amont-push-*`.
  - Precondition: `--version` cannot tell the builds apart, because it prints `CARGO_PKG_VERSION` (`main.rs:336`). Instead, a gate on that branch prints `sha256sum` of the native binary the snapshot resolved (`realpath` of `node_modules/@amont-hooks/darwin-x64/bin/amont`). It must equal the sha256 of the locally built `target/release/amont`, recorded before packing. Otherwise the check is void. The control run checks its own build the same way.
  - Expected: `amont rehearse --wait` from a worktree leaves `grep ^BAKED= $(git rev-parse --git-common-dir)/hooks/pre-push` byte-identical.
  - Control: the same with `origin/main`'s build reproduces the temp path (record it). Then restore the hooks with `pnpm install` in the live checkout.
  - A gate running `pnpm install` itself is covered by the marker; check it in the stub test above (a gate script running the stub install), not only by argument.
  - **Mixed versions, recording the scope limit.** Each run uses its own commit on the throwaway branch:
    - the rehearsed HEAD's lockfile pins one version;
    - the live checkout's `node_modules` holds the other.
    
    After each run, `pnpm install` in the live checkout, and confirm `BAKED` is back to its pre-run value before the next one.
    - First run: the runner is the fixed build (live `node_modules` from the local tarball), and the rehearsed commit's lockfile still pins 1.47.0. Expected and recorded: `BAKED` changes to a temp path, the documented limit. The reverse also gets a run: an old runner rehearsing a commit that pins the fixed build. Expected and recorded: `BAKED` changes too, because no marker was written. Only the both-fixed run above leaves it unchanged.
- The full gate as CI runs it (`cargo fmt --check`, clippy at the pinned toolchain, `cargo test`), and `amont rehearse --wait` before the push.

## Verification record (2026-10-05, branch at the code commit)

| check | expected | actual |
|---|---|---|
| `snapshot_deps` `a_snapshot_install_never_rebakes_the_shared_hooks` on `origin/main` 3ee6aa9 | fails, `BAKED` under `amont-push-*` | failed: `pre-commit` `BAKED="…/T/amont-push-95366-556fa2e69ae5644b/node_modules/.bin/amont"` |
| same test, and `a_prepare_command_in_the_snapshot_…`, on `origin/main` | fail | both failed |
| `init` marked-snapshot and redirect tests on `origin/main` | fail | both failed; the isolation test passes there, as it must |
| all new tests on the branch | pass | pass |
| mutation: `probe_marker` `Err` arm → `Ok(false)` | only the `0o000` probe test red | only `a_marker_that_cannot_be_looked_at_is_an_error_not_absent` red |
| mutation: `init_with` reads a probe `Err` as `false` | only the `init_with` test red | only `init_fails_closed_when_the_snapshot_probe_cannot_answer` red |
| `cargo fmt --check`, `clippy --workspace --all-targets --all-features -D warnings`, `make test` | green | green; 1,461 passed (the 3 "FAILED" lines are fixture crates, expected) |
| duro-design-system, fixed runner + fixed pin | snapshot binary sha = local build; `BAKED` unchanged | sha `f86bdbc7…` = `target/release/amont`; unchanged; stand-down line in the rehearsal log |
| control: 1.47.0 runner + 1.47.0 pin | `BAKED` → temp path | → `…/amont-push-37819-…/@amont-hooks+darwin-x64@1.47.0/…/amont` (sha `91589726…`) |
| mixed: fixed runner + 1.47.0 pin | `BAKED` changes (documented limit) | changed → `amont-push-42409-…` (snapshot ran sha `91589726…`) |
| mixed: 1.47.0 runner + fixed pin | `BAKED` changes (no marker written) | changed → `amont-push-43987-…` (snapshot ran sha `f86bdbc7…`) |
| hooks restored after each run | `BAKED` back to its pre-run value | yes, all four |

How the real-repo check was run, and where it departs from the plan (deliberate):
- **Scratch clone:** it ran in a scratch clone of duro-design-system at f92b580d, not a branch of the live checkout. A worktree shares its repository's hooks, so the control and mixed runs would have re-baked the live repository's hooks.
- **Packing:** the fixed build was packed as version `1.47.1-snapfix.0`. pnpm writes `file:` lockfile entries relative to the lockfile, so a temporary `$TMPDIR/tgz` symlink made them resolve from the snapshot; it was removed afterwards.
- **Measuring the binary:** the sha256 was taken by the throwaway commit's `prepare` (`node sha.cjs && amont init`) rather than by a gate. It measures the binary that `prepare` resolves, which is the one that bakes.
- **Stale stamps:** two first mixed runs were void ("already stamped on this tree"). They were re-run after removing the scratch clone's `amont-gate` note on the tree.
- **Prepare test:** the `snapshotPrepare` coverage is a new sibling test rather than an edit to `a_prepare_command_owns_the_dependencies`. Its second `init` runs under `env -u AMONT_SNAPSHOT`, the shape of an install a gate runs itself.
- **Unanswerable path:** when git cannot answer at all (`Unanswerable`), `init` keeps git's own error over "cannot tell". Both fail without writing.

## Implementation review

- **approve** after a Delta. Round 1 approve-with-changes (59k, 47 s), Delta approve (30k, 22 s).
- Fixed: the marked-snapshot test asserts its sentinel took; `snapshot_marker_path` carries git's stderr (`errors.never-swallowed`).
- deliberate: gate-run install covered by `env -u AMONT_SNAPSHOT` in `snapshotPrepare` (recorded deviation above).
- Re-verified after the fixes: lib + `init` + `snapshot_deps` tests, clippy at the pin; one gate run hit a timing flake under load 65-84 (`ctrl_c_mid_run…`, passes alone in 7 s).

## After merge

- Release on request. Consumers that pin amont via npm (duro-design-system, duro-app) take the release in their next bump.

<!-- panel: repos=amont reviewers=backend,lang:rust body-sha=c97d58091d53 -->
