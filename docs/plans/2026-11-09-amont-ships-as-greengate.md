---
status: active
branch: feat/greengate-name
repos: [amont, amont-agent, homebrew-tap, decisions]
adrs: [ADR-0064]
---
# amont ships as greengate

## Review panel

📄 Full reviews: [2026-11-09-amont-ships-as-greengate.reviews.md](2026-11-09-amont-ships-as-greengate.reviews.md)

👉 **Decide:** none — approve if greengate 3.0.0 (names module, crates, binaries with copy aliases, packaging, shipped manifest), gated on greenpiste learning `greengate` first, is the right first rename slice.
📍 amont, amont-agent, homebrew-tap, decisions · nothing built · next: Phase 0 in greenpiste. Panel: backend, lang:rust, lang:python, platform, architect, unix (opus-5-5).
**Changed by review:** greenpiste's guards learn `greengate` first (blocking); aliases are copies written by rename; old-name archives built separately until the census; shared crates named greenharness-slots/-quiet (the person).
**Verdicts:** round 1 approve-with-changes 6 (1 blocking); round 2 approve-with-changes 6 (1 blocking, 2 high, applied); final checks approve-with-changes 4; the person renamed the shared crates (greenharness-slots, greenharness-quiet), delta approve 2, approve-with-changes 2; 0 rework; 28 findings carried into implementation (listed in the full reviews).

## Picture

```
 today (2.3.x)                    after this plan (3.0.0)
 -------------                    -----------------------
 crates  amont amont-fleet        greengate greengate-fleet
         amont-runtime            greengate-runtime
         amont-quiet amont-slots  greenharness-quiet greenharness-slots
                                  (shared; old crates frozen)
 binary  amont, amont-fleet       greengate, greengate-fleet
                                  amont, amont-fleet: aliases (copies)
 npm     amont, @amont-hooks/*    @greenharness/greengate(+ targets)
                                  amont: deprecated, frozen at 2.3.0
 brew    fredericrous/tap/amont   .../greengate; amont renamed to it
 assets  amont-<v>-<t>.tar.gz     greengate-<v>-<t>.tar.gz, the old
                                  name too until the census, and
                                  greenharness.toml (grh pins it)
 says    "amont: ..."             "greengate: ..." (one names module)
 reads   AMONT_*, amont.*, ...    unchanged here: 3a-2 reads both
```

The tool is installed, published and spoken of as greengate; every name
it reads from disk, git config or the environment stays as it is until
plan 3a-2, and old hook shims keep working through the aliases.

## Goal

Build step 3 of ADR-0064 for greengate, first slice. The programme
(`decisions/docs/programmes/gate-guardrails`) renames amont to greengate
with aliases (`harness.renames`: read both, write new, warn; old binaries
stay as links). The person split greengate's step into three plans: 3a-1
(this one: the names, binaries, crates, packaging, the shipped manifest
and a release), 3a-2 (every name a program reads: env vars, git config,
`amont.conf`, ledgers, state paths, markers, k8s labels, the slots
contract), 3a-3 (signed-notes namespaces, attest first). The fleet tool
stays greengate's own as `greengate-fleet`; a generic fleet view in grh
is a later ticket.

## Non-goals

- Every name the program reads (env vars, git config keys, `amont.conf`,
  `.git/amont-*` ledgers, `$TMPDIR/amont-push-*`, state paths, file-format
  headers, AGENTS.md markers, k8s labels, the slots env and directory):
  plan 3a-2. Messages that name those keys keep naming them.
- Signed-notes namespaces and git-attest: plan 3a-3.
- greenpiste's (amont-agent) own rename, relais, grh's pins and catalog
  `bin`: their own step-3 plans; this plan touches amont-agent only for the
  guard prerequisite below. Renaming the local `amont/` folder: after worktrees
  drain.
- The fleet sweep (step 4): other repositories' shims, pins and docs.

## Code review
- amont: backend, lang:rust, lang:python, platform, unix, tui, architect
- amont-agent: backend, lang:rust (the land/push guard prerequisite)
- homebrew-tap: none — no reviewed file type (the formula is checked by `brew audit` and `verify-brew`)
- decisions: none — markdown only (the proposal counted amont's Phase 5 files)

## Behaviour

- **Prerequisite: greenpiste reads `greengate` as `amont`.** greenpiste's
  land guard and the rules that name a program (`land.rs`, `shape.rs`,
  `rules/push_preflight.rs`, `chain_with_and.rs`, `pipe_to_tail.rs`,
  `heavy_build_unqueued.rs`, `test_output_trimmed.rs`) recognise only the
  program `amont`; a hint saying `greengate land` would
  walk around them. They treat `greengate` (and `greengate-fleet`) exactly
  as `amont`, released and installed on this machine before greengate
  3.0.0 is tagged. Until then greengate's hints are not switched.
- **One names module.** `greengate-runtime` gains `names.rs` holding the
  product, binary, fleet binary and old alias names, the installer URL and
  the package names; every message, usage text, error prefix
  (`greengate: …`, `greengate-fleet: …`), `--version` line and command hint
  (`greengate land`, `greengate run …`) is built from it (about 105 source
  files print a name today). A test fails on a new literal `amont `,
  `amont: ` or `amont-fleet` outside `names.rs` and the 3a-2 key and path
  names.
- **Crates.** The five crates are published as `greengate`,
  `greengate-fleet` and `greengate-runtime`, and the two crates shared
  with greenpiste and relais as `greenharness-quiet` and
  `greenharness-slots` (the person chose: shared harness infrastructure
  carries the harness's name, not greengate's), at 3.0.0, in the existing
  order; directories and Rust
  paths follow (`greengate_runtime::…`), and `[workspace.dependencies]`
  takes the new keys with `version = "3.0.0"`. The `amont-*` crates are not
  republished and not yanked (the person chose): their pages keep 2.3.1,
  and greengate's README and docs say they were renamed.
  `scripts/check-no-deps.sh` names the new crates. The crates.io token has
  the `publish-new` scope and patterns covering both `greengate*` and
  `greenharness-*`, both checked before the tag; before Phase 2,
  `greengate`, `greengate-fleet`, `greengate-runtime`, `greenharness-quiet`
  and `greenharness-slots` are checked free on crates.io (HTTP 404 on
  `https://crates.io/api/v1/crates/<name>`, sent with a `User-Agent`
  header: crates.io answers 403 without one).
- **Binaries and aliases.** The binaries are `greengate` and
  `greengate-fleet`. `greengate install` (and `install.sh`) also places
  `amont` and `amont-fleet` in the same directory as copies of them, so
  shims with a baked `amont` path keep working and an old 2.x `amont
  install`, which copies onto its target, replaces only the alias. Each
  binary and alias is written to a temporary name in the same directory
  and `rename(2)`d over the target, never removed first, so a commit
  during install never finds it missing or half-written; on Windows an
  existing `.exe` is renamed aside first. `installed_name` and `name_for`
  take their names from `names.rs`. An alias invocation prints no warning.
  New shims bake the `greengate` path and on the fallback try
  `~/.local/bin/greengate`, `greengate.exe`, `amont`, `amont.exe`, then
  PATH; their error text names greengate, its installer URL, `cargo
  install greengate` and `brew install fredericrous/tap/greengate`.
  `greengate-fleet` finds `greengate` on PATH, then `amont`.
- **The fleet view stays quiet.** `greengate-fleet` keeps a frozen copy of
  the 2.3.1 shim template and counts a shim rendered from it, or baked to a
  path whose file name is `amont` or `greengate` (no canonicalising: an
  alias is a copy, so its canonical target is itself), as
  Ok/Current, so 3.0.0 reports no repository Drifted or Stale and `fix
  --apply` does not rewrite the fleet (that is step 4's sweep).
- **Release assets.** `greengate-<v>-<target>` archives hold `greengate`,
  `greengate-fleet` and `templates/`. Until the step-5 census finds zero,
  each release also builds a separate `amont-<v>-<target>` archive from an
  `amont-<v>-<target>/` directory holding regular files `amont` and
  `amont-fleet` (and `templates/`), because 2.x `install.sh`/`install.ps1`
  and CI images (the homelab runner's Dockerfile extracts the member
  `amont-<v>-<t>/amont`) build that name from `releases/latest`. All go in
  `SHA256SUMS` and the attestation.
- **The shipped manifest.** `greenharness.toml` at the repository root
  (grh's v2 format, `bin = "greengate"`, no sums);
  `scripts/release-manifest.py` (standard library) writes `[tool.release]`
  (repo, version, the four unix targets' sha256 from `SHA256SUMS`) into the
  copy uploaded before `SHA256SUMS`; the guard checks `[extension] version`
  against the tag. A re-run of Publish on an existing release uploads
  without `--clobber` and skips `greenharness.toml` when it is already
  there, so a released manifest is never replaced.
- **Standalone install check.** A `standalone` job in `release.yaml`
  (`needs: build`, listed in `publish.needs`) installs the built archive on
  a clean runner with no grh and runs `greengate --help`, `greengate list
  --json` in a scratch repository and `amont --version` through the alias
  (ADR-0064 `harness.standalone-tools`). CI's installer test skips on a
  missing `greengate-*` asset as it did for 2.x.
- **npm.** `@greenharness/greengate` with platform packages
  `@greenharness/greengate-<target>`; its bin maps `greengate` and `amont`.
  🧑 The person publishes each new package once (a one-time granular token
  or npm's pending publisher) before the 3.0.0 tag; if that is not done,
  `publish-npm` stays off for 3.0.0 and is re-run on the tag afterwards.
  Then trusted publishing takes over. `amont` on npm stays at 2.3.0 and
  gets `npm deprecate` naming the new package.
- **Homebrew, ordered after the assets.** The release's `publish-tap` job,
  after Publish succeeded, writes `Formula/greengate.rb` (installs
  `greengate`, `greengate-fleet` and the aliases), adds
  `formula_renames.json` (`amont` → `greengate`) and removes
  `Formula/amont.rb` in one tap commit, so `brew upgrade` never meets a
  formula whose assets do not exist; `verify-brew` installs
  `fredericrous/tap/greengate`. `bump-tap.py` reads only `greengate-`
  entries of `SHA256SUMS`, accepts `amont-` or `greengate-` urls in a
  formula and writes `greengate-`, and its stale-version check covers both
  prefixes. Caveats name greengate.
- **Docs.** README, `docs/` (book title greengate), `install.sh` and
  `install.ps1` say greengate and mention the aliases; the programme map
  records the three-plan split and adds the `grh` fleet-view ticket.
- **Rollback.** Published crate and package names cannot be withdrawn;
  the way back is `install.sh` pinned to 2.3.1 (its `amont-2.3.1-*` assets
  stay), and the old formula name keeps resolving through the rename.

## Phases

- [ ] Phase 0 — prerequisite in greenpiste: its land and push guards treat
  `greengate` and `greengate-fleet` as `amont`, released and installed
  before the 3.0.0 tag:
  `amont-agent/crates/amont-agent-guard/src/land.rs`,
  `amont-agent/crates/amont-agent-guard/src/shape.rs`,
  `amont-agent/crates/amont-agent-guard/src/rules/push_preflight.rs` (its
  hook-file check accepts a greengate shim),
  `amont-agent/crates/amont-agent-guard/src/rules/chain_with_and.rs`,
  `amont-agent/crates/amont-agent-guard/src/rules/pipe_to_tail.rs`,
  `amont-agent/crates/amont-agent-guard/src/rules/heavy_build_unqueued.rs`,
  `amont-agent/crates/amont-agent-guard/src/rules/test_output_trimmed.rs`,
  `amont-agent/CHANGELOG.md`; every match on the program `amont` goes
  through one shared `is_gate_program()`, so a later name is one change.
- [ ] Phase 1 — the names module and every message through it:
  `crates/amont-runtime/src/names.rs`, `crates/amont-runtime/src/lib.rs`,
  `crates/amont/src/main.rs`, `crates/amont-fleet/src/main.rs`,
  `crates/amont-runtime/src/dispatch.rs`, `crates/amont/tests/it/names.rs`,
  and every crate source printing a name (`rg -l '"amont[ :-]'
  crates/*/src`: `land.rs`, `slot_cmd.rs`, `rehearsal.rs`, …).
- [ ] Phase 2 — crate and binary renames, the aliases, the shim template
  and the fleet's frozen template: `Cargo.toml` (`[workspace.dependencies]`
  keys and `version = "3.0.0"`), `crates/*/Cargo.toml` (directories renamed
  to `crates/greengate*` and `crates/greenharness-{quiet,slots}`),
  `crates/amont-runtime/src/install.rs`,
  `crates/amont-runtime/templates/hooks/pre-commit`, `templates/hooks/`,
  `crates/amont-fleet/src/shim.rs`, `scripts/check-no-deps.sh`, `Makefile`.
- [ ] Phase 3 — packaging and CI: `install/install.sh`,
  `install/install.ps1`, `npm/amont/package.json.in`,
  `npm/platform/package.json.in`, `scripts/npm-pack.sh`,
  `scripts/bump-tap.py`, `scripts/test_bump_tap.py`,
  `scripts/release-manifest.py`, `scripts/test_release_manifest.py`,
  `.github/workflows/release.yaml` (the publish-crates loop in the order
  `greenharness-quiet greenharness-slots greengate-runtime greengate
  greengate-fleet`, an already-uploaded crate counting as published on a
  re-run, its dependency-order comments and the `cargo install greengate`
  step summary; old-name archives, `standalone`,
  Publish's re-run branch, `publish-tap` writing the new formula),
  `.github/workflows/ci.yaml` (installer test skip glob), `greenharness.toml`.
- [ ] Phase 4 — docs, the map and release 3.0.0 (tag only after Phase 0 is
  installed, the crates.io token scope is checked and the person's first
  npm publish is done or `publish-npm` is held): `README.md`, `docs/`,
  `CHANGELOG.md`, `decisions/docs/programmes/gate-guardrails/map.md`,
  `decisions/docs/programmes/gate-guardrails/t12-grh-fleet-view.md`; the
  tap files are written by the release's `publish-tap`
  (`homebrew-tap/Formula/greengate.rb`, `homebrew-tap/formula_renames.json`,
  `homebrew-tap/Formula/amont.rb` removed).

## Decision log

- 2026-11-09 — the person chose three plans for greengate's step 3:
  names/binaries/crates/packaging (this one), every read name (3a-2),
  signed notes (3a-3).
- 2026-11-09 — the person chose: the fleet tool stays greengate's own as
  `greengate-fleet` (a separate binary, because the hook binary must have
  no dependencies), with `amont-fleet` as an alias; a generic
  `grh status --fleet` over every part's declared query is a later ticket.
- 2026-11-09 — the person chose: the slots contract renames in 3a-2,
  reading both env names and directories. Until then `amont-slots` 2.x
  (relais, greenpiste) and `greenharness-slots` 3.x implement the same
  contract unchanged; 3a-2 writes no new slot name before both consumers
  link `greenharness-slots`.
- 2026-11-09 — the person chose: the crates shared with greenpiste and
  relais are `greenharness-slots` and `greenharness-quiet`, not
  `greengate-*`; their env names and directory in 3a-2 follow the
  harness's name too.
- 2026-11-09 — the person chose: the `amont-*` crates are left as they are
  (no final docs-only publish); greengate's README and docs carry the
  rename.
- 2026-11-09 — the aliases in `~/.local/bin` are copies written by rename,
  not links: an old 2.x `amont install` copies onto its target, and through
  a link it would replace the greengate binary itself (panel: backend,
  unix, lang:rust).
- 2026-11-09 — old-name `amont-<v>-<t>` archives ship on every release
  until the step-5 census, built from their own directory with regular
  files, because 2.x installers and CI images build that name from
  `releases/latest` (panel: architect, platform, lang:python).
- 2026-11-09 — the tap changes only from the release's `publish-tap`,
  after the assets exist (panel: platform, backend).
- 2026-11-09 — version 3.0.0: the command, crates and packages change
  name; the aliases keep 2.x behaviour, but a new major marks the switch.
- 2026-11-09 — an alias invocation prints no warning: hook shims call the
  baked `amont` path on every commit; the warning `harness.renames` asks
  for is per old name *read*, which 3a-2 handles.
- 2026-11-09 — greenpiste's guards learn `greengate` first (Phase 0): a
  `greengate land` hint must not walk around them (panel: backend,
  blocking).

## Verification

- Phase 0: greenpiste's guard tests treat `greengate land` and a push via
  `greengate` exactly as `amont` in land, shape, push-preflight (a
  greengate shim reads as guarded), chain-with-and, pipe-to-tail,
  heavy-build-unqueued and test-output-trimmed; the released greenpiste is installed here
  before the tag.
- Phase 1: the names test fails on each of a planted `"amont "`,
  `"amont: "` and `"amont-fleet"` literal; `greengate --help`, an error and
  `--version` say greengate; the suite passes with messages updated.
- Phase 2: `cargo build` produces `greengate` and `greengate-fleet` and
  the package names alone (`cargo tree -p greengate -e normal,build
  --prefix none --format '{p}' | cut -d' ' -f1 | sort -u`, compared as a
  list, never through grep's exit status: every crate path contains the
  `amont/` checkout folder) include no name starting with `amont`; `greengate
  install` in a scratch HOME places both binaries and both alias copies;
  shims baked by 2.3.1 (at `~/.local/bin/amont`, a brew Cellar path and a
  pnpm store path) still run the gate; a loop of `git commit --allow-empty`
  through a 2.3.1 shim while `greengate install` replaces the copy has 0
  failures; after a 2.3.1 `amont install` over the aliases, `greengate
  --version` still says 3.0.0; a new shim bakes `greengate`;
  `greengate-fleet` reads a 2.3.1-rendered shim as Ok/Current (unit test);
  `check-no-deps.sh` passes.
- Phase 3: a release dry run builds `greengate-*` and `amont-*` archives;
  `tar tzf amont-…tar.gz | head -1` is `amont-<v>-<t>/`, and the homelab
  Dockerfile's member extraction and 2.3.1's `install.sh` against it both
  leave an `amont` that answers `--version`; `SHA256SUMS` lists every
  archive and the manifest, whose `[tool.release]` sums match
  (`test_release_manifest.py` reads it back with `tomllib`);
  `test_bump_tap.py` passes (both-names `SHA256SUMS` writes only
  `greengate-` urls, an `amont-` formula moves to `greengate-`, a stale
  `greengate-` version is refused); `npm-pack.sh` stages
  `@greenharness/greengate` and its targets and the packaging test agrees;
  the `standalone` job passes on a runner with no grh; a re-run of Publish
  leaves the manifest asset's digest unchanged.
- Phase 4: the tag's release run succeeds; the crates, npm packages (or
  the held npm job), the tap commit and GitHub assets are checked by name
  and version; `brew update && brew upgrade` on a machine with
  `fredericrous/tap/amont` 2.3.1 moves it to `greengate` and `amont
  --version` answers (the brief unlink window during the upgrade is
  known); `install.sh` from a scratch HOME installs greengate and the
  aliases; `relais programme check` passes on the map.

## Outcome

A day after 3.0.0: every shim in the fleet resolves (`git hook run
pre-commit` in each repository finds a binary: 0 "not found"), `amont
--version` on this machine says greengate, `greengate-fleet` reports no
repository Drifted or Stale, and grh's step-3 plan can pin
`greenharness/greengate` v3.0.0's manifest.

<!-- panel: repos=amont,amont-agent,homebrew-tap,decisions adds=contract reviewers=backend,lang:rust,lang:python,platform,architect,unix body-sha=5c1090a8bc50 -->
