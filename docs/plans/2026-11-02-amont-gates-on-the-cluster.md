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

## Deferred to 3b

Phase 3a (ADR-0016, the executor and the cluster note ref) shipped without:
- the `x86_64-linux` attestation block for the `platform=any` gates a cluster
  token satisfies, and verify passing their ids as `anywhere`;
- a `cluster` source field in `amont stamps` (a cluster token reads
  `stamped`, as the plan says, with no field naming where it came from);
- tests of the Ctrl-C path (the Job cancel, the temp-file and temp-ref
  cleanup, a second signal leaving the Job), of a rehearsal after a cluster
  pass, and of push-twin reuse of a commit-time cluster pass. A push twin of
  a cluster-proved commit gate still runs at push time: pairing reads only
  local stamps.

Added in 3a beyond the plan: the machine key `amont.cluster.startWait`
(default 30 s), the bounded wait for `fetch` to be Running, which the tests
shorten.

## Phase 3a record

| Check | Expected | Actual |
|---|---|---|
| `amont slot run -- make check` on `bcd885b` | exit 0 | exit 0 (lint, test, attest-inputs) |
| `cargo test --test it cluster` | the cluster suite passes | 13 passed |
| `cargo test -p amont-runtime --lib cluster` | the unit tests pass | 10 passed |
| `aval check` | no findings | 70 decision records, no findings |
| relais verification | the base verdict | **not used**: relais#383, a 0-CPU stall at the `flows::` journeys only under relais. Built with relais's implementation worker and verified in-session. |

## Implementation review (phase 3a)

- **implementation-review:**
  - round 1 approve-with-changes, 4 blocking (discarded results, flag arguments, names, banners) plus mediums;
  - a Delta (3 small blockers);
  - approve on `bcd885b`, with all 39 candidates ruled.
- **Code-review profiles,** each bound with a fresh `--profile` brief on `bcd885b`:
  - backend approve-with-changes: round 1 found the concurrent-manifest blocker, now fixed;
  - rust approve; unix approve (its Ctrl-C high is fixed); tui approve (its warn-on-every-hook high is fixed).
- **Deliberate, carried into 3b:**
  - set `DYING` in `on_signal` itself, and park at the top of `run_inner`, to close the narrow race where Ctrl-C kills kubectl and reaches the fallback before the watcher (backend, unix, tui);
  - a drop guard for the second-signal flag (rust);
  - a SIGTERM test of the park (backend);
  - delete the Job by name when `create` prints no UID (rust);
  - the wording of the bundle-too-big and repeated "did not answer" lines (tui).
