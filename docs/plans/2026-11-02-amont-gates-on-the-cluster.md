---
canonical: decisions:docs/plans/2026-11-02-amont-gates-on-the-cluster.md
phases: [3]
status: active
branch: feat/cluster-attest
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

Phase 3a (ADR-0016, the executor and the cluster note ref, amont#397)
shipped without these. **Phase 3b** (branch `feat/cluster-attest`) adds:
- the `x86_64-linux` attestation block, signed per pushed tip for the
  trusted `platform=any` gates the cluster proved on that tip's tree, in
  the same publish as the laptop block;
- `amont stamps` passing those gate ids to the verifier as `anywhere`
  (`amont attest covered` keeps passing none);
- push-twin reuse: a trusted `platform=any` push gate whose commit twin's
  cluster token is on every tip's tree is gated at commit, and counts as a
  cluster pass under its own id;
- tests: the linux block and its absence from the laptop block, 20 pushes
  of one tree stay under `MAX_BLOCKS`, a linux-only attestation reads
  `attested`, a rehearsal after a cluster pass, and the push-twin reuse;
- the `Scratch` doc comment saying the 0700 mode is unix-only.

**Dropped:** a `cluster` source field in `amont stamps`. The canonical plan
keeps the `stamps --json` schema unchanged.

**Phase 3c** (its own PR, before the release): the hardening carried from
the 3a review: `DYING` set in `on_signal` and a park at the top of
`run_inner`, a drop guard for the second-signal flag, a SIGTERM test of the
park, deleting the Job by name when `create` prints no UID, the wording of
the bundle-too-big and repeated "did not answer" lines, and tests of the
Ctrl-C path (the Job cancel, the temp-file and temp-ref cleanup, a second
signal leaving the Job).

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
  - the wording of the bundle-too-big and repeated "did not answer" lines (tui);
  - the `Scratch` doc comment says "mode 0700" without "on unix" (all four profiles).
- **CI fix `85a0460`** (unused `mut` on Windows, a cfg split): all five re-bound and approve on tree `cc7cfcce`, 39 candidates ruled.

## Phase 3b record

| Check | Expected | Actual |
|---|---|---|
| `cargo test --test it cluster` on `d8216ba` | the cluster suite passes | 20 passed (7 new) |
| `cargo test -p amont-runtime --lib attest` / `--lib cluster` | the unit tests pass | 22 / 10 passed (on `327c818`) |
| `make lint-cross` | clippy clean on darwin, linux, windows | exit 0 (on `327c818`) |
| `make check` | exit 0 | exit 0 (on `327c818`: 684 passed, 1 ignored) |
| a regression each new test catches | red when the change is reverted | per-tip filter, twin reuse and `anywhere` each turned their test red; the rehearsal test covers 3a behaviour; `stamps_reads_a_linux_only_attestation…` discriminates only off linux, so only the macOS leg proves it |
| relais verification | the base verdict | **not used**: relais#383 is still open. Built by relais's implementation worker, verified in-session. |

## Implementation review (phase 3b)

- **implementation-review:** round 1 approve-with-changes (a wrong comment); Delta approve on `d8216ba`, 9 candidates ruled.
- **Profiles:** backend approve-with-changes (twin reuse matched by name only, now by cluster identity; a doubled count); rust approve-with-changes (the claim reads as an action, `pub(crate)`); tui approve-with-changes (each list names its platform; the cluster twin line); unix approve. Every Delta approves.
- **Deliberate, carried into 3c:** one helper for `token()` and the twin's token, not three `git config` spawns (rust, implementation-review); short gate names in the attested line (tui); a struct for the two gate lists (rust); assert the commit twin's Job in the different-command test (implementation-review); `MAX_BLOCKS` copied as a literal (no git-attest dev-dependency).

