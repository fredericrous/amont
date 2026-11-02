---
id: ADR-0016
status: accepted
decisions:
  - key: hooks.cluster-executor
    scope: hook-binary
    choice: "A declared gate marked `platform=any` in a trusted `amont.conf` that says `set executor cluster`, on a machine whose git config names `amont.cluster.kubeconfig` and an `amont.cluster.image` pinned by `@sha256:`, runs as one Kubernetes Job (`amont-gate-slot-<k>`, k below `amont.cluster.slots`, at most 2) through `kubectl --kubeconfig`, on the judged tree sent as a git bundle of a parentless commit, and takes no host slot. The cluster proves only passes: a pass needs the Job `Complete` with no `DisruptionTarget`, `prepare` and the gate exited 0, `fetch` reporting `tree <expected>` and a recorded imageID; anything else, the cluster's own trouble included, re-runs the gate locally, unchanged, and the local outcome is the verdict, with one line in `amont/cluster.log`. A cluster pass is recorded as `<id>@<cluster identity>` in `refs/notes/amont-gate-cluster`, never in `refs/notes/amont-gate`; the identity hashes the command and flags, `clusterPrepare`, the image digest and the contract version `amont-cluster-v1`, and never the laptop's executable; only a trusted `platform=any` gate reads it, and it never becomes a local token or a laptop attestation. `platform=any` with `fix` or `files` is a broken line"
    first: true
    reason: The laptop runs at a load average of 150–200 on 12 CPUs and pushes queue behind its own heavy gates, while the homelab has an idle node; a lint, type or cycle check whose verdict does not depend on the platform can run there, as long as a pass from there is never mistaken for one made here and the cluster can never fail a gate
---
# 0016 — a gate may run on the cluster

Plan: the decisions repository's plan "amont runs platform-independent gates
on the homelab cluster" (2026-11-02),
<https://github.com/fredericrous/decisions/blob/main/docs/plans/2026-11-02-amont-gates-on-the-cluster.md>,
with its pointer in
[`docs/plans/2026-11-02-amont-gates-on-the-cluster.md`](../docs/plans/2026-11-02-amont-gates-on-the-cluster.md).
Fleet records: `decisions:ADR-0053` (`ci.platform-schedule`: a gate declared
`platform=any` is proven once on any platform; `harness.tool-boundaries`:
amont decides where a gate runs, the homelab cluster included).

## Context

The laptop's heavy gates queue on its host slots (ADR-0014), and the load
reaches 150–200 on 12 CPUs. The homelab has a node with free capacity. Many
gates do not depend on the platform: a type check, a lint, a module-cycle
script give the same verdict on Linux as on macOS. The relais programme
"homelab-offload" planned to move them with a Lease and a separate
`amont-cluster` crate (ticket t18).

## Decision

**The opt-ins.** Three, all required:

- the check line carries the `platform=any` marker, beside `heavy`;
- the repository's `amont.conf` says `set executor cluster`, and
  `set clusterPrepare <cmd>` when the checkout needs a step before the gate;
- the machine's git config names `amont.cluster.kubeconfig` and
  `amont.cluster.image` pinned by `@sha256:` (optional:
  `amont.cluster.namespace`, `amont.cluster.slots`,
  `amont.cluster.maxBundleMiB`, `amont.cluster.startWait`).

The marker and both settings are policy (ADR-0004): from an untrusted manifest
they do nothing. A local `git config amont.executor local` turns the cluster
off for one clone; nothing local turns it on. `platform=any` with `fix` or
with `files` is a broken line: the pod rewrites nothing, and it is not handed
the matched file set.

**The executor.** `External::run` asks the `cluster` module first. It:

1. writes the judged tree (the index at pre-commit, the pushed tip's tree at
   pre-push) as a commit with no parent, bundles it from a temporary ref
   `refs/amont/cluster/<k>`, and deletes the ref;
2. creates Job `amont-gate-slot-<k>` for the first free `k`. A `409
   AlreadyExists` or a `403 … exceeded quota` means the next slot. The Job
   sets `backoffLimit: 0`, `restartPolicy: Never`, `activeDeadlineSeconds`
   of `amont.timeout` plus 60 s, `ttlSecondsAfterFinished: 300`,
   `priorityClassName: gate-batch`, the node selector, no service account
   token, and `args:`, never `command:`;
3. waits, bounded, for the `fetch` init container to be Running, then streams
   the bundle with `kubectl exec -i -c fetch -- env EXPECTED_BYTES=<n>
   fetch-receive`;
4. streams the gate's logs under the ceiling clock, then reads the verdict
   from the Job and its pod, the pod found by `controller-uid` only;
5. deletes the Job with `--cascade=foreground --wait`, on every path.

Every `kubectl` call has stdin null, except the `exec -i` of the bundle. A
SIGINT or SIGTERM asks the apiserver to delete the live Job, without waiting.
The cluster run takes no host slot: the local re-run takes its slot and
honours `onBusy` as before.

**The cluster proves only passes.** A pass needs, as Kubernetes recorded them:
the Job `Complete` with no `DisruptionTarget` pod condition, `prepare` (when
set) and the gate exited 0, `fetch`'s termination message exactly
`tree <expected>`, and the pod's `imageID`.

**The fallback order.** Anything else runs the gate here, unchanged, and the
local outcome is the verdict. The cases, in the order they are met:

1. the configuration: no kubeconfig, an image without `@sha256:`,
   `amont.timeout 0`;
2. the apiserver failed earlier in this hook process (one failed call is
   cached);
3. no free slot: every slot taken, or refused by the quota;
4. the bundle over `amont.cluster.maxBundleMiB`;
5. the pod not Running within the start wait (30 s);
6. `fetch` or `prepare` failed;
7. the verdict: a non-zero exit, `OOMKilled`, `DisruptionTarget`,
   `DeadlineExceeded`, a tree mismatch, no imageID;
8. `kubectl` missing or failing anywhere.

Each fallback appends `<epoch> <gate> <reason>` to `amont/cluster.log` in the
common git directory. A gate never fails because of the cluster.

**The cluster note ref and its identity.** A cluster pass writes
`<id>@<identity>` into `refs/notes/amont-gate-cluster`, keyed by the judged
tree, with a `provenance` line (`platform=x86_64-linux`, the observed
imageID, the Job UID) that every token reader drops. The identity is its own:
the command and flags, `clusterPrepare`, the image digest and the contract
version `amont-cluster-v1`. It never inspects the laptop's executable, so a
tool present only in the image is reused, and a changed image runs the gate
again. The pre-commit reuse, the pre-push stamp lookup and `amont stamps`
accept a cluster token only for a trusted `platform=any` gate. A cluster pass,
run or reused, never enters the commit marker, the tree, commit or
input-fingerprint tokens of `refs/notes/amont-gate`, or the laptop's
attestation.

## Consequences

- **This amends ADR-0015.** "A remote executor must never stamp" stays true
  of `refs/notes/amont-gate`: the cluster writes only its own ref, under its
  own identity, so the pre-push hook never signs a cluster pass under the
  laptop's platform. Signing an `x86_64-linux` attestation for it, and
  verify accepting it as `anywhere`, are the plan's next slice.
- **This amends ADR-0014.** A cluster run takes no host slot: the cluster has
  its own two slots and its own quota. The local re-run queues as before.
- **This supersedes the relais programme ticket t18's Lease design for this
  slice:** no Lease and no `amont-cluster` crate. The Job name is the slot,
  and the apiserver's `409` is the lock. Worker pods (t02) revisit it.
- An amont that does not know the marker reads `platform=any` as the program
  name, so the gate fails rather than being skipped. A repository adds the
  marker only once everyone reading its manifest has upgraded.
- Off switches: unset `amont.cluster.kubeconfig`, or `set executor local`.
  `refs/notes/amont-gate-cluster` is local and is removed with the other
  stamp refs on uninstall.
