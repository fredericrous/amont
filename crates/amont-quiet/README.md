# amont-quiet

The quiet-run active entry: while a check whose output is kept out of an
agent's context runs, one small JSON file says so, so a pane can tail its log.
amont and amont-agent write it, relais reads it.

This crate is the one implementation of the contract in amont's
[`docs/quiet-runs.md`](https://github.com/fredericrous/amont/blob/main/docs/quiet-runs.md).
It has no dependencies.
