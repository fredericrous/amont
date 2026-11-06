+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["amont-agent"]
+++
# The push guard reads amont land --tag

## Question

amont-agent#204: the implementation-review push guard reads only `amont land [--json] [--force]`, so `amont land --tag <name>`, the only way to push a `v*` tag that owes audits on amont 2.0, is held. How does the guard read it, and judge it as a tag push needing no review?
