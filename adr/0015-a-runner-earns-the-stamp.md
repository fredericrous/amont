---
id: ADR-0015
status: accepted
decisions:
  - key: hooks.runner-stamps
    scope: hook-binary
    choice: "`amont run --stamp [<gate>…]` runs the named gates (default: every blocking commit-time declaration and every scoped push gate the repository opted into) on HEAD of a worktree with no tracked change, and writes the tokens the hooks would: a blocking commit-time declaration on the tree note under its script name, a push gate on the tree and the commit, a built-in pre-commit check's built-in token on the tree note. A gate that does not pass, is deferred, cannot run or judges nothing writes no token, and the command exits non-zero. Nothing is signed: the pre-push hook signs what it finds stamped, with its existing key. `amont stamps [--json] [--tree <rev>] [<gate>…]` reports each gate as stamped, attested, deferred or absent, for the gate as it is declared now"
    first: true
    reason: A tool that verified a tree in a clean worktree of its own (relais) had no way to leave amont's record, so the person's commit and push of that exact tree ran the suite again; the one-shot marker lives in the worktree-private gitdir and dies with a throwaway worktree, and nothing could ask which gates a tree already carries a verdict for
---
# 0015 — a runner earns the stamp

Plan: the decisions repository's plan "One verdict per tree: build the rest of the map" (2026-11-01), phase S1, <https://github.com/fredericrous/decisions/blob/main/docs/plans/2026-11-01-one-verdict-build.md>. Programme: "one verdict per tree" in the same repository, tickets t01, t02 and t03.

## Context

ADR-0010 keyed a stamp by the tree and the gate's identity. Its
Consequences made `amont run` for pre-commit checks write no marker, on
purpose: a rehearsal is not the commit the marker waits for, and
`--all-files` judges the working tree, which is not the tree the marker
names.

That left no way for anything but the hooks to earn a stamp. relais verifies
a candidate in a clean, detached worktree that shares the repository's notes,
and its commits are made with `git commit-tree`, without hooks. One relais
change then ran its suite about 3.7 times on the laptop:

- relais's baseline;
- relais's candidate;
- the pre-commit hook;
- the pre-push hook, or its rehearsal.

The audit is the programme's t00.

## Decision

**`amont run --stamp [<gate>…]`** (`hooks.runner-stamps`):

- **Clean tree only.** It refuses a worktree with tracked changes, captured before any gate runs (exit 2). An untracked artefact a gate leaves behind does not count, for the reason `stamp_tips` gives. It also refuses a HEAD that names no commit.
- **What runs, and on what:**
  - A commit-time gate judges every tracked file in its scope (`--all-files`), not an index nobody staged.
  - A push gate judges the one ref a rehearsal would push, `@{u}` (else the remote's default branch) to HEAD.
  - A gate not opted in here, or with nothing in its scope to judge, is not run and not stamped.
- **What gets written,** through the writers the hooks use:
  - A blocking commit-time declaration goes on the tree note, under its script name (`stamp_tree`). That is what the pre-commit hook's reuse and the push twin's pairing read. The marker is not used, because it dies with a throwaway worktree.
  - A push gate goes on the tree and the commit (`stamp_push`).
  - A built-in pre-commit check gets its built-in token on the tree note. The commit hook still reruns built-ins: they read the index. The token is evidence for `amont stamps`.
- **Twins run once.** A push gate whose commit-time twin passed in the same run is reported as covered and not run. Its pairing at push time reads the twin's token.
- **No pass, no token.** A gate that fails, warns, repairs files, cannot run or judges nothing writes no token. A deferred gate (ADR-0013) also writes none.
- **Exit codes:** 0 when everything named was stamped or covered, 1 when a blocking gate failed, 2 when the command refused, 4 when a gate was deferred or not judged.
- **Signing does not change.** Nothing here signs. The pre-push hook signs what it finds stamped, with the key it already uses (the programme's t02: nobody new).

**`amont stamps [--json] [--tree <rev>] [<gate>…]`** answers, per gate, for the gate as it is declared now:

- `stamped`: a valid local token, from the tree note, an input fingerprint or the canonical tree. For a push gate, its commit-time twin's token also counts.
- `attested`: a signed attestation on this platform. This is read only for the tree checked out here, which is the tree the verifier reads.
- `deferred`: a recorded busy deferral.
- `absent`: none of these.

The JSON shape is `{"tree":…,"platform":…,"gates":{"<id>":"<state>"}}`. It exits 0 with an answer, and 2 on usage.

## Consequences

- **relais can earn the stamp.** It names `amont run --stamp <gate>` as its profile commands (the plan's S2), and a green run leaves the tree stamped. The person's later commit and push of that exact tree skip the suite, as long as the base has not moved.
- **This amends ADR-0010.** `amont run` still writes no one-shot marker. `run --stamp` writes tree notes, which are bound to content rather than to a commit that has not happened, so the reason ADR-0010 gave does not apply.
- **The forgery surface does not grow.** A process in the same git directory could already write `refs/notes/amont-gate` with `git notes add`. Hardening a worker's sandbox is relais's business (homelab-offload t09).
- **Stamps stay on one machine.** A remote executor must never stamp, because the pre-push hook would sign its result under the laptop's platform.
