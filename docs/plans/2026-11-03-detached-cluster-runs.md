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

## Phase 2 record (amont)

Written 2026-11-03 after the review fixes. "Actual" is what ran in the
worktree; the cluster rows are the coordinator's server-side dry run.

| Check | Expected | Actual |
| --- | --- | --- |
| `make check` | exit 0 | exit 0 on the third full run; the `it` suite 617 passed, 0 failed. Run 1 (after the first review fixes): 616 passed, 1 failed, `cluster_gates::a_push_after_a_rehearsal_does_not_repeat_a_cluster_gate`. Run 2 (these fixes): 615 passed, 2 failed, `linters::ruff_fixes_what_it_can_and_still_blocks_on_the_rest` and `tool_tables::opted_in_a_clean_input_passes`, both `uvx ruff` unable to reach pypi.org ("Failed to fetch https://pypi.org/simple/ruff/", the same DNS outage that stopped the session). Run 3: green |
| The run-1 failure, evidence it is not this diff | same failure on `origin/main` under the same load, or no difference | not reproduced on either tree. On a throwaway worktree of `origin/main` (23ddd42d) and on this branch, each under `amont slot run`: the test alone 5 times each, 5 of 5 passed (10.7 to 14.5 s against 11.0 to 15.1 s); the whole `cluster_gates::` module (27 tests in parallel) 3 times each, 3 of 3 passed (263 to 292 s against 141 to 228 s). The test fails when the Job lane's `startWait 1` (1 s for the fake pod to reach Running) is missed under load, which makes the rehearsal run the gate here and the push's `local_runs() == 0` fail; the rehearsal's path through this diff adds a few `git` spawns (`known_for`, the stamp lookup) before the snapshot is made and nothing inside the Job lane's wait. It passed in four other full-suite or `cluster`-filter runs of this branch. This is a load-sensitive test, shown by absence on both trees, not by a reproduction |
| `cargo test --test it detached` | all pass | 39 passed (34 in `detached_cluster`, 5 older tests with "detached" in their names), in the last round |
| `cargo test --test it cluster` | all pass | 61 passed (the 27 `cluster_gates` and the 34 detached tests among them) |
| `cargo test -p amont-runtime --lib cluster` | all pass | 38 passed: pending file (4), Workflow JSON (6), collect (13), submit (4), plus the Job lane's |
| `git::probe_tests` (2 tests) | pass | pass: a 3000-ref `ls-remote` is drained in under 10 s; a child that exits while a grandchild holds stdout is `Probe::Failed` with no output, never `Exit(0)` and empty (`land` would read that as "no such ref") |
| `make lint-cross`, `cargo clippy --workspace --all-targets -D warnings` | exit 0 | exit 0 for aarch64-apple-darwin, x86_64-unknown-linux-gnu, x86_64-pc-windows-gnu |
| `cargo fmt --all --check`, `aval check` | clean | clean; 75 records, no findings |
| New tests, by name | present | `rehearse_cluster_pushes_the_snapshot_reads_it_back_and_creates_one_workflow`, `rehearse_cluster_returns_once_the_workflow_is_created`, `the_snapshot_commit_has_head_as_parent_and_a_fresh_committer_date`, `a_snapshot_that_is_not_read_back_submits_nothing`, `a_commit_already_on_the_mirror_pushes_nothing_and_fetches_from_the_mirror`, `a_mirror_without_the_commit_is_not_used`, `collect_writes_the_job_lanes_token_and_the_push_skips_and_signs`, `collecting_twice_writes_one_token`, `every_non_pass_collects_nothing_and_logs_one_line`, `a_tree_mismatch_or_a_failed_prepare_is_no_pass`, `a_pass_with_a_prepare_step_needs_prepare_to_have_passed`, `a_workflow_gone_after_its_ttl_leaves_the_file_with_one_line`, `status_shows_the_phase_and_stop_terminates_the_workflow`, `collect_with_nothing_pending_spawns_no_kubectl`, `collect_makes_one_batched_call_for_three_pending_records`, `a_busy_lock_returns_at_once_without_asking_the_cluster`, `a_closed_lid_loses_nothing`, `a_crash_before_create_leaves_an_intent_and_creates_nothing_twice`, `a_create_whose_answer_was_lost_is_adopted_by_name_not_repeated`, `a_create_refused_by_the_apiserver_submits_nothing`, `an_unconfirmed_create_keeps_the_intent_and_the_gates_run`, `a_changed_image_between_submission_and_collect_is_not_accepted_at_push`, `a_changed_prepare_between_submission_and_collect_is_not_accepted_at_push`, `a_changed_command_or_args_between_submission_and_collect_is_not_accepted_at_push`, `a_pod_running_another_command_is_not_the_submission`, `with_no_detached_keys_a_rehearsal_is_unchanged`, `the_machine_key_alone_detaches_the_rehearsal`, `stamps_collects_and_keeps_its_json_clean`, `a_token_git_refuses_to_write_keeps_the_record_for_the_next_collect`, `an_unreadable_cluster_still_lets_old_records_expire`, `status_shows_a_finished_run_and_what_it_did_not_pass`, `submission_talks_on_stderr_and_stdout_stays_empty`, `stop_fails_loudly_when_a_workflow_cannot_be_terminated`, `exit_codes_follow_the_cli_contract` |
| Revert-red, first round (12 mutations) | each fails its test | skipping `check_pod`; believing the Workflow phase (integration and unit); treating a lost answer as a refusal; creating twice; no read-back; a stale committer date; a mirror that never matches; one group per record; a blocking lock; a token under another name; the collected line on stdout: each failed exactly its named test |
| Revert-red, final round (1 mutation) | fails its test | `probe_run` returning `Exit(code)` with empty output when the stream does not end: `a_stdout_held_open_after_the_child_exits_is_a_failure_not_an_empty_answer` failed |
| Revert-red, review fixes (6 mutations) | each fails its test | dropping the record when its token is unwritten (unit and integration); no expiry when the cluster is unreadable; no same-tree duplicate guard; a token that ignores the arguments; `probe_run` reading stdout after exit (hangs to the 20 s deadline): each failed its named test |
| Zero `kubectl` with nothing pending | 0 calls | asserted (`kubectl_calls() == ""`); no mutation can make it red, since with no record there is no group to ask about |
| Server-side dry run of the generated Workflow (real image, relais mirror, as `amont-gate-runner`) | accepted, fields kept | accepted, and every field survived: the annotations, `dag.failFast: false`, the template `activeDeadlineSeconds: 960`, `podPriorityClassName: gate-detached`, `parallelism: 1`, TTL 172800, init containers `fetch` and `prepare`, and the template annotation `amont.dev/gate` |

### Deviations from the plan

- **`prepare` is 3840Mi, not 512Mi.** Init containers count as a maximum, not a
  sum, so the pod's effective limit stays 4Gi, inside the `gate-detached`
  quota; a dependency install in 512Mi would always OOM.
- **Collect is guarded by `collect_if_declared`.** The hooks and `stamps` collect
  only where the manifest declares a `platform=any` gate, so a repository
  without one pays no extra `git rev-parse`.
- **`claim_twin_cluster_pass` is not on the detached path.** It reads the
  pre-commit twin's token; a rehearsal submits pre-push gates only, so a
  collected token reaches the push through `vouched_on_every`.
- **Unix only, failing closed.** The lane needs `flock`; elsewhere no lock is
  ever held, so nothing is written or submitted and the rehearsal is what it was.
- **Two machine keys the plan did not name:** `amont.cluster.snapshotFetch` (the
  in-cluster URL) and `amont.cluster.mirrorRemote` (the mirror as the laptop
  lists it); `amont.cluster.mirror` is the in-cluster mirror URL, as planned.
- **The submission talks on stderr** and `--status` answers on stdout; exit
  codes are in `docs/configuration.md`.
