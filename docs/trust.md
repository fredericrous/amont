# The trust model

**A repository you clone cannot run code on your machine until you say it may.**

That is the whole claim. This page is how it is enforced and where it stops.

## The problem it solves

A repository can declare checks of its own in a committed
[`amont.conf`](custom-checks.md), and those declarations are shell
commands. Committing the file is the point — it is how a team shares a check.

The consequence is that cloning a repository and committing to it would
otherwise run commands *that repository chose*, and neither of those acts is
one anybody performs as a decision about trust. Reviewing a diff before running
it is such a decision. Nothing asked for that.

This matters most for people who set `init.templateDir`
([see install](install.md)), because for them the hooks are already present in
every repository they clone — including one cloned only to read.

## What happens instead

A manifest is **inert until trusted**. Its declared checks are listed by
`amont list` with the reason they will not fire:

```
declared in an untrusted amont.conf — review it, then `amont trust`
```

```sh
amont trust          # show what this repo declares, and accept it
amont trust --show   # what is trusted here
amont trust --revoke # forget it
```

`amont trust` prints the declarations before asking. That is not politeness:
"trust this file" is not a question anybody can answer without seeing it, and a
prompt that does not show the file is a prompt that trains people to press `y`.

`amont trust` outside a repository is refused rather than falling back to
`.`. Trust is recorded per repository, keyed by the root that resolves — so a
`.` fallback would let `amont trust` in `~` read `~/amont.conf`, show
its declarations, and record trust against a repository that does not exist, in
a state no later `--revoke` would find.

## Consent is bound to content, not to a path

The record is a **fingerprint of the file**, stored in `--local` git config
under `amont.trusted` — local, never committed, so a repository cannot
declare itself trusted.

Because it is keyed on content, a `git pull` that adds a command does not
inherit the consent given to the file before it. That state is reported
distinctly from "never trusted", because *somebody changed it* is a different
thing to tell a reader than *you have not looked at this yet*:

```
amont.conf changed since it was trusted — review it, then `amont trust`
```

## Why `git hash-object --no-filters`

`amont` links no external crates ([and CI enforces
that](https://github.com/fredericrous/amont/blob/main/scripts/check-no-deps.sh)),
and the only hash in `std` is `DefaultHasher` — SipHash with a fixed key, not
collision-resistant, so a crafted manifest could be made to match a trusted
one's fingerprint. Hand-writing SHA-256 is a hundred lines nobody would review
as carefully as they should.

`git` is already a hard dependency of every path in this binary, and
`git hash-object` is the identity git itself uses for content. It is SHA-1 (or
SHA-256 in a repository configured for it) — not a guarantee against a
determined attacker with a chosen-prefix collision, but enormously better than
SipHash, it costs no dependency, and **you can reproduce it by hand** to check
what you trusted:

```sh
git hash-object --no-filters amont.conf
git config --local --get amont.trusted
```

`--no-filters` is the load-bearing flag. Without it, git applies the clean
filter and eol conversion that the repository's *own committed*
`.gitattributes` asks for — so the repository would be choosing the transform
its consent is taken through, and two manifests the parser reads differently
could be given the same id. Consent is bound to the bytes that are **parsed**.

For the same reason, where a caller already holds the file's bytes, the state
is decided about *those* bytes rather than by re-opening the path. Two reads of
a file somebody is deciding about can disagree, and the decision would then be
recorded about bytes nobody was shown.

## Two windows that were closed, and how

**Between showing and answering.** `amont install` prints the manifest, then
blocks on a keypress — sometimes for several seconds — before recording
anything. Re-hashing at that point would trust whatever is on disk *then*,
which is not necessarily what was shown. So callers fingerprint what they show
*before* asking, and pass that same value back to be verified again once the
answer is in. If the file changed in the window, nothing is trusted and the
prompt says so.

**Concealing a declaration inside the listing.** Every field in that listing is
repository-controlled text, and it is the text somebody is about to say yes to.
It is sanitised **before** the column padding is computed: a terminal escape
sequence is zero columns wide, so it would silently shift the alignment even if
it did nothing worse — and a repository that can move the rendering can hide a
line from the person consenting to it. A repository must not be able to pick
how its own consent is rendered any more than it can pick how it is hashed.

## A vendored pack is consented to like anything else

`amont add` ([custom checks](custom-checks.md#shipping-a-check--packs)) copies
declarations from somebody else's repository into your `amont.conf`. It grants
no trust, and it gets no exemption:

- the append changes the file's content, so the fingerprint no longer matches
  and every declared check goes inert — including ones you had already trusted;
- the pack's rows are shown by `amont trust` alongside your own, in the same
  listing, with no marking that would invite skimming past them;
- editing a vendored row by hand revokes consent exactly as editing a
  hand-written one does.

The commit id recorded in the block says *where the text came from*. It is
provenance, not authority: it tells you the bytes are the ones that repository
published, and says nothing about whether the commands are a good idea. Reading
them is still the gate, and it is still yours.

## What this does not protect against

Stated plainly, because a security boundary described only by what it stops is
a marketing claim:

- **It is not a sandbox.** Once you trust a manifest, its commands run with
  your privileges. Trust is a review gate, not containment.
- **It says nothing about the built-in checks.** Those are code in the binary
  you installed, and are governed by
  [`hook.skip` and severity](opting-out.md), not by trust.
- **It does not protect a repository you wrote the manifest in.** Your own
  `amont.conf` is trusted by you, once.
- **SHA-1 is the floor**, in a repository using git's default object format.
  See above for why that trade was taken.

To report something this model gets wrong, see
[SECURITY.md](https://github.com/fredericrous/amont/blob/main/SECURITY.md).
