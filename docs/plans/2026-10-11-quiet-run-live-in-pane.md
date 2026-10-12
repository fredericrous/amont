---
canonical: relais:docs/plans/2026-10-11-quiet-run-live-in-pane.md
phases: [1]
status: done
---
Part of [A quiet run is live in relais's pane](https://github.com/fredericrous/relais/blob/main/docs/plans/2026-10-11-quiet-run-live-in-pane.md).

This repository carries phase 1: while a quiet run is live it writes one active-run entry, `${XDG_STATE_HOME:-~/.local/state}/quiet/active/<pid>-<n>.json` (or `$QUIET_ACTIVE_DIR`), rewrites it with `exit`/`ended` when the run ends and leaves it for relais's watcher or a later sweep; relais's pane tails the run's log from it.

An entry is written only inside a Claude Code session (`CLAUDE_CODE_SESSION_ID` set); `.cargo/config.toml` pins an empty session and a `target/` dir for every test, after the suite wrote 27 fixture entries to the real dir under an agent's gate.

Verified:
- `capture` opened then ended: unit tests (`active.rs`) show the entry with its documented fields, and after the run it carries `exit`/`ended` and stays.
- Two concurrent captures leave two entry files.
- A dead-pid entry is swept on open.
- An unwritable state dir does not stop the capture.
- Windows CI: the sweep test now matches the off-unix `alive()` (every pid counts as alive).
- `tests/quiet_run.rs`: a quiet run leaves an ended entry with its result.
- Live against the relais branch watcher: `amont run clippy` → `started`, live `output`, `ended` exit 0.

## Implementation review

approve-with-changes, delta on c66d6af (89k, 43 s). Round 1's five findings are resolved. The delta's two findings are carried: the entry contract wording and phase 1 tick go in relais's closing commit (the canonical plan); the session line in the CHANGELOG goes in the 1.51.0 release PR.
Delta 2 on 9d2af79: the Windows sweep test now asserts that off unix a dead pid's entry stays (resolved). Delta 3 on 43221f8, **approve**: the repo is looked up only when an entry is written, so a hook with no session runs no extra `git rev-parse` (macOS CI's rehearsal race tests failed twice with that call in place). The holds-until comment and the CHANGELOG session line landed here, not in the release PR.

