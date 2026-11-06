# Reviews: A failed quick check skips the heavy ones

- **lang:rust** (opus, 32k, 19 s): approve-with-changes. high: what wave 2 reads after a Light fixer (accepted). medium: exhaustive Outcome matches, closing live slots, thread death counts as Failed (accepted). low: rendezvous test (accepted), `Warned` (rejected, it exists at check.rs:290).
- **tui** (opus, 26k, 25 s): approve-with-changes. high: roll-up shape and sign, quiet mode and order (accepted). medium: the silent wait shown as `waiting for quick checks`, the off switch named in docs, the word `skipped` (accepted). low: never also deferred (accepted).
- **backend** (opus):
  - round 1 (37k, 44 s): approve-with-changes. high: the pre-commit ledger for the measure (accepted). medium: `amont run` shares the waves (accepted). low: the live-slot test (duplicate).
  - round 2 (42k, 23 s): approve-with-changes. The missed `amont run` test and a hint per check (accepted).
  - final (41k, 46 s): approve.
- **unix** (opus, 32k, 43 s): approve-with-changes. medium: setting precedence with `AMONT_FAIL_FAST` (accepted). low: a run-now hint and `ui::skipped_sign()` (accepted).
- Tokens: about 251k, four seats over three backend rounds. That is over the t10 budget of 150k, which is not built yet.
- Tool note: `amont-agent plan-panel` printed `panel=delta` with backend only, because this plan reuses the earlier plan's file path. The full panel was run by hand; worth fixing in amont-agent.

