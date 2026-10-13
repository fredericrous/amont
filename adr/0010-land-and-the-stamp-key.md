---
id: ADR-0010
status: accepted
decisions:
  - key: hooks.gate-stamp-key
    scope: pre-push
    choice: A stamped gate result is keyed by the git tree and, per token, by the gate's identity (full command, resolved program, tool version, declared inputs; for a built-in, its name and amont's version); only a complete namespaced token authorizes a skip, and an identity that cannot be computed is a cache miss
    first: true
    reason: The tree alone said the content was unchanged but not that the same check had judged it, so a changed command or tool kept a stale pass, and a commit note hid the tree note after an amend
  - key: cli.land-verb
    scope: hook-binary
    choice: "`amont land` pushes an explicit sha to the one effective push URL with the hooks running, then reads that URL back; equality is landed, anything else is reported as observed, never as a proven lost update"
    first: true
    reason: A push that exits 0 says the remote accepted something, not that the branch points where it was pushed, and the only check was a skill a person had to remember
---
# 0010 — land, and what a stamp is keyed by

Plan: [`docs/plans/2026-10-13-land-and-tree-keyed-gates.md`](../docs/plans/2026-10-13-land-and-tree-keyed-gates.md).

## Context

Two failures with one root: trusting a result because of where it came from
instead of what it was about.

A push that exits 0 means the remote accepted something. A forge that loses
an accepted ref update, a replica that lags, and a pipeline that trims the
exit status all leave a green terminal and a stale branch. The only guard was
a skill a person had to remember to run.

A gate stamp was keyed by the git tree alone, so it proved the content was
unchanged and nothing about which check had judged it. After an amend that
kept the tree, the commit's own note (commit-time scripts only) also hid the
tree note's push-time tokens, and the push gate ran again for no reason.

## Decision

**Stamps** (`hooks.gate-stamp-key`).

- `stamps_for` returns the union of the commit's note and its tree's note.
  Both are written only for that tree.
- Each token is `<script>@<identity-hash>`. The identity is the full command
  line, the resolved program, the output of its bounded version probe and the
  blobs of its declared `inputs=` for a declared check; the gate name, amont's
  own version and its declared inputs for a built-in. Version probe failing,
  timing out or the program missing means no token is written and none is
  honoured: a miss, never a skip. This is `ci.skip-needs-the-same-command`
  applied to the local stamp.
- Every reader that authorizes a skip compares the complete token. A bare
  name is for display and accounting only. A legacy bare token is not
  honoured by this version.
- `FORMAT` stays `amont-gate-v1`: a bump makes `parse` drop the other
  version's note, so two installed binaries would erase each other's stamps.
  An older binary keeps the new tokens but still matches legacy bare tokens by
  its own rules; compatibility is claimed for parsing and preservation, not
  for invalidation.
- The identity hash reads `ls-tree -r <tree>`, not the index, so the key
  describes the commit being pushed.

**Landing** (`cli.land-verb`).

- `amont land [--force] [--json]` resolves the one effective push URL
  (`git remote get-url --push --all`), reads the ref from that URL, pushes
  `<sha>:refs/heads/<branch>` with the pre-push hooks running, and reads the
  same URL back. More than one push destination is refused in v1.
- Exit `0` landed, `1` refused, `2` usage (unchanged), `3` unreachable or
  accepted-but-unverifiable, `4` accepted but the expected tip was not
  observed. `4` does not claim a lost update: another writer may have
  advanced the ref. The observed sha is reported.
- The push has no deadline of its own, because its hook runs the whole gate.
  The no-prompt environment bounds the network legs when there is no
  terminal; a custom ssh command or credential helper can still stall, and
  Ctrl-C is the answer (exit 128+signal, no retry).
- One retry, only when the hook said `push again`, HEAD moved, and the new
  HEAD descends from the tip that was read. The hook only fast-forwards a
  strictly-behind branch, so the retry lands nothing new.
- After a verified landing the tracking ref is moved to the proven sha and an
  upstream is set the first time.

## Consequences

- A rebase, an amend or a reword that keeps the tree no longer re-runs a gate
  that already passed for it; a changed command or tool does.
- Stamps written before this version cost one re-run each.
- `amont run` for pre-commit checks writes no marker; `hooks.fix-outcome`
  covers fixing and restaging, not stamping, so this stays a separate,
  recorded gap (see the plan, Part A.3).
- A push on a TTY can block on a credential prompt or a stalled hook with no
  deadline; that is deliberate, since the same hook legitimately runs minutes.
