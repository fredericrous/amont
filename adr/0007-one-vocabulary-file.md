---
id: ADR-0007
status: accepted
decisions:
  - key: vocab.commit-branch-parity
    choice: One file, plus a test that fails unless each name is shared or declared an exception with a reason
    first: true
    reason: They had already drifted — eight of twelve
  - key: lang.ban-terms-tokenizer
    choice: Code-aware, with a depth stack for template-literal substitutions
    first: true
    reason: Blanking a substitution as string content hides the code inside it
---
# 0007 — one vocabulary, and a scanner that reads code as code

Detail is in
[`docs/commit-convention.md`](../docs/commit-convention.md) and the migration
record.

## The vocabularies had drifted

Commit types and branch prefixes are two lists that ought to agree, and eight
of twelve names did not. They now live in one file, with a test that fails
unless a name appears in both or is declared an exception **with a reason
written down**. The reason matters: an exception without one is
indistinguishable from an oversight, which is how the drift happened.

## The scanner reads template literals as code

A banned term inside `${...}` is code, not string content. Blanking the whole
literal as a string would hide it. The tokenizer carries a depth stack so it
can tell which it is looking at.
