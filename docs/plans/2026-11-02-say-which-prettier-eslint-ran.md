---
status: active
branch: feat/ambient-tool-warning
repos: [amont]
adrs: [build.toolchain-source (ADR-0019)]
---

# Say which prettier and eslint a check ran

## Review panel

👉 **Decide:** none — approve if "undeclared tool: warn and run; declared but not installed: skip with an install hint" is the policy you want.
📍 amont · plan reviewed · next: worktree, Phase 1. Panel: backend, lang:rust, unix, tui (opus-5-5), codex.
**Changed by review:** source reported by the resolver, not guessed from the path; two-line warning with the exact `pnpm add -D` command; probes bounded by `tree_run::run_output` (5 s, whole-group kill).
**Verdicts:** round 1: 4 approve-with-changes; Codex approve-with-changes; final-body re-run: 4 approve or approve-with-changes, two highs applied. 7 findings carried into implementation (in the full reviews).
📄 Full reviews: [2026-11-02-say-which-prettier-eslint-ran.reviews.md](2026-11-02-say-which-prettier-eslint-ran.reviews.md)

## Goal

amont's prettier and eslint checks find their binary through `resolve_tool`
(`crates/amont-runtime/src/hooks/common.rs:219`). The order is the repository's
`node_modules/.bin`, then the main worktree's, then whatever is on PATH, then a copy
`npx --no-install` has cached. They fall back to PATH without a word. On 2026-11-02, in
greenharness.github.io, three prettier versions on one laptop disagreed about the same files, and
the commit check quietly used a fourth source, npx's cached 3.9.10. ADR-0019
(`build.toolchain-source`) says a repository pins its tools, "never the machine's PATH". After
this change the check says when it does not use a pinned copy, and it refuses to run a version
the repository did not ask for.

## Non-goals

- No blocking on an undeclared tool. An unpinned tool warns and still runs, and the commit goes
  through as before. Hooks stay advisory where the code itself is not wrong.
- No change for tools other than prettier and eslint, the only two callers of `resolve_tool`.
- No workspace-package lookup. Only the root `package.json` (and the main worktree's, for a linked
  worktree) is read. A tool declared only in a sub-package of a monorepo counts as undeclared, and
  warns only when no `node_modules/.bin` copy exists (a hoisted copy is pinned). The warning text
  and the docs say so.
- No version comparison. A stale `node_modules` (or the main worktree's) still runs. Only PATH and
  npx copies are refused when the tool is declared.
- No JSON dependency. The crate stays parser-free (`snapshot_prep.rs:33-36`). The scan is a
  targeted one, like `declared_manager` (`snapshot_prep.rs:407`).

## Code review

- amont: backend, lang:rust, unix, tui

## Behaviour

`resolve_tool` keeps its order and its signature. It is split so that it delegates to
`resolve_tool_from(root, tool) -> Option<(Vec<String>, Source)>`, with
`Source::{Pinned, Path(String), Npx { version: Option<String> }}`. The source is reported by the
step that found the binary, never guessed from the path text. The npx probe keeps its stdout,
which is the version, instead of throwing it away. A new `resolve_js_tool(root, tool)` adds whether
the root `package.json` declares the tool under `dependencies` or `devDependencies`. When it does,
the lookup stops after the two `node_modules/.bin` dirs: no `which`, no npx probe. The prettier
(`hooks/prettier.rs:59`, which drops its `contains("node_modules/.bin")` guess) and eslint
(`hooks/lint_js.rs:48`) checks use it.

| package.json | binary found | What the check does |
| --- | --- | --- |
| declares it | pinned | as today, no new line |
| declares it | none in `node_modules` | does not run. The check prints one warning, `prettier is in package.json but not installed: run pnpm install` (the manager named by `packageManager`, else from the lockfile, else npm), and the dispatcher adds its usual `could not run` line (`dispatch.rs:1300`). Returns `Unavailable`, so the commit is not blocked. A PATH or npx copy never runs. |
| does not declare it | pinned | runs as today, no new line (hoisted from a workspace package or a transitive dependency; the copy is pinned, so ADR-0019 holds) |
| does not declare it | PATH or npx | runs as today, plus two warning lines. First the fact: `prettier 3.9.10 from /usr/local/bin/prettier is not pinned in package.json (only the root package.json is read)`, or `from npx's cache (npx --no-install prettier)` for npx. Then the action: `Pin it so every machine and CI agree (ADR-0019): pnpm add -D prettier@3.9.10`, with the manager chosen as for the install hint. The outcome is unchanged (`Passed`/`Failed`), so a clean run is still stamped (`Warned` would stop stamping, `run_stamp.rs:365`). |
| does not declare it | none | as today (`Unavailable` with the install hint, or `Passed` when the repo has no config) |

The version on the PATH row comes from one `<argv> --version`. Both the PATH `--version` and
the npx probe run through `tree_run::run_output` (`tree_run.rs:246`) with a fixed 5 s deadline,
not `status_within` (`common.rs:957`). `status_within`'s bound is `amont.timeout` (default
3600 s), its kill reaches the direct child only, it captures no stdout, and it takes a host slot.
`run_output` gives the probe a null stdin, its own process group, and one pipe for stdout and
stderr (`tree_run.rs:69-83, 271-283`). So the version is the first line of that output shaped
like a version (`^v?\d+\.\d+\.\d+`), with a leading `v` stripped (eslint prints `v9.x.y`),
never simply the first line: an `npm warn …` or node warning line is skipped. When the probe
times out, the warning says `unknown version (--version did not answer in 5 s)`. On a non-zero
exit or with no such line it says `unknown version`. In both cases the `@<version>` is dropped
from the command and the check still runs. A timed-out npx probe gives no candidate, as a failed
probe does today. One branch of `run_inner` returns `TimedOut` without `kill_tree`: the direct
child has exited, but a grandchild still holds the pipe (`tree_run.rs:343-344`). Phase 1 makes
that branch signal the whole group too. On the npx row the version is the probe's stdout, so no process
starts beyond today's. The hard-coded `Run npm install` hints (`prettier.rs:70-73`,
`lint_js.rs:51-54`) are replaced by the manager-aware hint, not printed alongside it.

The scan of `package.json` is targeted. It finds the `"dependencies"` and `"devDependencies"`
objects by brace matching outside strings, then looks for the `"<tool>"` key at their top level. A
top-level `"prettier": { … }` config key does not count as a declaration. A file that cannot be
read or parsed counts as "not declared".

The warning is printed with the existing `warn()` (`common.rs:1803`). It is suppressed for a
deferred check (ADR-0013), as every warning is. `amont.quiet` does not hide it.

## Phases

- [ ] Phase 1: `resolve_tool_from`, `resolve_js_tool` and the `package.json` scan. Unit tests of the
      scan cover: a declared devDependency → declared; a top-level `"prettier": {…}` config → not;
      `"prettier-plugin-x"` in devDependencies → not; prettier only in peer/optional deps or in
      `pnpm.overrides` → not; a `scripts.prettier` key → not; `\"` and `{` inside string values;
      cut-off JSON → not declared. Prettier and eslint move onto it. Integration tests use a
      recording fake `prettier`/`eslint` with PATH set to the shim dir alone plus a git symlink, as
      `zero_warnings.rs:34-51` does (never `set_var`), under `#[cfg(unix)]`. A bare script is not
      found through PATHEXT on the Windows job (`common.rs:262-266`). Cases:
      undeclared on PATH → it runs, both warning lines name its version and path, commit exit 0;
      undeclared through npx → the source reads `npx's cache`, and npx starts once;
      a fake that reads stdin → the `--version` call does not hang;
      a fake whose `--version` starts a child and then stalls, and one that exits leaving a
      background child holding stdout → each commit completes within about 5.5 s and
      `tree_run::group_alive(pgid)` is false afterwards;
      a fake that prints `npm warn …` on stderr before its version → the warning and the pin
      command name the version;
      declared and not installed → the fake never runs, the install warning prints, commit exit 0;
      undeclared but in `node_modules/.bin` → no new line; declared and pinned → no new line.
      An eslint pair covers the declared/undeclared states. The plan also lists any CI job that
      runs these checks before installing.
- [ ] Phase 2: docs. Update the "Tool versions are not pinned" paragraph of
      `docs/index-fidelity-and-run-modes.md` (~line 787), plus CHANGELOG and AGENTS.md (`amont
      agents-md`).
- [ ] Phase 3: release (`tag-release`, minor bump), install on the laptop as amont is installed
      today, and the live check below.

## Decision log

- 2026-11-02: warn and run when the tool is undeclared; skip with an install hint when it is
  declared but not installed. The person chose both ("commit checks should use the prettier
  declared in package.json … if not declared it should warn"). Skipping instead of failing follows
  the existing `Unavailable` convention for a missing binary (`prettier.rs:67-73`).
- 2026-11-02: greenharness.github.io already pins prettier 3.9.10 (#10), so it is the first repo
  where the "pinned" row applies.
- 2026-11-02: a declared tool with no `node_modules` now reports `Unavailable`, which is not
  stamped (`run_stamp.rs:366`), where it used to run the PATH copy. Any CI job that runs these
  checks before installing loses formatting. Phase 1 lists such jobs (expected: none).

## Verification

- The scan's unit tests: `{"devDependencies":{"prettier":"3.9.10"}}` → declared;
  `{"prettier":{"semi":false}}` → not declared; a string value containing `\"devDependencies\"` →
  not declared (plus the Phase 1 cases).
- Each integration test fails with the change reverted, checked once by hand; the PR names the
  failing tests.
- `make check` exits 0, and Windows CI is green (the new tests are skipped there).
- Timing: a clean commit with an npx-only prettier, before and after the change. Expected: no
  extra process, and wall time within noise.
- Live, after install: in a scratch repo with `.prettierrc` and a `package.json` without prettier,
  stage a `.md` file and commit → the warning names the version and path, and the commit lands. Add
  `"prettier": "3.9.10"` to devDependencies without installing → the install warning, prettier
  does not run, and the commit lands. Run the install → no new line.
- In greenharness.github.io's live checkout (no `node_modules`), `amont run prettier` on a staged
  file → the install warning and `could not run`, not a run of PATH's prettier, and exit 0.

## Outcome

Filled at close.

<!-- panel: repos=amont reviewers=backend,lang:rust,unix,tui,codex body-sha=25247be5b9f9 -->
