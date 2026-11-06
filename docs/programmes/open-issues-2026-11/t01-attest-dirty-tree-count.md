+++
type = "research"
mode = "afk"
status = "open"
blocked_by = []
repos = ["amont"]
+++
# How many pushes signed a tree their gates did not run on

## Question

amont#289: with `amont.testPushedTree` off, push gates run in the working tree, but `attest_push` signs the tip commit's tree without checking the gate ran on it. The issue asks to count the pushes affected since 2026-08-20 (`gate_evidence`) before fixing, because dropping those signatures removes test skips CI relies on. How many, in which repositories, and does the fix need a migration or only a note?
