# Full reviews: amont + attest 1.4.0 parity

**Round 1** (plan body d7cb530, amont-only draft):
- backend — approve-with-changes (47k, 60 s): contract not on disk (live attest checkout was 1.3.1); silent failures; compare-and-delete; SIGKILL stale lock; no timeout/no-prompt test; Verification as "tests pass"; missing numbers; fingerprint count vs required paths.
- language:rust — approve-with-changes (44k, 72 s): pin the v1.4.0 reference; compare-and-delete; pure env function tested both ways; no-origin test is a guard; grandchildren outlive a kill; lone `?` check parity.
- tui — approve-with-changes (36k, 36 s): say why on an unreachable origin; delete/print only when a local ref existed; copyable undo lines; skip ls-remote after a timed-out fetch.
- unix — approve-with-changes (34k, 43 s): fetch into a throwaway ref (stale lock); GIT_TERMINAL_PROMPT does not stop askpass/GCM; compare-and-delete; stderr on timeout; state the worst case.

**Round 2** (backend, three binds): throwaway ref moved outside refs/notes/ with a sweep; reflog reason for the two-week window; lost-CAS and stale-lock semantics; askpass marker test with a negative control; user's own ssh may prompt (Known). Blocker at bind 2: a failed swap must be judged only when the ref equals the fetched oid, and the sweep only for dead PIDs — fixed. Final bind (body ccb097b): **approve**; low items carried (LC_ALL=C for kill, test naming).
