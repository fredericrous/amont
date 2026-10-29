# amont-slots

The host slots: one machine-wide queue that every heavy job on a host (amont's
git-hook gates, relais's verification commands, anything run through
`amont slot run`) takes its turn in. A slot is a `flock` on a file under one
per-user directory. Std-only, no dependencies.

The contract is [`docs/host-slots.md`](https://github.com/fredericrous/amont/blob/main/docs/host-slots.md).
