# Open amont and amont-agent issues, fixed and released

## Destination

Every issue open on 2026-11-06 in fredericrous/amont (#271, #288, #289, #307,
#411, #414) and fredericrous/amont-agent (#149, #204) is closed by a merged
fix that has shipped in a verified release.

## Notes

- Repositories: amont (holds this map), amont-agent. Every session reads the
  `worktree-task`, `merge-when-green` and `tag-release` skills.
- The person's instructions (2026-11-06): fix the issues one by one, with
  this programme and not relais; release the fixes rapidly; skipping local
  test runs is acceptable this one time, so CI is the test gate. A commit or
  push may skip the local suite with `-c hook.skip=<check>`, never
  `--no-verify`.
- One pull request per issue, released as soon as it merges: a patch, or a
  minor when it adds a flag or a setting.
- amont 2.0 refuses a bare push whose gates have not run. Push with
  `amont land --json`. A `v*` tag that owes audits takes
  `git -c amont.pushGates=inline push origin <tag>` until amont-agent#204
  ships (t09), then `amont land --tag`.
- Ticket branches: amont's branch-pattern rule refuses `prog/<slug>/<id>`, so a
  ticket's branch is `fix/oi-<id>` (or `docs/oi-<id>`, `refactor/oi-<id>`);
  that branch is the in-flight signal.
- Another session (fix/tip-tree-verdict, "A push trusts a passed commit with
  the same code") works near attestation; t02 checks with it first.

## Decisions so far

- [Which CI test steps run more than their local built-in gate](t03-ci-parity-survey.md): all of them differ; tree-parity compares each skipped step's argv with the gate's, with stated equivalences, at warn severity
- [How many pushes signed a tree their gates did not run on](t01-attest-dirty-tree-count.md): none can be counted (no clean/dirty record); the fix needs a CHANGELOG line, no migration
- [Tell a rejected attestation from a missing one](t05-attest-covered-rejected.md): rejections named, `--strict` exits 3 (amont#419, v2.1.0)
- [A transitive prettier does not opt a repository in](t06-prettier-opt-in.md): only config or package.json opts in (amont#420, v2.1.0)
- [Close the 1.70.0 index-hint follow-ups](t07-index-hint-lows.md): blind gates refused, git reason kept, push URL redacted (amont#421, v2.1.0)
- [Close the 2.0 follow-ups](t08-two-oh-follow-ups.md): heavy `*` gates owed up front, lows closed (amont#422, v2.1.0)
- [Attest only the tree a gate ran on](t02-attest-dirty-tree.md): per-tip witness shared by stamps and attestation (amont#424, v2.1.0)
- [Extend tree-parity to the built-in test gates](t04-tree-parity-test-gates.md): built-in gate steps compared, drift warns (amont#425, v2.1.0)
- [The push guard reads amont land --tag](t09-guard-land-tag.md): land guard reads `--tag` (amont-agent#208, v3.19.0)
- [Group three functions' arguments into structs](t10-parameter-structs.md): parameter structs, no behaviour change (amont-agent#210)

## Not yet specified


## Out of scope

- Fixing the ~25 repositories' CI test steps that tree-parity will name (t03's table): fleet work, one PR per repository, after t04 ships the check.
