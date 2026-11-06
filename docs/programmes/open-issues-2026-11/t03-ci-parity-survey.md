+++
type = "research"
mode = "afk"
status = "open"
blocked_by = []
repos = ["amont"]
+++
# Which CI test steps run more than their local built-in gate

## Question

amont#288: ADR-0024's attested-skip exception holds while nothing keeps `pre-push-run-tests-js`, `pre-push-pytest`, `pre-push-go-test` and `pre-push-cargo-test` identical to the CI step they skip. Survey the fleet's CI test steps against the local gates (coverage floors, markers, `-race`; trade-agents' `--cov-fail-under` is known) and say what `amont tree-parity` must compare.
