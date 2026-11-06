+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = ["t03-ci-parity-survey"]
repos = ["amont"]
+++
# Extend tree-parity to the built-in test gates

## Question

amont#288: with the survey's answer, what does `amont tree-parity` check for the built-in test gates, so that closing the issue ends the ADR-0024 exception?

Decided with the survey (2026-11-06): drift on a built-in gate is reported at **warn** severity, named and never blocking. `pre-commit-tree-parity` runs in every repository, and about 25 repositories' CI steps differ today, so a blocking check would stop their commits on upgrade. Fixing those CI steps is fleet work past this map's destination (see Out of scope).

## Resolution

Fixed in amont#425, released in v2.1.0, at warn severity as decided. A step
skipped with `!contains(…, '<gate>')` on `pre-push-cargo-test`, `-go-test`,
`-pytest` or `-run-tests-js` is compared with the gate's local command, under
the equivalences in docs/ci.md and with the tree gates' fail-closed reading.
Drift makes `pre-commit-tree-parity` report `Warned`; `amont tree-parity`
prints `warning:` lines and keeps its exit code. Live: amont-agent
(`--locked`), relais (job `env:`), trade-agents (block scalar) warn.
