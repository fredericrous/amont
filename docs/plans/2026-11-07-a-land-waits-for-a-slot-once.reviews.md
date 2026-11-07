# A land waits for a slot once, and not for ever: reviews

## Full reviews (reference)

Round 1, all opus-5-5, all approve-with-changes:
- tui (30k, 30 s): the ceiling's note names `amont.warnSlotWait`; the holder is renamed between gates and its age reset; a live `amont slots` check.
- unix (38k, 49 s): release on every exit path (high); `rename` via temp file and rename; pid reuse in the 7-day grouping; key precedence stated.
- lang:rust (48k, 73 s): a `static` is never dropped, so a scope guard (high); the handoff sets `held`, `settled` and `jobs`; a discriminating handoff test; the flag forces `Wait`.
- backend (49k, 106 s): the same guard (high); the scope of `in_snapshot()` (high, to the person, who chose handoff everywhere and the ceiling in land only); the 600 s arithmetic; `Wait` forced; `Current` state; test bounds; pid grouping; a numbered block test; the idle slot through light checks.

Round 2, backend (59k, 41 s): all round-1 findings resolved. Applied (high): a fixed `prestage;` format, parsed into `SnapshotEnv`. Carried:
- park only while a `ParkScope` is active, since `enter_check` also runs at `registry.rs:935` and `run_stamp.rs:186`.

Delta, backend (65k, 85 s): approve. Carried (low):
- version skew: an old binary reads `prestage;detached=…` as no detached gates (slower, not unsafe);
- `snapshot_env_value` reads `rehearsal::prestage()` and marks only a valid line.

Codex (gpt-6-astra), approve-with-changes. Applied (high): while a lease is parked, a light check's children inherit the slot marker and its width. Carried (medium):
- `record_wait_named` takes explicit rehearsal and pre-stage context from the runtime, since the std-only crate cannot read the consumed variable;
- a land that follows an already-running background rehearsal gets no ceiling: document it and test it.

Delta, backend (71k, 39 s): Codex's high resolved in substance. Carried:
- (medium) the mechanism: park `(Lease, jobs)`, `before_spawn` returns true for a light check under a parked lease, `bound()` falls back to the parked width, and light tools run width-bound meanwhile;
- (low) move the nested test into Verification: one slot, heavy gate → light `amont slot run` writing its width → heavy gate. Expected: no new `waits.jsonl` line, the width matches, the last gate is `handed`, and the run takes under 2 s once the slot is free.
