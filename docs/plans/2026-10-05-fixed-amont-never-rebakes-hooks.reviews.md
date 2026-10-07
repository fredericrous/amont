# Full reviews: A fixed amont never re-bakes hooks from a push snapshot

**plan-review-backend, round 1: approve-with-changes (37k, 53 s).**
- (high) The `AMONT_REHEARSAL` clause never fires, because `in_snapshot()` removes the variable first. Use a marker in the snapshot's `$GIT_DIR` instead.
- (high) The real-repo check runs the pinned 1.47.0; use a packed local build.
- (medium) `init` has no `--quiet`.
- (medium) `npm_packaging.rs:234` scans the body of `init()`.
- (low) `pnpm ls` gets the env var, and the line is captured by `bounded_success`.
- (low) A dead `BAKED` path falls through silently; follow up in doctor.

**plan-review-language rust, round 1: approve-with-changes (39k, 50 s).**
- (high) The `snapshot_deps` test would pass on main. Seed a sentinel and run a copy of the binary from under the snapshot.
- (medium) The `AMONT_REHEARSAL` clause is dead; never "fix" it by exporting to gate children.
- (medium) Drop `--quiet`.
- (medium) Extend `a_prepare_command_owns_the_dependencies` instead of a `pushed_tree` unit test.
- (low) Hermetic init tests and an indented early return.

**plan-review-backend, round 2: approve-with-changes (39k, 33 s).** All round-1 findings resolved. New:
- (medium) The marker write must fail closed.
- (medium) Pack the platform package too, use absolute `file:` paths, and add a `--version` precondition.
- (low) Place the check after `Nowhere` and before `Err`.

**plan-review-backend, delta: approve-with-changes (29k, 28 s).** All round-2 findings resolved. New:
- (medium) A read-only admin dir test can't be built; use a `mark_snapshot` seam.
- (low) Use one path helper.

**plan-review-backend, bind: approve (28k, 23 s).** Both findings resolved. Low: name the two helpers and use an absolute path (now in the body).

**plan-review-language rust, round 2: approve-with-changes (40k, 36 s).** All round-1 findings resolved. New:
- (medium) No single match arm sits "after Nowhere, before every Err"; bind `rh` first instead.
- (medium) The seam is a parameter, not a global.
- (low) Cleanup runs `worktree remove --force`.
- (low) Resolve a relative `--git-path`.

**Final bind on body 102c125c9907:**
- Rust: approve (28k, 17 s). Note: in `checkout_at`, a failed path query is a mark failure.
- Backend: approve (35k, 34 s). Low: give the crate path, and have the seam test assert on its own `base`. Suggests cleanup through `PushedTree` `Drop`.

**Delta on the person's review (body 4d8d3480d479):**
- Backend: approve-with-changes (39k, 31 s).
  - (high) The PermissionDenied test cannot be reached through a worktree admin dir.
  - (medium) The header is stale.
  - (low) Mixed-version runs need a restore step and a lockfile setup.
- Rust: approve-with-changes (38k, 29 s).
  - (high) `init_with` needs a directory parameter, never a process cwd.
  - (high) Split out `probe_marker`, test it on a `0o000` dir, skip as root.
  - (low) The header is stale.

**Binds:**
- Body 2d2c55b55fab:
  - Rust approve (29k, 19 s). Note: one mutation per test.
  - Backend approve-with-changes (30k, 25 s): line 69 must say `repo_hooks_in`, plus the mutation pairing.
- Body c97d58091d53:
  - Backend approve (27k, 17 s). Low: `exe` B ≠ A.
  - Rust approve (27k, 16 s). Low: line 73 wording.
