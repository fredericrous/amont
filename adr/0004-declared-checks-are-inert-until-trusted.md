---
id: ADR-0004
status: accepted
decisions:
  - key: config.repo-declaration
    choice: A committed `amont.conf`, five columns
    first: true
    reason: A reviewed line beats sixty identical local configurations
  - key: config.trust-model
    choice: Inert until trusted; consent is per machine
    first: true
    reason: Cloning a repository is not a decision to run its code
  - key: config.fingerprint
    choice: git hash-object over the file, so consent is bound to content
    first: true
    reason: An append changes the content, so consent is asked again
---
# 0004 — a declaration is inert until it is trusted

Detail is in [`docs/trust.md`](../docs/trust.md) and
[`docs/custom-checks.md`](../docs/custom-checks.md).

## The declaration

`amont.conf` at the repository root, committed, five columns: stage, name,
scope, severity, command. "clippy is warn-only here" becomes one reviewed line
in the repository rather than the same setting typed into sixty machines.

## Inert until trusted

**A repository you clone cannot run code on your machine until you say it
may.** Cloning is not consent. Committing is not consent. Running `amont trust`
is, and it is per machine.

## Consent is bound to content, not to a path

The fingerprint is `git hash-object --no-filters` over the file. Appending a
line changes the content, so the fingerprint no longer matches and the question
is asked again. Trusting a file once does not trust everything that file will
ever say — which is the property that makes it safe to trust at all.

A vendored pack is consented to the same way. There is no separate mechanism
for "checks that came from somewhere else", because there is no reason the
answer should differ.
