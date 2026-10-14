# Full reviews: agents land through amont land

## Full reviews (reference)

- backend: round 1 approve-with-changes (45k, 64 s; 6 findings, all applied); round 2 approve-with-changes (51k, 27 s; retry check not resolved, applied; 2 carried); delta approve-with-changes (53k, 16 s; `record_before` skips non-gated repos, exit-0 only, applied); delta approve (55k, 54 s; 2 carried); person delta (four P1s) → delta approve-with-changes (6 findings: background land, PostToolUseFailure routing, unverified/not-observed retry, stale panel lines, receipt checks, `receipt_dir`; all applied) → delta approve (all resolved; 1 low carried).
