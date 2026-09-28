---
status: active
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
- [ ] Phase 1: scope to touched units (range from the rehearsal's push ref,
  or from `@{upstream}` at push time)
- [ ] Phase 2: newer-than-install-record check in `reuse`
- [ ] Phase 3: docs + CHANGELOG v1.43.0, release on request

## Decision log
- 2026-09-28 — mtime rather than content hashing: 92,695 files are walked
  in 1.7s on website-builder. The check found two real in-place edits to
  `@duro-app/ui` there, made 18h after the install.
- 2026-09-28 — skipping untouched units fails loudly (a gate that reaches
  into one finds no deps) rather than passing falsely. `snapshotPrepare`
  stays the override.

## Verification
- Phase 1: rehearse a website-builder commit that touches only root
  packages → the `spikes/*` units are named as skipped and not installed.
- Phase 2: reuse on website-builder's live checkout (with the edited
  `@duro-app/ui` files) → rejected, naming the file. Then a fresh scratch
  install → accepted.
- Every guard is broken on purpose once to prove its test catches it.

## Outcome
