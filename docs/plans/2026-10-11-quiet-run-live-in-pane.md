---
canonical: relais:docs/plans/2026-10-11-quiet-run-live-in-pane.md
phases: [1]
status: active
---
Part of [A quiet run is live in relais's pane](https://github.com/fredericrous/relais/blob/main/docs/plans/2026-10-11-quiet-run-live-in-pane.md).

This repository carries phase 1: while a quiet run is live it writes one active-run entry, `${XDG_STATE_HOME:-~/.local/state}/quiet/active/<pid>-<n>.json` (or `$QUIET_ACTIVE_DIR`), rewrites it with `exit`/`ended` when the run ends and leaves it for relais's watcher or a later sweep; relais's pane tails the run's log from it.
