+++
type = "task"
mode = "afk"
status = "open"
blocked_by = ["t01-attest-dirty-tree-count"]
repos = ["amont"]
+++
# Attest only the tree a gate ran on

## Question

amont#289: how should `attestable`/`attest_push` apply the stamp's own rule (`tree_is_tip || ran_on_tip`), so an uncommitted change is never vouched for as the pushed tree? Check first with the session on fix/tip-tree-verdict, which works on the same evidence.
