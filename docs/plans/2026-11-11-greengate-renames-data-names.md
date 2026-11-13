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
- [x] Phase 3 — ledgers, tracked files, markers, fleet migrate:
  `amont/crates/greengate-runtime/src/manifest.rs`,
  `amont/crates/greengate-runtime/src/bypass.rs`,
  `amont/crates/greengate-runtime/src/staged_only.rs`,
  `amont/crates/greengate-runtime/src/agents_md.rs`,
  `amont/crates/greengate-fleet/src/fix.rs`.
- [x] Phase 4 — Kubernetes and the homelab cutover:
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
- [x] Phase 5 — release 4.0.0, the slots install window (relais and greenpiste
  switch to `greenharness-slots` and `GREENHARNESS_HOST_SLOT`:
  `relais/crates/relais/Cargo.toml`,
  `relais/crates/relais/src/verify/heavy.rs`,
  `amont-agent/crates/amont-agent-guard/src/rules/heavy_build_unqueued.rs`),
  migrate this machine, the fleet sweep, docs: `amont/CHANGELOG.md`, `amont/docs/configuration.md`,
  `decisions/docs/programmes/gate-guardrails/map.md`,
  `decisions/docs/programmes/gate-guardrails/t08-alias-policy.md`.

- [ ] Phase 6 — census: remove the fallback read and the consumers' old
  reads. Precondition: `Apicil/archi-mcp` is swept or the person says it
  keeps its old names (without the fallback its hooks stop finding
  `amont.conf`), and the Outcome check passed:
  `amont/crates/greengate-runtime/src/names.rs`,
  `relais/crates/relais/src/repo/detect.rs`,
  `amont-agent/crates/amont-agent-guard/src/guidance.rs`; also what the
  sweep left: `homelab/greengate.conf` (two globs naming `amont.conf`),
  `relais/relais.toml` (`inputs`, with the policy-hash pin in
  `relais/crates/relais/src/policy/mod.rs`), the comments in the
  manifests of trade-agents, application-landscape and relais (each needs
  the person's `greengate trust`), `duro-app/package.json` and
  `homelab/services/forgejo-runner/Dockerfile` (old `amont` version pins),
  `decisions/docs/principles/ci-attested.md` (then `aval pack --write`),
  and the `audit-go` gate's toolchain (it must run govulncheck under
  go.mod's version, not the machine's). Added by plan `2026-11-13-greengates-last-amont-names.md`:
  - the fallback reads of the old pack markers (`pack.rs`), the old
    `.amont-audit-waivers` (`hooks/audit.rs`) and the legacy stash suffixes
    (`staged_only.rs`);
  - the six waiver files that `greengate migrate --tracked` renames, in
    activity-vision, adr-lint, duro-app, duro-design-system,
    duro-lexical-multi and ticket-vision;
  - this repository's own `greengate.conf`, whose `amont:pack` adr-lint
    block is re-added with `greengate add`;
  - `refs/notes/amont-audit` in repositories 4.0–4.2 migrated (removed by
    an explicit `greengate migrate`);
  - greengate-pack-homelab's `.amont-homelab.yaml` and
    `amont-homelab: unreferenced-ok` fallbacks, which go in a pack release;
  - the decisions corpus's mentions (ADR-0018, HEADS, the vendored pack).

  `Apicil/archi-mcp`: the person said on 2026-11-12 that they do not care
  about it, so it keeps its old names and is not swept; the precondition
  holds.

## Decision log

- 2026-11-13 — the Outcome check passed, observed a day after the sweep:
  - Every repository reads its manifest. The sweep had missed
    ticket-vision: it read `amont.conf` through the fallback, was swept
    on 11-13, and ticket-vision#29 is merged.
  - The host has one slot queue: `greengate slots` shows 0 of 3 held, 0
    waiting.
  - No 3.x held store is left: 156 git dirs were scanned for `amont-held`.
  - `greengate doctor` reports no 3.x names.
  - The 29 Phase-3 sweep landings and three releases (relais v0.58.0,
    greenpiste v3.33.0, greengate v4.2.0) ran on 4.1.0 without a changed
    verdict.

  The second homelab change was split by its implementation and platform
  reviews, so the fence is never absent:
  - **Part A, homelab#1208, merged:** it adds `greengate-workflow-fence`
    beside the old one and drops `amont-gates` from Argo. Both fences were
    observed, and 127 of 127 Kustomizations were Ready.
  - **Part B:** removes the app, the signer's Roles there, the Forgejo
    lists and the image pin. It waits until homelab#1210 (open
    at this record) is merged and the signer Deployment runs that image,
    which lists only `greengate-gates`. The one
    Succeeded 3.x Workflow left in `amont-gates` is a finished run, not one
    in flight, and goes with the namespace.

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
- 2026-11-11 (Phase 3) — the tracked rename is never automatic: only
  `greengate migrate --tracked` (and the fleet sweep through it) renames
  `amont.conf`/`amont.pack` and respells the AGENTS.md / CLAUDE.md markers.
  Until then 4.0 reads the old manifest, pack and marker pair, names the
  file the repository actually has in every message, and regenerates or
  checks a block under the pair the file already uses, so an unswept
  repository is not "drifted" for its markers alone. `greengate add` still
  writes the `amont:pack:*` markers inside the manifest until the census.
  greengate's own `amont.conf` and markers stay until the sweep.
- 2026-11-11 (Phase 3) — the fleet sweep (`greengate-fleet fix --apply
  --migrate`) commits one rename per repository through that repository's
  own hooks and pushes nothing; it skips a checkout on its default branch
  and one with uncommitted changes. This amends "lands one commit per
  repository through the normal land": Phase 5 runs it over per-repository
  task worktrees and lands each commit by pull request, never on a live
  checkout's default branch. After review (platform), it commits only on
  a branch named `chore/greengate-names`, so the rename never rides an
  unrelated pull request, and a failure after it started (a refused
  commit included) is undone with `git reset --hard HEAD` on a tree it
  found clean, then reported as failed (exit 1). An untracked `amont.conf`
  is a local file and is left alone.
- 2026-11-11 (Phase 3) — the sweep waits for Phase 4: the homelab gate
  runner image still pins a 3.x binary, which finds no manifest in a swept
  tree (platform).
- 2026-11-11 (Phase 3) — the old spellings moved from `migrate.rs` to
  `names::old`, so runtime modules no longer depend on the migration to
  know them (architect). The census (Phase 6) removes the read-only ones
  and their reads in `manifest.rs` (`manifest_name`), `registry.rs`,
  `gate_stamp.rs`, `pack.rs`, `hooks/manifest_trust.rs`, `agents_md.rs`,
  `migrate/tracked.rs` and the fleet's `migrate.rs`; `staged_only.rs`
  keeps the held store's. `bypass.rs` and `staged_only.rs`, listed for
  Phase 3, needed no change: their old names are 3.x stores read only to
  restore them.
- 2026-11-11 (Phase 3) — the items carried from Phase 2: the `--help` text
  names `greengate.conf`/`greengate.pack` (done); the hint after a refusal
  no longer repeats "run the command again" (done); `doctor` reports a
  reverse that is holding, per repository and global (done); a reverse
  refuses while a run 4.0 submitted is pending (done); the lock-binding
  nit stays optional (kept: the lock is held for the whole move either
  way); the `rm` test's path tail (kept: Windows canonical paths).
- 2026-11-11 (Phase 3) — incident, recurred by hand: at 15:10 another
  session ran a branch-built 4.0 `greengate slot run` from inside an amont
  worktree with only a scratch slot dir. It migrated the live common dir
  and `~/.config` again, and 3.2.0 hooks lost trust. Repaired by hand
  (ledgers merged back, keys moved back as a union, markers removed).
  `greengate-reversed` guards now sit in the live `.git` and in
  `~/.config/greengate`, so any 4.0 first run stands down until Phase 5's
  `greengate migrate` / `migrate --global` on purpose. Found beside it:
  the runtime unit tests appended to the live checkout's `cluster.log`
  (`cluster::log_line` writes to the process's own common dir). It now
  writes nothing under `cfg(test)`.
- 2026-11-11 (Phase 4) — the second homelab change (remove `amont-gates`,
  move the Workflow fence to `apps/greengate-gates/`, drop amont-gates'
  image pin from `build-gate-runner.yaml`) runs after the Outcome check,
  as the Behaviour says: it is the last step of the cutover, not of this
  phase. greengate 4.0 code is merged but unreleased until Phase 5.
- 2026-11-11 (Phase 4) — the detached lane (`rehearse --cluster`, a
  `greengate-rh-*` Workflow) is proven on the first real repository after
  the 4.0 install (Phase 5): it pushes a snapshot to that repository's
  `amont-snapshots` mirror, which a scratch repository has none of. The
  Job lane is proven now (below).
- 2026-11-11 (Phase 4) — kept on the old spelling, with the snapshot
  repository step (a later step, Non-goals): the `refs/amont/cluster/`
  temp refs and the cache's `amont` subPath. Phase 5 adds one step: the
  machine's `greengate.cluster.kubeconfig` is repointed to
  `~/.config/greengate/kube/homelab.yaml` (written today by homelab's
  `greengate-gates-kubeconfig.sh`) in the install window; `greengate
  doctor` names a value still under `~/.config/amont/`.
- 2026-11-12 (Phase 5) — released (greengate 4.0.0, greenpiste 3.31.0,
  relais v0.56.0), installed and migrated on this machine, and the fleet
  swept: every own repository merged through its own pull request (the
  two pilot packs and the rest, except happier below), one commit
  per repository, a text commit where a CI step, script or doc named an old
  name, and `greengate agents-md` where AGENTS.md was behind. Not swept:
  `Apicil/archi-mcp` (the employer's, awaits the person's word). happier's
  rename commit landed on `dev` directly: its worktree branch tracked
  `origin/dev` and `amont land` pushed there; the sweep brief now unsets
  the upstream first. Four Go repositories waited on a red govulncheck
  (GO-2026-6859, fixed in go1.26.11) and took a toolchain bump PR first.
  Their rename PRs were brought up to date with `gh pr update-branch` (a
  merge commit, no force-push). The local `audit-go` rehearsal still runs
  govulncheck under the machine's Go, not go.mod's, so it reports the
  advisory after the bump: CI's result is the proof.
- 2026-11-12 (Phase 5) — left for the census (Phase 6), not renamed
  because a manifest edit needs the person's re-trust or the code still
  reads the name: manifest comments naming `amont.*`, two `amont.conf`
  globs in homelab's `greengate.conf`, `inputs = ["amont.conf"]` in
  relais.toml (policy hash pinned by a test), and the Rust constants.
  The `package.json` and Dockerfile pins of an old `amont` version are a
  dependency choice, not a data name: the agents did not guess at them.
  All of it is listed in Phase 6.

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
  Observed, before the push:
  - A 3.x repository (trusted `amont.conf`, old AGENTS.md markers,
    `.github/attest-inputs` naming the manifest, an `AMONT_` CI setting):
    its check runs unswept → `migrate --tracked` stages `R100 amont.conf
    greengate.conf`, respelled markers and inputs, and lists the CI line
    unedited; the rename commit passes the gate with the check still
    running, so trust was kept; a second run has nothing to do. Both
    manifests tracked → refused naming `git rm`, no staging hint, nothing
    changed. `--tracked --reverse` → exit 2, "stands alone", no hint →
    `migrate_tracked` passes (3 tests).
  - Seven scratch repositories on `chore/greengate-names` unless named:
    manifest, markers only, dirty, on `main`, on `feat/unrelated`, an
    untracked `amont.conf` plus an untracked old-marker `CLAUDE.md`, one
    already renamed. The dry run lists five (not the untracked one, not
    the renamed one); `--apply` commits `chore: rename to greengate
    names` once in the two clean ones, skips the dirty one and the two on
    another branch naming `chore/greengate-names`, leaves the untracked
    files byte for byte; a second dry run lists the three skipped →
    `the_sweep_commits_once_per_repository_and_skips_dirty_ones` passes.
  - A pre-commit hook that refuses → exit 1, `FAILED` with the hook's
    reason and "nothing was kept", log and tracked status unchanged, old
    manifest back → `a_refused_commit_is_undone_and_fails_the_sweep`; a
    read-only `AGENTS.md` after the manifest moved → the same undo, exit 1
    → `a_failure_after_the_rename_started_is_undone`; `--migrate
    --agents-md` → exit 2 naming the flag →
    `migrate_refuses_the_flags_it_would_ignore`.
  - An old-pair block is regenerated under its own pair and checks as
    current; a new block uses `greengate:start` →
    `agents_md::tests` and the `agents_md` integration tests pass.
  - A push gate, a land and a tree gate declared in `greengate.conf`
    → `push_gates` and `tree_gates` pass; every other fixture still uses
    `amont.conf`, the unswept case.
  - A reverse with a live 4.0 detached run → refused naming `cluster
    collect`, nothing moved → `a_live_4_0_run_refuses_a_reverse`;
    `doctor` after a reverse → exit 1 with `reversed: this repository`.
  - Full suites on c128d8b5: greengate 731, greengate-runtime 808,
    greengate-fleet 168 + 27, all pass; after the last review fixes
    (wording, `staged_nothing`, `trust`'s talk on stderr, the halfway
    test without permission bits) the affected ones again: migrate
    integration 9, trust/flows 47, greengate-fleet 168 + 27, runtime
    trust/migrate 34, all pass, and the full suite in each commit's gate; `cargo clippy --workspace
    --all-targets -D warnings` clean; after the runs, the live `.git` and
    `~/.config` hold only the two `greengate-reversed` guards.
- Phase 4: after the homelab PR (applied with no 3.x Job running), a
  cluster gate runs in `greengate-gates` with the new labels and names;
  `auth whoami` with the new kubeconfig names the `greengate-gates` runner;
  `amont-gates` still present after the first PR and removed by the second.
  Observed, before the push:
  - homelab#1167 merged and applied: Kustomization `greengate-gates`
    Ready; ExternalSecret `greengate-bot-read-token` SecretSynced from
    `secret/greengate-gates/bot-read-token` (Vault push: written 1,
    updated 0, unchanged 34); `forgejo-registry-pull` reflected; Argo
    HelmRelease upgraded with `greengate-gates` in workflowNamespaces;
    `amont-gates` untouched (no Jobs).
  - `scripts/greengate-gates-kubeconfig.sh` → `auth whoami` names
    `system:serviceaccount:greengate-gates:greengate-gate-runner`.
  - A `platform=any` gate in a scratch repository (sandboxed
    `XDG_CONFIG_HOME` and global config, this branch's binary) → "lint:
    running on the cluster", Job `greengate-gate-slot-0` in
    `greengate-gates` completed on `gate-runner:main-6a5cf6b@sha256:01e1…`,
    `gate-cache-0` bound, the pass recorded with `name=greengate-gate-slot-0`;
    the live `.git` and `~/.config/greengate` unchanged but the new `kube/`.
  - Tests: cluster unit 39, `cluster_gates` + `detached_cluster` +
    `names` + `migrate` 97 (with `doctor_names_a_3x_kubeconfig`), clippy
    clean, the full suite in each commit's gate.
- Phase 5: 4.0.0 released and installed; this machine's state migrated (a
  commit and a push in a migrated repository pass); the sweep merged in
  every repository; `migrate --reverse` tested once on a scratch
  repository.

  Phase 5 prep, observed before the push (this change: docs, CHANGELOG, the carried trust test; the release and the install window are the next change):
  - `mdbook build docs` succeeds; every in-page anchor link resolves except one broken on main before this change (`checks.md#the-stamp-contract`).
  - A scratch repository, this branch's binary: `trust` prints nothing on stdout and its listing and verdict on stderr; the second `trust` says "already trusted" on stderr; `trust --show` prints the manifest name, listing and state on stdout → `trust_talks_on_stderr_and_trust_show_answers_on_stdout` passes (`trust_display`, `names`: 34).
  - The CHANGELOG and `docs/install.md` give the ordered upgrade (empty slot directory, install the three, first command, `migrate --global`, `migrate --tracked` or the fleet sweep, `doctor` last) and the rollback the code supports (`migrate --reverse` per repository and `--global`, revert the sweep commits, repoint the kubeconfig).

  Phase 5, observed 2026-11-11 and 2026-11-12:
  - 4.0.0 released and installed: input `greengate --version` after the install, expected 4.0.0, actual `greengate 4.0.0`; the release run concluded success on every job (tag, audit, every platform, crates.io, tap, npm; the public registry's `latest` read 4.0.0 a few minutes after a first 404 that was registry lag).
  - This machine migrated: input the first 4.0 command in the live checkout and `greengate migrate --global`, expected the local and global `amont.*` keys moved and the kubeconfig repointed to `~/.config/greengate/kube/homelab.yaml`, actual both done; since then every commit and push of the sweep passed its hooks through `amont land` on 4.0 names.
  - `migrate --reverse` on a scratch repository: tested once, expected the 3.x names back and a `greengate-reversed` marker, actual as expected.
  - The detached lane on a real repository: proven in the install window on a real repository's snapshot, as the Phase 4 log entry asked.
  - The sweep: expected every own repository merged by pull request; actual merged in all but happier (landed on `dev` directly, see the Decision log) and `Apicil/archi-mcp` (excluded, the employer's).
  - Not observed yet, so Phase 5 keeps these as its remainder: the Outcome check (a day after the sweep) and the second homelab change.
  - 2026-11-13: the Outcome check passed (see the Decision log). Second homelab change: part A is merged (homelab#1208). Part B is open and tracked under Phase 4's "removed by the second" item, not Phase 5.

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
- Phase 3, round 1 then two deltas: implementation-review (round 1: tick the phase, file list; delta: verification rows re-recorded, the detached-HEAD read, untracked refusals; fixed) ·
  backend approve (partial failures reported as skips, exit 0 on git errors, a real-hook test, read errors; fixed) · lang:rust approve (`names::old` above the tests, typed branch read; stringly reasons kept) ·
  platform approve (undo a refused sweep, its own branch, untracked files never touched) · unix approve (exit codes, ignored flags refused) · tui (next steps named, the hint only after a move) ·
  architect approve (old names in `names::old`, one `needs`, `staged_nothing` in the runtime; its doc scope to the tracked rename carried to Phase 6).
  Fixed in the last deltas: `trust` without `--show` says everything on stderr (listing, heading, verdict, "nothing to do"), `--show` stays data on stdout, `add`'s "already carries" too;
  `staged_nothing()` matches every variant; the halfway undo test fails at the attest-inputs read after the manifest moved, without permission bits, so it holds as root.
  Carried to Phase 5 (backend, medium): a test pinning `trust`'s stream split (accept path: stdout empty, verdict on stderr; `--show` listing on stdout), with the 4.0 CHANGELOG entry that names the move.
- Phase 4 (amont), round 1: lang:rust, backend, unix, architect approve; implementation-review, tui, platform approve-with-changes —
  the 3.x kubeconfig hint and a `doctor` line for a 3.x kubeconfig (fixed), comments and a pin reason (fixed), the live run recorded and the detached lane deferred (above).
  Kept: local alias constants for the names (low, rust/architect). homelab#1167: implementation-review and platform approve after one delta (image pin, a dedicated Vault reader, the fence move named).
  Delta: all seven approve (tui approve-with-changes). Deliberate: the doctor and hint lines keep their word order (tui, low). Carried to Phase 5:
  `old_kubeconfig` matches `/.config/amont/` only, so this machine's value is checked by hand in the install window (architect, rust; a unit table rides the census), and the hint's folder from `names::STATE_DIR` (architect, low).

- Phase 5 prep (docs, CHANGELOG, trust test): implementation-review, backend, platform, unix, tui, architect approve-with-changes, then approve on the delta (lang:rust approve);
  fixed: state paths 4.0 really writes, the shim placeholder, the Windows install text, an ordered upgrade and the rollback the code supports, the temporary old-name fallback, the names that stay, doctor last, trust's stdout contract.
  Deliberate (tui, low): the CHANGELOG's upgrade list and install.md's number the rename step 4 and 5 (install.md splits install and first command in two); both name it in words too, and both end on the same completion condition.
  Kept: the `amont_cmd()` test-harness name; `amont-snapshots` named as kept (a later step); the CHANGELOG's `## Unreleased` becomes `## 4.0.0` in the release commit.

- Phase 5 (plan text only), round 1: implementation-review approve-with-changes (46 s) · blocking and high fixed (observed rows for the install-window checks; Phase 6 waits on `Apicil/archi-mcp` and the Outcome check) · medium and low fixed (Phase 6 file list, the reason for the version pins, the `audit-go` toolchain follow-up). Kept as a hand check: happier's `dev` is its default branch and the rename commit landed there without a pull request (recorded in the Decision log; its AGENTS.md and CLAUDE.md carry no generated greengate block, so nothing there was behind).

<!-- panel: repos=amont,amont-agent,relais,homelab,decisions adds=contract reviewers=backend,lang:rust,platform,architect body-sha=e2e1e65c8300 -->
