+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["amont-agent"]
+++
# Group three functions' arguments into structs

## Question

amont-agent#149: `arith_for`, `iterate` (interp.rs) and the test helper `ask` carry `too_many_arguments`. Which structs remove every such attribute with no behaviour change?

## Resolution

Fixed in amont-agent#210 (no behaviour change, no release). `arith_for`
takes an `ArithLoop` and `iterate` a `Counted`. The test helper `ask` became
`ask_pre` and `ask_post` over an `Asked` struct. No `too_many_arguments`
attribute is left outside the lint-suppression fixtures.
