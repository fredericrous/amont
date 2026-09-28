---
status: done
branch: feat/snapshot-scope
repos: [amont]
adrs: []
---
# Snapshot scope and tamper check

## Goal
v1.42.0 has two known limits. First, every lockfile unit in a snapshot gets
installed, including ones the push never touches: website-builder installs
its 7 `spikes/*` projects on every snapshot. Second, `snapshotDeps reuse`
cannot see a package edited in place. Fix both, cheaply, and without
weakening what a stamp means.

## Non-goals
- npm reuse. It stays install-only: checking the install record against the
  lockfile needs a JSON parser this crate does not have.
- Changing the default (`install`).

## Behaviour
- **Scope.** A snapshot prepares the root unit, plus each unit that
  encloses a file changed between the merge-base of (tip, upstream) and the
  tip. "Encloses" means the nearest unit dir above the file.
  - When the range is unknown (no upstream, git failure), every unit is
    prepared, as in 1.42.
  - Skipped units are listed in one line: `snapshot: not preparing
    spikes/S1/, spikes/S2/ — the push changes nothing there`.
- **Tamper check (pnpm reuse).** A clone is rejected, then installed, when
  any regular file in the source's `node_modules` trees is newer than the
  root's `node_modules/.modules.yaml`, pnpm's install record, which is
  rewritten at the end of every install.
  - Ignored: `.bin/` (rewritten right after it) and the `.cache/` and
    `.vite*/` dirs (tools' own caches). The `.pnpm/lock.yaml` file is
    also ignored, because pnpm writes it after `.modules.yaml`.
  - The walk never follows symlinks.
  - The reason names the first newer file.

## Phases
- [x] Phase 1: scope to touched units (range from the rehearsal's push ref,
  or from `@{upstream}` at push time)
- [x] Phase 2: newer-than-install-record check in `reuse`
- [x] Phase 3: docs + CHANGELOG v1.43.0, release on request

## Decision log
- 2026-09-28 — mtime rather than content hashing: 92,695 files are walked
  in 1.7s on website-builder. The check found two real in-place edits to
  `@duro-app/ui` there, made 18h after the install.
- 2026-09-28 — skipping untouched units fails loudly (a gate that reaches
  into one finds no deps) rather than passing falsely. `snapshotPrepare`
  stays the override.
- 2026-09-28 — the range is `merge-base(tip, @{upstream})..tip` for both
  callers, rather than the rehearsal's push ref. This keeps one code path in
  `PushedTree::prepare` and needs no new parameter through `where_to_run`'s
  five callers. With no upstream, everything is prepared.

## Verification
- Phase 1: rehearse a website-builder commit that touches only root
  packages → the `spikes/*` units are named as skipped and not installed.
- Phase 2: reuse on website-builder's live checkout (with the edited
  `@duro-app/ui` files) → rejected, naming the file. Then a fresh scratch
  install → accepted.
- Every guard is broken on purpose once to prove its test catches it.

### Record (2026-09-28)
- **Phase 1:** website-builder, in a scratch worktree at the live HEAD, with
  a probe commit to `packages/blob-store`. Expected: the spikes are
  skipped. Actual: `snapshot: not preparing spikes/S1/, spikes/S1/vite-app/,
  spikes/S2/, spikes/S3/, spikes/S5/, spikes/S6/, spikes/S7/ — the push
  changes nothing there`, and the rehearsal passed.
- **Phase 2a:** the same scratch worktree, holding a `cp -cRp` clone of the
  live `node_modules` (including the edited `@duro-app/ui` Grid.tsx and
  styles.css.ts). Expected: reuse rejected, naming the file. Actual:
  `not reusing … @duro-app/ui/… changed after the install`, then
  `pnpm install --frozen-lockfile` ran, and the rehearsal passed.
- **Phase 2b:** after `pnpm install --force` in the scratch worktree.
  Expected: accepted. Actual: `node_modules in the root reused from the
  working tree — pnpm accepted it against the lockfile`, and the rehearsal
  passed.
- **Tests:** snapshot_deps.rs has 27 cases, plus unit tests. Mutations:
  scope disabled, outer unit claiming nested files, tamper check off, and
  `.bin` not skipped were all caught.

## Outcome
Shipped as v1.43.0. website-builder's snapshots go from eight installs
(root plus seven spikes) to one verified clone. A surprise: the tamper
check's first real run found two hand-edited `@duro-app/ui` files in the
live checkout.
