# A warn gate waits for a slot where no push is waiting: reviews

## Full reviews (reference)

Round 1, all opus-5-5, all approve-with-changes:
- unix (33k, 48 s): a comma-safe, exit-checked `hook.skip` removal; the test gate leaves a marker.
- tui (36k, 52 s): no false "git opened its connection" note in a snapshot (high); the `announce_deferred` advice for warn gates; the `waited` line format and its assertion; a shorter holder.
- lang:rust (42k, 62 s): marker file; an upper time bound; the `Current::cannot_block` doc; an absolute path for the unset.
- backend (45k, 84 s): state the land cost; marker file; observable verification plus a 7-day `evidence-missing` count; the safe unset.

Round 2, backend (55k, 53 s): round-1 findings 1–4 resolved. Applied (high): the live check runs in a fresh clone, since the live checkout still has `hook.skip`. Carried into implementation:
- the heartbeat is already correct at `live.rs:161-164`, so step 2's heartbeat bullet becomes only a pinning test on `beat_line`;
- `announce_deferred` needs each name's severity, keeping `set onBusy wait` advice when a `block` gate is in the list.

Delta, backend (58k, 95 s): finding 5 resolved. Carried:
- run the branch binary's `amont trust` in the clone (step 0);
- the list header becomes "in a scratch clone", and "until step 5" becomes "until Approach step 5".

Codex (gpt-6-astra), approve-with-changes. Carried (medium, both):
- **The `run now` advice.** `amont run <gate>` with captured output takes `run_quiet`, which still applies the warn zero-wait override (`dispatch.rs:1083,1121`). Advise `AMONT_ON_BUSY=wait amont run --raw <gate>` (the registry path, `registry.rs:935`), and test that exact command under a held slot.
- **An end-to-end test before the release.** In `busy_defers.rs`, run the test binary's real `amont land` against a local bare remote with a held slot. Cover a passing warn gate and a warn gate that runs and exits nonzero. Assert that land succeeds, the remote tip moves, the gate runs exactly once, and no `evidence-missing` appears. The passing case leaves a stamp; the failing case leaves completion evidence without a passing stamp (`dispatch.rs:1840-1846,1896-1908`).
