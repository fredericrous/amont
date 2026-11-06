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

## Not yet specified

- Whether the CI-parity survey (t03) finds CI test steps the built-in gates
  cannot express (coverage floors, markers, `-race`), and if so what the gate
  grows: a setting, or a declared gate in their place.

## Out of scope
