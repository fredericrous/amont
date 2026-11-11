---
status: active
branch: feat/greengate-hard-rename
repos: [amont, amont-agent, relais, homelab, decisions]
adrs: [ADR-0064]
---
# greengate 4.0 renames its data names

## Review panel

👉 **Decide:** none — approve if greengate 4.0 hard-renames its data names (one-time migration of this machine, a fleet sweep for tracked files, consumers and slots switched in one install window, a side-by-side homelab cutover) is the right replacement for the read-both plan.
📍 amont, amont-agent, relais, homelab, decisions · nothing built · next: Phase 1 (greenpiste and relais read both). Panel: backend, lang:rust, platform, architect (opus-5-5).
**Changed by review:** migration walks every worktree under a common-dir flock and never renames a live 3.x held store; the slot names switch once in an install window (the person's choice); homelab adds `greengate-gates` beside `amont-gates`, removing the old one in a second PR held until the Outcome check.
📄 Full reviews: [2026-11-11-greengate-renames-data-names.reviews.md](2026-11-11-greengate-renames-data-names.reviews.md)
**Verdicts:** person's change (hard rename) delta: rework 1 (lang:rust), approve-with-changes 3; the slot-queue conflict decided by the person; re-binds approve-with-changes, then approve 3 (backend, lang:rust, platform); 0 rework left; 9 findings carried into implementation (listed in the full reviews).

## Picture

```
 3.0.x (today)                     greengate 4.0 (hard rename)
 -------------                     ---------------------------
 env      AMONT_X                  GREENGATE_X only
 config   amont.key                greengate.key only; local keys
                                   moved by the first 4.0 run
 state    .git/amont-*, amont/     .git/greengate-*, greengate/; moved
          $TMPDIR/amont-*          by the first 4.0 run (held store too)
 headers  amont-bypass-v1 ...      greengate-…-v1 (ledgers rewritten)
 tracked  amont.conf, markers      greengate.conf, greengate markers;
                                   `greengate migrate` + fleet sweep;
                                   one fallback read until swept
 slots    AMONT_HOST_SLOT,         GREENHARNESS_HOST_SLOT,
          amont-slots dir          greenharness-slots dir; relais and
                                   greenpiste switch in the same window
 k8s      amont.*, amont-gates     greengate.*, greengate-gates (one
                                   homelab cutover)
 notes    refs/notes/amont-*       unchanged: plan 3a-3
```

Each row is a kind of name greengate reads or writes: 3.0 on the left,
4.0 on the right, with how the old one goes away.

## Goal

Build step 3, plan 3a-2 of ADR-0064 for greengate, as a hard rename. The
person decided (2026-11-11) that with no users yet the read-both/write-new
compatibility layer of `harness.renames` is not worth its cost for
greengate's data names: greengate 4.0 reads and writes only the new names.
What would break is the person's own machine and repositories, so it is
migrated once, and the two other tools that read these names (greenpiste,
relais) switch in the same release window. The binary aliases (`amont`,
`amont-fleet` copies) stay: hook shims call them on every commit until the
fleet's shims are re-baked.

## Non-goals

- Signed-notes namespaces (`refs/notes/amont-*`) and their headers: plan
  3a-3 (old notes verify forever, decided in t08).
- The binary aliases and the shim template: kept; the step-4 sweep re-bakes
  shims.
- The Forgejo `amont-snapshots` repository name (its in-cluster URL,
  scripts, prune CronWorkflow and policies name it): a later step.
- Outside users: none; no deprecation period, warnings or twin writes.

## Code review
- amont: backend, lang:rust, platform, unix, tui, architect
- amont-agent: backend, lang:rust
- relais: backend, lang:rust
- homelab: platform
- decisions: none — markdown only

## Behaviour

- **Names.** `greengate-runtime/src/names.rs` holds every data name: the
  `GREENGATE_` env prefix, the `greengate.` config prefix, `greengate.conf`,
  `greengate.pack`, the `.git/greengate-*` ledger and marker names, the
  `greengate/` common-dir folder, `$TMPDIR/greengate-*`, the
  `greengate-<kind>-v1` headers, the AGENTS.md markers, the k8s labels and
  names. The names test fails on an `AMONT_`, `amont.<key>`, `amont-<name>`
  or `amont:start` literal outside `names.rs` and `migrate.rs`.
- **Environment and config.** greengate reads and sets only `GREENGATE_*`
  and only `greengate.*` keys; `setup`, `enroll`, `trust` and
  `usual_name` write `greengate.*`. An `amont.*` key or `AMONT_*` variable is
  ignored, and `greengate doctor` lists any it finds with the command that
  moves it.
- **First-run migration, per repository and per machine.** The first 4.0
  command in a repository moves that repository's local state under a flock (the platform lock greenharness-slots already uses, never a create-new file, which a crash would leave behind) on
  `$GIT_COMMON_DIR/greengate-migrate.lock`, walking every worktree's private git dir from `git worktree list --porcelain`, each step idempotent: `.git/amont-*`
  ledgers and markers to `.git/greengate-*` (headers rewritten to
  `greengate-…-v1`), `amont/` to `greengate/`, the held and preserved
  stores (renamed with rename(2), never restored; a 3.x store records no owner, so an `amont-held` store is never renamed: the command is refused with `greengate restore` (which reads `amont-held` through the alias in `migrate.rs`), and the migration goes on once that store is gone; the install window also requires `pgrep -x amont` empty; 4.0 also parses the `amont-held-v1` header, kept in `migrate.rs`), local `amont.*` config keys
  to `greengate.*` (each key copied with `--get-all`/`--add`, so multi-valued `amont.trusted` keeps every value, then removed). The machine's global
  keys move the same way, once, under a machine-level flock with retry while `.gitconfig.lock` is held (`greengate install` and the first command
  both trigger it). A migration that cannot finish leaves the old names
  untouched and refuses the command with what to fix.
- **Tracked files.** `greengate migrate` renames `amont.conf` to
  `greengate.conf` (`git mv`, content unchanged: 4.0 keeps reading the
  `amont:pack:*` markers inside it until the census; the trust fingerprint is the content's blob
  id, so trust is kept) and rewrites the AGENTS.md / CLAUDE.md markers,
  leaving the change staged for the repository to commit.
  `greengate-fleet fix --apply --migrate` runs it across the fleet, lists `AMONT_` settings in tracked files (CI, `amont.conf`, dotfiles), and
  lands one commit per repository through the normal land. Until every
  repository is swept, 4.0 reads `amont.conf` and the old marker pair when
  the new ones are absent (the only fallback); the census removes it.
- **Slots.** `greenharness-slots` uses `GREENHARNESS_HOST_SLOT`,
  `GREENHARNESS_SLOT_DIR` and the `greenharness-slots` directory; relais
  moves from the `amont-slots` crate to `greenharness-slots` and greenpiste
  reads the new env name. A lock directory cannot be read twice, so the
  slots names switch in one window: the crate, relais, greenpiste and
  greengate 4.0 are released, then installed together with no slot held
  (slot directory empty), so the host keeps one queue.
- **greenpiste and relais.** greenpiste reads `GREENGATE_INDEX_FILE` /
  `GREENGATE_INDEX_GIT_DIR`, `GREENHARNESS_HOST_SLOT`, `greengate.conf` /
  `greengate.pack`, `greengate.severity.*`, the `greengate:start` marker and
  `$GIT_DIR/greengate/land`; relais reads `greengate.conf`,
  `greengate.severity.*`, and `greenharness-slots` from the install window
  (the slot names switch once, never read both). For the other names, both still read the old
  names too (the new name wins; the old one only when the new is absent)
  until greengate 4.0 is installed here, so the order of
  releases cannot break them; their old reads go at the census.
- **Kubernetes.** greengate 4.0 labels, annotates and names everything
  `greengate`; homelab adds the namespace and its objects in one PR next to `amont-gates` (`amont-gates.yaml` stays listed in `apps/kustomization.yaml`; the pull-secret and Forgejo policy lists name both), never a git rename
  (`greengate-gates`, runner and executor ServiceAccounts, workflow fence,
  egress, quota, PVCs under the same names and sizes, the registry pull
  secret's and Forgejo policy's namespace lists, the Argo
  `workflowNamespaces` entry with the Namespace in controllers, the read
  token as a Vault ExternalSecret, never hand-seeded: the Keychain token is first written to Vault, with a Vault role and SecretStore for `greengate-gates`, and `scripts/amont-snapshots-repo.sh` stops creating it), then `scripts/greengate-gates-kubeconfig.sh` is rerun and `greengate.cluster.kubeconfig` repointed (migration drops a `cluster.namespace` value of `amont-gates`). The second PR runs only when no 3.x Job or detached Workflow is left (`kubectl get jobs,workflows -n amont-gates` empty, no pending detached record): it first drops `amont-gates` from `workflowNamespaces`, and deletes the namespace only after the Argo HelmRelease is Ready without it.
- **Release order.** greenharness-slots published; greenpiste and relais
  releases (config and markers read both; relais still on `amont-slots`)
  first; then homelab cutover; then the install window: relais's
  `greenharness-slots` release, greenpiste and greengate 4.0.0 installed
  together once `ls` shows the slot directory empty, and the first-run
  migration of this machine; then the fleet sweep; then the census removes
  the fallback and the consumers' old reads. Rollback: greengate 3.0 reads
  only old names, so a downgrade after migration needs `greengate migrate
  --reverse` (same steps backwards); it is tested once. After the fleet
  sweep, rollback also reverts each repository's sweep commit. The second homelab PR waits until the Outcome check passes: 3.0 reads only `amont-gates`, so a rollback before then points `amont.cluster.kubeconfig` back at the old kubeconfig, and a rollback after it first reverts that PR.

## Phases

- [x] Phase 1 — consumers read both (config and markers; the slot names wait for the install window): `amont-agent/src/suppressions.rs`,
  `amont-agent/crates/amont-agent-guard/src/guidance.rs`,
  `amont-agent/crates/amont-agent-guard/src/rules/heavy_build_unqueued.rs`,
  `amont-agent/crates/amont-agent-guard/src/rules/suppressions_override.rs`,
  `amont-agent/crates/amont-agent-guard/src/land.rs`,
  `relais/crates/relais/src/repo/detect.rs`,
  `relais/crates/relais/src/verify/heavy.rs` (config and markers only; the
  slots switch and `relais/crates/relais/Cargo.toml` move to the 4.0
  install window); released.
- [x] Phase 2 — greengate names, env, config and first-run migration:
  `amont/crates/greengate-runtime/src/names.rs`,
  `amont/crates/greengate-runtime/src/migrate.rs` (new),
  `amont/crates/greengate-runtime/src/config.rs`,
  `amont/crates/greengate-runtime/src/registry.rs`,
  `amont/crates/greengate-runtime/src/trust.rs`,
  `amont/crates/greengate-runtime/src/setup.rs`,
  `amont/crates/greengate-runtime/src/rehearsal.rs`,
  `amont/crates/greengate-runtime/src/hooks/common.rs`,
  `amont/crates/greengate-runtime/src/hooks/run_tests.rs`,
  `amont/crates/greengate-runtime/src/install.rs`,
  `amont/crates/greenharness-slots/src/lib.rs` (published to crates.io before relais's install-window build),
  `amont/crates/greengate/tests/it/names.rs`.
- [ ] Phase 3 — ledgers, tracked files, markers, fleet migrate:
  `amont/crates/greengate-runtime/src/manifest.rs`,
  `amont/crates/greengate-runtime/src/bypass.rs`,
  `amont/crates/greengate-runtime/src/staged_only.rs`,
  `amont/crates/greengate-runtime/src/agents_md.rs`,
  `amont/crates/greengate-fleet/src/fix.rs`.
- [ ] Phase 4 — Kubernetes and the homelab cutover:
  `amont/crates/greengate-runtime/src/cluster.rs`,
  `amont/crates/greengate-runtime/src/cluster/detached/workflow.rs`,
  `homelab/kubernetes/homelab/apps/greengate-gates/kustomization.yaml`,
  `homelab/kubernetes/homelab/apps/greengate-gates.yaml` (the Flux Kustomization, dependsOn `packages` and the SecretStore's Kustomization),
  `homelab/kubernetes/homelab/apps/kustomization.yaml` (lists it next to `amont-gates.yaml`),
  `homelab/kubernetes/homelab/controllers/kustomization.yaml`,
  `homelab/kubernetes/homelab/controllers/argo-workflows-helm-release.yaml`,
  `homelab/kubernetes/homelab/apps/forgejo/registry-pull-secret.yaml`,
  `homelab/kubernetes/homelab/apps/forgejo/authorization-policy.yaml`,
  `homelab/scripts/amont-snapshots-repo.sh`,
  `homelab/scripts/amont-gates-kubeconfig.sh` renamed to `homelab/scripts/greengate-gates-kubeconfig.sh` (`git mv`; `GREENGATE_KUBE_DIR`, `~/.config/greengate/kube`, Keychain service `greengate-gates-homelab`, namespace and runner `greengate-gates`).
- [ ] Phase 5 — release 4.0.0, the slots install window (relais and greenpiste
  switch to `greenharness-slots` and `GREENHARNESS_HOST_SLOT`:
  `relais/crates/relais/Cargo.toml`,
  `relais/crates/relais/src/verify/heavy.rs`,
  `amont-agent/crates/amont-agent-guard/src/rules/heavy_build_unqueued.rs`),
  migrate this machine, the fleet sweep, docs: `amont/CHANGELOG.md`, `amont/docs/configuration.md`,
  `decisions/docs/programmes/gate-guardrails/map.md`,
  `decisions/docs/programmes/gate-guardrails/t08-alias-policy.md`.

- [ ] Phase 6 — census: remove the fallback read and the consumers' old
  reads: `amont/crates/greengate-runtime/src/names.rs`,
  `relais/crates/relais/src/repo/detect.rs`,
  `amont-agent/crates/amont-agent-guard/src/guidance.rs`.

## Decision log

- 2026-11-11 — the person chose a hard rename of greengate's data names
  with a one-time migration, over the reviewed read-both plan: greengate
  has no users yet, so the compatibility layer protects only the person's
  own machine, which is migrated instead. Amends the alias policy (t08)
  for data names; the binary aliases stay.
- 2026-11-11 — tracked files (`amont.conf`, AGENTS.md markers) keep one
  fallback read until the fleet sweep has committed the rename in every
  repository; local state is migrated by the first 4.0 run.
- 2026-11-11 — greenpiste and relais read both names before greengate 4.0
  ships, so no release order breaks them.
- 2026-11-11 — the person chose to switch the slot names and directory in
  one install window (greenharness-slots published, then relais, greenpiste
  and greengate 4.0 installed together with no slot held), over keeping the
  `amont-slots` directory until the census: a lock directory cannot be read
  under two names, and the host keeps one queue.
- 2026-11-11 (Phase 2) — the Kubernetes label and annotation keys
  (`greengate.run`, `greengate.dev/gate`, `greengate.dev/tree`) moved with
  the config keys, ahead of Phase 4: homelab selects on none of them. The
  namespace, ServiceAccounts, secret, `managed-by: amont` and the Job and
  Workflow name prefixes stay `amont` until the Phase 4 cutover. A 3.x
  detached run left pending refuses the migration (4.0 cannot collect a
  Workflow labelled `amont.run`) unless it is past its Workflow's TTL.
- 2026-11-11 (Phase 2) — greenharness-slots shares the workspace version,
  so its directory and variable rename ships as 4.0.0, a major; relais
  pins it in the Phase 5 install window. Changing that later means a new
  major of the crate and a relais release.
- 2026-11-11 (Phase 2) — a reverse (`greengate migrate --reverse`, with
  `--global` for the machine's keys) leaves a marker that every automatic
  first run respects; only `greengate migrate` asked by name moves forward
  again. A dry run, `restore`, `uninstall`, `--help`, `--version` and a
  usage error never migrate.
- 2026-11-11 (Phase 2) — incident: a test that started the 4.0 binary
  with no working directory migrated the live amont repository's state
  and keys (shared common dir) and wrote markers under the real
  `~/.config` (02:17, 03:25, 05:09). Repaired by hand each time; the test
  build now forces `XDG_CONFIG_HOME` and `GIT_CONFIG_GLOBAL` into
  `target/test-sandbox` (`.cargo/config.toml`), every hand-built start
  goes through `sandbox_env`, and a guard test enforces it.
- 2026-11-11 (Phase 2) — carried into Phase 3: the `--help` text still names
  `amont.conf`/`amont.pack` (renamed with the tracked files); the refusal
  and the hint both say "run the command again" (tui); `doctor` could say
  a reverse is holding (backend); the reverse's lock binding (lang:rust,
  optional); a `--reverse` while a 4.0 detached run is pending should refuse
  like the forward run does (platform).
- 2026-11-11 (Phase 2) — carried into Phase 5's docs: `docs/checks.md` still
  tells operators `git config amont.rootRunsMembers` (3.2.0, rebased in);
  the code reads `greengate.rootRunsMembers`. It changes with the 4.0
  release, since until then the published docs describe 3.2.0, which reads
  the `amont.` key (backend, unix, architect).

## Verification

- Phase 1: greenpiste and relais tests read each new name and its old
  one; released; installed here.
- Phase 2: in a scratch repository with 3.0 state (ledgers, held store,
  local `amont.*` keys), the first 4.0 command migrates it: names moved,
  headers rewritten, work restored, a second run is a no-op; a migration
  interrupted halfway resumes; `greengate doctor` lists a leftover
  `AMONT_*` variable; the names test fails on a planted old literal.
  Observed, before the push:
  - 3.x repository with a ledger, a key and a held store → `list` refused
    naming `restore`, nothing moved; `restore` takes the 3.x store; the next
    `list` moves the ledger (header rewritten) and the key, writes the
    marker → `a_3x_repository_is_moved_once_and_a_reverse_holds` passes.
  - Second run no-op, interrupted move resumes, both names refused, linked
    worktrees walked, interleaved and partly copied multi-valued keys kept,
    a git failure keeps the old key, a half reverse recovered by `migrate`,
    a live 3.x pending run refused and an expired one let through → the
    `migrate::tests` unit tests pass (15).
  - `doctor` with `AMONT_TIMEOUT` set → exit 1 naming `GREENGATE_TIMEOUT`
    → `doctor_names_a_3x_variable_left_set` passes; a global reverse holds
    until `migrate --global` → `a_global_reverse_holds_until_migrate_global`
    passes; `list --help` / `list --bogus` / `land --dry-run` move nothing →
    `asking_for_help_moves_nothing`, `a_dry_run_moves_nothing` pass; a
    refusal's `rm <path>` line pastes → `a_refusal_keeps_its_lines_and_the_rm_line_pastes` passes.
  - A planted `AMONT_X`, `amont.x`, `amont-x`, `amont-{x}` or `amont:start`
    literal → counted → `each_planted_data_name_is_counted` passes; the
    tree holds old data names only where `ALLOWED_DATA` pins them.
  - Full workspace suite on the tree rebased onto 3.1.0 → passes (1179 s);
    `cargo clippy --workspace --all-targets -D warnings` clean; after every
    run: no `~/.config/greengate`, no marker and no `greengate.*` key in the
    live repository.
  - First-run cost: the done-marker `stat` on later runs; on the first run
    the config files are read before any git starts, so the spawn budget
    test (≤ 27 git spawns per small commit) still passes.
- Phase 3: `greengate migrate` on a repository with `amont.conf` and old
  markers stages `greengate.conf` and new markers, trust kept; 4.0 reads
  an unswept repository's `amont.conf`; `greengate-fleet fix --apply
  --migrate` on two scratch repositories commits one change each.
- Phase 4: after the homelab PR (applied with no 3.x Job running), a
  cluster gate runs in `greengate-gates` with the new labels and names;
  `auth whoami` with the new kubeconfig names the `greengate-gates` runner;
  `amont-gates` still present after the first PR and removed by the second.
- Phase 5: 4.0.0 released and installed; this machine's state migrated (a
  commit and a push in a migrated repository pass); the sweep merged in
  every repository; `migrate --reverse` tested once on a scratch
  repository.

## Outcome

A day after the sweep: every repository's hook runs on greengate names
(0 "not found", 0 changed verdicts), greenpiste and relais work, the host
has one slot queue, and no held store was lost.


## Implementation review

- Phase 2, three rounds then two deltas: implementation-review approve (round 1: test spawns reached the live config, swallowed errors, value loss, weak names; all fixed) ·
  backend approve (resumed-copy value loss, a half reverse, the reverse race, the pending TTL; fixed) · lang:rust approve (interleaved keys, typed errors, bounded lock via greenharness-slots, injected clock; one optional nit kept) ·
  unix approve (`--help` and dry runs migrated, read-only HOME; fixed) · tui approve-with-changes (messages, 80 columns, sanitized line breaks; one wording item carried to Phase 3) ·
  platform approve (homelab selects on no old label) · architect approve (names/migrate seam; pending file read through its owner).
  Kept as deliberate: `sandbox_env(&mut Command)` mirrors `Command`'s builder; `slots`/`doctor` data on stdout.

<!-- panel: repos=amont,amont-agent,relais,homelab,decisions adds=contract reviewers=backend,lang:rust,platform,architect body-sha=e2e1e65c8300 -->
