+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["amont"]
+++
# Tell a rejected attestation from a missing one

## Question

amont#271: `amont attest covered` is fail-open and prints nothing both when no attestation exists and when one fails to verify. How does it say which, without changing the default stdout contract (a flag, or a stderr line)?
