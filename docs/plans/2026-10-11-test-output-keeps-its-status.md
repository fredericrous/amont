---
canonical: amont-agent:docs/plans/2026-10-11-test-output-keeps-its-status.md
phases: [1, 2, 3]
status: active
---
Part of [Test output that is cheap and keeps its exit status; a re-read that can be refused](https://github.com/fredericrous/amont-agent/blob/main/docs/plans/2026-10-11-test-output-keeps-its-status.md).

This repository carries phases 1–3: `amont run <check>` captured in a one-slot `Stage` whenever quiet is on, `Stage::finish` taking the check's `Outcome`, the quiet result (one ok line for `Passed`, messages plus excerpt and `full log:` otherwise, full log in CI), the shared capture and excerpt rules, and a copy of amont-agent's golden excerpt fixtures with a cross-check against its `MANIFEST.sha256`.
