+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["amont"]
+++
# A transitive prettier does not opt a repository in

## Question

amont#307: `hooks/prettier.rs` treats any `node_modules/.bin/prettier` as an opt-in, so a hoisted transitive prettier turns the gate on while `amont list` says inert. What signals opt in (a config marker, a direct dependency, a `\"prettier\"` key), so the gate and `amont list` agree?

## Resolution

Fixed in amont#420, released in v2.1.0. A `node_modules/.bin/prettier` alone
no longer opts a repository in. Only a prettier config file, or `prettier` in
the root package.json, does. `amont list` reads the same config names as the
check. A tool_tables row covers a hoisted binary.
