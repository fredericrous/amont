+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["amont"]
+++
# Close the 1.70.0 index-hint follow-ups

## Question

amont#411: the low review notes on the commit-index hints (typed lookup errors with git's stderr, a one-newline trim, a failed lookup and unmarked gates, the AnyPlatform lookup, `index_env`'s signature, the unreachable Unusable arm, the tests), plus `land --json` printing a raw `push_url`. Which change closes each?

## Resolution

Fixed in amont#421, released in v2.1.0.
- A failed index lookup now refuses every pre-commit gate.
- The error is typed (`IndexLookupError`) and keeps git's stderr.
- Only one trailing newline is stripped.
- `index_env` takes references, and `runs_on` comes from `run`.
- `land --json` redacts credentials in `push_url`.
- The tests assert both variables and give the gate a unique name.
- Not done: skipping the lookup for `platform=any` gates. `execute` runs only
  when the cluster declines the gate, and then it runs locally, where the
  index is readable.
