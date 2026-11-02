---
canonical: decisions:docs/plans/2026-11-02-amont-gates-on-the-cluster.md
phases: [3]
status: active
branch: feat/cluster-gates
---
# amont runs platform-independent gates on the homelab cluster (pointer)

The plan and its review panel live in decisions. This repository carries
**phase 3**:
- amont ADR-0016, "a gate may run on the cluster", with amendments to
  ADR-0014 and ADR-0015;
- the `platform=any` check-line marker, plus `set executor` and
  `set clusterPrepare`;
- the cluster executor, one Kubernetes Job per gate;
- the cluster token ref, `refs/notes/amont-gate-cluster`, with its own
  identity;
- the `x86_64-linux` attestation block, and verify passing the trusted
  `platform=any` gate ids as `anywhere`.

Read the canonical plan's Design (amont code) section.

**The contract homelab phase 2 delivered** (homelab#1082; see its pointer):
- Namespace `amont-gates`, Jobs `amont-gate-slot-<k>` with k in {0,1}, and
  PVCs `gate-cache-<k>`.
- Every gate pod sets `priorityClassName: gate-batch`. A quota refuses any
  other priority.
- `nodeSelector kubernetes.io/hostname: talos-wk-3-gpu`.
- `automountServiceAccountToken: false` (the namespace's `default` SA).
- The image's ENTRYPOINT is tini. The Job uses `args:`, never `command:`.
- Init container `fetch` (`/usr/local/bin/fetch`):
  - env `EXPECTED_TREE`, `GATE_REF`, `REPO_DIR` (a plain name, equal to
    the cache subPath), `WAIT_SECONDS` (default 30);
  - mounts the PVC root at `/cache`, plus emptyDirs at `/work` and
    `/checkout`;
  - writes `tree <oid>`, or `error <reason>`, to `/dev/termination-log`.
- The bundle arrives through
  `kubectl exec -i -c fetch -- env EXPECTED_BYTES=<n> fetch-receive`.
  `fetch-receive` requires `EXPECTED_BYTES`.
- Init container `prepare` and the main container mount
  `gate-cache-<k>` with subPath `<REPO_DIR>` at `/cache`, and `/checkout`.
- Quota: 8Gi of limits.memory, so 2 pods at the 4Gi default. A quota
  refusal is a 403 ("exceeded quota") and falls back exactly like a 409.
- Machine git config:
  - `amont.cluster.kubeconfig = ~/.config/amont/kube/homelab.yaml`;
  - `amont.cluster.image`, copied by the operator as `tag@sha256:`.
