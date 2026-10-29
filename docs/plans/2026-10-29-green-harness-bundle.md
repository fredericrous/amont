---
canonical: github:fredericrous/decisions/docs/plans/2026-10-29-green-harness-bundle.md
phases: [2b]
status: done
branch: feat/grh-queries
---
# Green Harness bundle

Pointer. The plan and its review panel live in decisions, with ADR-0050
(harness.installer). This repository carries phase 2b: `amont uninstall --repo` and an installed-state object in `amont list --json`, the published queries grh reads to decide ownership.

## Verification

| Check | Input | Expected | Actual |
|---|---|---|---|
| uninstall --repo leaves the machine alone | install, then `uninstall --repo` | repo shims gone; template dir and `.gitconfig` bytes unchanged | as expected (`uninstall_repo_leaves_the_template_dir_and_global_config_untouched`) |
| no template dir created | template dir removed, then `uninstall --repo` | stays absent | as expected (`uninstall_repo_without_a_template_dir_creates_none`) |
| --repo with --binary | `uninstall --repo --binary` | exit 2, "mutually exclusive" | as expected |
| list --json installed state | before install / after / one shim gone / outside a repo | absent / present + baked binary / partial / unknown | as expected (`list_json_reports_the_installed_shims`) |
| unreadable hook | pre-commit chmod 000 | unknown | as expected (`an_unreadable_hook_reports_unknown`) |
| lint, host | `cargo clippy --all-targets -D warnings` | clean | clean (the gate deferred it on a busy host; run by hand) |
| ctrl-c fixture pinned (pre-existing flake on main, blocked the push) | `index_fidelity::ctrl_c_mid_run…` at load 69 | pass | passed in 7 s (failed 3/3 in the full gate before the pin) |
| Windows build | `cargo check -p amont-runtime --target x86_64-pc-windows-gnu` | builds | builds, no warnings |

## Implementation review

approve after two deltas (65k + 80k + 83k). Fixed: an unreadable hook now reads unknown (exhaustive match, unix-only arm gated), `--repo` documented with what it forgets, the gitconfig check compares the bytes install left, `InstalledState` enum.
