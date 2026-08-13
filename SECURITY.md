# Security policy

This project installs a program that runs on every `git commit` and `git push`,
with your credentials, reading every staged file, while nobody is watching. It
also has write access to your working tree. Both facts are the reason this
document is longer than a contact address.

## Reporting a vulnerability

**Use GitHub's private vulnerability reporting:**
<https://github.com/fredericrous/amont/security/advisories/new>

That opens a private advisory visible only to the maintainer. Please do not
open a public issue, a discussion, or a pull request that demonstrates the
problem, until a fix is available.

Useful in a report, in rough order of usefulness:

- what an attacker gets, stated as an outcome — "a cloned repository executes
  code before the user trusts it", not "the parser is unsafe";
- a **reproduction**: the smallest repository, config or sequence of commands
  that shows it. Every finding in the v1.0.0 review landed with one, and it is
  what makes a fix verifiable rather than plausible;
- the version (`amont --help` names the binary; `git log -1` if you built
  from source), OS, and git version.

Expect an acknowledgement within a week. If you have not heard anything in two
weeks, please ping the advisory — this is a single-maintainer project, not a
security team, and that is worth knowing before you rely on a timeline.

There is no bounty programme. Credit in the advisory and the release notes, if
you want it.

## Supported versions

The latest release. There is no long-term support branch; fixes ship in a new
version.

## Threat model

Two guarantees this project actually makes. Everything else in it is a
convenience.

### 1. A repository you clone cannot run code without `amont trust`

Cloning a repository, opening it, and committing to it are not acts anybody
performs *as a decision about trust*. So a repository's own declared checks —
`amont.conf`, which is committed, because that is how a team shares a check
— are **inert until explicitly trusted**.

The consent is bound to the file's **content**, fingerprinted with
`git hash-object --no-filters`, recorded in `--local` git config so a
repository cannot declare itself trusted. A `git pull` that adds a command does
not inherit the consent given to the file before it, and that state is reported
distinctly from "never trusted".

`--no-filters` is load-bearing: plain `hash-object` applies the clean filter
and eol conversion the repository's *own committed* `.gitattributes` asks for,
which would let the repository choose the transform its consent is taken
through. The listing shown at the prompt is sanitised **before** column padding
is computed, because a zero-width escape sequence can shift alignment and hide
a declaration from the person consenting to it.

Full reasoning: [docs/trust.md](docs/trust.md).

**In scope:** anything that runs a repository-controlled command, or influences
what the trust prompt displays, without a matching `amont trust`. Anything
that lets a repository's contents (file names, config values, manifest fields,
branch names, commit messages) escape into a shell, a path, or a terminal
control sequence.

### 2. The hooks must never destroy uncommitted work

A hook that loses work you had not committed has done something git itself
will not do for you, and no amount of linting is worth it.

This is why the run-mode machinery is the most carefully argued part of the
codebase ([docs/index-fidelity-and-run-modes.md](docs/index-fidelity-and-run-modes.md)),
why the release profile deliberately omits `panic = "abort"` — so the `Drop`
that restores unstaged work still runs when a check panics — and why that
choice is asserted against `Cargo.toml` in a test, since cargo ignores the
setting for test targets and no behavioural test could catch the regression.

**In scope:** any sequence that loses staged or unstaged content, leaves a
repository in a half-applied state, writes through a symlink onto a tracked
file, or writes outside the worktree.

### Also in scope

- **The installer** (`install/install.sh`) accepting an archive that does not
  match the published `SHA256SUMS`, or being made to install to an unintended
  path.
- **Shim resolution** picking up a binary from an attacker-controlled location
  — a relative path, a writable directory, an inherited environment variable.
- **`amont uninstall` or `amont-fleet` deleting a file it did not write.**
  The promise is that only our own shims are removed; a way to make either
  remove somebody else's hook, or a tracked file, is a vulnerability.
- Anything in the commit path that gains an **external crate dependency**
  without the argument being made. See
  [CONTRIBUTING.md](CONTRIBUTING.md).

### Out of scope

Not because they do not matter, but so a report is not wasted:

- **Trust is a review gate, not a sandbox.** Once you run `amont trust`, the
  declared commands run with your privileges. That is the intended behaviour.
- **Checks you configured.** If `amont.conf` in your own repository runs
  something dangerous, that is your manifest.
- **The tools the checks invoke.** A vulnerability in `ruff`, `prettier`,
  `eslint` or `kubeconform` belongs upstream. How this project *invokes* them
  is in scope.
- **`--no-verify`.** Bypassing hooks is git's feature and a deliberate escape
  hatch here. Bypasses of a commit-time gate are tallied in a local file that
  is never pushed and never transmitted — see
  [docs/opting-out.md](docs/opting-out.md) for what is recorded and how to
  turn it off or erase it.
- **SHA-1 collision resistance as an absolute.** The fingerprint uses git's own
  content id, and [docs/trust.md](docs/trust.md) states the trade explicitly: a
  chosen-prefix collision is not defended against, and the alternatives were a
  non-collision-resistant `std` hash or a hand-written SHA-256 in a
  dependency-free binary. A *practical* attack against this use of it is very
  much in scope; the abstract observation is already documented.
- Anything requiring an attacker who already has code execution as your user.

## What v1.0.0 already went through

v1.0.0 shipped after a full security and correctness review, and each finding
landed with a committed reproduction. Naming them, because a security document
that only says "we take security seriously" tells you nothing:

- a **drive-by RCE** via a relative path in the hook shim;
- a **held-store format** that let a repository delete a tracked file and plant
  a symlink outside the worktree;
- a **trust prompt a repository could conceal declarations from**;
- guards that **wrote through symlinks onto tracked source**;
- a commit **subject shaped like a trailer destroying commit trailers**;
- `pre-commit-pyright` **blocking every commit it ran on**.

The dependency guard was also changed to fail closed in the same pass: it
previously discarded stderr and ended in `|| true`, so any failure to answer
the question read as "no external dependencies".

## Verifying what you installed

```sh
# what the installer checks for you
curl -fsSLO https://github.com/fredericrous/amont/releases/latest/download/SHA256SUMS
sha256sum amont-<version>-<target>.tar.gz

# what you trusted in a given repository
git hash-object --no-filters amont.conf
git config --local --get amont.trusted

# what is actually installed here
amont list
amont trust --show
```
