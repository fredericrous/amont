---
canonical: decisions:docs/plans/2026-10-15-gates-keep-unchanged-verdicts.md
phases: [4]
status: done
---
# Gates keep unchanged verdicts (pointer)

The plan lives in the decisions repository, which owns its ADR-0034. This
repository carries phases [4].

## Verification (phase 4)

- Cause found, a fourth one the plan did not list: `pre-push-cargo-test` declared `.rs` while `rust_tools::test` runs for every `RUST_PATHS` file. Rehearsal picks gates by the declaration, the push runner by `RUST_PATHS`, and `stamp_tips` stamps by the declaration. The fix keeps the gate selection as it is: each push suite now declares what it consumes (`RUST_PATHS`, `GO_PATHS`), and `a_push_suite_declares_every_path_it_consumes` keeps it so. `pytest` consumes only what it declares.
- Evidence: relais 0.22.0's release head `0cdb3f25` has commit-time tokens and `run … pre-push-cargo-test pass 557260` but no push token. Live again on 2026-10-15: amont-agent's 2.42.0 release push (Cargo.toml only) ran `pre-push-cargo-test` and did not stamp it (installed amont 1.55.0).
- Red loop. Input: a tiny crate in `/tmp/rehearse-red`, branch from `origin/main`, one commit bumping `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md`, installed amont 1.55.0. Expected after the fix: the rehearsal runs the suite and the push skips it. Actual before: `nothing to rehearse: no test gate has work to do for what 02ba86f3 would push`, then the push ran `pre-push-cargo-test` (3043 ms) with no stamp.
- `a_manifest_only_commit_is_rehearsed_and_the_push_skips_cargo_test`: with the old scope, fails with "nothing to rehearse…"; with the fix, passes (the HEAD note carries `pre-push-cargo-test`, the push says it passed on this exact tree).
- Still to do: on the next real release branch built with the released amont, `amont rehearse --wait` runs the gate and the push skips it (`stamped`); recorded in the canonical plan's closing PR.
- Left as is: `pre-commit-clippy` declares `.rs` while consuming `RUST_PATHS` (a `holds-until:` comment names it).

## Implementation review

- approve after a delta. Fixed: the plan records the fourth cause and the fix that keeps gate selection (the scope declaration plus its guard test); the verification is recorded; the clippy scope gap has a `holds-until:` comment. Checked: `pytest` has no drift, and over-matching by `ends_with` only runs more. 37k + 40k tokens, 73 s + 16 s.
