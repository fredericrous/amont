+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["amont-agent"]
+++
# The push guard reads amont land --tag

## Question

amont-agent#204: the implementation-review push guard reads only `amont land [--json] [--force]`, so `amont land --tag <name>`, the only way to push a `v*` tag that owes audits on amont 2.0, is held. How does the guard read it, and judge it as a tag push needing no review?

## Resolution

Fixed in amont-agent#208, released and installed as amont-agent v3.19.0.
The land guard reads `amont land --tag <name>` (`--tag=`, one tag, no
`--force`; a `$` or backtick name stays unread), asks
`amont land --tag <name> --dry-run`, and accepts a `refs/tags/` answer. The
v3.19.0 tag itself went through a bare `git push` with inline gates, because
the installed 3.18 guard could not read `land --tag` yet.
