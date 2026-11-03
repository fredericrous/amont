---
id: ADR-0017
status: accepted
decisions:
  - key: hooks.cluster-detached
    scope: hook-binary
    choice: "`amont rehearse --cluster`, or `git config amont.rehearse.cluster true` for the post-commit rehearsal, sends the trusted `platform=any` gates the rehearsal would run, and only those, to one Argo Workflow in the ADR-0016 namespace: a DAG with one pod per gate (`failFast: false`, `parallelism: 1`), the same pod as ADR-0016's Job with `fetch` in git mode and the gate as Argo's `main` container, created as `amont-gate-runner` with a name derived from the rehearsal's run label and carrying that label on the Workflow and its pods. The code is a snapshot commit of HEAD's tree with HEAD as parent, dated now, pushed with the person's own credentials to `refs/snapshots/<run label>` on `amont.cluster.snapshotRemote` and read back by `ls-remote` (a push not read back submits nothing), or, when `amont.cluster.mirrorRemote` advertises HEAD's commit among its refs, fetched from `amont.cluster.mirror` with nothing pushed. Before `create`, the intent (namespace, name, run label, tree, source, and for each gate its command, image digest, prepare command, identity contract and token) is written atomically and durably to `$GIT_COMMON_DIR/amont/cluster-pending` under a non-blocking flock; the UID is persisted before success is reported; a `create` whose answer is lost is reconciled by `get` on its name, validated against the submission, and never repeated. `cluster::collect()` runs at the start of pre-commit and pre-push and in `amont stamps`, `amont cluster collect` and `rehearse --status`; with nothing pending it spawns no `kubectl`, with a busy lock it returns at once, and otherwise it makes one `kubectl get workflows,pods -l 'amont.run in (…)'` call per hook under a 1 s request timeout. A gate passes only on Kubernetes-recorded facts of a pod owned by the saved Workflow UID and made from the frozen submission: `fetch` terminated 0 reporting `tree <expected>`, `prepare` 0 when submitted, `main` terminated 0, an imageID, no disruption; Workflow and node phases only say a pod finished. The token written is the frozen one, in `refs/notes/amont-gate-cluster`, never recomputed from the configuration of the day; anything else writes one line in `amont/cluster.log` and nothing more, and the gate runs where it always has. The laptop is the only signer: the existing pre-push reuse paths honour a collected token and sign it into the `x86_64-linux` block. All the `amont.cluster.snapshot*`, `mirror*` keys and `amont.rehearse.cluster` unset means no detached lane at all"
    first: true
    reason: A closed laptop lid must not lose a rehearsal that is already running on an idle node, and a pass earned there must still count at the next push without the cluster ever holding a signing key or being believed about more than the facts Kubernetes recorded
---
# 0017 — a detached cluster run

Plan: the homelab repository's "Detached cluster runs: a rehearsal that
outlives the laptop" (2026-11-03),
`docs/plans/2026-11-03-detached-cluster-runs.md` there, with its pointer in
[`docs/plans/2026-11-03-detached-cluster-runs.md`](../docs/plans/2026-11-03-detached-cluster-runs.md).
Fleet records: relais t26 (relais#388) chose the shape; `decisions:ADR-0053`
(`ci.platform-schedule`) lets a gate declared `platform=any` be proven once on
any platform.

## Context

ADR-0016 runs a commit-blocking gate as a Kubernetes Job the laptop streams its
bundle to, polls, and deletes 300 s after it ends. That is right for a gate a
commit waits for, and wrong for work nobody waits on: close the lid and the
pass is lost, because the Job is polled from the laptop and the laptop holds
the only copy of the bundle. A rehearsal is that other kind of work. It exists
to stamp a tree before the push, and it can run while the person is away.

The homelab already runs Argo Workflows. Phase 1 of the plan (homelab#1088 and
its follow-ups) gave `amont-gates` a `gate-detached` priority class with its
own quota of one pod, an executor service account, a fence that admits only
`amont-gate-runner`, Flux and the Argo controller to create Workflows, a
Forgejo org `amont-snapshots` (a repo per opted-in project, plus read-only pull
mirrors), and `fetch` in git mode. A hand-run Workflow passed there, and its
pod and Workflow JSON are the fixtures of the tests.

## Decision

**A second lane beside ADR-0016's, not a replacement.** The Job lane stays for
gates a commit waits on. The Workflow lane takes a rehearsal's gates. The
opt-ins ADR-0016 requires still apply to every gate sent: `platform=any`, in a
trusted manifest that says `set executor cluster`, on a machine with
`amont.cluster.kubeconfig` and a pinned `amont.cluster.image`. The detached
lane adds machine keys, all in git config and none from a manifest:

- `amont.cluster.snapshotRemote`: where the laptop pushes, `{repo}` standing
  for the name of the repository (the last segment of `origin`'s URL, without
  `.git`); `amont.cluster.snapshotFetch`: where the cluster fetches the same
  repository from;
- `amont.cluster.mirrorRemote` and `amont.cluster.mirror`, optional and
  together: the pull mirror as the laptop lists it and as the cluster fetches
  it;
- `amont.rehearse.cluster`: the post-commit rehearsal goes detached on its own.

With none of them set the lane does not exist: a rehearsal is what it was.

**What goes, and what stays.** `rehearse --cluster` sends only the gates
`scoped_push_gates` decides the rehearsal would run, that are trusted
`platform=any` pre-push gates, and that nothing vouches for yet. The rest run in
the rehearsal snapshot exactly as before, and the snapshot's pre-push is told
which gates not to repeat. `AlreadyStamped` accepts a cluster token too.

**The code goes in as a Forgejo snapshot, or is read from the mirror.** The
snapshot is a commit of HEAD's tree whose parent is HEAD, with a committer date
of now (the prune of `refs/snapshots/*` ages a ref by it), pushed to
`refs/snapshots/<run label>` with the person's own credentials: no prompt, a
deadline, and this repository's own pre-push hook standing down. The ref is read
back with `ls-remote`, because Forgejo has lost accepted pushes; a push that is
not read back submits nothing, and the gates run here with one line in
`cluster.log`. When the mirror advertises an object equal to HEAD (compared
among the advertised objects, since `ls-remote <sha>` matches names), nothing is
pushed and `fetch` reads the mirror at that commit. The clone is shallow, so
only gates that read the working tree and need no history go.

**One Workflow, one pod per gate.** A DAG with a task per gate, `failFast:
false` so a failing gate does not stop the others from being scheduled, and
`parallelism: 1` so the pods take the one detached slot in turn. Each pod is
ADR-0016's: `fetch`, `prepare` when `clusterPrepare` is set, and the gate, which
Argo names `main`, as `command` because Argo's emissary needs it. The Workflow
is named from the rehearsal's run label (at most 63 characters, so that it fits
a label value), labelled with it, and its pods with it too. Its deadline covers
every gate (each gets `amont.timeout` plus 60 s), its TTL is 48 hours, and
finished pods are kept until then so their status can be judged.

**The intent is durable before anything is created.** The record in
`$GIT_COMMON_DIR/amont/cluster-pending` is written first, by a temporary file,
fsync, rename and a directory fsync, under a non-blocking flock on a lock file
every writer shares. It holds, for each gate, the frozen identity: command,
image digest, prepare command, identity contract and token. The Workflow is
created once, as `amont-gate-runner`. A `create` whose answer is lost (an EOF, a
reset, a deadline) is followed by `get` on the deterministic name; a Workflow
found there is adopted only if its spec is the one submitted. A refusal by the
apiserver deletes the intent; an unconfirmed `create` keeps it, so a request that
lands late is adopted by the next collect and never repeated. The gates run here
in the meantime.

**Collect, at hook start.** `cluster::collect()` runs at the start of pre-commit
and pre-push, in `amont stamps`, `amont cluster collect` and `rehearse
--status`, once per process. With no pending file it costs a `stat`. It takes the
lock without waiting and returns when it is busy. It asks the cluster once per
cluster: `kubectl get workflows,pods -l 'amont.run in (…)' -o json` under
`--request-timeout=1s` and a wall deadline, so a hook never waits long on a
cluster that is away; a failed read keeps every record. A pod belongs to a gate
by its `workflows.argoproj.io/node-name` annotation, checked against the
Workflow's `status.nodes`, and to the Workflow by its controller ownerReference
to the saved UID.

**A pass is read from Kubernetes-recorded facts only.** The pod's `fetch` init
container terminated 0 reporting `tree <expected>`; `prepare`, when submitted,
terminated 0; the `main` container status terminated with exit code 0; the pod
recorded an imageID and was not disrupted; and the pod's images, tree, commit and
command are the frozen ones. The Workflow's and the nodes' phases say the pod
finished and nothing else, because gate code can write what Argo's executor
reads: a node `Succeeded` with `main` at exit 1 is no pass. This is ADR-0016's
verdict (`judge`), with `main` for `gate` and the Job's terminal condition
replaced by `main`'s own termination.

**The token is the frozen one.** A pass is written to
`refs/notes/amont-gate-cluster` under the token worked out at submission, by the
function the push uses to look it up. A changed command, prepare command or image
between submission and collect means a push that looks the gate up finds no token
for it, and runs it. Anything but a pass writes one line in `cluster.log` and
nothing more: the gate runs at the push, or in CI, as today. A Workflow that has
finished, or that has outlived its TTL, leaves the pending file. Collecting twice
writes one token.

**What a closed lid does.** Nothing: the Workflow is the cluster's from the
moment `create` returns, the snapshot is on Forgejo, and the intent is on disk.
The next commit or push, from any worktree of the repository, collects.

**The laptop is the only signer (t25).** A collected token is honoured by the
reuse paths ADR-0016 already added (`vouched_on_every`,
`claim_twin_cluster_pass`) and signed into the `x86_64-linux` block of the
attestation by `attest_push`, with no change there. The cluster holds no key and
is believed about nothing but the facts above.

**Cancel and visibility.** `rehearse --stop` patches the pending Workflows with
`spec.shutdown: Terminate`, each call bounded. `rehearse --status` shows each
Workflow's phase and the gates collected. `kubectl get wf -n amont-gates` follows
a run.

## Consequences

- **This amends ADR-0016.** There are two lanes: the Job lane for what a commit
  waits on, polled from the laptop and deleted at the end; the Workflow lane for
  what nobody waits on, left to the cluster and collected later. The token
  identity, the note ref, the verdict rules and the opt-ins are shared, so a pass
  from either lane is the same pass.
- Gate pods of this lane reach the apiserver (for Argo's executor) and Forgejo,
  over the network but with no token, and the quota allows one detached pod: a
  second rehearsal queues in Argo, not on the laptop.
- A rehearsal whose gates all went to the cluster ends at once; a failed gate is
  found at the push, which runs it, not in the rehearsal.
- Off switches: unset `amont.cluster.snapshotRemote`, or `amont.rehearse.cluster`
  and do not pass `--cluster`. `cluster-pending` and `refs/snapshots/*` are
  disposable.
- The lane is unix only: it needs `flock`.
