+++
type = "research"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["amont"]
+++
# Which CI test steps run more than their local built-in gate

## Question

amont#288: ADR-0024's attested-skip exception holds while nothing keeps `pre-push-run-tests-js`, `pre-push-pytest`, `pre-push-go-test` and `pre-push-cargo-test` identical to the CI step they skip. Survey the fleet's CI test steps against the local gates (coverage floors, markers, `-race`; trade-agents' `--cov-fail-under` is known) and say what `amont tree-parity` must compare.

## Resolution

Explore research, 2026-11-06. **Every CI test step skipped on an
attestation runs a different command from its local built-in gate**, and
`amont tree-parity` never inspects them: it reads only steps that name a
`tree-*` gate (`tree_parity.rs:259` `referenced`, `:315` `check_texts`).

- Local gates: `npm run <typecheck|test:unit|test>` (`run_tests.rs:27,494`),
  `uv run --no-sync pytest` (`python_tools.rs:57-79,297`), `go test ./...`
  (`go_tools.rs:308`), `cargo test --workspace --all-features`
  (`rust_tools.rs:511`).
- Drift that changes the verdict: trade-agents' `--cov-fail-under=80` and
  dev-group sync; the five Go operators' `-race`, coverage and envtest
  (`pr.yml`), their `make test-unit` package subsets and `|| true`;
  amont-agent's `--locked`; duro-lexical-multi's `pnpm coverage`;
  cluster-vision's `test:run`; duro-app's sharded vitest; homelab's per-service
  `uv run --no-project …` and `GOTOOLCHAIN` wrapper.
- Harmless differences: `-q`, `pnpm run` for `npm run`, `uv run` sync flags
  where the project config carries the flags, `--workspace` on a
  single-package repo.

What tree-parity must compare (t04): recognise the four built-in gate names,
compare each skipped step's single-line `run:` against the gate's argv with
a stated set of equivalences, and reuse the existing fail-closed rules
(`env:`, `working-directory:`, `${{ }}`, block scalars). A CI-only check
(coverage floor, `-race`, envtest) must never sit behind the attestation
skip.
