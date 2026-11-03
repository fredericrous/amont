---
id: ADR-0018
status: accepted
decisions:
  - key: hooks.gates-before-connection
    scope: hook-binary
    choice: "A heavy push gate — a scoped pre-push gate the pushed range touches (`dispatch::scoped_push_gates`) — never runs while git holds the remote's connection open, unless the machine asks for it. `amont.pushGates` is `refuse` by default: the pre-push hook asks one shared decision (`decide`: a push stamp or cluster token on every tip, a recorded non-pass completion, a commit-time pair gated or deferred, a twin proved on the cluster) for each owed gate, and if any would have to run it prints the gates and the commands that run them first (`amont land`, `amont rehearse --wait`; `git config amont.pushStamps true` instead when stamp reuse is off) to stderr and exits 1 before any gate starts, without waiting for a background rehearsal. `inline` runs them in the push, as amont did before 2.0. `amont land` runs a pre-stage before every `git push` attempt: it fetches a remote tip the clone lacks or refuses, refuses when stamp reuse is off and gates are owed, and otherwise runs a child `amont rehearse --wait` told the push's real ref line (`AMONT_LAND_PRESTAGE`), whose stdout goes to land's stderr; a failing gate or a pre-stage that cannot run refuses with nothing pushed, a signal ends land `interrupted`. Its push carries `AMONT_LAND=<sha> <remote tip>`, read once and removed; that hook refuses anything still owed, printing `amont-land: remote-moved` when the range's remote side is not the pre-stage's and `amont-land: evidence-missing` otherwise, and land matches those sentinels. Gates that completed without passing (a warning, an unavailable tool) are recorded per tree in `refs/notes/amont-gate-done`, never as a stamp or an attestation, written under the same clean-content condition as push stamps and only with stamp reuse on. An unwalkable push range counts every tracked file as changed. Deletions owe nothing; rehearsal snapshots never refuse"
    first: true
    reason: git connects to the remote before it runs pre-push, so every gate the hook runs holds that connection open; a suite of minutes then loses an otherwise good push to the remote dropping the idle session ("Connection reset by peer"), which amont itself warned about on every such push
---
# 0018 — gates run before the push connection

Plan: [`docs/plans/2026-11-03-gates-before-the-push-connection.md`](../docs/plans/2026-11-03-gates-before-the-push-connection.md).
Amends [ADR-0010](0010-land-and-the-stamp-key.md), whose `amont land` left
"gates, stamps, the rehearsal wait and attestation" in the pre-push hook.

## Context

`git push` opens the connection, reads the remote's refs, and only then runs
`pre-push`, handing the hook those refs on stdin. Every gate the hook runs
therefore holds the connection open. On 2026-11-03 a homelab `amont land` ran
an audit for over a minute that way, and amont's own watchdog line said what
could happen next: the remote drops the idle session and the push fails with
"Connection reset by peer", after every check passed.

`amont rehearse` already ran the scoped gates with no connection open and
stamped the tree, but it was advice, its ref line came from a possibly stale
tracking ref, and nothing stopped a bare push from running the suite inline.

## Decision

**Heavy gates leave the hook; the cheap questions stay.** The unscoped checks
(branch-protect, branch-pattern, secrets, pull-rebase) ask about the push
itself and need its ref lines and the remote's state at push time; they are
fast, and they keep running in the hook.

**`amont land` runs the gates first.** Before every push attempt (including
the retry after a hook moved HEAD) it runs `amont rehearse --wait` with the
push's real ref line, so the rehearsal owes exactly the gates the push will.
It never cancels another tree's rehearsal; it waits for it, then plans again,
because a same-tree rehearsal against another base can have proved a smaller
set. `--wait` itself now plans again after following another run, for the same
reason.

**A bare push refuses by default.** A breaking change, decided by the person:
`amont.pushGates refuse` is the default for every amont user, and `inline` is
the opt-out. The refusal never waits for a background rehearsal — that wait
would hold the connection too — and never suggests a remedy that cannot work
(with `amont.pushStamps false`, rehearsing earns nothing the push can read).

**One decision, asked twice.** The refusal and the loop that runs gates call
the same `decide`, so a gate the loop would skip is never refused and one it
would run never passes the refusal.

**Non-pass completions are their own record.** A warn-severity failure or an
unavailable tool in a rehearsal earns no stamp, and a refusing push would ask
for it forever. `refs/notes/amont-gate-done` records it by tree; only the
refusing push and land's push read it. It is never attested.

**No deferred push.** The hook could refuse and push again in the background
once the gates pass; that would make `git push` report a failure that later
succeeds, leave the pushed sha ambiguous after further commits, and break any
check that the remote ref moved. The work happens in the foreground, in
`amont land`, before git connects.

## Consequences

- A person who pushes with plain `git push` meets the refusal once per unstamped
  tree, with the commands that fix it on their own lines.
- Tag pushes of a merge commit whose tree nobody stamped are refused too:
  `amont run --stamp` in a checkout of that commit stamps it.
- Known limit: the rehearsal and land environment variables outrank the
  configuration, so a process that sets them by hand skips the refusal. They
  are read once and removed, so no gate inherits them.
- Rollback: `git config --global amont.pushGates inline`; a reverted pre-stage
  ships with `inline` as the default again, since land without it is a bare push.
