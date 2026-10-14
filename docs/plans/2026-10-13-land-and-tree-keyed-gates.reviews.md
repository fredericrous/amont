# Full reviews: land and tree-keyed gates

## Full reviews (reference)

- backend: approve-with-changes (r1 39k, 80 s; r2 45k, 110 s; final-body delta 30k, 14 s). Resolved: stderr tee, exit 4, v1 format, retry re-read. Carried: Unreachable stderr markers and unit test; TTY push can block (note in ADR); first landing without upstream needs `set-upstream-to`; `namespace_for` vs `namespace` hash material normalized and tested, readers authorizing a skip compare the complete namespaced token (supersedes the earlier script-only note).
- rust: approve-with-changes (40k, 61 s). All applied.
- tui: rework (36k, 71 s), approve-with-changes in round 2 (29k, 13 s). Carried: state the TTY push has no deadline; enumerate `--json` status values `landed|refused|unreachable|unverified|not-observed|interrupted|usage` and assert per exit code; verdict line is plain text, no colour or glyphs.
- unix: approve-with-changes (39k, 92 s). All applied.
