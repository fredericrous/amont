+++
type = "research"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["amont"]
+++
# How many pushes signed a tree their gates did not run on

## Question

amont#289: with `amont.testPushedTree` off, push gates run in the working tree, but `attest_push` signs the tip commit's tree without checking the gate ran on it. The issue asks to count the pushes affected since 2026-08-20 (`gate_evidence`) before fixing, because dropping those signatures removes test skips CI relies on. How many, in which repositories, and does the fix need a migration or only a note?

## Resolution

Explore research, 2026-11-06. **No count is possible, and none is needed.**
Run evidence (`run <epoch> <gate> <outcome> <ms>`, `gate_stamp.rs:257-274`),
attest notes and land receipts carry no clean/dirty flag, so which past pushes
signed a dirty tree cannot be recovered. The fix only stops future dirty-tree
attestations (CI then runs those tests); old notes stay. A CHANGELOG line, no
migration.

- Where: `pre_push` (`dispatch.rs:1759-1771`) passes `attestable(...)` gates to
  `attest::attest_push` (`attest.rs:128`), which signs each ref's tip tree with
  no tree check. `stamp_tips` (`dispatch.rs:1902-1990`) already applies
  `is_head && (in_snapshot || worktree_clean)` or `pushed_tree::ran_on_tip`.
- Fix shape: extract that per-tip rule into one helper used by both, take the
  clean/dirty reading independently of `amont.pushStamps` (`tree_at_start` is
  empty when stamps are off), and give `attest_push` per-ref gates.
- Exposure under 2.0 is small: land's pre-stage runs in a clean snapshot. Still
  exposed: a bare push under `amont.pushGates inline`, and a push of a ref that
  is not HEAD.
- fix/tip-tree-verdict (another session) changes `pair_verdict` only; no
  overlap with the attest tree choice, at most a textual conflict in
  `dispatch.rs`.
