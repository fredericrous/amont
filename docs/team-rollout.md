# Rolling out to a team

Hooks only protect the machines that installed them. One person with amont
has one protected machine; a team has sixty, plus new hires, plus the
laptop that got reimaged on Tuesday. This page is the whole story of
closing that gap, and it is honest about the one thing git will not do.

## The constraint nobody gets to skip

Git deliberately runs nothing on `git clone`. A repository cannot install
its own hooks into your machine, and any tool that made it look otherwise
would be a supply-chain attack with good ergonomics. So "the repo declares
it, the clone self-installs" needs a *machine-side* grant somewhere —
the only question is what shape the grant takes and how often somebody
has to type it.

Three shapes exist, and they compose:

| shape | typed how often | covers |
|---|---|---|
| `"prepare": "amont init"` in `package.json` | never (npm types it) | JS repositories, on `npm install` |
| `amont enroll` | **once per machine** | every future `git clone` and `git init` |
| `amont init` | once per repository per machine | that one clone |

## The machine grant: `amont enroll`

```console
$ brew install fredericrous/tap/amont   # or the curl installer, or cargo
$ amont enroll --conventions declared
```

`enroll` does three things `amont install` has always done one repository
at a time — puts the binary somewhere stable, populates the template
directory, and (the part `install` deliberately left to the user) points
`init.templateDir` at it. From then on every `git clone` and every
`git init` on that machine arrives with the shims already in
`.git/hooks`, resolving whatever amont binary the machine has. Upgrading
the binary upgrades every repository at once; nothing is re-run per
clone.

It refuses to overwrite an `init.templateDir` that already points
somewhere else — something else installs hooks on that machine, and
silently disabling it is the husky failure one level up. And it is
idempotent: re-running it is a no-op that says so.

Two lines in the onboarding doc — install the binary, `amont enroll` —
replace a per-clone ritual forever. Repositories cloned *before* the
grant are the one thing it does not reach: `amont init` wires one,
`amont-fleet install --root ~/work` wires all of them.

## The repository declaration: `amont.conf`, and `--conventions declared`

The objection to a standing grant is real: the same machine clones the
team's services *and* upstream open-source projects, and those did not
agree to your commit-subject shape, your branch naming, or your
auto-rebase. A grant that imposes house rules on somebody else's
repository is a grant people revoke.

`--conventions declared` (or `git config --global amont.conventions
declared` by hand) splits the checks in two:

- **The safety net runs everywhere**: merge-conflict markers, leaked
  secrets, oversized files, and debug leftovers (`debugger`, `dbg!(…)`,
  `breakpoint()`) in the diff you are committing. These are mistakes in
  any codebase, with near-zero false positives, and catching them in an
  upstream clone is a favour to the upstream.
- **The conventions wait for a declaration**: commit-message shape,
  branch patterns, lint and format gates, test suites, audits,
  auto-rebase. They run only in a repository that has **committed an
  `amont.conf`** — even an empty one:

```console
$ echo "# this repository subscribes to amont" > amont.conf
$ git add amont.conf && git commit -m "chore: declare amont"
```

Presence is the declaration; presence executes nothing, so it needs no
trust decision. What the file *says* — declared checks, tool pins — stays
[trust-gated](trust.md) exactly as before. A held-back stage says so in
one line (`N convention check(s) held back … the safety net still runs`)
rather than silently doing less, and `amont list` reports the state in
text and in `--json` (`"conventions_apply"`).

The default is `everywhere`: nothing changes for anyone until a machine
opts into `declared`.

## The team recipe

1. **Each machine, once** (onboarding doc, two lines):
   ```console
   $ brew install fredericrous/tap/amont     # pick your installer
   $ amont enroll --conventions declared
   ```
2. **Each repository, once ever** (committed, travels with the clone):
   - commit an `amont.conf` — empty declares; [custom checks](custom-checks.md),
     [committed policy](custom-checks.md#repo-policy--severity-and-skip-lines)
     (`severity`/`skip` lines for the built-ins) and `tool` pins can come
     later;
   - JS repositories additionally get `"prepare": "amont init"` so even an
     unenrolled machine is covered by `npm install`.
3. **Repositories cloned before enrollment**: `amont init` in one,
   `amont-fleet install --root <dir>` for all of them.

New hire day one: install, enroll, clone — protected. No per-clone step,
no per-repo step, nothing to forget.

## What this does not solve

- **Hooks remain advisory.** `--no-verify` still works, deliberately, and
  is [counted](checks.md#the-stamp-contract) rather than prevented. The
  guarantee lives in CI, not on laptops; put the same checks there and
  the hook becomes the fast feedback, not the enforcement — [the CI
  backstop](ci.md) ships copyable workflow templates for exactly that.
- **Version skew.** Enrolled machines resolve whatever binary they have;
  two teammates on different amont versions run different check sets,
  silently, until a shim is newer than a binary (which warns). Pin the
  version in your installer of choice if this matters to you.
- **Machines that never enrolled.** The fleet dashboard sees one
  machine's checkouts. A teammate who skipped onboarding is invisible —
  which is one more reason the real backstop belongs in CI.
