---
canonical: homelab:docs/plans/2026-11-03-detached-cluster-runs.md
phases: [2]
status: active
branch: feat/detached-rehearse
---
# Detached cluster runs: a rehearsal that outlives the laptop (pointer)

The plan was approved on 2026-11-03 (body-sha `609d879126a3`). It lives in
homelab, together with its review panel and the phase 1 record.

**Phase 1** (homelab) is done:
- homelab#1088: the Argo lane, the fence, the CNP split, the quotas and the
  prune;
- homelab#1089: `fetch` treats the checkout as safe;
- homelab#1090: the executor runs as uid 8737, fixing a trade-agents
  regression.

**This repository carries phase 2.** Read the canonical plan's Design, amont
changes and Verification sections, and its carried items 1, 3, 4, 5 and 8,
before building. In short:
- amont ADR-0017, "a detached cluster run";
- `amont rehearse --cluster` submits the rehearsal's trusted `platform=any`
  gates:
  - one Argo Workflow, one pod per gate;
  - a Forgejo snapshot with the real parent, or the mirror when HEAD's OID
    is advertised;
  - then it returns;
- `cluster::collect()` runs at the start of pre-commit and pre-push, in
  `stamps`, in `amont cluster collect` and in `rehearse --status`, and turns
  passes into cluster tokens, with the submission identity frozen.

## What phase 1 established live (2026-11-03)

These facts constrain the phase 2 design:
- **Workflow shape.** It is accepted and admitted under PSA restricted.
  The hand-run Workflow, its pod, and their recorded JSON are the collect
  test fixtures:
  - the pod is init, fetch, main and wait, with memory limits 256Mi, 512Mi,
    3840Mi and 256Mi;
  - the namespace-wide quota is 4 pods / 12Gi, and `gate-detached` allows
    1 pod / 4Gi.
- **Pod spec.** The pod sets:
  - `runAsUser`/`runAsGroup`/`fsGroup` 1000 and seccomp RuntimeDefault;
  - `automountServiceAccountToken: false`, `executor.serviceAccountName:
    amont-gate-executor` and `podPriorityClassName: gate-detached`;
  - the wk-3 nodeSelector and `imagePullSecrets: forgejo-registry-pull`;
  - `podMetadata` label `istio.io/dataplane-mode: ambient`, which Forgejo
    requires.
- **`fetch`, git mode.**
  - env: `GATE_SOURCE=git` and
    `GATE_URL=http://forgejo-http.forgejo.svc.cluster.local:3000/<org>/<repo>.git`;
  - `GATE_REF` is an OID or a ref, with `EXPECTED_TREE` and `REPO_DIR`;
  - the token is mounted from Secret `amont-bot-read-token` at
    `/run/secrets/forgejo`;
  - the cache mount is `gate-cache-detached`: the root at `/cache` for
    fetch, subPath `<repo>` at `/cache` for main;
  - `/checkout` and `/work` are emptyDirs.
- **Mirror fetch.** A fetch by OID from the mirror `amont-snapshots/relais-mirror`
  works for a commit that a ref points to (`main`).
- **The main container** is named `main`. Its verdict is the
  `containerStatuses` exit code. The node maps to the pod through the
  `workflows.argoproj.io/node-name` annotation.
- **Network.** From an ambient Workflow pod, a bare TCP connect "succeeds"
  through ztunnel even to denied destinations. Probe with a real exchange.
- **Fence.** Only `amont-gate-runner`, Flux and the Argo controller may
  create Workflows in amont-gates. amont's kubeconfig is
  `~/.config/amont/kube/homelab.yaml` (`amont-gate-runner`).
- **Image** (the current pin): `git.daddyshome.fr/fredericrous/gate-runner:main-a5d5db2@sha256:fa7a661059493955d9590559a3ff62bc956fedd83f41eea739e73d1996797f7c`.
- **Still to check:** the prune's first scheduled run, 2026-11-04 03:17, on
  the merged image.
