+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["amont"]
+++
# A transitive prettier does not opt a repository in

## Question

amont#307: `hooks/prettier.rs` treats any `node_modules/.bin/prettier` as an opt-in, so a hoisted transitive prettier turns the gate on while `amont list` says inert. What signals opt in (a config marker, a direct dependency, a `\"prettier\"` key), so the gate and `amont list` agree?
