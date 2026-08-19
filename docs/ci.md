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
| `pre-commit-lint-js` | `npx --no-install eslint --max-warnings 0 .` |
| `pre-push-run-tests-js` | `npm run typecheck / test:unit / test --if-present` |
| `pre-push-audit-js` | `npm audit` |
| `pre-commit-ruff` / `pre-commit-pyright` | `ruff check .` / `pyright --warnings` |
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

## Skipping what pre-push already proved

"CI is the backstop" does not require CI to repeat work it can *verify*
happened. When `pre-push` has just run the suite against the pushed tree
and every block gate passed, re-running the identical suite on the
identical tree buys a second copy of the same answer — real money on a
resource-constrained runner fleet. So amont can leave a receipt:

```sh
ssh-keygen -t ed25519 -N "" -f ~/.ssh/amont-attest   # once, per machine
git config amont.attest true                          # per repository
```

With the toggle on, a push whose pre-push block gates all passed writes a
note on each pushed tip in `refs/notes/amont-attest` and pushes that ref
alongside the branch. The note is a four-line payload plus an SSH
signature over exactly those bytes:

```text
amont-attest-v1
tree <the tree the gates ran against>
gates pre-push-cargo-test pre-push-audit-rust
amont 1.8.0

-----BEGIN SSH SIGNATURE-----
…
-----END SSH SIGNATURE-----
```

This does **not** repeal "amont does not run in CI". CI still never
executes amont — the templates' `attest` step verifies the note with stock
`git` and `ssh-keygen`, then skips a test step only when *all three* hold:

1. the signature verifies against `.forgejo/allowed_signers` (or your
   platform's path), a file **committed in the repository**, whose entry is
   pinned to the `amont-attest` namespace:

   ```text
   you@example.com namespaces="amont-attest" ssh-ed25519 AAAA…
   ```

2. the attested **tree** is byte-for-byte the tree CI checked out — not
   the commit hash: a reword keeps its attestation, a single changed byte
   loses it, and a PR merge commit whose tree drifted from the tested tip
   never skips;
3. the step's own gate is **named** in `gates`. The list holds only checks
   that PASSED — `Warned` and `Unavailable` never appear, because "could
   not run" is not "passed" — so a CI step with no local mirror (an e2e
   suite, an image build) is never skippable by construction.

What signs is the machine that ran the tests, so the trust statement is
exactly "whoever holds `amont.attestKey` vouches for this tree" — the same
trust you extend by pushing at all when you are the only committer. On a
team, that key is a shared authority: hand one to each developer (one
`allowed_signers` line each) or accept that any holder can mint "tests
passed". And the failure doctrine is the gate stamps' own, one direction
only: no note, an unknown format version, a foreign or tampered note, a
mismatched tree, a signer CI never heard of — every one of them reads as
"no attestation", and no attestation means CI **runs the tests**. The
mechanism can only ever save a redundant run; it cannot skip a check that
did not happen. `--no-verify` skips pre-push entirely, mints nothing, and
CI quietly does the full job — which is the backstop doing exactly what it
is for.
