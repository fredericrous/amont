<!--
Moved here from attest's SPEC.md (decisions ADR-0037): the provenance record
is amont's, signed by amont from amont-agent's handoff (ADR-0036), and attest
reads no part of it. The text is unchanged but for links to attest's SPEC and the sentence
that said where it was specified.
-->
# The provenance record: `amont-provenance-v1`

A second, separate note (decisions ADR-0036): a signed record of what a push
carried besides its tests — the plan it implements and the verdicts given
on it. It is **not** an attestation. CI never reads it, and attest's verifier
does not either. amont produces it with the same key and the same publishing
rules as an attestation, from the payload amont-agent writes. Each line has one owner: amont-agent records
what it witnessed (the plan, the review, the preview), and the signer states
the tree facts it checks or computes itself (`tree`, `canon`).

```
amont-provenance-v1
commit 9f1c2ab0d6e4c1b2a3f405e6d7c8b9a0f1e2d3c4
tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904
canon 1d2c3b4a5f6e7d8c9b0a1f2e3d4c5b6a7f8e9d0c
patch 7e6d5c4b3a2f1e0d9c8b7a6f5e4d3c2b1a0f9e8d
plan docs/plans/2026-10-15-provenance-recorded-at-the-push.md 031240153fa4ecee3f2f1f7e1ab3eae5403dd1990dbc1b104b8c6569069944cd
review reviewed 5be1b4e3a1d0c7f2e9a8b6d4c3f1e0a9b8c7d6e5f4a3b2c1d0e9f8a7b6c5d4e3
preview not-required
producer amont-agent 2.43.0

-----BEGIN SSH SIGNATURE-----
…
-----END SSH SIGNATURE-----
```

Payload and signature are separated exactly as in an attestation block (attest [SPEC.md, "The note"](https://github.com/fredericrous/attest/blob/main/SPEC.md#the-note)) (the
payload's trailing newline is signed), fields are read **by prefix**, the
first occurrence wins, and an unknown line is ignored.

| line | meaning |
|---|---|
| `amont-provenance-v1` | format token, always first; any other value means absent |
| `commit <oid>` | the pushed commit |
| `tree <oid>` | its full tree |
| `canon <oid>` | canon(T) of `tree` (attest [SPEC.md, "Canonical tree"](https://github.com/fredericrous/attest/blob/main/SPEC.md#canonical-tree)), whatever `planExempt` says; written by the signer, never by amont-agent (see "The handoff") |
| `patch <id>` | optional: `git diff --binary <merge-base(origin/main, commit)> <commit> -- . ':!docs/plans'` piped to `git patch-id --verbatim`; omitted when it cannot be computed |
| `plan <path> <body-sha>` | the plan file in `tree` and its body-sha (`amont-agent plan-sha`) |
| `review <how> <content-id>` | `reviewed`, `by-patch` (the hook's `reviewed-by-patch`) or `overruled`, and the content id (see "Content id") of the tree the review read |
| `preview <outcome>` | `approved`, `evidence` or `not-required` |
| `producer <name> <version>` | who wrote the payload, informational |

`commit`, `tree`, `canon`, `plan` and `review` are required in a signed
record.

**Where it lives.** `refs/notes/amont-provenance`, one or more blocks per
object, keyed by the **tree** and by the **commit**, as attestations are, and
published by the same append-and-push loop (attest [SPEC.md, "Producing"](https://github.com/fredericrous/attest/blob/main/SPEC.md#producing)). The signature's
ssh-keygen namespace is `amont-provenance`, never `amont-attest`: a
provenance block can never be replayed as an attestation, and an
`allowed_signers` entry accepts it only once it lists that namespace
(`namespaces="amont-attest,amont-provenance"`).

**The handoff.** amont-agent writes the unsigned payload to
`<config dir>/amont-agent/provenance/<repo>/<commit>`, where `<config dir>`
is `$CLAUDE_CONFIG_DIR`, else `$HOME/.claude`, else `%USERPROFILE%\.claude`
(amont-agent's `settings::config_dir`), and `<repo>` is spelled as in amont-agent's
`implementation-review/by-tree/<repo>/`: the name of the directory holding
the repository's common git directory. Only amont-agent's hook writes there.
The payload it hands over has **no** `canon` line. amont's pre-push reads
that one path — never a path a git config key could redirect — and:

1. refuses a payload whose `tree` is not the pushed commit's tree;
2. refuses a payload that already carries a `canon` line (a pre-release
   amont-agent wrote it), names the file to delete, and neither signs nor
   removes it;
3. computes canon(T) of the pushed tree in the repository's own object format
   and inserts `canon <canon(T)>` right after the `tree` line. `planExempt`
   does not apply: it governs skipping a gate, not a tree's identity. If there
   is no canon(T), it signs nothing and keeps the payload for the next push;
4. signs and publishes the result, and deletes the file once published.

**What the signature says, and no more.** This machine's amont-agent recorded
`plan`, `review` and `preview`, amont checked `tree` against the pushed commit
and computed `canon` from it. A reader checks `plan` itself, against the
plan file in `tree`, and can recompute `canon` and the `review` content id
from `tree`. Nothing checks the verdicts `review` and `preview` state: only
amont-agent's guard on its store protects them, and that guard is
best-effort against a shell command that writes the file.
Anyone holding the key can sign any payload, as for attestations.

**Reading it.** A record whose `commit` is not reachable from origin is
ignored: amont publishes before the push delivers, so a push that failed
can leave a note behind. A record is found on the pull request's head
commit, else on the merge commit's tree, else by its `patch` line. To match by
`patch`, a reader hashes the merge commit's own change the same way: the
diff from its first parent, `docs/plans` excluded, through `git patch-id
--verbatim`.

## Content id

The value an implementation review and a preview approval bind to (decisions
ADR-0034), and the one `amont-agent tree-sha` prints. It is **not** canon(T),
on purpose: canon(T) counts whether a plan file exists, and the content id
does not, so adding the plan a branch implements leaves its review standing.

**Definition.** Take the raw records of `git ls-tree -r -z --full-tree T`, in
git's order, each `<mode> <type> <oid>TAB<path>`, unquoted and undecoded. Leave
out every record whose raw path starts with `docs/plans/` (whatever its mode:
a symlink or a gitlink there is left out too; a nested `x/docs/plans/` and a
`docs/plans-…` are kept). Follow each kept record by one NUL, the last one
included, and take the lowercase hex sha256 of the bytes: always 64 hex
digits, in a sha1 or a sha256 repository. No records left hashes the empty
string.

**Known answers**, where `O1` is forty `1`s, `O2` forty `2`s and `⇥` a TAB
(amont-agent's tests pin each one):

| records | content id |
|---|---|
| none | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| `100644 blob O1⇥README.md` | `6f0214fffd79cb8eb5afb2283e46911f5f9022a25b3f339ae8a50a1ee223a0b2` |
| `100644 blob O1⇥README.md`, `100644 blob O2⇥docs/plans/a.md`, `100644 blob O2⇥docs/plans-old/a.md`, `120000 blob O1⇥docs/plans/link`, `100644 blob O1⇥x/docs/plans/a.md` | `0fca850e419d8a202129087fd90f00fe285c4b9cc63b42f4db6162e0a2b12eaf`, the same as without the two `docs/plans/` records |
| `100644 blob O1⇥docs/a` LF `b.md`, `100644 blob O2⇥docs/plans/c` LF `d.md` (raw newlines in the paths) | `88a9c5d4a7b3892be98a8492631b3c436954c2000a038b5d641bd054e76e19e0` |
