# The quiet-run active entry

amont (`amont run`, pre-commit checks) and `amont-agent quiet` keep a
check's output out of the agent's context: the output goes to a
`.log.tmp` file. So that a person can still see such a run live, each
tool writes one small JSON file while the run is active, and a reader
such as relais's `relais quiet watch --protocol` turns those files into
something it can draw. A writer never depends on a reader.

This page is the contract for that file. amont owns it (decisions
ADR-0039, `harness.cross-tool-reads`): amont-agent writes the same file
and conforms to this page, and a reader documents only what it does with
the file. relais's watcher protocol, with its own retention rules, is in
relais `docs/quiet-runs.md`.

## The directory

`$QUIET_ACTIVE_DIR` when set and not empty (every test sets it), else
`${XDG_STATE_HOME:-$HOME/.local/state}/quiet/active`, and
`%LOCALAPPDATA%\quiet\active` on Windows. A writer creates the directory
0700.

With no `HOME` at all, there is no directory and nothing is written. An
**empty** `HOME` is a known divergence: amont joins `.local/state` onto
it and writes under the working directory, while amont-agent and relais
name no directory. It is recorded here, not changed; making the writers
agree is the follow-up ADR-0039 names.

## The entry file

`<pid>-<n>.json`, mode 0600, written atomically: a temp file whose name
does not end in `.json` or starts with `.` (amont writes `<name>.json.tmp`),
then a rename over the entry. A reader ignores dotfiles and anything not
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

   A pid is alive when `kill(pid, 0)` succeeds or fails with `EPERM`. Off
   unix amont counts every pid alive, and such an entry waits for its own
   `ended` or for a reader to remove it. The sweep is best-effort: a file
   that cannot be read or removed is left for the next one.

A reader may also delete an entry, by the rules of its own protocol.
