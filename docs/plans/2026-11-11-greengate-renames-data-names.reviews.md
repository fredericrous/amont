# greengate 4.0 renames its data names — full reviews


The read-both plan (bodies 7380961540bd to 910086add97f) was reviewed in two rounds and final checks by backend, lang:rust, platform and architect (approve at 910086add97f), then rejected by the person: "why keep both GREENGATE_X, else AMONT_X? I mean it's not like I have users yet"; the person chose "Hard rename + one-time migration".

Delta on the hard rename (body 1ccbb86f58d1): architect approve-with-changes (27.6k/38 s: slot queue split, rollback after the sweep, which name wins, missing files), lang:rust rework (45.6k/72 s: slot queue split, crate order, worktrees and a real flock, held store restore under a live 3.x, hot-path trigger, names-test exemptions, global config lock), backend approve-with-changes (33.8k/82 s: slot queue, worktrees, pack markers, census phase, multi-valued trusted, tracked AMONT_ settings, rollback, profile line), platform approve-with-changes (51.6k/88 s: PR 1 would prune amont-gates, Argo removal order, Vault token, laptop kubeconfig, detached runs in the cutover check). The slot conflict went to the person ("Switch all three in one install window"). Re-binds (bodies 8ab054364767, 82a01ad4408a, e2e1e65c8300): approve from backend (47.8k/91 s), lang:rust (58.7k/41 s), platform (66.3k/90 s). codex: not installed.

Carried into implementation:
1. lang:rust: the first-run migration's trigger is a done-marker `stat` on the fast path; the hook's cost is measured before and after 4.0.
2. lang:rust: old names the plan keeps (notes refs, `amont-snapshots`, the fallback read, `migrate.rs`) are constants in `names.rs`; the names test keeps per-file pins for them.
3. lang:rust: a check that no `amont-slots` directory is left after the install window.
4. backend: the empty-slot-directory check can race a hook that queues after it; retry or lock around the install.
5. backend: the held-store sentence says "renamed" for the preserved store only (wording); `pgrep -x amont` is only meaningful before 4.0 is installed (the alias is 4.0 after).
6. platform: set `greengate.cluster.kubeconfig` after the first-run migration, inside the install window (the migration copies the old value), or have the migration rewrite that one value.
7. platform: the runner ServiceAccount is `greengate-gate-runner` (pattern of `rbac.yaml:17`); the script's `whoami` check names it exactly.
8. platform: three more 50Gi `local-path` PVCs on wk-3 during the overlap, with cold caches; free disk checked before PR 1.
9. backend: the Vault path of the read token is named in the homelab PR.

