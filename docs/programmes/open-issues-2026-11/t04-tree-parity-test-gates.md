+++
type = "task"
mode = "afk"
status = "open"
blocked_by = ["t03-ci-parity-survey"]
repos = ["amont"]
+++
# Extend tree-parity to the built-in test gates

## Question

amont#288: with the survey's answer, what does `amont tree-parity` check for the built-in test gates, so that closing the issue ends the ADR-0024 exception?

Decided with the survey (2026-11-06): drift on a built-in gate is reported at **warn** severity, named and never blocking. `pre-commit-tree-parity` runs in every repository, and about 25 repositories' CI steps differ today, so a blocking check would stop their commits on upgrade. Fixing those CI steps is fleet work past this map's destination (see Out of scope).
