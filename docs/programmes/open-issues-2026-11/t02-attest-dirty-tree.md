+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = ["t01-attest-dirty-tree-count"]
repos = ["amont"]
+++
# Attest only the tree a gate ran on

## Question

amont#289: how should `attestable`/`attest_push` apply the stamp's own rule (`tree_is_tip || ran_on_tip`), so an uncommitted change is never vouched for as the pushed tree? Check first with the session on fix/tip-tree-verdict, which works on the same evidence.

## Resolution

Fixed in amont#424, released in v2.1.0. A new `TipWitness` (dispatch.rs)
says which pushed tips a gate's run saw: the working tree was the tip at the
start (HEAD, and clean or a rehearsal snapshot), or the gate ran in a
checkout of it. The push stamps, the attestation (`attest_push_per_tip`), the
audit records and the completion records all ask it. The dirty-tree check is
now read whatever `amont.pushStamps` says. Test: `attest_dirty_tree.rs`.
