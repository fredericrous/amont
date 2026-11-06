+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["amont"]
+++
# Close the 2.0 follow-ups

## Question

amont#414: a heavy `*`-scoped declared gate still runs on the open connection; the connection advice prints inside a rehearsal snapshot; the review lows (one shared gate lookup, a Kind::Unusable test case, the unstampedPush docs, a Windows refuse-path test). Which change closes each?

## Resolution

Fixed in amont#422, released in v2.1.0.
- A declared `heavy` gate is owed before the connection whatever its scope
  (`dispatch::stamped_push_gate`).
- The connection advice is silent inside a snapshot.
- `gate_identity` uses one `declared()` lookup, and its test gains an
  `Unusable` case.
- The `unstampedPush` docs are rewrapped.
- `push_gates_portable.rs` tests the refusal on Windows.
