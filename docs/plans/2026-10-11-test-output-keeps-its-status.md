---
canonical: amont-agent:docs/plans/2026-10-11-test-output-keeps-its-status.md
phases: [1, 2, 3]
status: active
---
Part of [Test output that is cheap and keeps its exit status; a re-read that can be refused](https://github.com/fredericrous/amont-agent/blob/main/docs/plans/2026-10-11-test-output-keeps-its-status.md).

This repository carries phases 1–3: `amont run <check>` captured in a one-slot `Stage` whenever quiet is on, `Stage::finish` taking the check's `Outcome`, the quiet result (one ok line for `Passed`, messages plus excerpt and `full log:` otherwise, full log in CI), the shared capture and excerpt rules, and a copy of amont-agent's golden excerpt fixtures with a cross-check against its `MANIFEST.sha256`.

## Departures from the canonical plan (this repository's slice)

Recorded in the canonical plan's Decision log by its closing pull request.

1. **No SIGINT/SIGTERM handler for the quiet capture.** `staged_only` already installs one that brings back the user's parked unstaged changes; a second on the commit path would compete with it. An interrupted run leaves `<check>.<pid>.log.tmp`, complete up to the interrupt and named by the running line (`holds-until:` in `capture.rs`).
2. **Inside a stage a passing check prints nothing**; only `amont run <check>` prints the `ok` line. Piloted on amont-agent, a cached pre-commit stage grew 471 → 633 bytes from `ok … log=` lines; it is now 302.
3. **A check whose tool printed nothing keeps today's output** and no log; a failure whose tool printed nothing says `(no output)` and the runner's exit code, with no log path (there is no log).
4. **Two pipes merged by whole lines, not one shared pipe.** The capture reuses `run_observed`'s two readers; each now hands on whole lines, so the streams merge at line boundaries rather than mid-line.
5. The relais run was refused (baseline `make check` over its 1200 s limit at load ~15–30); the work was done in the session.

## Observed — amont PR (branch `feat/quiet-run`)

| Input | Expected | Observed |
|---|---|---|
| `amont run clippy --all-files` on amont-agent, piped, installed 1.49.1 vs branch | under 200 B on a pass | 3.4 KB → 206 B (`running clippy, log …tmp` after 5 s, then `ok clippy 17.3s log=…`); log holds clippy's full output |
| `amont run --all-files` (pre-commit stage) on amont-agent, cached | no bigger than today | 471 B → 302 B: branch warning and `24 check(s) passed`, no per-check lines |
| `amont run cargo-test` on amont-agent's stamped tree | as today | 0 B with both binaries (stamped skip) |
| `quiet_run`: passing declared check | one `ok` line, log holds everything | pass |
| `quiet_run`: failing check | exit 1, panic in full, `probe failed, runner exit 101`, `full log:`, no build noise | pass |
| `quiet_run`: `--raw`, `amont.quiet never` | the tool's own output | pass, both |
| `quiet_run`: `CI=true` failure | whole log printed | pass (lines 0 and 1999 of 2000) |
| `quiet_run`: `CI=true` + injected write failure (`test-hooks`) | excerpt, `log not written:`, no `full log:` | pass |
| `quiet_run`: `warn` check failing | exit 0, excerpt, `full log:` | pass |
| `quiet_run`: `--quiet` with `amont.progress false` | still captured, one `ok` line | pass |
| `quiet_run`: tool printing nothing, exit 4 | `(no output)`, `probe failed, runner exit 4` | pass |
| `quiet_run`: run still going at 5 s | `running probe, log ….log.tmp` on stderr, then `ok` | pass |
| `quiet_run`: declared tool missing | warns, exit 0, names the tool, no `ok` | pass |
| `quiet_run`: stage pass / stage failure | silent / excerpt + `full log:` | pass |
| `excerpt_golden` (4) over the fixtures shared with amont-agent | same excerpts as amont-agent; manifest pinned | pass |
| capture unit tests incl. 50 MB bound, 0600, pruning keeps 10 and every `.tmp`, injected write failure | pass | pass |
| CI cross-check, run locally against amont-agent `main` | manifests equal | equal |
| `cargo clippy --workspace --all-targets --all-features -D warnings` | clean | clean |
| Open: twice (first run after a fixture change, load ~15) the failing-check test lost the summary's last line; 22 runs since passed, incl. 16 under parallel load. Consistent with the stream splice fixed in `d390a25`; the test now prints the log if it recurs. | — | — |
