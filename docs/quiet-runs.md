# The quiet-run active entry

amont (`amont run`, pre-commit checks) and `amont-agent quiet` keep a
check's output out of the agent's context: the output goes to a
`.log.tmp` file. So that a person can still see such a run live, each
tool writes one small JSON file while the run is active, and a reader
such as relais's `relais quiet watch --protocol` turns those files into
something it can draw. A writer never depends on a reader.

This page is the contract for that file. amont owns it (decisions
ADR-0039, `harness.cross-tool-reads`), and it has one implementation, the
std-only [`amont-quiet`](https://crates.io/crates/amont-quiet) crate, which
amont and amont-agent write through and relais finds the directory with
(`harness.shared-code`). A reader documents only what it does with the
file. relais's watcher protocol, with its own retention rules, is in
relais `docs/quiet-runs.md`.

## The directory

`$QUIET_ACTIVE_DIR` when set and not empty (every test sets it), else
`${XDG_STATE_HOME:-$HOME/.local/state}/quiet/active`, and
`%LOCALAPPDATA%\quiet\active` on Windows. A writer creates the directory
0700.

An empty variable counts as unset. With no `HOME`, or an empty one, and no
`XDG_STATE_HOME`, there is no directory and nothing is written: never a
relative `.local/state` under the working directory. A writer also writes
nothing outside a Claude Code session (an empty `CLAUDE_CODE_SESSION_ID`):
only a session's runs are ever drawn.

## The entry file

`<pid>-<n>.json`, mode 0600, written atomically: a temp file
`.<pid>-<n>.json.tmp`, created new, then a rename over the entry. An
existing temp, a symlink included, fails the write rather than being
followed or truncated. A reader ignores dotfiles and anything not
ending in `.json`. `<n>` tells apart the concurrent checks of one process.
The file's name without `.json` is the run's `id`.

| field     | type             | meaning                                                              |
| --------- | ---------------- | -------------------------------------------------------------------- |
| `v`       | number           | format version, `1`: the version token a reader checks; a reader ignores any other |
| `tool`    | string           | `amont` or `amont-agent`                                             |
| `session` | string or null   | `CLAUDE_CODE_SESSION_ID` of the starter; null outside Claude Code    |
| `check`   | string           | the check name, or the program name for `amont-agent quiet`          |
| `repo`    | string           | the git toplevel, else the working directory                         |
| `log`     | string           | absolute path of the `.log.tmp` the output goes to                   |
| `pid`     | number           | the writing process                                                  |
| `started` | number           | unix milliseconds                                                    |
| `exit`    | number or null   | only once ended: the exit code, null when the run was killed         |
| `ended`   | number           | only once ended: unix milliseconds                                   |

Unknown fields are ignored. A new field that a reader may ignore keeps
`v` at 1; anything else is a new version.

## The writer's lifetime

1. The tool writes the entry when its capture opens.
2. When the run ends, the tool rewrites the entry with `exit` and `ended`
   (and `log` where the log now is) and leaves it, so a reader that polls
   still sees a run that ended between two of its polls.
3. Before writing, a tool sweeps the directory. It removes:
   - an ended entry, of either tool, whose `ended` is more than 60 s old;
   - an entry of its own tool without `ended` whose pid is dead.

   - a temp file older than an hour: `.<id>.json.tmp`, and the two forms
     writers used before `amont-quiet`, `<id>.json.tmp` and
     `.amont-agent-tmp-*.json`.

   A pid is gone only when `kill(pid, 0)` fails with `ESRCH`; any other
   answer keeps the entry. A pid that is no process, zero, negative or
   wider than an `i32`, is dead and is never passed to `kill`. Off unix
   every real pid counts as alive, and such an entry waits for its own
   `ended` or for a reader to remove it. The sweep is best-effort: a file
   that cannot be read or removed is left for the next one.

A reader may also delete an entry, by the rules of its own protocol.
