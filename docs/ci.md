# The CI backstop

Hooks are advisory by construction: `--no-verify` and `hook.skip` are each
one command away, deliberately, and a teammate whose machine never enrolled
runs nothing at all. So the question "what actually stops a bad commit
reaching the default branch" has one honest answer, and it is not a hook —
it is CI, where nothing can be skipped from a laptop.

## amont does not run in CI — on purpose

The obvious move would be an `amont run` step, or a marketplace action.
Deliberately not. amont's job is *local* ergonomics: one binary
orchestrating many tools with scoped selection, index fidelity, live
progress, and escape hatches a human sitting at the keyboard is entitled
to. CI wants none of that — it wants the **real tools, called directly**:

- failures attribute to the tool that found them, in the tool's own words,
  with the platform's log folding and annotations;
- each tool gets first-class caching, matrices, and versions pinned by the
  workflow, not by whatever binary a runner happens to have;
- there is no second opinion to keep in sync: when `cargo clippy` disagrees
  between laptop and CI, that is a toolchain version question, not an
  amont question.

The checks that exist *only* inside amont — `ban-terms`, the secrets scan,
`large-files`, `merge-conflict`, the commit-message and branch-name
conventions, `pull-rebase` — are deliberately not reproduced in CI. They
are local ergonomics (catch it before it exists) or they have a better
server-side answer (GitHub's own push protection and 100 MB limit,
platform merge tooling). Losing them in CI loses nothing the tools below
don't already guard: a `debugger;` that slips past the local hook still
has to survive the test suite and review.

## The templates

Copy the file for your stack into `.github/workflows/` (GitHub) or
`.forgejo/workflows/` (Forgejo), then prune the steps your repository does
not use. Each step is annotated with the amont check it mirrors, so the
local and CI stories stay legible against each other.

| stack | GitHub Actions | Forgejo Actions |
|---|---|---|
| Rust | [`templates/ci/github/rust.yaml`](https://github.com/fredericrous/amont/blob/main/templates/ci/github/rust.yaml) | [`templates/ci/forgejo/rust.yaml`](https://github.com/fredericrous/amont/blob/main/templates/ci/forgejo/rust.yaml) |
| JS/TS | [`templates/ci/github/js.yaml`](https://github.com/fredericrous/amont/blob/main/templates/ci/github/js.yaml) | [`templates/ci/forgejo/js.yaml`](https://github.com/fredericrous/amont/blob/main/templates/ci/forgejo/js.yaml) |
| Python | [`templates/ci/github/python.yaml`](https://github.com/fredericrous/amont/blob/main/templates/ci/github/python.yaml) | [`templates/ci/forgejo/python.yaml`](https://github.com/fredericrous/amont/blob/main/templates/ci/forgejo/python.yaml) |
| Go | [`templates/ci/github/go.yaml`](https://github.com/fredericrous/amont/blob/main/templates/ci/github/go.yaml) | [`templates/ci/forgejo/go.yaml`](https://github.com/fredericrous/amont/blob/main/templates/ci/forgejo/go.yaml) |

Or fetch one directly:

```console
$ curl -fsSL https://raw.githubusercontent.com/fredericrous/amont/main/templates/ci/github/rust.yaml \
    -o .github/workflows/checks.yaml
```

## What maps where

| amont check (local) | CI step |
|---|---|
| `pre-commit-cargo-fmt` | `cargo fmt --all -- --check` |
| `pre-commit-clippy` | `cargo clippy --workspace --all-targets --all-features -- -D warnings` |
| `pre-push-cargo-test` | `cargo test --workspace --all-features` |
| `pre-push-audit-rust` | `cargo audit` |
| `pre-commit-lint-js` | `npx --no-install eslint .` |
| `pre-push-run-tests-js` | `npm run typecheck / test:unit / test --if-present` |
| `pre-push-audit-js` | `npm audit` |
| `pre-commit-ruff` / `pre-commit-pyright` | `ruff check .` / `pyright` |
| `pre-push-pytest` | `pytest` |
| `pre-push-audit-python` | `pip-audit -r requirements.txt` |
| `pre-commit-gofmt` / `pre-commit-go-vet` | `test -z "$(gofmt -l .)"` / `go vet ./...` |
| `pre-push-go-test` | `go test ./...` |
| `pre-push-audit-go` | `govulncheck ./...` |
| `ban-terms`, `secrets`, `large-files`, `merge-conflict`, commit/branch conventions, `pull-rebase` | **deliberately local-only** — see above |

One shape carries over exactly: the audits. Locally they warn on a branch
push and block a `v*` tag push; the templates express the same split
natively with `continue-on-error: ${{ !startsWith(github.ref,
'refs/tags/v') }}` — advisory red on branches, a hard failure when a
release is leaving the building. That mirrors what this repository's own
release workflow enforces for itself.

## Keeping the two in step

The hook is the fast feedback; CI is the same verdict, slower and
unskippable. When they disagree, it is almost always a tool version —
pin the versions your workflow installs, and consider a
[`tool` pin](custom-checks.md#pinning-tool-versions) in `amont.conf` so the
laptop warns when it drifts from what CI runs.
