---
id: ADR-0006
status: accepted
decisions:
  - key: fleet.management-tool
    scope: fleet-tool
    choice: amont-fleet, a TUI; propagate.sh is deleted
    first: true
  - key: fleet.symlink-policy
    scope: fleet-tool
    choice: Report it, never write through the link
    first: true
    reason: Writing through a symlink edits a file the repository does not own
  - key: fleet.reporting-format
    scope: fleet-tool
    choice: N of M, never a bare adjective
    first: true
    reason: "\"mostly managed\" is not a number anyone can act on"
  - key: cli.check-identifier
    choice: "`<trigger>-<name>`, resolved identically by every surface that names a check"
    first: true
---
# 0006 — the fleet tool, and how a check is named

Detail is in [`docs/fleet-dashboard.md`](../docs/fleet-dashboard.md), marked
built, and [`docs/hook-skip-management.md`](../docs/hook-skip-management.md),
marked shipped.

## The tool replaced a script

`amont-fleet` is a terminal interface over the managed repositories.
`scripts/propagate.sh@90b0d30` is gone rather than kept alongside it — two ways to do
the same thing is how they drift. The path is pinned to the revision that
removed it, because the record is about a file that no longer exists and
deleting the citation would hide that.

## A symlinked repository is reported, not repaired

A repository whose dispatchers are symlinks is no longer managed, and `fix`
says so instead of writing through the link. Writing through would edit a file
the repository does not own, which is a surprise nobody asked for and a change
nobody can find afterwards.

## Counts, not adjectives

State is reported as N of M. "Mostly managed" reads as reassurance and contains
no information; "9 of 11" tells you there are two to look at.

## One identifier

A check is named `<trigger>-<name>`. Three things refer to a check — the
dashboard's toggle, `hook.skip`, and `amont.severity` — and all three resolve
the name the same way. An identifier that means something slightly different
depending on where it is typed is a bug that presents as confusion.
