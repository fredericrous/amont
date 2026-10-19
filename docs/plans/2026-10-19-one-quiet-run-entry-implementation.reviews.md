# Reviews: one implementation of the quiet-run active entry

Plan: [2026-10-19-one-quiet-run-entry-implementation.md](2026-10-19-one-quiet-run-entry-implementation.md)


**Round 1, backend** (approve-with-changes, 40k, 81 s):
- high: the temp-file names were wrong, and the legacy temp files would never be swept. Accepted.
- medium: the `cfg!(test)` guard and the session check move into the crate. Accepted.
- medium: the Windows branch could not be tested on unix. Accepted: `dir_for`.
- medium: the HOME check did not isolate the case. Accepted.
- low: a token may not be allowed to publish a new crate. Accepted.

**Round 2, backend** (approve-with-changes, 45k, 28 s): low, an escaped test name. Accepted.

**Final review, codex** (approve-with-changes, high effort):
- high: an atomic write could follow a symlink. Accepted: `create_new`.
- high: amont-agent's lifecycle does not map one call for one call. Accepted: explicit adaptation.
- medium: the no-deps guard skips build edges and other targets. Carried into implementation.

**Backend, binding the final body** (approve-with-changes, 20k, 18 s): medium, non-Unix `alive`; low, rollback by yank. Both carried into implementation.

**Backend, on the Codex edits** (approve-with-changes, 51k, 34 s): both edits resolved. low: a failed write must remove only a temp it created. Carried into implementation.

**Carried into implementation**
- `check-no-deps.sh` queries `--edges normal,build --target all`, with negative tests for a normal, a build and a target-specific dependency.
- After a failed write or rename, the writer removes the temp it created, and never one whose `create_new` failed.
- `alive` declares `extern "C" fn kill` on Unix, so the crate needs no `libc`. On non-Unix it returns `true`, and the reader's 1 h rule removes the entry. Verification adds a Windows-target `cargo check -p amont-quiet`.
- A bad `amont-quiet` release is fixed with a patch release and a `cargo yank`, never a revert: crates.io cannot unpublish.
