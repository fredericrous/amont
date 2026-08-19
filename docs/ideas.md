# Ideas, not a roadmap

Nothing here is planned, promised, or assigned. Ideas are kept with their
objections attached, because a rejected-for-now idea with its reason is more
useful than a list that quietly loses the ones somebody already thought about.

## Already landed

- **Lint more languages.** Python (`ruff`, `pyright`), Rust (`cargo fmt`,
  `clippy`, `cargo test`), YAML (`yamllint`) and Kubernetes (`kubeconform`,
  `kube-linter`, Argo) all have checks now. See [the checks](checks.md).
- **A repository declaring its own checks**, which was the general answer to
  most of the "add a hook for X" requests: `amont.conf` means a check that
  belongs to one repository does not need to be in everybody's binary. See
  [custom checks](custom-checks.md).

## Still open

- **`post-commit`: tag automatically when the package version is bumped.**
  Noted at the time as possibly harmful depending on your CD workflow, and that
  objection has if anything got stronger: a tag that triggers a publish should
  not be created by a hook nobody invoked deliberately.
- **`pre-commit`: check the other lockfiles** — `Gemfile.lock`, `Pipfile.lock`,
  `Cargo.lock`, `composer.lock` — the way `pre-commit-package-lock` already
  does for npm.
- **`pre-commit`: check you are committing with the usual GPG key.** Same
  shape as `pre-commit-usual-name`. Flagged as possibly slow, which is the
  thing to measure before building it: this runs on every commit.
- **`pre-push`: require a JIRA id in the branch name.** `prepare-commit-msg`
  already extracts one when it is there; requiring it is a different, more
  opinionated thing, and probably belongs in a repository's own
  `amont.conf` rather than in the built-in `branch-pattern`.
- **`pre-push`: prevent a force-push to a remote branch with a different
  name.** The original note asks "almost impossible?" and does not answer it.
- **`commit-msg`: a message alias** — `"."` expanding to
  `"(previous prefix): more on <previous subject>"`.
- **`commit-msg`: require a description of more than three words and a body of
  more than five.** Word counts are a poor proxy for a meaningful message and
  are easy to satisfy meaninglessly; the current rules constrain *shape*
  (a type, a description, lengths) rather than trying to measure effort.

## Out of scope

Functionality already covered by `.gitattributes` and `.gitignore` should not
be reimplemented as a hook.

## Before proposing one

Ask whether it belongs in the binary at all. A check that is right for your
repository — a house lint, a schema check, a smoke test — can be declared in
`amont.conf` today, shared with your team by committing it, and skipped or
downgraded by the same `hook.skip` and `amont.severity` keys as any built-in —
or by committed `severity`/`skip` policy lines in the same file.
A built-in earns its place by being right for *most* repositories, and by being
inert in the rest.

Everything in the commit path also has to be paid for in dependencies, which is
to say: in nothing. See
[CONTRIBUTING.md](https://github.com/fredericrous/amont/blob/main/CONTRIBUTING.md).
