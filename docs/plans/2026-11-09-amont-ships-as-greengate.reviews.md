# Reviews: amont ships as greengate

## Full reviews (reference)

Round 1 (body 4a1a9bb31f8d): approve-with-changes from lang:python (35k/48 s), platform (39k/53 s), unix (32k/43 s), backend (46k/88 s, one blocking: greenpiste's guards), architect (38k/67 s), lang:rust (49k/87 s). 31 findings: 29 accepted (merged by hand), 1 the person's decision (old crates left as they are), architect#3 rejected by that decision.

Round 2 (body c60fd1bab354): approve-with-changes from lang:python (27k/11 s), backend (39k/38 s), platform (34k/22 s), architect (38k/25 s, high: more guards name `amont`), unix (34k/25 s), lang:rust (55k/62 s, blocking: Phase 0 named the wrong files; high: the fleet rule with copies). The blocking and highs applied by plan-apply.

Final checks (body a723e0dc9e94): backend (31k/26 s), lang:rust (31k/35 s), lang:python (27k/15 s) and platform (27k/18 s) approve-with-changes, medium and low only. codex: not installed.

Carried into implementation:
1. lang:python, platform: a Publish re-run lists the release's assets (`gh release view --json assets`), uploads each missing one without `--clobber`, compares an existing asset's digest with `SHA256SUMS` (a mismatch fails the run); tested by a re-run after deleting one archive.
2. lang:python: `release-manifest.py` and its test need python >= 3.11 (`tomllib`), run as `python3 -I`.
3. lang:python: the old-name archive doubles build and upload size; accepted until the census.
4. backend: `cargo install greengate` places no alias; README, release notes and the crate docs tell cargo users to run `greengate install`.
5. backend: Phase 4 checks a commit through a shim baked to `Cellar/amont/2.3.1` after `brew upgrade` and `brew cleanup` (fallback to the PATH `amont`).
6. backend: a re-run of the crates job skips crates already at 3.0.0; the census counts the `amont-*` assets' `download_count` plus the homelab runner's pulls, weekly.
7. backend: the Goal says "stay as aliases (copies)", not links.
8. platform: release notes and CHANGELOG say 2.x installers and CI images now receive the 3.0.0 binary under the name `amont`.
9. unix: the temporary is `.<name>.tmp.<pid>`, removed on error; each install sweeps stale `.greengate*.tmp.*`, `.amont*.tmp.*` and renamed-aside `*.old.exe`.
10. unix: the Phase 3 archive check lists to a file then reads its first line (no `| head` under `pipefail`).
11. lang:rust: the map records the copies-for-links change (Phase 4).
12. backend: the fleet's file-name rule splits on `/` and `\` and strips `.exe`; the unit test includes `C:\...\bin\amont.exe`.
13. backend: `guidance.rs` and `push_preflight.rs` spawn `amont` by name (see 14).
14. lang:rust: every spawn in greenpiste's guards (`land.rs`, `push_preflight.rs`, `heavy_build_unqueued.rs`, `guidance.rs`) goes through one `gate_binary()` trying `greengate` then `amont`; `guidance.rs` joins Phase 0.
15. lang:rust: the push-preflight test's hook names only `greengate` (so it fails before the change), and its check matches `amont` or `greengate`; the land test runs with only `greengate` on PATH.
16. lang:rust: `is_gate_program()` lives in its own `gate.rs`; `shape.rs`'s `VERB_PROGRAMS` gains `greengate` and `greengate-fleet`.
17. unix (round 1, low): the brew unlink window during `brew upgrade` is known (Phase 4).
18. backend (round 1): the Outcome uses a per-repository `git hook run pre-commit` probe.
19. architect (round 1): CI's installer test skips on a missing `greengate-*` asset.
20. architect (round 1): 3a-2 writes no new slot name before relais and greenpiste link `greenharness-slots`.
21. platform (round 1): `publish-npm` is held for 3.0.0 unless the person's first publish is done before the tag.
22. platform (final): `SHA256SUMS` covers the archives only; the manifest is derived from it and its own digest is checked separately on a re-run; the Behaviour re-run bullet is read as item 1 says (list assets, upload missing, compare digests).
23. lang:python (final): Phase 0 includes `guidance.rs` (item 14); the Phase 3 archive check reads the listing from a file (item 10).
24. backend (slots delta): greenpiste does not depend on `amont-slots` (only `amont-quiet`); the slots-contract note in the Decision log names relais as the consumer that must link `greenharness-slots` before 3a-2 writes a new slot name.
25. backend (slots delta): Phase 2 paths are named after the directory rename (`crates/greengate-runtime/...`).
26. lang:rust (slots delta): `check-no-deps.sh` updates its `ALLOWED` list, the workspace-crate regex and the echo line; the root `members` array and the crates' `.workspace = true` keys move too; the shared crates' Rust paths become `greenharness_slots::` and `greenharness_quiet::`, not `greengate_*`.
27. lang:python, platform (final): the re-run skip probes `https://crates.io/api/v1/crates/<name>/3.0.0` (HTTP 200 skips, version read from the registry, not the error text), waits briefly between dependent publishes for index lag, and lives in `scripts/publish-crates.sh` with a stubbed-probe test that checks the order and the skip.
28. platform (final): the token check is recorded in the PR (scopes and patterns read on the crates.io settings page) plus `cargo publish --dry-run` of the first crates; a half-published set is recovered by the re-run skip; `publish-crates` needs `standalone`, so a broken archive never leaves published crates.

