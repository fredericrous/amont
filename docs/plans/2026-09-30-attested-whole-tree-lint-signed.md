---
status: active
branch: feat/tree-lint-attest
canonical: https://github.com/fredericrous/decisions/blob/feat/attested-skip/docs/plans/2026-09-30-attested-whole-tree-lint-signed.md
phases: [2]
repos: [decisions, amont]
adrs: [ADR-0024]
---
# Attested whole-tree lint, signed only when warm (pointer)

The plan lives in the decisions repository, which owns ADR-0024
(`ci.attested-skip`). This repository carries **Phase 2**: repository-declared
`tree` gates in `amont.conf`, the `tree-parity` check, the side-car runner
with an enforced slack, namespaced caches with a warm-up, the exact-tree
push reader, rehearsal integration, skew withholding and evidence.

Read the canonical plan for the design, the review panel and the
verification list. Implementation decisions taken here are logged below.

## Decision log

- 2026-09-30 — **Options grammar:** options after `attest` are lowercase
  `key=value` tokens. An uppercase `NAME=value` begins the command, so an
  env-prefixed command stays expressible.
- 2026-09-30 — **Inherited `shell: bash` accepted for a simple command**
  (the person's decision). The pilots' workflows set
  `defaults: run: shell: bash` workflow-wide for `pipefail`, and the reviewed
  fail-closed rule rejected every inherited default. tree-parity now accepts
  exactly that block when the gated command has no pipe, list, redirection or
  substitution. Inherited `env:`, `working-directory` and any other shell stay
  rejected. Checked on both pilots' real workflows: all gated steps pass, and
  a drifted `run:` is caught with its line.
- 2026-09-30 — **tree-parity is `Reach::Convention`, not Safety.** It fires
  only in a repository that declares tree gates. The safety net stays the
  low-false-positive set.
- 2026-09-30 — **`.env` in application-landscape's allow-list** (the
  person's decision). The live checkout holds a gitignored `.env`, and
  `snapshotCarry .env` copies it into rehearsal snapshots, so without it no
  commit or rehearsal there could stamp. The allow-list only decides whether
  the file's presence withholds a stamp. Nothing reads, hashes or uploads
  it: the namespace hashes staged files only, and the attestation carries
  gate names and a tree id. Kept per repository, not a default: a linter
  whose verdict depended on `.env` would pass here and not in CI, and
  eslint, prettier and ruff do not read it.
- 2026-09-30 — **Skew checked against real installs.**
  - **application-landscape:** `node_modules` reads **in sync** (the npm
    comparison skips optional packages for another platform and link
    entries, as the carried must-fix required).
  - **duro-app:** a true positive. `@duro-app/eslint-config@3.0.0` is locked
    but not installed (last install Aug 10).
  - **website-builder** uses pnpm, which the plan did not cover. Added:
    `node_modules/.pnpm/lock.yaml` must be byte-identical to
    `pnpm-lock.yaml`. That found a true positive there too (the lock gained
    overrides two days ago; the install dates from Sep 6), and
    duro-design-system reads identical. yarn and bun installs cannot be
    verified, so they withhold.
- 2026-09-30 — **uv drift rule.** A bare `uv sync --locked --check` flagged
  trade-agents' `.venv` as drifted because it lists `--all-packages` extras
  as removals: a false positive. Rule: in sync when the environment
  EXACTLY matches the default sync or the `--all-packages` sync (checked
  both ways). `--inexact` was rejected, since it would admit extras a linter
  could resolve locally and CI could not.
- 2026-09-30 — **Pilot (application-landscape clone, this branch's binary,
  13 commits).**
  - **Code commits (`.ts`):** **5/5 proven** (`pass`), hidden behind the
    commit-time test run (340–565 s).
  - **Docs-only commits:** **0/5 proven**, each `cancelled` at the 2 s
    slack. Warm eslint takes ~3.1 s and the commit's own checks ~1.5 s, so
    each docs commit took 4.1 s against a 1.5 s baseline, for nothing.
  - **Allow-list:** the pilot also showed that the commit-time tests create
    `.react-router/`, `build/`, `test-results/` and `*.db*`. eslint ignores
    all of them (`eslint.config.mjs`), so the pilot's
    `snapshotPrepareOutputs` lists them with `.env`.
- 2026-09-30 — **Skip what cannot fit** (the person's decision). A warm gate
  starts only if its last commit-time run fits this commit's expected cover
  plus the slack. The cover is the longest last-measured duration among the
  repository's declared commit checks in scope. Otherwise it is skipped,
  with one line, and recorded as the evidence outcome `slow`. Measured, not
  guessed from names: `aval check` is a blocking declaration too, but its
  0.1 s gives no cover.
- 2026-09-30 — **Defect found while testing, fixed: the version probe
  blocked the commit.** The namespace's `<tool> --version` probe ran in the
  hook's main thread, unbounded, BEFORE the commit's checks started.
  pyright's wrapper can reach PyPI, so a slow network could stall every
  commit. Fix: each gate's whole preparation now runs in its own thread,
  overlapping the checks:
  - version probe (through `tree_run`: deadline, cancel, process-group kill);
  - skew check;
  - namespace;
  - warm and fit tests;
  - lock.

  A probe that does not answer in time withholds. A tool that is absent or
  errors reads `absent`, as before. Regression test: a fake `pyright` whose
  `--version` sleeps 300 s does not delay the commit.
- 2026-09-30 — **Timing tests are bounded by what they prove, not by the
  machine.** Wall-clock bounds of 15–60 s failed under this machine's load
  (whole-suite time swung between 43 and 184 s). Instrumented runs showed
  the process-group kill lands every time. The slow-gate and hanging-probe
  tests now use a 300 s gate against a 200 s bound, which only a commit that
  waited for the gate can exceed. The lock test allows eventual release: a
  concurrent fork holds the lock's description until its exec.
- 2026-09-30 — **Re-pilots (application-landscape clone, this branch).**
  - **Fit rule, first run:** code commits 2/2 proven (eslint measured
    4–5 s behind a ~6 min test run). Docs commits were skipped but still paid
    ~1 s of preparation. It also exposed that a docs commit's instant "pass"
    of the unscoped test run (41 ms) overwrote the run's real 340 s. Fixed
    in `ee4c49a`: the fit test runs first, and durations are recorded only
    for declarations in scope.
  - **Interleaved run:** a skipped docs commit costs **1.3 s, the
    baseline**. The first docs commit learns the gate's time (ran, was
    cancelled once), and the code commit after it is **proven**. A fresh
    start skipped one code commit because the covering test run had never
    been measured. Fixed next: **unknown cover counts as run**, which
    teaches both numbers. Two other docs commits took 3–4.8 s while a 20-min
    test gate ran on the same machine: load, not the gate (they were
    `slow`-skipped at once).
- 2026-09-30 — **Implementation review, round 1: approve-with-changes.**
  - **Fixed:**
    - the tree guard withholds when git cannot list files, instead of
      reading "none";
    - typed-eslint detection fails closed (bounded `--print-config` via
      `tree_run`, first file that yields JSON, a marker only when
      definite, undetermined reads typed);
    - an exhaustive `TreeRun` match in the rehearsal;
    - a re-check before stamping: the working tree must still equal the
      index, with nothing new untracked, so an autosave or a writing gate
      mid-run withholds.
  - **Tests added:**
    - a reword keeps the tree proof through push;
    - a prepare writing an untracked module cannot forge the tree;
    - the slow gate's child is gone when the commit returns;
    - a moving tree is withheld;
    - typed/untyped config and undetermined → typed.
  - **Deferred, with reasons:**
    - *SIGTERM warm-up → next commit cold.* The completion marker is
      written by temp + rename only after a full clean run, so a killed
      warm-up leaves no marker, and `tree_run` group-kill is tested.
    - *uv probe leaves `.venv` mtime unchanged.* The probe is
      `uv run --frozen --no-sync`; a test needs a uv project fixture, left to
      the trade-agents pilot.
    - *`treeLintWait` as one aggregate deadline.* A single `until` computed
      from pre-push start (`await_verdict`); no rehearsal-timing fixture.
    - *`unstampedPush refuse` does not block.* Tree gates never enter the
      push-check loop that rule governs, by construction.
    - *80 columns with `NO_COLOR`.* The lines are short fixed prefixes plus
      gate names; no test.
  - **Wording:** the plan's `… — 3 problems outside this commit; CI will
    lint` became `… — <the tool's last line> — CI will lint`. amont cannot
    know which problems are outside the commit; the tool's own summary line
    is what it has.
- 2026-09-30 — **Implementation review, delta: approve-with-changes.**
  Round-1 items 1–6 are resolved; 7 (tick Phase 2) happens at merge.
  - **Fixed:**
    - typed-eslint samples where eslint runs (the gate's `cwd`) and asks
      about every sampled file: typed if any is, or if none answered;
      untyped only when some answered and all were untyped. Tested with a
      fake eslint typed under `src/` only;
    - the slow-gate test checks its own child's pid, not a machine-wide
      `pgrep`.
  - **By-hand items answered:**
    - *re-check → stamp window.* The stamp names the INDEX tree (`git
      write-tree` under the hold). An autosave changes the working tree,
      never the index, and the re-check proves the gates ran on exactly
      the index's content.
    - *rehearsal re-check.* The snapshot is a private worktree, and
      `snapshotPrepare` has finished (and been guarded) before any gate
      starts; nothing else writes to it.
    - *`unstampedPush refuse`.* Left to the first pilot push, as deferred
      above.
- 2026-09-30 — **Implementation review, confirmation pass:
  approve-with-changes.** A first-10 sample in `ls-files` (alphabetical)
  order could still read untyped when ten untyped files sort before a typed
  `src/**`. Fixed: one file per (directory, extension) pair, as flat configs
  select files. Every pair answered is definite; more pairs than the cap
  (256) reads typed with no marker. The cap was sized from
  application-landscape (83 pairs, 778 files): the probe runs once per
  namespace in the background warm-up, and a commit reads the marker. By
  hand: an eslint or tsconfig change moves the namespace (config-like by
  basename), so an old marker is never reused.
- 2026-09-30 — **Final bind pass: approve-with-changes (two lows), plus the
  person's decision on the residual gap.** A config that types files by
  NAME (`**/*.test.ts`) could escape a (directory, extension) sample. The
  person chose a **config-text check**: any tracked `eslint.config.*` or
  `.eslintrc*` naming `projectService`, or `parserOptions` with `project`,
  reads typed. A false positive only costs the cache.
  - **Residual gap, accepted:** a shared config PACKAGE that types by file
    name, whose words never appear locally and whose files no sample hits.
  - **Wording:** pairs eslint ignores (it prints `undefined`) answer
    nothing and do not block "untyped". Untyped needs at least one answer,
    and every answer untyped.
  - **Warm-up budget:** the typed probe in `amont warm` runs under its
    600 s ceiling, which covers application-landscape's ~83 calls (~1 min).
  - **Also fixed:** the stale doc comment, and a test for the over-the-cap
    branch (257 pairs: typed, no marker).
