# Gates run before the push connection: full reviews

- **backend** r1 (approve-with-changes, 40k, 148 s): 1 blocking, pre-stage outside the retry; 2 high, the rollback claim was false; 3 high, concurrency with the rehearsal worker; 4 high, `rehearse` used the tracking ref; 5 medium, tag-release worktree; 6 medium, verification bounds; 7 medium, measurement bars. All accepted.
- **lang:rust** r1 (approve-with-changes, 40k, 89 s): retry; one `skip_reason`; `AMONT_LAND` read once and removed; a sentinel constant; shim-log, not wall-clock; refuse skips `await_for`. All accepted.
- **tui** r1 (approve-with-changes, 37k, 66 s): `--json` stdout; the refusal as lines of at most 80 columns; retry (dup); slot-wait line. Accepted except the retry duplicate.
- **unix** r1 (approve-with-changes, 34k, 68 s): stdout (dup); SIGINT and snapshot failure; sentinel (dup); precedence. Accepted 2 and 4.
- **backend** r2 (approve, 47k, 30 s): two lows carried (the env outranks config; `rehearse --wait` and a running same-tree verdict).
- **codex** (approve-with-changes): 4 highs, all accepted: a same-tree rehearsal covers another gate set; non-pass outcomes need their own evidence; `pushStamps=false`; a remote oid absent locally.
- **backend** 1b delta (approve-with-changes → approve, 49k + 36k): 3 highs and 1 medium accepted (records only where the working tree is the tip; release key = tree; absent tool or timeout records CouldNotCheck; the ref stays local); carried: an audit timeout maps to CouldNotCheck, and cite audit.rs:594.
- **backend** final (approve-with-changes → approve, 36k + 40k): 2 highs accepted (Verification entries, an evidence store); 1 medium carried (a `LAND_EVIDENCE_MISSING` sentinel, and the refusal text when stamps are off); a low carried (bound and measure land's wait behind another tree's rehearsal).
