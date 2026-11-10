# Full reviews: pairs cover their push twin

Round 1 (sha 9e89cfde): architect a-w-c (26k, 21 s); po a-w-c (27k, 27 s); unix a-w-c (26k, 28 s); tui a-w-c (28k, 37 s); backend a-w-c, 1 blocking (39k, 63 s); lang:rust **rework**, 1 blocking (57k, 100 s).
- Blocking (backend 1, rust 1): count missing stamps with the widened scope too. Accepted.
- The pnpm flags (architect 1, po 1, unix 1, backend 2, rust 6): accepted as rust's allow-list plus unix's single-simple-command rule. The other seats' deny-lists were rejected.
- Rust 2–5 and 7: accepted (`scope()` returns the trigger; severity; commit-time coverage only through a recursive declared command; std-only parsers; a reason for `hook.skip`).
- Architect 2–5: accepted (js_workspace.rs, second pass, ADR-0019 note, Phase 3 separable).
- PO 2–5: accepted (7-day measurement, release after Phases 1–2, decision-log line, a `list` test).
- Tui 1–5: accepted (80-column lines, a commit-time line, a summary line, `list` width, Verification wording).
- Backend 3–6: accepted (commit cost, `$AMONT_FILES`, the filtered fixture, a picture line).
- Unix 3 and 4: accepted.
- Unix 2 (stderr): rejected for these lines alone, because the sibling pair lines use `say!`. Escalated in round 2 as a decision.

Round 2 (sha f37c6c7a): architect approve (33k, 11 s); rust a-w-c, 1 high (65k, 19 s); backend a-w-c (52k, 26 s); po a-w-c (37k, 21 s); unix a-w-c (41k, 22 s); tui a-w-c (45k, 31 s).
- Rust 8 (high, applied): `resolve_triggers(&settings)`, because `scope()` has no settings.
- Unix 2 and 6: 👉 decisions.
- Carried into implementation:
  - the decision log's "3.1.1" means 3.2.0 (architect, rust 9, backend 1, po 1, unix 5, tui 7);
  - add a 3.2.0 release check for Verifications 4–5 (backend 1);
  - the rollback budget is the suite time from Verification 6 plus 20%, not "2x" (backend 2);
  - Verification 4 reads the summary line (backend 3, po 3);
  - pnpm membership comes from `pnpm-workspace.yaml` only (po 2);
  - announce every remaining run, even one (tui 6);
  - the code review keeps no po seat, since the plan has no product surface beyond the CLI (backend 4);
  - the commit-time line goes unpictured (backend 5).

Delta 1 (sha 1a67e9a0): backend a-w-c, high (58k, 25 s); architect a-w-c, high (35k, 29 s); rust a-w-c (67k, 24 s); po a-w-c (35k, 7 s); tui a-w-c (42k, 8 s); unix a-w-c (40k, 27 s).
- All six flagged the same gap: an unnamed entrypoint that misses the resolve.
- Merged and applied: `pending_trigger`, narrow until resolved; the named load sites; one severity function; per-entrypoint tests (warn to block, block to warn, a missed call).
- Carried into implementation:
  - say why `tree_lint.rs` and `main.rs:1017` build no check;
  - a `list` row says why it is not widened (tui 2).

Delta 2 (sha b55b8a89): approve from all 6: architect (38k, 5 s), po (37k, 6 s), rust (69k, 13 s), unix (44k, 7 s), backend (61k, 20 s), tui (45k, 18 s).
- Carried into implementation:
  - a grep or test that counts `manifest::load` sites against resolves;
  - the scope-gap line's second line follows its reason: the files hint, or "unresolved trigger: a greengate bug" (tui).

Delta 3 (your change, sha 7fa7466e): backend approve (73k, 38 s); rust a-w-c (79k, 26 s). The other four seats stay current on the body, per `plan-panel` (panel=delta).
- Carried into implementation:
  - A local `git config amont.rootRunsMembers` may only remove names or set `none`, never add one, like `executor` (manifest.rs:520); a test pins it (rust 1).
  - A withheld (untrusted) line makes the hint name trust, not "declare it"; the hint's "not set" test reads the parsed line (rust 2, backend 3).
  - The strict parse has its own `ParseError` variant: names in the gate vocabulary, and `none` not mixed with names, with unit tests (rust 3).
  - A greengate older than 3.2.0 warns on the new line at every run: note it in the CHANGELOG and docs/checks.md, and name 3.2.0 as the minimum in the hint (backend 1).
  - A root whose `S` was gated at commit does not run at push, so its members run again; a safe repeat, to be stated in Behaviour (backend 2).
  - The summary line shows the root script's command text, so a stale declaration is visible (backend 3).
