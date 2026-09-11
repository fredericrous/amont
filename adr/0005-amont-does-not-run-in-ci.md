---
id: ADR-0005
status: accepted
decisions:
  - key: ci.runs-amont
    choice: No, deliberately; CI calls the real tools directly
    first: true
    reason: A second opinion about the same code is a second thing to keep in step
  - key: ci.attestation-format
    choice: A signed note in refs/notes/amont-attest over tree, gates, platform and version
    first: true
  - key: ci.attestation-tool-home
    choice: A separate single-purpose repository
    first: true
    reason: So the rule above can stay written as it is
  - key: ci.template-distribution
    choice: Copied per stack, not published as a reusable action
    first: true
---
# 0005 — amont does not run in CI, on purpose

Detail is in [`docs/ci.md`](../docs/ci.md).

## The rule

CI runs the real tools — `cargo clippy`, `eslint`, `pytest` — directly. It does
not run amont.

The reasons are practical rather than principled. A build server wants
attribution in the tool's own words, first-class caching and matrices, and no
second opinion about the same code that has to be kept in agreement with the
first. amont's job is to catch a bad commit before it exists; CI's job is to be
the backstop, and the backstop should not be a wrapper.

Some checks are amont-only on purpose and are **not** reproduced in CI: banned
terms, the secrets scan, large files, merge-conflict markers, the commit and
branch conventions, pull-rebase. Each is either a local ergonomic or has a
better server-side answer.

## Attestation, and why the verifier lives elsewhere

The push gate leaves a signed note in `refs/notes/amont-attest` — format
version, tree, the gates that passed, platform, and tool version, signed with
an ed25519 SSH key over exactly those bytes. CI verifies it and skips what was
already proved. The verifier outputs an empty gate array on any doubt, so a
failure to verify costs a re-run rather than a false pass.

**The verifier is a separate repository.** It could have been an amont
subcommand, and then amont would run on the build server, and the rule above
would need an exception explaining itself. Keeping it separate is what lets the
rule stay a rule.

## Templates are copied

Per stack, not as a reusable action. A repository's workflow should be readable
in that repository without following an indirection to find out what it does.
