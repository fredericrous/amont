---
status: done
branch: feat/greengate-last-amont-names
repos: [amont, amont-pack-homelab, amont-pack-java, homelab, decisions]
adrs: [ADR-0064, ADR-0068]
---
# greengate's last amont names, and the packs renamed

## Review panel

👉 **Decide:** none — approve if greengate's and the packs' last `amont` names go by hard rename (one fallback read for committed files until the 3a-2 census), with your `greengate trust` in homelab after Phase 3.
📍 amont, amont-pack-homelab, amont-pack-java, homelab, decisions · nothing built · next: Phase 1 (greengate 4.3.0). Panel: backend, lang:go, lang:rust, platform, architect (opus-5-5).
**Changed by review:** no pack-marker respell in `migrate --tracked` (trust holds); the stash suffixes stay (a read-only legacy format); the full pack file list with fallback tests, a two-build diff and `Repo.ConfigPath`; trust in the same sitting as the Phase 3 merge, with a Rollback section.
**Verdicts:** round 1 approve-with-changes 5; round 2 approve 1, approve-with-changes 3; final approve 2 (backend, lang:go); 0 rework; 1 rejected; 6 carried into implementation (in the full reviews).
📄 Full reviews: 2026-11-13-greengates-last-amont-names.reviews.md

## Picture

```
 where              today                       after
 -----              -----                       -----
 greengate.conf     # amont:pack:start/end      # greengate:pack:start/end
 tracked file       .amont-audit-waivers        .greengate-audit-waivers
 inline comment     amont:allow-secret          greengate:allow-secret
 .git cache         refs/notes/amont-audit      refs/notes/greengate-audit
 check JSON         amont-check-v1              greengate-check-v1
 printed text       amont trust/list/land       greengate trust/list/land
 hook template      __AMONT_BIN__               __GREENGATE_BIN__
 stamp commits      amont <amont@localhost>     greengate <greengate@...>
 AGENTS.md block    Git hooks (amont)           Git hooks (greengate)
 homelab pack       amont-pack-homelab          greengate-pack-homelab
   its binary       amont-homelab               greengate-homelab
   its config       .amont-homelab.yaml         .greengate-homelab.yaml
 java pack          amont-pack-java             greengate-pack-java

 1 greengate 4.3: new names written, consumer files read both
 2 packs renamed, released   3 homelab re-adds its pack
 4 recorded; 3a-2's census renames the six waiver files, drops old reads
```

Each row is one `amont` name greengate or a pack still uses; the right
column is the only name left once the census has run.

## Context

The person's end state for the gate-guardrails programme (2026-11-12): "no
amont, only green*", and "amont-pack-homelab and amont-pack-java get
renamed". Plan 3a-2 renamed greengate's data names and plan 3a-3 renames the
signed notes, but a read of greengate 4.2 found names neither covered: the
pack markers, the audit waivers file and cache ref, the inline secret
waiver, the check JSON envelope, the stamp commit author, a slot holder,
the hook template placeholder, and the `amont` commands
named in printed and generated text. The two packs still carry `amont` in
their repository, binary, config and marker names.

## Goal

After the 3a-2 census, greengate and both packs read and write no `amont`
name outside the signed notes (3a-3) and the known later steps (the
`amont` / `amont-fleet` binary aliases, `refs/amont/...` and the
`amont-snapshots` repository); the packs live at `greengate-pack-homelab`
and `greengate-pack-java`, and homelab runs its pack under the new names.

## Non-goals

- The signed-notes names (3a-3), and `amont-cluster-v1`
  (`gate_identity.rs:102` `CLUSTER_CONTRACT`): it is hashed into every
  cluster gate identity and the in-cluster signer computes the same hash, so
  it moves with 3a-3 Phase 4, which rebuilds the signer image anyway.
- The `amont` / `amont-fleet` binary aliases, `refs/amont/...`, the
  `amont-snapshots` repository (later steps of the programme).
- greenpiste's own names (`amont.agent.*`, `~/.claude/amont-agent/...`: step 3b).
- Renaming the local checkout directories (`~/Developer/Perso/amont-pack-*`):
  they move with the other local checkouts at the programme census.
- Regenerating every repository's AGENTS.md block now: the block rides each
  repository's next change (`guidance.generated-block-rides-the-change`).

## Code review
- amont: backend, lang:rust, unix, tui, architect
- amont-pack-homelab: backend, lang:go, unix (platform left out: one CI build path renamed; tui left out: usage strings renamed, no layout change)
- amont-pack-java: none — markdown and pack comment text only
- homelab: lang:go (platform, backend, unix, tui, architect left out: a re-vendored pack block, a renamed config file, yaml comments and two Go help strings)
- decisions: none — markdown only

## Behaviour

- **Consumer-held names are read both until the census.** A name that lives
  in a consumer's tracked file — the pack markers, `.amont-audit-waivers`,
  `.amont-homelab.yaml` and the `amont-homelab: unreferenced-ok` comment —
  is written only under the new name and still read under the old one, so
  no repository breaks between an install and its rename; the old reads go
  at the 3a-2 Phase 6 census (ADR-0068's one fallback read for committed files).
  The read is one fallback in each place: the new name first, the old one
  only when the new one is absent; when both exist the old one is ignored
  with a one-line warning naming it.
- **Names only greengate holds are renamed outright.** `refs/notes/amont-audit`
  (a 24-hour local cache, never pushed) becomes `refs/notes/greengate-audit`
  with format `greengate-audit-v1`; `greengate migrate` deletes the old ref
  (one extra audit on the next tag push). `amont:allow-secret` becomes
  `greengate:allow-secret` (only greengate's own repository uses it); the
  check envelope becomes `greengate-check-v1` (no reader outside greengate);
  stamp commits are authored `greengate <greengate@localhost>` with message
  `greengate stamps`; the slot holder uses `names::SLOT_HOLDER`; the
  staged-only stash suffixes `.amont-absent` / `.amont-symlink` stay: nothing
  writes them, only `put_back_legacy` reads them (`staged_only.rs:715`), and
  that legacy read goes at the census;
  `__AMONT_BIN__` becomes `__GREENGATE_BIN__` (installed hooks hold the
  resolved path, never the placeholder, so nothing is re-baked).
- **Printed and generated text names `greengate`.** The untrusted-manifest
  warning, `install`'s pointer line and the generated AGENTS.md block
  (heading `## Git hooks (greengate)`, `greengate list --json`, `greengate
  land`, ...) name the `greengate` command; `migrate`/`doctor` help keeps
  naming the old names on purpose.
- **`greengate migrate --tracked` renames the tracked ones:** it adds
  `.amont-audit-waivers` → `.greengate-audit-waivers` to its `FILES` table
  (a `git mv`, content unchanged, so trust holds). It does not respell
  pack markers: only homelab vendors a pack and Phase 3 replaces that block,
  and a respelled marker would change the manifest's blob id and cost a
  `greengate trust` (`migrate/tracked.rs:13-15` keeps content unchanged for
  that reason); `splice` reads the old markers until the census.
- **The packs.** `amont-pack-homelab` is renamed on Forgejo to
  `greengate-pack-homelab` (Forgejo keeps a redirect from the old path): Go
  module `git.daddyshome.fr/fredericrous/greengate-pack-homelab`, binary
  `cmd/greengate-homelab`, config `.greengate-homelab.yaml` (falls back to
  `.amont-homelab.yaml`; `Repo.ConfigPath` records the file loaded, and parse errors and the
  substitution-pins / unreferenced-yaml findings name it), marker `greengate-homelab: unreferenced-ok`
  (`amont-homelab: unreferenced-ok` still accepted), env
  `GREENGATE_HOMELAB_TRACE`, rows `files greengate-homelab <rule>`, README,
  CI and AGENTS.md block. `amont-pack-java` is renamed on GitHub to
  `greengate-pack-java` (GitHub redirects): pack comments and README name
  greengate; tagged `v1.1.0` (the `v1` tag is never moved).
- **homelab** deletes its `amont:pack` block and re-adds the pack with
  `greengate add forgejo:git.daddyshome.fr/fredericrous/greengate-pack-homelab`
  (rows call `greengate-homelab`), `git mv .amont-homelab.yaml
  .greengate-homelab.yaml`, respells its three `unreferenced-ok` comments
  and the text in `ops/newapp` and `docs/operations/onboard-application.md`.
  Before it merges the person installs `greengate-homelab` (until then the
  rows report "could not run", a warning, never a block) and runs
  `greengate trust` in homelab after the merge.

## Phases

- [x] Phase 1 — greengate 4.3.0: `amont/crates/greengate-runtime/src/pack.rs`,
  `amont/crates/greengate-runtime/src/hooks/audit.rs`,
  `amont/crates/greengate-runtime/src/hooks/secrets.rs`,
  `amont/crates/greengate-runtime/src/finding.rs`,
  `amont/crates/greengate-runtime/src/run_stamp.rs`,
  `amont/crates/greengate-runtime/src/host_slots.rs`,
  `amont/crates/greengate-runtime/src/trust.rs`,
  `amont/crates/greengate-runtime/src/install.rs`,
  `amont/crates/greengate-runtime/src/agents_md.rs`,
  `amont/crates/greengate-runtime/src/names.rs`,
  `amont/crates/greengate-runtime/src/migrate.rs`,
  `amont/crates/greengate-runtime/src/migrate/tracked.rs`,
  `amont/crates/greengate-runtime/templates/hooks/pre-commit`,
  `amont/AGENTS.md`, `amont/CLAUDE.md`, `amont/CHANGELOG.md`,
  `amont/docs/custom-checks.md`, `amont/docs/index.md`,
  `amont/docs/index-fidelity-and-run-modes.md`, `amont/docs/coding-flow.md`; released; the
  install is the person's.
- [x] Phase 2 — the packs renamed and released: the Forgejo and GitHub
  repository renames, `amont-pack-homelab/go.mod`,
  `amont-pack-homelab/cmd/amont-homelab/main.go` (moved to
  `cmd/greengate-homelab/main.go`), `amont-pack-homelab/internal/repo/repo.go`,
  `amont-pack-homelab/internal/rules/rules.go`,
  `amont-pack-homelab/internal/rules/unreferencedyaml.go`,
  `amont-pack-homelab/internal/rules/substitutionpins.go`,
  `amont-pack-homelab/internal/rules/{fluxenvsubst,generatedcmns,promreleaselabel,rules_test}.go`,
  `amont-pack-homelab/internal/{index/index.go,index/index_test.go,flux/flux.go,render/render.go,finding/finding.go}`
  (module-path imports; `render.go:152` text; `finding.go:69` envelope `amont-homelab-v1` → `greengate-homelab-v1`),
  `amont-pack-homelab/testdata/crossdir/apps/thing/deliberate.yaml`, `amont-pack-homelab/relais.toml`,
  `amont-pack-homelab/CLAUDE.md`,
  `amont-pack-homelab/greengate.pack`, `amont-pack-homelab/Makefile`,
  `amont-pack-homelab/.gitignore`, `amont-pack-homelab/README.md`,
  `amont-pack-homelab/.forgejo/workflows/ci.yaml`,
  `amont-pack-homelab/AGENTS.md`; `amont-pack-java/greengate.pack`,
  `amont-pack-java/README.md`; each git remote repointed; the homelab pack's release is a tag (no release workflow exists, so the README drops its release-asset curl line); `greengate-homelab`
  installed by the person with `make install` from a clone, then `command -v greengate-homelab` checked in the PATH the hooks use (the old binary is in `~/.local/bin`, `go install` writes `$GOBIN`).
- [x] Phase 3 — homelab runs the renamed pack (precondition: `greengate --version` ≥ 4.3.0, else `greengate add` writes `amont:pack` markers, and `command -v greengate-homelab` answers; verify the block reads `greengate:pack`): `homelab/greengate.conf`,
  `homelab/.amont-homelab.yaml` (moved to `.greengate-homelab.yaml`),
  `homelab/kubernetes/cloud/apps/stalwart/dmarc-dnsendpoint.yaml`,
  `homelab/kubernetes/cloud/apps/stalwart/dnsendpoint.yaml`,
  `homelab/kubernetes/homelab/apps/diag-sandbox/isolation-test-job.yaml`,
  `homelab/ops/newapp/internal/gen/app.go`, `homelab/ops/newapp/main.go`,
  `homelab/docs/operations/onboard-application.md`; then the person's
  `greengate trust`, in the same sitting as the merge (an edited greengate.conf turns every declared check in homelab off until trusted); no other homelab PR merges in between.
- [x] Phase 4 — record: `decisions/docs/programmes/gate-guardrails/map.md`
  (the step, the packs' new names, and `amont-cluster-v1` /
  `CLUSTER_CONTRACT` added to the 3a-3 entry as a Phase 4 item), and the 3a-2 Phase 6 entry
  (`amont/docs/plans/2026-11-11-greengate-renames-data-names.md`) gains this
  plan's old reads (greengate's three: the pack markers, the waivers file and the legacy `.amont-absent` / `.amont-symlink` stash read in `staged_only.rs`; plus the pack's `.amont-homelab.yaml` and
  `amont-homelab: unreferenced-ok` fallbacks, dropped in a pack release) and the six waiver files its census sweep renames with
  `greengate migrate --tracked` (activity-vision, adr-lint, duro-app,
  duro-design-system, duro-lexical-multi, ticket-vision).

## Verification

- Phase 1: `splice` tests: both spellings of one label present, mixed start/end spellings (unpaired error naming both), old block replaced in place; waiver messages quote the file read; `migrate` deletes an absent audit ref without error; `agents_md` still finds old-marker blocks; real-git tests: a manifest with an `amont:pack` block is
  replaced in place by `greengate add` of the same label and comes back with
  `greengate:pack` markers; a repository with only `.amont-audit-waivers`
  still waives, and with both files the new one wins; `migrate --tracked`
  on a scratch repository renames the waivers file and leaves
  `greengate.conf`'s blob id unchanged (`git hash-object` before = after); the legacy-suffix restore test is untouched; `greengate:allow-secret` waives and `amont:allow-secret` no
  longer does; `git grep -n 'amont' -- crates ':!*tests*'` lists only the
  allowed names (aliases, `refs/amont/`, `amont-snapshots`, `names::old`,
  migrate/doctor help, the legacy stash-suffix read in `staged_only.rs`, the signed-notes names and `CLUSTER_CONTRACT`).
  A stamp commit's `git log -1 --format='%an <%ae> %s'` reads
  `greengate <greengate@localhost> greengate stamps`; the check JSON
  envelope's format reads `greengate-check-v1`; `greengate agents-md` in a
  scratch repository writes `## Git hooks (greengate)`, and a 4.2-written
  block makes `pre-commit-agents-md` warn, not fail.
- Phase 2: `go test ./...` in the pack, with new tests: a repo holding only
  `.amont-homelab.yaml` loads it, with both files the new one wins, and a file
  marked `amont-homelab: unreferenced-ok` and one marked `greengate-homelab:
  unreferenced-ok` are both skipped; each of the five rules, run on
  homelab's tree from a /tmp build of the rename commit and a /tmp build of
  the commit before it, gives the same finding tuples (path, line, rule; the renamed `amont-homelab: ` prefix, the `not rendered` text and the envelope are normalised; diffed, before Phase 3 moves
  `.amont-homelab.yaml`); the pack's comment text names `greengate` too (repo.go:118-119, rules.go:5, render.go:105, finding.go:4-52, the greengate.pack header); `git grep -n amont` in the pack lists only the two
  fallback reads, their fixture and `relais.toml` (relais's keys); `git ls-remote` on the old URLs still answers (the redirect);
  `greengate add github:fredericrous/greengate-pack-java@v1.1.0` in a
  scratch repository vendors the rows.
- Phase 3: `greengate list` in homelab shows the five rows calling
  `greengate-homelab`, active, after the person's trust; a scratch commit
  touching `kubernetes/homelab/apps/diag-sandbox/kustomization.yaml` prints
  the five rows by name with no "could not run" line.

## Rollback

- Phase 3: `git revert` the homelab PR (the old block and `.amont-homelab.yaml` come back), then the person's `greengate trust`; no cluster state changes.
- Phase 2: rename the repositories back (nothing was created at the old names); the old binary stays installed; tags stay.
- Phase 1: reinstall 4.2; it rebuilds `refs/notes/amont-audit` on the next tag push (one audit), and a `.greengate-audit-waivers` renamed by migrate needs a `git mv` back.

## Outcome

After the 3a-2 census: `git grep -n amont` over greengate's crates and both packs
finds only the allowed names, and homelab's hooks run the pack as
`greengate-homelab` with no "could not run" line.

## Results (observed)

- Phase 1 (code done at 97d729ee; ticks at the 4.3.0 release, whose install is the person's):
  - `cargo fmt`, `cargo clippy --workspace --all-targets -D warnings` and `cargo test --workspace` pass, the last run by the commit gate on 97d729ee.
  - Each fallback test fails with its fallback removed, checked by reverting it. They are the old-marker block in `splice` (three unit tests plus `pack_add::an_amont_pack_block_is_replaced_in_place_under_new_markers`), the old waivers file (`audit::a_waivers_file_under_the_old_name_still_waives`) and the frozen 2.3.1 shim placeholder (`shim::a_shim_amont_2_3_1_rendered_is_still_ours`).
  - Tests also cover both spellings of one label, mixed and nested markers, the unpaired message, the stamp author, `greengate-check-v1`, `greengate:allow-secret` (and the old pragma's hint), the audit-ref delete (absent and present), and `greengate.conf`'s blob id, unchanged by `migrate --tracked`.
  - Grep check, restated: `git grep -n amont -- crates ':!*tests*'` gives 820 lines. 507 are comments or doc comments (prose, which the census does not count, ADR-0068). The 313 code lines fall in the allowed categories:
    - `migrate.rs` and `names::old`;
    - the frozen 2.3.1 shim and the `amont`/`amont-fleet` aliases (`shim.rs`, `install.rs`, `fix.rs`);
    - the signed-notes names (`gate_stamp.rs`, `attest.rs`, `attest_inputs.rs`, 3a-3);
    - `CLUSTER_CONTRACT` and `refs/amont/`;
    - the legacy stash read (`staged_only.rs`);
    - greenharness-quiet's tool-name strings;
    - greenpiste's `amont-agent` paths (step 3b).
- Phase 2: code and renames done 11-13; the homelab pack's tag is open (audit-go refuses on the machine's go 1.27.3). Recorded in the pack pointer plans (two-build tuple diff empty, fallback tests, repositories renamed with redirects, java `v1.1.0`).

## Implementation review

- implementation-review: approve-with-changes ×3, then the record above. Fixed: the vacuous `.ok()`, discard reasons, the printed `amont.conf` strings, `amont-gates` in the docs, and the `Span` arms named. Deliberate: `list` names `greengate.conf` (every repository is swept).
- backend: approve-with-changes, then approve. Fixed: overlapping blocks refused, the stale audit ref noted in CHANGELOG, and the in-flight amont branches named in the PR.
- lang:rust: approve-with-changes ×2, then approve. Fixed: names constants, the typed `Span`, `cuts`, `Waivers.file`, and a pure `findings`. Deliberate: the proven `unreachable!` arm.
- unix: approve-with-changes, then approve. Fixed: `--json` format named as breaking, and the old-pragma hint.
- CI fix (918428f0): the unwrapped second `notes add` had no git identity on runners (exit 128); it now passes `-c user.*` like the test's other calls; it passes under an empty HOME. implementation-review approve (f1adf751).
- tui: approve-with-changes, then approve. Fixed: the unpaired message, the undo hint, the audit-cache line and the wording. architect: approve (names constants applied).

## Decision log

- 2026-11-13 (Phases 2–4 closed):
  - **Phase 2.** With the machine's go at 1.27.4, `greengate-pack-homelab` was tagged `v1.0.0` at b4d97562 (`amont land --tag`); `govulncheck ./...` under go1.26.11 found no vulnerabilities. The person asked for the install: `make install` wrote `~/.local/bin/greengate-homelab`, built with go1.26.11 and found on a minimal PATH.
  - **Phase 3.** homelab#1213 is merged with a merge commit. Its pointer plan records the checks: five rows active, each `greengate run` exits 0, the commit gate passed 32 checks, and every rule gives 0 findings with both binaries. The person ran `greengate trust` on the branch before the commit, and main's `greengate.conf` blob equals the trusted one (4ca1a4ad).
  - **Phase 4.** It was recorded earlier (decisions#196, greengate#475).
  - **Left for the census.** The old `~/.local/bin/amont-homelab` binary (the person's to remove), and two old-name comments in homelab's `greengate.conf` (lines 2 and 20, left so trust is not revoked).
- 2026-11-13 (release) — greengate v4.3.0 is released:
  - #473 and #474 are merged, and the tag's release run concluded success.
  - crates.io lists 4.3.0, and the GitHub release has 14 assets.
  - homelab#1210 (open at this record) pins the signer image to 4.3.0;
    the musl archive's sha256 matches SHA256SUMS.
  - The install is the person's.
  - Phase 2 is done apart from the homelab pack's tag. greengate's
    audit-go runs govulncheck under the machine's go 1.27.3
    (GO-2026-6859, fixed in 1.27.4), so it refuses the tag. The pack itself
    builds on go 1.26.11 (#12).
  - Phase 3 waits for the person's installs.
- 2026-11-13 (Phase 1) — deviations the reviews accepted:
  - `RUNNER_NAMESPACES` drops `amont-gates`, because homelab removes it (its second change, part B, waits for this release in the signer image);
  - `names::old::SHIM_PLACEHOLDER` keeps the 2.3.1 shims recognised (`greengate-fleet/src/shim.rs`);
  - also touched: the root `templates/hooks/*`, `docs/checks.md`, `docs/configuration.md`, `docs/fleet-dashboard.md` and CI;
  - `list --json`'s untrusted reason names `greengate.conf` and `greengate trust`;
  - the audit-ref delete runs only when migrate runs.

  Found and left for the census: this repository's own `greengate.conf` still holds an `amont:pack` block (adr-lint), read through the fallback; the decisions corpus (ADR-0018, HEADS, the pack) names `.amont-audit-waivers` and `refs/notes/amont-audit`.
- 2026-11-13 — ADR-0068's hard-rename path (greengate has no outside users):
  greengate reads and writes only the new names, migrates this machine once
  (`refs/notes/amont-audit` deleted), and keeps one fallback read for
  committed files (the pack markers, the waivers file, the pack's config and
  marker comment) until a fleet sweep renames them, the 3a-2 census.
- 2026-11-13 — `amont-cluster-v1` moves to 3a-3 Phase 4, not here: it is part
  of every cluster gate identity and of the signer's.

<!-- panel: repos=amont,amont-pack-homelab,amont-pack-java,homelab,decisions adds=contract reviewers=backend,lang:go,lang:rust,platform,architect body-sha=92a7afacfe5a -->
