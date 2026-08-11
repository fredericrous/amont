# Commit and branch conventions

Two vocabularies, kept in one place —
`crates/amont-runtime/src/vocabulary.rs` — with a test that fails unless
every name is either shared between them or declared an exception *with a
reason*.

That test exists because they drifted: 8 of 12 commit types were rejected as
branch prefixes, so `docs/…` and `refactor/…` could not be pushed even though
`docs:` and `refactor:` were valid commit types. It cost two branch renames
before anyone looked.

## The commit subject

```
<type>[optional scope][optional !]: <description>
```

Enforced by `commit-msg`:

- a subject is present, and is at most **72** characters;
- it carries one of the types below, followed by a required colon and space;
- a description follows the prefix, and is at most **50** characters.

Messages git itself writes are passed through, not judged: `Merge …`,
`Revert "…"`, `Reapply "…"`, and the autosquash shapes `fixup!`, `squash!`
and `amend!` all carry no conventional type by design, and blocking them
would block `git merge`, `git revert` and `git commit --fixup` themselves.

An optional scope is a noun in parentheses naming a section of the codebase:
`fix(parser): …`. A `!` before the colon marks a breaking change.

Both numbers are defaults, not laws — see
[if the defaults do not fit](#if-the-defaults-do-not-fit) below.

## If the defaults do not fit

`commit-msg` is the one hook `hook.skip` and `amont.severity` do not reach —
`--no-verify` skips it for a single commit (git's rule, and an emergency
exit, not a dial). So it is the one hook whose opinions
have to be adjustable in themselves, and they are — four `git config` keys,
walked by `amont setup` and listed in
[configuration](configuration.md#amontcommit--what-a-commit-message-must-look-like):

```sh
amont setup                                   # ask me the four questions
git config amont.commit.descriptionMax 68     # or set one directly
```

**68 is the number worth knowing** if 50 feels tight. It is the longest
description that still fits a 72-column subject after a short type and a colon,
so it buys you eighteen characters without breaking the line-length convention
that the 72 comes from.

By default **nothing decorates your subject**: you write `feat: add a cart` and
that is what is stored. If you want the type's gitmoji, choose where it goes:

```sh
git config amont.commit.gitmoji suffix
```

| | stored as |
|---|---|
| `none` | `feat: add a cart` |
| `prefix` | `✨  feat: add a cart` |
| `suffix` | `feat: add a cart ✨` |
| `replace` | `✨  add a cart` |

Prefer `suffix` over `replace` unless you have decided otherwise on purpose:
it keeps a clean conventional subject at the start of the line, where
commitlint, changelog generators and `git log --grep '^feat'` look for it.
`replace` puts the emoji where the type word was, which is a real trade — an
emoji is not something conventional-commit tooling knows how to parse.

**Write the bare subject with no emoji of your own.** The limits measure what
you wrote, so a gitmoji this hook adds never counts against your budget — but
one you type yourself is yours, and does.

## The types

This table is derived from `COMMIT_TYPES` in the source, which is the
authority — what you read here is what the hook enforces and prepends.

| icon | type | for |
|---|---|---|
| 👷 | `build` | the CI or build system |
| 🔧 | `chore` | configuration, auxiliary tooling, generated docs |
| 📝️ | `docs` | documentation only |
| ✨ | `feat` | a new feature |
| 🐛 | `fix` | a bug fix |
| ⚡️ | `perf` | a performance improvement |
| ♻️ | `refactor` | neither fixes a bug nor adds a feature |
| ⏪️ | `revert` | reverting; ideally via `git revert` |
| 🎨 | `style` | structure or formatting of the code |
| 🚨 | `test` | adding, updating or fixing tests |
| ➕ | `add` | adding files as part of a larger feature |
| ➖ | `remove` | the opposite of `add` |

## The rest of the message

`commit-msg` also reformats what you wrote, rather than rejecting it for
whitespace:

- hard-wraps the body at 72 columns (`amont.commit.bodyWrap`, or `0` to
  leave a pasted stack trace or a fenced code block exactly as it is);
- ensures one blank line after the subject;
- groups the trailing footers, with one blank line before them.

Reformatting is idempotent: an amend, a rebase reword and a `--no-verify`
retry all hand the hook a message it wrote itself, and it gives the same one
back.

`prepare-commit-msg` appends an issue id found in the branch name — JIRA first
(`ABC-1234`), else a bare Kanbanize id (`1234`) — but only for a commit you are
authoring. `-m`, `-t`, a merge, a squash and `--amend` all pass a source in
`$2` and are left alone.

The footer scaffold lives in
[`message`](https://github.com/fredericrous/amont/blob/main/message):

```sh
git config --global commit.template ~/.config/git/git-templates/message
```

## Branch names

`pre-push-branch-pattern` requires `prefix/branch-name` — for example
`feat/3002-image-crop` — unless the branch already exists on the remote, in
which case renaming it is nobody's idea of an improvement.

Prefixes: `add`, `automation`, `build`, `chore`, `docs`, `feat`, `fix`,
`hotfix`, `perf`, `refactor`, `remove`, `revert`, `style`, `test`.

Only `chore/` allows dots, because they suit version-bump branches
(`chore/duro-1.50.50`) and would only be noise elsewhere. Git already rejects
the dangerous forms (`..`, a trailing `.lock`).

Two prefixes are deliberately not commit types, and the reason is recorded in
the source rather than in anyone's memory:

- **`hotfix`** — an urgency, not a kind of change. The commits inside it are
  still `fix:`.
- **`automation`** — bot-authored branches; their commits carry their own
  types.

The rejection message is *rendered from the same lists*, so what you are told
always matches what is enforced.

## Where these come from

- <https://git-scm.com/docs/git-commit>
- <https://www.conventionalcommits.org/en/v1.0.0/>
- <https://gitmoji.dev/>
