---
status: active
branch: feat/gates-before-the-connection
repos: [amont, greenharness-skills]
adrs: [ADR-0010, ADR-0013, ADR-0014, ADR-0015, ADR-0016]
---
# Gates run before the push connection

## Review panel
👉 **Decide:** none: you chose the audit design (a fresh record per lockfile, 24 h). Approve if `amont land --tag` is the right way to push release tags.
📍 amont + greenharness-skills · Released amont 2.0.0 and skills 0.3.7 (grh 0.1.13) · open: the Phase 2 older/no-amont runs and the tag land, and the Phase 3 homelab and relais live checks. Panel: backend (opus-5-5; delta of a revision), with the original panel and codex on the Phase 1 body.
**Changed by review (1b):** records written only where the working tree is the tip; release mode keyed by the tip's tree, since the dev-only split reads manifests; a missing tool or a timeout records "NOT checked" for 1 h; the record ref is local only.
**Verdicts:** Phase 1 body: 4 approve-with-changes, then approve. 1b delta: backend approve-with-changes, then approve. Carried: map an audit timeout (`None`) to a `CouldNotCheck` record; cite `audit.rs:594`.
📄 Full reviews: [2026-11-03-gates-before-the-push-connection.reviews.md](2026-11-03-gates-before-the-push-connection.reviews.md)

## Context
`git push` opens its connection to the remote **before** it runs `pre-push`.
It has to: the hook reads the remote's refs on stdin. So every gate the hook
runs holds that connection open.
- On 2026-11-03 every homelab `amont land` printed amont's own warning: "git
  opened its connection to the remote before calling this gate … the push then
  fails with Connection reset by peer". audit-go alone ran over a minute.
- Under load, the litellm test timed out again and again, inside that window.

**What exists (amont 1.68.1):**
- `amont rehearse` runs the scoped push gates with no connection open and
  stamps the tree (`rehearsal.rs:493`). But it feeds a *synthetic* ref line, so
  its gate set is only "what this change touched", not exactly what the real
  push will compute.
- `amont land` (`land.rs:374`) runs no gate. It reads the tip, pushes, and
  reads back. Its module doc says gates "stay in the pre-push hook".
- `amont.unstampedPush refuse` (`rehearsal.rs:122`) refuses only pre-commit/
  pre-push *pairs*, not heavy gates in general.

**The person's decisions (2026-11-03):**
- A bare `git push` with an unstamped heavy gate refuses **by default**, for
  every amont user. Agents get a skill path that never hits the refusal.
- **Every part of the Green Harness is optional.** amont, relais, the skills and
  amont-agent each run without the others. So:
  - amont's behaviour and messages name only amont commands, never relais, a
    skill or amont-agent;
  - the cluster collect is a no-op where nothing declares it (it already is:
    `collect_if_declared`);
  - each skill keeps working where amont is absent or older, falling back to a
    bare `git push` as `relais-verified-push` does today.

**Sequencing:** this lands after `feat/detached-rehearse` (amont ADR-0017, phase
2 of the detached-cluster plan) is merged and released. That session confirmed
the constraints:
- the pre-push hook starts with `cluster::collect_if_declared()`;
- cluster tokens stamp scoped (test) gates only;
- `git.rs::probe_run` now fails closed on a drain timeout, so an empty
  `ls-remote` is never "no such ref";
- the rehearsal's detached-gate marker moves from a global to a parameter.

## Goal
A push holds the remote connection only for the cheap checks that need the
push's own refs. `amont land` runs every heavy gate on the exact pushed sha
first, in the foreground, with no connection open, then pushes. A bare
`git push` whose heavy gates are not stamped for its tree refuses in about a
second, naming `amont land`, instead of running them on the open connection.

## Non-goals
- Not moving the unscoped checks out of the hook: branch-protect,
  branch-pattern, secrets and pull-rebase. They need the push's own ref lines
  and the remote state at push time, and they are fast. Phase 1 measures them.
- Not a background "push later" (the hook exiting 1 and a push happening
  behind the person's back): the exit code would lie, the pushed sha would be
  ambiguous after further commits, and agents' "remote ref moved" checks
  would fail.
- Not changing what a stamp is keyed by (ADR-0010/0011/0012) or the cluster
  lane (ADR-0016/0017). Completion evidence for non-pass outcomes is a new record beside the stamps, not a change to them.
- Not CI: amont does not run there (ADR-0005), so nothing changes for CI pushes.
- Not making any Green Harness part depend on another.

## Code review
- amont: backend, lang:rust, unix, tui
- greenharness-skills: none — markdown skills only; their behaviour is checked by the Phase 2 runs

## Behaviour
- **`amont land` gains a pre-stage**, inside the push loop before every `push()`. A `PUSH_AGAIN` retry (`land.rs:423-446`) re-runs it on the new HEAD and sets `AMONT_LAND` to that sha:
  1. It builds the real ref line: `refs/heads/<b> <sha> refs/heads/<rb> <remote tip or zeros>`.
  2. It runs the rehearsal worker **in the foreground** on a snapshot of the
     sha, through the real `amont pre-push`, with that line on stdin and
     `ON_BUSY_ENV=wait`. Before it waits for a host slot, it prints one stderr
     line naming the wait. The snapshot's pre-push collects cluster passes
     first where a cluster gate is declared, so a gate already passed there is
     not run again. It runs every scoped pre-push gate the real push will
     select, and stamps the tree.
  2b. **Concurrency:** if any rehearsal is running, land waits for it and never cancels it. After waiting, land re-reads evidence and recomputes the required gates for its pinned sha and push ref line. A same-tree verdict alone is insufficient: another remote base can select a different gate set. Land runs every remaining required gate before pushing. Test a same-tree rehearsal using a different base and proving only a subset of land's gates. If land's pre-stage is cancelled, land ends `refused` and pushes nothing.
  3. When every heavy gate is already stamped for the sha, the pre-stage
     costs one `stamps` read and runs nothing.
  4. A failing gate ends `land` with exit 1 (`refused`), naming the gate and
     its log. Nothing is pushed.
     - A pre-stage that cannot run (the snapshot was not prepared) also exits
       1, naming why.
     - SIGINT or SIGTERM during the pre-stage ends `land` `interrupted`
       (128+n), with nothing pushed. The next rehearse or land reclaims the
       leftover snapshot (dead-worker path, `rehearsal.rs:555-557`).
  5. Output: one line per gate, on stderr. Today the rehearsal prints on
     stdout (`println!` at `rehearsal.rs:522-653`, and its child pre-push
     inherits stdout), so the worker takes an output sink. In the pre-stage,
     land points the sink and the child's stdout at stderr. `--json` keeps
     stdout for the report alone and adds a `gates` object (ran / stamped /
     failed); a test parses stdout as one JSON document after a failing gate.
- **The push that follows** is `git push` with `AMONT_LAND=<sha>` in its env.
  The hook reads it once in `main` and removes it, as `rehearsal::in_snapshot`
  does (`rehearsal.rs:126-151`), so no gate child inherits it.
  In that mode the hook requires evidence covering every selected heavy gate. Define a typed pre-stage result for every Outcome and effective severity. Only actual passes earn pass stamps or attestations; preserve existing nonblocking warning, unavailable, inert and deferred outcomes through separate completion evidence bound to the staged sha, gate identity and applicable policy. It lives in its own notes ref, beside the stamps and the cluster tokens (ADR-0016). It is written only under `stamp_tips`' clean-tree-at-start condition and is off when `amont.pushStamps` is false. ADR-0018 records it. The shared skip logic consumes that evidence without treating it as a pass. Test warning failures, unavailable tools and inert outcomes end to end, including their exclusion from pass attestations. A missing gate is not by itself evidence that the remote moved: compare the real ref range and required gate set with the pre-stage inputs. Report LAND_REMOTE_MOVED only for a range change that invalidates coverage; otherwise report the actual missing-evidence cause;
  - the hook also prints a `LAND_REMOTE_MOVED` sentinel line. `land` matches
    it in `run.seen`, as it does `PUSH_AGAIN`, never the prose, and reports
    `refused` with that `next` step.
- **Bare `git push`:** a new key `amont.pushGates`.
  - `refuse` (the default) makes the hook, after the cluster collect and
    stamp read, exit 1 before running any heavy gate, if any selected scoped
    gate is unstamped and unvouched on a pushed tip. "Unvouched" comes from
    one `skip_reason` fn that the inline loop also calls (tree stamp, cluster
    vouch, twin claim, pair `Gated` or `Deferred`), so the decision is never
    written twice:

    ```
    amont: <n> push gate(s) have not run on <short>: <names>
        run them before git connects, then push:  amont land
        or stamp this tree first:                 amont rehearse --wait
        or run them in this push:  git config amont.pushGates inline
    ```
    Each line is at most 80 columns (the names wrap onto indented lines), and
    each command stands alone on its line, so it copies cleanly.
  - Under `refuse` the hook does not wait for a background rehearsal
    (`await_for`, `dispatch.rs:1059`); it refuses at once.
  - `inline` is today's behaviour.
  - Precedence: the land/rehearsal environment, then `git -c`, then local,
    then global `amont.pushGates`, then the default `refuse`. The refusal goes
    to stderr and never prompts.
  - Deletions (zero local oid) select no gate, and pushes inside a
    rehearsal or land pre-stage are never refused.
  - `amont.unstampedPush` keeps working; `refuse` covers it.
  - Handle `amont.pushStamps=false` explicitly. When required scoped gates need stamp handoff, land refuses before starting expensive work and explains that stamp reuse must be enabled for land's pre-stage. Bare refusal diagnostics name this configuration and offer enabling stamp reuse or using bare `git push` with `amont.pushGates inline`. Do not suggest rehearsal alone as a remedy while stamp reuse is disabled. Test both pushGates modes with pushStamps enabled and disabled.
- **Audits leave the connection too (revision, 2026-11-05).** Phase 1 measured
  homelab: the checks that stay in the hook take ~2.6 s, but the dependency
  audits (`pre-push-audit-{go,js,python,rust}`) add ~33 s on every push. They
  run whether or not a lockfile changed, so they can block `v*` tags, and the
  owed set above (scoped gates the range touches) never counted them. The
  person chose option 1: a freshness-bounded record keyed to the lockfiles.
  - **The seam** (`hooks/audit.rs`). Each ecosystem function splits into
    `report(settings, release) -> Option<Merged>`, the expensive tool runs over
    `audit_each_dir`, and the existing `conclude_waivable`, which applies the
    push's stakes and today's waivers. The report depends on the stakes (a
    release drops dev-only crates and audits production dependencies), so it
    is computed per mode: `branch` or `release`.
  - **The record:** `refs/notes/amont-audit`, keyed by a synthetic oid, sha1 of
    `amont-audit-v1`, the check, the mode, and, for `branch`, the sorted
    `path blob` lines of every lockfile that audit reads (for `release`, the
    tip's tree oid, since the dev-only split reads the manifests too), taken from the pushed tip's tree (as the
    inputs ref keys by fingerprint, ADR-0011). The body holds the unix time,
    the report class, its ids, `waivable`, and the tools' full output. It is
    written wherever an audit actually ran, in a push, a rehearsal snapshot or
    `run --stamp`, only under `stamp_tips`' clean-tree-at-start condition with
    HEAD equal to the keyed tip (the tools read the working tree, audit.rs:660,
    :951), and is never a stamp or an attestation. It is a local ref, never
    pushed.
  - **Freshness:** `amont.auditMaxAge`, in hours, default 24. A `CouldNotCheck`
    report stays fresh for 1 h, so an offline land is never blocked and is
    retried soon. An absent tool or a timed-out audit writes a record of that
    class too, so the audit is never owed forever.
  - **The hook:** an opted-in audit (its lockfile is present) is owed on every
    non-deletion push, in the push's mode, whether or not the range touches the
    lockfile.
    - When every pushed tip has a fresh record, the hook replays
      `conclude_waivable`: the same lines and the same verdict, judged against
      today's waivers and this push's stakes, with no tool run.
    - Otherwise, under `refuse` or land, the audit joins the refusal's missing
      set; under `inline` it runs and is recorded, as before.
  - **The pre-stage:** the rehearsal plan owes stale audits besides
    `scoped_push_gates`. The snapshot's pre-push already runs every opted-in
    scoped check, so it records them.
- **Tags: `amont land --tag <name>`.** It pre-stages and pushes one tag. The
  ref line is `refs/tags/<name> <commit> refs/tags/<name> <zeros>`, so a `v*`
  tag gets release-mode audits and the gates its range owes, run on a snapshot
  of the tagged commit: the worker snapshots the pre-stage line's `local_oid`,
  not HEAD.
  - A tag that already exists on the remote is refused; there is no `--force`
    for tags.
  - It reads the remote tag back, exactly as a branch land does.
  - `tag-release` uses it on amont ≥ 2.0, instead of `run --stamp` in a worktree
    followed by a bare tag push.
- **Guidance:** the `amont agents-md` block ("Run `amont rehearse --wait`
  BEFORE `git push`…") is rewritten:
  - push with `amont land`;
  - a bare `git push` refuses unstamped heavy gates;
  - `amont rehearse --wait` and `amont run --stamp` are the ways to pre-stamp.
- **ADR-0018, "gates run before the push connection"**, key
  `hooks.gates-before-connection`. It records:
  - the refusing default, why heavy gates leave the hook, and which checks stay;
  - why there is no deferred push;
  - that it amends ADR-0010's "gates stay in the pre-push hook".
- **greenharness-skills:** each skill detects what is installed and degrades:
  - **`relais-verified-push`** pushes with `amont land` when amont ≥ the
    release that adds the pre-stage. Older amont: rehearse, then land. No
    amont: a bare `git push`, verified with `git ls-remote`. It also tells an
    agent that met the refusal what to do.
  - **`worktree-task` F6** drops the separate `amont rehearse --wait` before
    `amont land` on the new amont, and keeps today's paths otherwise.
  - **`tag-release`**, on amont ≥ 2.0, pushes the tag with
    `amont land --tag v<version>`. Below 2.0, or without amont, it keeps the
    plain `git push origin v<version>`.

  Their descriptions name the refusal text, so an agent that meets it
  loads the right skill.

## Phases
- [x] Phase 1 — amont: land pre-stage, `AMONT_LAND` mode, `amont.pushGates` (default refuse), ADR-0018, agents-md text, docs; measure the unscoped checks' wall time (bar: p95 < 3 s on homelab; over it, the plan is revised) and the pre-stage time, with every gate stamped and with none
- [x] Phase 1b — amont: audits off the connection (the seam, `refs/notes/amont-audit`, `amont.auditMaxAge`, hook replay, owed in the pre-stage) and `amont land --tag`; then measure the hook again on homelab (bar: p95 < 5 s with fresh audit records)
- [ ] Phase 2 — greenharness-skills: relais-verified-push, worktree-task F6, tag-release, each with the absent and older amont fallbacks; grh release
- [ ] Phase 3 — release amont (major: the default changes), install, the live checks, then reinstall the skills and check an agent push end to end

## Rollback
- `git config --global amont.pushGates inline` restores today's push for
  everyone on a machine, with no release.
- A defective pre-stage is reverted together with a patch release that sets
  `amont.pushGates` back to `inline` by default. Without the pre-stage, land is
  a bare push, which the `refuse` hook would refuse.
- The skills keep their older-amont paths, so a skills rollback is not needed
  for an amont rollback.

## Decision log
- 2026-11-03 — A bare push refuses by default (the person). The opt-out is a key, not a flag on every push.
- 2026-11-03 — Every Green Harness part is optional (the person): amont names only amont commands; the skills detect amont and its version and fall back.
- 2026-11-03 — The pre-stage feeds the real ref line, not the rehearsal's synthetic one, so its gate set is exactly the push's (`scoped_push_gates` filters on what the range touches).
- 2026-11-03 — The release is a major version: a default that refuses pushes is a breaking change for amont's users.
- 2026-11-03 — `rehearsal::worker` takes the ref line as a parameter. `rehearse` builds it from `ls-remote`, as land does. Before calculating the range, verify that the advertised nonzero remote OID is available locally; fetch the needed object before staging, or refuse with an explicit diagnostic if it cannot be obtained. Range calculation must distinguish failure from an empty range, including in the real hook if the remote moves again. Never interpret an unreadable range as no gates selected. Add integration coverage for an advertised remote commit absent locally, including a force-push case and object-fetch failure. The tracking ref (`pushrefs.rs:88`) can be stale.
- 2026-11-03 — Sequenced after `feat/detached-rehearse`, building on its collect hook point and `probe_run`.
- 2026-11-05 — Phase 1's measurement missed its bar: the checks that stay in the hook took 2.5–3.8 s (bar 3 s, roughly met), but the audits added ~33 s on every push, outside the owed set. The person chose to give audits a freshness-bounded record keyed to the lockfiles (Phase 1b) over running them in every land or leaving them for later. Also, the first probe runs attested to the real remote for a never-pushed probe commit, leaving an orphan note there; timing runs since set `amont.attest false`.
- 2026-11-05 — Implementation:
  - The audits have no file scope (`Scope::new(&[], &["go.sum"])` is opt-in only), so a rehearsal snapshot skipped them; it now runs them.
  - `stdout_piped` keeps git's newline, so the record key is trimmed.
  - Both were found by the new tests going red. A per-process memo cut the hook from ~6 s to ~3.1 s.
  - Four `snapshot_deps` tests skip `audit-js`: they read every command the stub saw.
- 2026-11-05 — Implementation review (implementation-review, backend, lang:rust, unix, tui; all approve-with-changes). Fixed:
  - Audits return what they concluded as a value; a per-push `Ledger` on `Ctx` replaces the thread-local and the global statics, and the land variables are parsed once in `main` into checked types, carried on `Ctx` and passed to `rehearse`.
  - A rehearsal no longer cancels land's pre-stage, and the pre-stage waits at most 3 h for another tree's rehearsal; the state is read again before registering. `rehearse --wait` follows the pre-staged commit's tree.
  - `amont-gate-done` records how the gate ended: a failure under `warn` answers only while the gate is `warn`, and a missing tool for an hour.
  - The Python audit in a snapshot reads the source worktree's `.venv` (`AMONT_SOURCE_WORKTREE`): the same environment, and the same staleness, as the hook in that worktree.
  - A refused bare tag push names `amont land --tag <name>`; `land --tag --dry-run` says `would-land`; `--tag=` alone is a usage error; JSON gains `ref` and `branch` is empty for a tag; `prestage` is an enum and reports `stamped` when nothing new ran; the fetch of a missing remote tip is bounded and names its failure.
  - A push of no commit runs no audit under `refuse` (found by the new stale-record land step).
  - Flag arguments, an output parameter, wildcard arms over amont's own enums, banners, progress on stdout, weak names.
- 2026-11-05 — Second review round (all five approve-with-changes). Fixed:
  - The snapshot borrows the source worktree's `.venv` only when the source's HEAD tree is the snapshot's and its Python manifests are unmodified; otherwise the audit could not check (backend: a `.venv` synced to a newer HEAD would vouch for an older tag). The source worktree is read once in `main` and carried on `Ctx`.
  - The force-push fetch gets the check budget (`amont.timeout`), and its refusal names `git fetch …` to see git's own words (backend, lang:rust).
  - `land --tag` writes its receipt again; `--tag ""` exits 2; branch and tag dry runs print one shape of document, with `ref`, and the headline names the bare tag (unix, tui).
  - A followed rehearsal stopped by a signal records `signal=n` and ends `rehearse --wait` with 128+n, so land reports `interrupted` (unix). `prestage` is `ran` when land followed a running rehearsal (tui).
  - Next steps: a tag refused for missing evidence is offered only `amont land --tag`; stamps off names `git config amont.pushStamps true` and land; a malformed `AMONT_LAND` names `unset` (tui).
  - `rehearse` is handed the pre-stage by `main`; the last progress line moved to stderr; the dry-run flag argument is gone; `Ran` is `Concluded | Inert | Unavailable`; `AuditMode` is computed once; `into_report` returns `ControlFlow`; one `signal_of`; completion records are written once per kind and return a `Result`, and an `unavailable` record replaces the previous one (implementation-review, lang:rust, backend).
  - A deletion-only push runs no audit (tested), and the comment says so.
- 2026-11-05 — Declined at review, with reasons:
  - `gates.failed` in the JSON: the failing gate names itself in the hook output above land's verdict; land's `next` names `amont rehearse --status` and `amont land`.
  - Hiding the `amont-land:` sentinel lines: land streams the push's output as it comes, and a line filter would hold it back.
  - A lock around the rehearsal state file between its last read and the write: the window is now the snapshot checkout, and a lost race costs one rehearsal, not a wrong verdict.
  - The source-venv check is NOT TESTED end to end (it needs a uv project); its logic compares two trees and one `git status`.
  - govulncheck reachability: a branch record is keyed by `go.sum`/`go.mod`, so a `.go` change that reaches a vulnerable function goes unseen for up to `amont.auditMaxAge`. Bounded: branch audits only warn, and a release record is keyed by the whole tree.
  - `.adr/packs/decisions.pack` is refreshed in its own commit: it carries the fleet's current code rules, which the review briefs read.
- 2026-11-05 — A tag push needs release-mode audits, which no branch rehearsal computes, so tags get `amont land --tag` rather than a stamp in a worktree.
- 2026-11-06 — Rebased onto amont 1.70.0 (#409, declared gates see the commit index); the patches carried over unchanged, and `push_gates.rs` became `cfg(unix)` (its fixtures use the unix-only `tree_gates`, which failed #409's Windows build). Review of the rebased tree (all five):
  - A declared push gate whose program is not installed has no token, so no record could ever answer for it, and land refused forever (backend, high). Such a gate is no longer owed: it runs in the push, fails to start, and warns (tested, red without the fix). The delta review (lang:rust, backend, unix) narrowed the exemption to `gate_identity::program_missing`, a declared command whose program cannot be found, so a token that failed for another reason (a hash git would not compute) stays owed and refuses rather than run on the connection; a unit test pins it, and the integration test asserts the warning.
  - `strip_git_env` and the rehearsal's `strip_repo_env` drop the 1.70.0 index hints too, so no child of the audit, the snapshot prepare or the rehearsal inherits them (unix, low).
  - CHANGELOG `## Unreleased` names the breaking default, the opt-out, and the first refused push after an upgrade from before 1.70.0; three doc passages that still had gates running in the push now say where they run (tui).
- 2026-11-06 — Released: amont 2.0.0 (#412 merged as `e1bb1e97`, release #413, tag at `62546881`). Every release job succeeded; GitHub, crates.io (amont, amont-runtime, amont-quiet, amont-slots) and npm were verified directly. npm lagged about a minute behind the job. The release had to move the inter-crate `amont-runtime` floor from 1.24.0 to 2.0.0: it is a caret requirement, so a major release must move it. Installed here (Homebrew), so every hook on this machine runs 2.0.0. The skills went out as greenharness/skills#11 (v0.3.7), pinned by grh 0.1.13 (greenharness/grh#18). Follow-ups: fredericrous/amont#414 (a heavy `*`-scoped declared gate still runs on the connection; the connection advice is printed inside a rehearsal snapshot; the review lows) and fredericrous/amont-agent#204 (the guard does not read `amont land --tag`).

## Verification
- Phase 1, integration tests in `crates/amont/tests/it/`, with the bare-origin fixture of `it/land.rs`:
  - `land` with a slow fake gate:
    - the gate runs before the push child starts (ordering is recorded by a `git` shim);
    - the push's hook prints the stamp-skip line;
    - the shim log shows no heavy gate started inside the push (the hook's duration is printed, not asserted) → `land_runs_the_gate_before_the_push_and_the_push_does_not_repeat_it`: the gate log holds one run (the pre-stage's), and the push prints the stamp-skip line.
  - The hook fast-forwards HEAD (`PUSH_AGAIN`): the pre-stage re-runs on the new sha, and `land` reports `landed`, not "remote moved" → NOT TESTED. The pre-stage is inside the push loop, by construction, but no test drives the retry.
  - SIGINT during the pre-stage: `land` exits 130 with nothing pushed, and the next `land` reclaims the snapshot → NOT TESTED. A pre-stage gate stopped by a signal now ends `rehearse` with 128+n and land `interrupted`, but no test sends one. SIGTERM to land's pid alone leaves its `rehearse --wait` child running.
  - `land --json` with a failing gate: stdout parses as one JSON document (`jq -e .gates`) → stdout is one `{…}` document carrying `"prestage":"failed"` (`a_failing_gate_in_the_pre_stage_pushes_nothing`), and the success case asserts the exact `gates` object.
  - `AMONT_LAND` is not visible to a gate child; a commit-gated pair and a `Deferred` pair do not refuse → `neither_land_variable_reaches_a_gate`: a scoped gate runs under `AMONT_LAND_PRESTAGE` in the pre-stage and an unscoped (`*`) gate runs under `AMONT_LAND` in land's push, and neither sees its variable. (The first version ran no gate under `AMONT_LAND`; review caught it.) A malformed `AMONT_LAND` refuses, and is never reported as a moved remote (tested). The pair cases under `refuse` are NOT TESTED; they go through the shared `decide`, and their tests run inline.
  - A failing gate: `land` exits 1 and nothing reaches the bare origin (ref unchanged) → passes.
  - Bare `git push`, unstamped heavy gate, default: exit non-zero with no heavy gate started (shim log), with the message, and the origin unchanged. With `amont.pushGates inline`: today's behaviour → passes; every refusal line is at most 80 columns. 38 existing tests that drive a gate inside the push now opt into `inline`.
  - Stamped tree (after `rehearse --wait` or `run --stamp`): a bare push passes, running only unscoped checks → passes (`run --stamp`).
  - The remote moves between the pre-stage and the push: the hook refuses in `AMONT_LAND` mode, and `land` reports `refused` with "run amont land again". A push whose range is unchanged but whose evidence is missing prints no `LAND_REMOTE_MOVED` (`run.seen`) → both sentinels are tested at the hook. `LAND_EVIDENCE_MISSING`, carried from review, is added.
  - A same-tree rehearsal against an older base proves a subset of land's gates: land waits, then runs only the remaining gates (shim log), then pushes → NOT TESTED. `rehearse --wait` plans again after following a run, follows the pre-staged commit's tree (not HEAD's), and neither a rehearsal nor the pre-stage cancels the other (state `kind=prestage`, a 3 h bound; the record round-trips in a unit test).
  - A warning-severity failure, an unavailable tool and an inert gate: each writes completion evidence and no pass attestation (`git notes` read), and the AMONT_LAND push neither refuses nor re-runs it → the warning case is tested end to end (no pass token, an `amont-gate-done` entry, no re-run). The record now names how the gate ended; a unit test checks that a failure under `warn` stops answering once the gate is `block`, and that an unavailable tool answers for an hour only. Inert gates owe nothing (not opted in).
  - `amont.pushStamps false` × {refuse, inline}: land exits 1 before any gate starts, naming the setting. A bare push under refuse names the setting and does not offer `rehearse`. Inline runs the gates as today → land and the bare refusal are tested; `inline` with `pushStamps false` is NOT TESTED separately.
  - The origin was force-pushed to a commit absent locally: land fetches it and selects the same gates as a fresh clone would. If the fetch fails, land exits 1 naming the oid and pushes nothing. An unreadable range in the hook refuses, and is never read as "no gates" → the hook's unwalkable range refuses (tested). Land's fetch path is NOT TESTED.
  - A tag push of an unstamped merge commit refuses; after `run --stamp` it passes. A deletion is never refused → both pass.
  - A cluster pass collected before the pre-stage is not re-run. A repository with no cluster gate makes no cluster call → NOT TESTED here; the existing `cluster_gates` and `detached_cluster` suites pass (under `inline`).
  - No amont message names relais, a skill or amont-agent (a grep test) → `the_push_refusals_fit_and_name_only_amont` checks every refusal text.
  - The unscoped checks' wall time is measured on the homelab tree (they stay in the hook) → 2.4–3.8 s over 8 runs (bar p95 < 3 s: roughly met). The full hook was ~35 s, almost all of it audits, which led to Phase 1b.
  - `make check`, clippy across targets, and fmt are clean; each new test fails when its behaviour is reverted (revert-red) → the workspace suite, clippy `-D warnings` and fmt are clean on the pinned 1.94.1 (Homebrew's 1.99.1 shadows it and flags `staged_only.rs`, which this branch does not touch). Revert-red is not systematic: two defects were caught red by the new tests (audits skipped in rehearsal snapshots; a newline in the record key), and the sentinels and refusal are asserted exactly.
- Phase 1b, integration tests (fixture with a lockfile and a fake audit tool whose runs are logged):
  - A docs-only push after land: the audit runs once in the pre-stage and the push replays it (the shim log shows no tool run in the hook) → passes.
  - A record older than `amont.auditMaxAge` (timestamp injected) is owed again: a bare push refuses and land runs it; a `CouldNotCheck` record expires after 1 h; a lockfile change makes a new key → all three pass; the stale case now also lands and checks that land ran the audit (the first version only checked the refusal).
  - A replay applies today's waivers and stakes: a branch push only warns, while a `v*` tag push with an unwaived vulnerability blocks even on a fresh record; a branch-mode record does not satisfy a `v*` tag push → a branch record does not satisfy a tag, and `land --tag` blocks a vulnerability (both tested). Today's waivers at replay are NOT TESTED; replay calls `conclude_waivable`, which loads them.
  - `amont land --tag v1.0.0` pre-stages release mode on the tagged commit (not HEAD), pushes the tag and reads it back; an existing remote tag is refused → passes.
  - Offline (the tool reports it could not check): land is not blocked, and the replay says "NOT checked" → passes.
  - No `cargo-audit` on PATH, and an audit timeout: under refuse, land lands and the replay says "NOT checked" → `a_missing_audit_tool_never_blocks_a_land` runs land on the system PATH alone (no `~/go/bin`) and checks both "did NOT run" and "NOT checked". (The first version skipped itself on this machine.) The timeout path is NOT TESTED; it records "could not check" the same way.
  - A dirty lockfile, or a push of a non-HEAD branch: no record is written (`git notes --ref amont-audit list`) → the dirty lockfile is tested; the non-HEAD branch takes the same condition (HEAD equal to the tip).
  - A crate moved from dev-dependencies to dependencies with the lockfile unchanged: a `v*` tag push re-runs the audit and blocks → NOT TESTED (it needs a cargo-audit fixture). A release record is keyed by the whole tree, so any manifest change is a new key.
  - Homelab, branch binary (attest off): hook wall time with fresh audit records, 5 runs, p95 < 5 s → 3.02–3.74 s against a 2.38–2.49 s baseline without the audits, alternated, at load 14–18 (~6 s before per-process memoization; ~35 s before Phase 1b). The pre-stage that recorded the audits took 83 s once.
- Phase 2: for each skill and each of {new amont, 1.68 amont, no amont}, the command it issues and the remote tip observed. Each changed skill takes the right path with the new amont, an older amont, and no amont (each shown by its own run or check) → with the new amont, run live:
  - **relais-verified-push / worktree-task F6:** `amont land --json` landed greenharness/skills#11 (`3e0dd69`, `"prestage":"stamped"`) and greenharness/grh#18 (`97f3733`, `"prestage":"ran"`, then `stamped` after a branch rename). Each remote tip was read back.
  - **tag-release:** `amont land --tag v0.1.13` was held by the amont-agent push guard, which reads only `amont land [--json] [--force]` (fredericrous/amont-agent#204). A bare `git push origin v0.1.13` was refused as designed: it named `amont land --tag` and offered no rehearse. The tag was pushed with the refusal's own `git -c amont.pushGates=inline` remedy, and the tag read back on the remote.
  - **Older amont and no amont:** OPEN, not run as live pushes. Those paths are unchanged from before 2.0 in all three skills. The skills implementation review (three rounds, recorded in greenharness/skills#11) read each skill's version test and fallback order, but a read is not the run this phase asks for.
  - **tag-release with the new amont:** OPEN. The `amont land --tag` path waits on fredericrous/amont-agent#204.
  - grh 0.1.13 (greenharness/grh#18) pins skills 0.3.7, whose tarball hash was checked against its `SHA256SUMS`. It is installed here: `grh install gate workflow`, and the three installed skills carry the 2.0 text.
- Phase 3, after install:
  - In a scratch clone, a bare push → refused in < 2 s, origin ref unchanged → 0.7 s with the installed 2.0.0 hook on an unstamped range whose gate sleeps 30 s (so none ran); it named the gate and the three remedies, each line under 80 columns. Measured on the hook directly: the guards rightly refused a probe `git push` with `--no-verify` and a variable `cd`.
  - On a real homelab branch, `amont land` → gates run first, then the push, with the push's hook time < 5 s → OPEN: taken from the next real homelab push. The branch binary measured 3.0–3.7 s in Phase 1b.
  - A relais run's push lands → OPEN: taken from the next relais run's push.

## Outcome



<!-- panel: repos=amont,greenharness-skills reviewers=backend,lang:rust,tui,unix,codex body-sha=7f4bea0085eb -->
