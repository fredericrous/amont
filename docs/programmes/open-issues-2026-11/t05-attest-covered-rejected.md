+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["amont"]
+++
# Tell a rejected attestation from a missing one

## Question

amont#271: `amont attest covered` is fail-open and prints nothing both when no attestation exists and when one fails to verify. How does it say which, without changing the default stdout contract (a flag, or a stderr line)?

## Resolution

Fixed in amont#419, released in v2.1.0. `amont attest covered` names each
attestation whose signature did not verify on stderr, and `--strict` exits 3
when no gate is covered and one was rejected. Without `--strict` it still
exits 0 (fail open).
