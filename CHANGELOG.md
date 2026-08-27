# Changelog

What an upgrader gets, in sentences. Each [GitHub
release](https://github.com/fredericrous/amont/releases) carries the
mechanical pull-request list too, generated; this file is the part a human
wrote, and the release workflow refuses to tag a version whose section is
missing here.

## Unreleased

### Fixed

- **An attestation is now findable by the tree it signs.** The note was keyed
  by COMMIT while the payload — and the signature — covered the tree, so the
  verifier had to hunt: `HEAD`, then `HEAD^2` for a pull request's merge
  commit. That hunt has a floor it cannot reach past. A squash-merge onto a
  `main` that has moved produces a commit carrying neither the note nor a
  parent that has one, while a valid attestation for that exact tree sits in
  the ref: signed for the content, findable only by the container.

  The note now goes on the tree as well, and `covered` looks there first.
  Demonstrated on a rewritten commit — the commit-keyed hunt finds nothing,
  the tree lookup returns the full gate list. The eight CI templates do the
  same, keeping `HEAD` and `HEAD^2` behind it for notes written by an older
  amont.

  Nothing is loosened, and the same two refusals still decide: the payload's
  tree must equal the checked-out tree, and the signature must verify against
  `allowed_signers`. A note attached to the wrong tree fails the first; a
  forged one fails the second.


- **A gate stamp now follows the content, so a squash-merge does not throw it
  away.** The stamp was written to the COMMIT, and the commit that reaches
  `main` is made by the forge — a squash-merge produces an object this
  machine never saw, carrying no note. So a `git push` of a tag on that
  commit re-ran every gate the branch had already proved. Measured here: the
  branch push took 13 seconds, and the tag push that followed ran the whole
  suite and died on a reset connection.

  The stamp is now written to the tree as well. Identical trees are identical
  content, which is the only thing a test suite reads — the argument `attest`
  already makes for signing the tree rather than the commit, and this
  module's marker has been tree-bound since it was written. This carries the
  binding through to the note. Across five squash-merges in one afternoon in
  this repository, the tree was identical every time.

  Both notes, not either: the commit note is what `git log --notes` shows,
  and dropping it would hide the stamps where people look for them.

  Nothing is loosened. A commit whose tree was never stamped still gets
  nothing, and every other failure — no note, an unparseable one, a git that
  will not answer — still means "no stamp", which runs the gate.

## v1.22.0

### Added

- **The GitHub CI templates carry the `attest` step.** All four forgejo
  templates have had it since attest shipped; none of the GitHub ones did, so
  most people could not adopt the feature even if they wanted to. The step is
  the same one, verbatim, with `.forgejo/allowed_signers` reading
  `.github/allowed_signers` — and the js template carries the long
  explanation the other three point at, as its forgejo twin does.

  Verified by running the step's actual shell against a real signed note:
  it fetched the notes ref, checked the version line, the platform and the
  tree, verified the signature with stock `ssh-keygen`, and emitted
  `covered=… pre-push-cargo-test` — which is what makes the template's
  `cargo test` step skip. With no `allowed_signers` it exits 0 with `covered`
  empty and every gated step runs, which is the only failure mode it has.

- **Every release now proves `brew install` works.** `publish-tap` verified
  that the formula parses and that its checksums match the release, and
  neither is the question a user asks. amont's own formula kept a
  `bin.install` line for a binary that had left the archive three releases
  earlier, so `brew install` failed outright the whole time while every
  release went green. A macOS job now installs from the tap with no
  checkout — seeing only what a stranger sees — asserts the version, and
  runs `brew test`, which is the only place the installed artifact is
  exercised at all.

### Fixed

- **An attestation now vouches for the gate worth skipping CI for.**
  `attestable` filtered on `Scope::matches`, which also asks whether the
  repository has *opted in* — whether a `Cargo.toml` exists. Asking that of a
  push DIFF can only answer no unless the push happened to touch the marker,
  so `pre-push-cargo-test` was vouched for by a push that edited `Cargo.toml`
  and never by one that edited only `.rs` files. Which is to say: never, on
  the ordinary Rust push, and precisely the gate a CI run most wants to skip.

  Measured on the same push, before and after:

  ```text
  1.21.0   attested … pull-rebase audit-rust
  fixed    attested … pull-rebase audit-rust pre-push-cargo-test
  ```

  Opt-in is a fact about the repository, settled when the dispatcher decided
  the check runs here at all. `Scope::touches` asks only about the change,
  which is what a caller holding a diff means. A push in another language
  still vouches for nothing — that guard is what `touches` keeps, and it is
  why this is not simply dropping the scope test.

  It remains a proxy, and the module says so: no gate reports whether it
  actually ran, so scope is the closest available stand-in. It is now a good
  one. Closing the last of the gap means an outcome that distinguishes "ran
  and passed" from "found nothing to do", which is a change to every gate and
  to `Outcome` itself.

## v1.21.0

### Added

- **A built-in push gate can be moved to commit time with one declaration.**
  Gate-pairing has always been documented, and until now it only worked for
  checks declared in `amont.conf` at *both* stages — the pairing looked
  exclusively at declared externals, and built-ins are never among them. So
  `pre-push-cargo-test`, `pre-push-pytest`, `pre-push-go-test` and
  `pre-push-run-tests-js` were structurally excluded from a feature this
  project ships and writes about. Adopting it for a Rust repository meant
  declaring `cargo test` at both stages *and* adding `hook.skip
  pre-push-cargo-test`, or running the suite twice.

  Now the commit-time half is the whole of it:

  ```text
  # stage       name        scope   severity  command
  pre-commit    cargo-test  *.rs    block     cargo test
  ```

  `pre-push-cargo-test` defers to the per-commit stamps that earns, saying
  `✓ cargo-test gated at commit instead`. The name must be the built-in's
  short one — `cargo-test`, not `pre-push-cargo-test`.

  Unchanged: a `--no-verify` commit carries no stamp and brings the gate
  back, out loud. Every failure in the stamp path still means "run the
  check", which is the only safe direction and the reason this was a small
  change rather than a risky one.

- **A push gate that runs long says what it is sitting on.** `git push`
  opens its connection to the remote, reads the remote refs — which is where
  `pre-push`'s own stdin comes from — and only then calls the hook. The
  connection is already open and goes idle for exactly as long as the gate
  runs, and a remote may close it first. git then reports `Connection reset
  by peer`, which reads as a network fault and says nothing about the wait
  that caused it.

  On the first heartbeat of a push gate — a check that has already run a full
  minute — amont now names that, once, and never at commit time where no
  connection is waiting.

  It deliberately does **not** recommend `ServerAliveInterval`. That setting
  was already in force on the machine where this was diagnosed and the remote
  reset the connection anyway; whatever it measures, it is not packets. The
  note says keepalive does not help and points at gate-pairing above, which
  does.

### Fixed

- **Per-file relevance in gate-pairing no longer asks the opt-in question.**
  It used `Scope::matches`, which also checks a scope's opt-in files — and
  asking "is there a `Cargo.toml`" of a single `.rs` path can only answer no.
  Harmless while pairing was declared-only, since `amont.conf` scopes carry
  no opt-in, and fatal the moment built-ins could pair: every changed file
  was judged irrelevant, so every push in every Rust repository would have
  reported "not paired" and silently run the suite anyway. Opt-in is a fact
  about the repository, settled when the dispatcher decided the check runs
  here at all.

### Note

Attestation is unchanged and now documented in a test: a gate with an opt-in
file is not vouched for by a push that did not touch that file, so a
`.rs`-only push does not attest `cargo-test` whether the gate ran or was
paired away. That predates this release and errs the safe way — CI re-runs a
suite rather than skipping one nobody proved on that tree.

## v1.20.1

### Fixed

- **The npm package could never run.** `amont@1.20.0` — and every version
  back to whenever `bin/native.js` was split out — shipped `bin/amont.js`
  without the `bin/native.js` it requires, so `npm i -D amont` installed
  cleanly and then died on first use:

  ```
  Error: Cannot find module './native.js'
  ```

  `package.json`'s `files` named `bin/native.js` the whole time. But `files`
  narrows what ships from a staged directory — it cannot conjure a file that
  was never put there, and `scripts/npm-pack.sh` copied only `amont.js`. npm
  does not warn about a `files` entry matching nothing, so a green release
  produced a package that could not start, four times over.

  Nothing else was affected: Homebrew, cargo, the shell installer and the
  release binaries never went through that wrapper. The npm route is the one
  that was broken, and it was broken completely rather than subtly.

### Added

- **Two guards, because the release was green every time this shipped.**
  `npm-pack.sh` now asserts that every file `package.json`'s `files` names is
  actually staged, and the release workflow packs the ROOT package and greps
  its tarball for both wrappers. It already did that for a platform package —
  checking only those is precisely what let the root ship broken.
  `tests/npm_packaging.rs` cross-checks the same thing statically.

## v1.20.0

### Changed

- **The six platform packages moved under the `@amont-hooks` npm scope.**
  `amont-darwin-arm64` and friends are now
  `@amont-hooks/darwin-arm64`, `@amont-hooks/linux-x64-gnu`, and so on.

  **`npm i -D amont` is unchanged** — the package people depend on keeps its
  bare name. Only the six build artifacts it resolves moved, and npm resolves
  them by `os`/`cpu`/`libc` exactly as before.

  The reason is a failure in the sibling project. `amont-agent`'s release
  tried to publish an unscoped `amont-agent-win32-x64` and npm refused it:
  *"Package name triggered spam detection"* — on the name alone, while
  `amont-agent-linux-x64-musl` beside it went through. Which unscoped name
  trips that heuristic is not something you can predict or appeal quickly, so
  every new target was a coin flip: adding aarch64-musl or aarch64-windows
  would roll it again. Names under a scope you own are not subject to it.

  It also stops six build artifacts squatting six unscoped top-level names,
  and makes plain that they are not packages to install directly. This is the
  same move esbuild made (`esbuild-linux-64` → `@esbuild/linux-x64`), and the
  shape rollup, swc and biome all ship.

- **The npm publish loop names its packages explicitly** instead of globbing
  `dist/npm/amont-*`. That glob now matches *nothing*, since scoped packages
  lay out under `dist/npm/@amont-hooks/` — it would have published zero
  platform packages and then published `amont` against six dependencies that
  do not exist, which npm's immutability makes a version bump to recover
  from. `tests/npm_packaging.rs` asserts the loop and `npm-pack.sh`'s table
  name the same six.

- The platform manifest now declares `publishConfig.access: public`. Scoped
  packages default to **restricted**; the workflow already passed
  `--access public`, and saying it in the manifest too means a hand-run
  `npm publish` from `dist/` cannot quietly publish six private packages the
  root then cannot resolve.

### Note for anyone who pinned a platform package directly

You should not have, and there was never a reason to — but if you did, the
old unscoped names stop receiving updates at 1.19.0 and are deprecated with a
pointer to the new ones.

### Fixed (recorded late)

These shipped in 1.20.0. Their entries sat under "Unreleased" through two
releases, so neither this release's notes nor 1.21.0's mentioned a change
both contained — the changelog said less than the tag did.

- **`pre-push-branch-protect` no longer refuses the push that creates a
  branch.** git sends 40 zeros as the remote oid when the branch does not
  exist on the remote yet; the check never read that field, so it treated
  the first push of a new repository — always a push to `main` — exactly
  like a direct push over shared history.

  That was the worst possible failure for this check in particular. The
  advice it prints, "Open a Pull Request", cannot be followed when there is
  no base branch to open one against, so the only way past was
  `git push --no-verify` — which switches off every other pre-push gate too,
  and having been taught once, gets reached for again. A guard that can only
  be satisfied by the blanket bypass has taught the bypass.

  Protection is unchanged from the second push onward, and a creation
  alongside a real update in the same push still fails.

- **`pre-commit-branch-protect` is quiet on a branch that has never been
  pushed.** Its whole content is "pushing it will be refused", which after
  the above is false in a repository whose first commit has not been pushed
  anywhere — and it would have sent somebody to `git switch -c` to escape a
  refusal that was not coming. It now speaks only when the branch actually
  exists on a remote.

## v1.19.0

### Removed

- **`amont-agent` has moved to its own project:**
  <https://github.com/fredericrous/amont-agent>. It is no longer bundled with
  amont — not in the release tarball, not in the `amont` npm package, and not
  in `install.sh`/`install.ps1`.

  **Your installed copy keeps working and is not removed by this upgrade.** To
  keep getting updates, install it from its own project:

  ```sh
  brew install fredericrous/tap/amont-agent
  # or
  curl -fsSL https://raw.githubusercontent.com/fredericrous/amont-agent/main/install/install.sh | sh
  ```

  Then `rm ~/.local/bin/amont-agent` is safe once the new one is on `PATH`, and
  `amont-agent doctor` will confirm which binary Claude Code is running.

  Nothing about the guard's behaviour changed in the move: same config keys
  (`amont.agent.*`, `$AMONT_AGENT_OFF`), same journal path, same decisions. Its
  first release from the new home is `v2.0.0` — the major bump marks that it
  must now be installed separately, and keeps the two version streams
  independent so `amont 1.19` and `amont-agent 2.0` never look like they must
  match.

  Why: it targets a different audience, needs none of this project's
  constraints — it is not on the commit path and takes dependencies happily —
  and nothing here ever depended on it. Bundling meant a stranger could only
  find it by first adopting a git-hook manager.

- `docs/agent-guard.md` moves with it, and is replaced by
  [a book of its own](https://github.com/fredericrous/amont-agent/tree/main/docs).

## v1.18.2 — 2026-08-25

- **`graduate` on an installed binary can see its evidence.** The reviewed
  corpus was looked up at the path of the machine that BUILT the binary — a
  CI runner, for every release — so every installed copy reported "0
  reviewed cases" and refused every promotion, while the evidence sat in the
  repository. Each rule's corpus is now compiled into the binary at the
  version it was reviewed for; a checkout's file still wins when present, so
  a line just appended counts. The refusal names a path that exists.

## v1.18.1 — 2026-08-25

- **A rule's question is asked in the directory the command moves to.**
  A third of real commands begin `cd /somewhere && …`, and `stale-base` and
  `bare-stash-pop` answered their "is this checkout behind / shared" question
  in the SESSION's directory instead — so a branch created in an up-to-date
  clone was advised as stale because the terminal happened to sit in a
  checkout that was. The last `cd` before the git command now decides where
  the question is asked; a `cd` nobody can resolve without running the shell
  (`cd $(mktemp -d)`, `cd -`) falls back to the session, never a guess.


- **A stale `AGENTS.md` is named before it is believed.** The generated
  block is what an agent reads at the start of a session and follows for the
  rest of it, and a release that changes the block — 1.17.0 changed what it
  says about timeouts — left every repository's copy quietly wrong until
  `amont-fleet` happened to run. Now `amont-agent` says at session start that
  the block is behind, and the thirty-fourth check, `pre-commit-agents-md`,
  says it again at the commit — warn-only, and under `amont.fix true` it
  regenerates and re-stages the file instead. Both are silent for a
  repository that never opted in, and the commit check stays out of merges,
  rebases and cherry-picks.

- **Two findings reach the model as two paragraphs.** `amont-agent`'s
  emitter escaped every control byte, newline included, so a command that
  tripped two rules — or a session notice with two things to say — arrived
  as one paragraph with a literal `\x0a\x0a` in the middle. The newline is
  now the one control character the text may carry; everything else is still
  escaped.

## v1.17.0 — 2026-08-25

- **A check is judged stuck by its silence, not by a wall clock.** One
  ten-minute `amont.timeout` had to answer two questions — "is this tool
  hung?" and "is this suite slow?" — and could only be right about one of
  them: a laptop under load ran the push gate's `cargo test` past it, twice
  in one evening, and the kill message arrived after the wait. Now a command
  that writes nothing for `amont.idleTimeout` (default 120s) is killed as
  stuck — faster than before for a real hang — and the wall clock becomes a
  ceiling, `amont.timeout`, that can afford its new default of an hour. The
  kill message says which clock fired and what that means: silent → look at
  the tool; still printing → raise the ceiling.

  The wait is no longer blind, either. On a terminal the live region shows
  `· quiet 45s/2m` once a check has been silent for half a minute and
  `· 50m/1h` once it nears the ceiling. Piped — an agent, CI — one plain line
  a minute per running check says it is alive, when it last printed, and,
  the first time, both budgets. The generated AGENTS.md now states this
  repository's actual budgets and tells an agent whose tooling caps a
  foreground command to run it in the background rather than pick a number.

- **A commit landing on `main` is told, at commit time, that its push will
  be refused.** `pre-push-branch-protect` fires at the first moment a push to
  `main` exists — right for `git push feature:main`, late for the other way
  it happens: a checkout left on `main`, commits stacked there, the refusal
  after the work is done, and `--no-verify` as the answer. The thirty-third
  check, `pre-commit-branch-protect`, is the same contract said at the
  commit, when the fix is one `git switch -c` and the commit comes along. A
  warning, never a block; quiet on a detached head and in a remoteless
  repository; `hook.skip branch-protect` silences both voices.

- **A session that opens in a stale checkout is told so.** Nothing fails when
  you build on code the remote has moved past; the feature just turns out to
  exist already, a day later. The `SessionStart` entry `amont-agent` already
  installs now refreshes `origin/main` (one branch, killed at 5s, skipped when
  `FETCH_HEAD` is under ten minutes old) and, when `HEAD` is behind, states
  the count and the newest commit it is missing. It never pulls: moving
  `refs/remotes/origin/*` is safe in every worktree at once, moving `HEAD` is
  not.

  A sixth rule, `stale-base`, catches the moment that gap is about to be
  inherited — `git worktree add`, `git checkout -b`, `git switch -c` from
  `HEAD` or a local branch that is behind. It ships advising, refuses
  nothing, and stays silent for the remedy (`… -b feat/x origin/main`).
  `git config amont.agent.fetch false` keeps the guard off the network;
  `checkout.defaultRemote` picks which remote counts.

## v1.16.0 — 2026-08-21


- **A guard that reads a shell command before Claude Code runs it.** `amont`
  gates commit and push; it cannot see a defect that lives in the command
  string and never reaches a git hook. `git push … | tail -5` reports the
  pipe's exit status, so a rejected or killed push reads as success and the
  error text is discarded with it. `amont-agent` is a Claude Code hook that
  refuses that command and says why, and it ships in the release archives, the
  npm packages and both installers alongside `amont` and `amont-fleet`.

  Install it with `amont-agent install --write`, check on it with
  `amont-agent doctor`, and turn any rule off with
  `git config --global amont.agent.<rule>.stance observe`. Five rules ship;
  only `pipe-to-tail` blocks, and only because seven weeks of measurement
  showed it was the one habit not correcting itself. The other four observe.

  Nothing here is asserted: `amont-agent backtest` replays your own transcripts
  and reports firings per 1,000 tool calls per week, and `amont-agent corpus
  check` replays reviewed judgements so a rule that regresses is a red build
  rather than a metric nobody is watching. See
  [The agent guard](https://fredericrous.github.io/amont/agent-guard.html).

- **`bare-stash-pop` sees the round-trip, and stops firing on its own remedy.**
  Asked whether the rule could move from `observe` to `deny` and tested it
  first: it missed the very shape it exists to catch, because a bare `git
  stash` earlier on the line abandoned the whole scan — so `git stash; …; git
  stash pop` went unseen. It also fired on `git stash pop stash@{2}`, the fix
  its own advice recommends, because the shell lexer ended a clause at `{` and
  truncated the reference; and on `refs/wtstash/<worktree>`, the namespace
  people adopt precisely to stop sharing `refs/stash`. `stash@{0}` still
  fires in every spelling — it names the shared top of stack, which is what a
  bare pop takes. The corpus grew 22 → 32 reviewed judgements, 9 → 14 of them
  negative, one per defect.

## v1.15.0 — 2026-08-20

- **The Windows build stopped shipping CRLF hooks.** This repository tracked
  no `.gitattributes`, so every checkout obeyed its own `core.autocrlf` —
  and Git for Windows defaults that to true. The consequence was not
  cosmetic: `install::SHIM` is an `include_str!`, so the Windows runner's
  CRLF checkout was compiled INTO the binary. v1.14.0's `amont.exe` carried
  `#!/bin/sh\r\n`, its archive shipped an 82-CR `pre-commit`, and every
  `amont install` there wrote a POSIX shell script with a carriage return in
  the shebang. Git for Windows tolerates it — its CI runs a real commit
  through those shims, which is exactly why nothing caught it — but a
  release artifact that differs by BUILD HOST is not a thing to leave
  standing. `.gitattributes` now pins both copies of the shims (the
  installable set and the one the crate embeds, which a root-anchored
  pattern would have missed) plus every `.sh` to `eol=lf`, and a test
  asserts the compiled-in bytes carry no `\r` — checked from the side that
  actually ships, on the Windows job that could regress it.
- **The gate-stamp flake is fixed, and it was a lock with half a
  contract.** `TEST_CWD` serialised the tests that MOVE the process cwd —
  but not the ones that merely depend on it, and production code spawns git
  without `-C`. So while `gate_stamp` held the cwd inside its fixture,
  `restage_distinguishes_nothing_from_failure` ran `git add` there, took
  that repository's `index.lock`, and the fixture's own `git commit` failed
  128; the panic then landed three lines later on a missing gate stamp.
  Roughly one run in forty. The cwd-reading test now takes the same lock and
  the lock says what it covers. Verified: 2 reproductions in 85 runs before,
  0 in 60 after.
- **The fixtures report git's exit status**, which is what made the above
  findable at all — `gate_stamp` and `attest` discarded it, turning a setup
  command that never ran into a product-shaped failure further down. Same
  rule the checks themselves obey: git failing is not git answering.

## v1.14.0 — 2026-08-20

- **`amont list --json` says which contract it is, and the field names are
  pinned to the page that documents them.** The document now opens with
  `"format": "amont-list-v1"` — every other machine-readable thing this tool
  writes carries a version and refuses what it does not recognise, and its
  most public one carried none. Bump it when a field's meaning changes or a
  field goes; adding a field does not, which is what the object shape was
  always for. `docs/checks.md` gained the envelope list and a table of every
  `checks[]` field, and a test now fails if the code emits a field the page
  does not name **or** the page promises one the code does not emit. The
  failure this closes is quiet by nature: a reader who guesses a field name
  gets `null` back, not an error, and `null` reads as a plausible answer —
  "nothing is skipped", "no override". That happened while verifying a
  release, and the wrong number looked entirely right.

## v1.13.3 — 2026-08-20

- **`audit-python` understands uv projects, instead of never running.** It
  invoked `pip-audit -r requirements.txt` and nothing else, so a PEP-621 /
  uv project — which has `pyproject.toml` and `uv.lock` and no
  `requirements.txt` — reported *"the audit did NOT run"* on every push,
  forever. Found across a whole fleet: six Python repositories in that
  state, one of them carrying 53 known vulnerabilities in 8 packages that
  nobody had been told about. Installing `pip-audit` did not help and could
  not have; the invocation was the bug.
  It now audits the INSTALLED tree (`pip-audit --path <venv>/site-packages
  --skip-editable`) when there is no requirements file. Exporting one is
  not an option: `uv export` emits workspace members and private-index
  dependencies, and pip-audit resolves a requirements file in a throwaway
  venv that can reach neither, dying on "No matching distribution found".
  The installed tree also happens to be the truer question — those are the
  versions actually imported. `$VIRTUAL_ENV` wins over `.venv`, the python
  version directory is discovered rather than guessed, and the Windows
  `Lib/site-packages` layout is handled. With neither a requirements file
  nor a virtualenv it still reports Unavailable, now naming both places it
  looked.

## v1.13.2 — 2026-08-20

- **The generated block is Prettier-clean, so amont stops contradicting
  itself.** amont ships `pre-commit-prettier`, and its own `AGENTS.md` /
  `CLAUDE.md` failed it: Prettier wants a blank line after an opening HTML
  comment, `_emphasis_` rather than `*emphasis*`, and a blank line before a
  closing one. Every JS repository was therefore stuck between two amont
  checks — and `prettier --write` on a generated file is exactly what
  `agents-md --check` then reports as drift, so satisfying one check
  created the drift the other exists to prevent. The generator now emits
  what Prettier wants; a unit test pins all three rules, and the real
  `prettier --check` was run against both files to confirm.

## v1.13.1 — 2026-08-20

- **`amont agents-md` now writes a CLAUDE.md signpost beside AGENTS.md.**
  Claude Code loads `CLAUDE.md`; the tool-neutral convention is
  `AGENTS.md`. A repository carrying only the latter was handing an agent
  no guidance at all — including this repository, whose block had sat in
  `AGENTS.md` since August and drifted unnoticed. The guidance still has
  ONE home; the signpost is a generated, marker-delimited pointer to it,
  so `--check` reports a stale signpost exactly as it reports a stale
  block, and `amont-fleet fix --apply --agents-md` writes both. A
  hand-written "see AGENTS.md" line would have been the one part able to
  rot in silence; this cannot.
- The block's timeout paragraph gained one sentence: run `git commit` and
  `git push` **bare** and check the effect, because trimming their output
  with `| tail` reports the pipe's exit status and a killed or rejected
  run then reads as success.

## v1.13.0 — 2026-08-20

- **"git could not answer" stopped reading as "nothing is stamped".** Four
  paths in the gate-stamp machinery treated a failed git call exactly like
  a clean negative answer: no tree to record, no stamp to bind, no stamps
  to find. The verdict was already the safe one — the gates re-run, never
  skip work on a question that could not be asked — but it was silent, so
  a single transient git failure was indistinguishable from an ordinary
  unstamped commit. Each now says which happened. (An absent notes ref is
  NOT one of these: `git notes list` answers 0 with empty output there, and
  a test now pins that, because the whole distinction rests on it.)
- **One test no longer swaps the whole process's `PATH` out from under the
  others.** The Windows extension-order test set `PATH` to a temp directory
  for the length of one call, while ~340 tests ran in parallel around it —
  any git spawned in that window failed with "not found", which is a hard
  error nothing retries, and the caller read it as git's answer. `which`
  gained a `which_on(path, tool)` seam so the test passes its path instead
  of installing it; `which` itself is unchanged for every caller.

## v1.12.1 — 2026-08-20

- **An attestation no longer vouches for a gate that had nothing to do.**
  Caught on a real note in the wild: a JS-only push minted
  `gates … pre-push-cargo-test pre-push-go-test pre-push-pytest`, because a
  language gate with no files of its language finds no crate/module root and
  returns `Passed` having run NOTHING. Right for a push gate — there was
  nothing to object to — and wrong for an attestation. Harmless in a
  single-language repository, unsound in a mixed one: CI would skip a suite
  nobody had run on that tree. The attestation now lists only gates whose
  declared `scope` the push actually touched; unscoped checks (secrets,
  branch-protect) still count, because they really do run every time, and a
  push whose changed files cannot be computed vouches for nothing scoped —
  the direction that makes CI run the suite.
- **Uninstall forgets what it wrote — everywhere.** `amont uninstall` swept
  its own repository's bookkeeping but left the `amont.conf` trust record
  behind, so a reinstall silently re-honoured consent given to a file
  somebody reviewed once, long ago; `amont-fleet uninstall` removed shims
  from every repository and swept none of them, leaving a stamp ref, a
  ledger and a trust record in each. Both now call one shared list — gate
  stamps, attestation notes, bypass ledger, skew marker, known-identity
  memo, trust — and say out loud which parts they forgot. Trust is revoked
  on purpose: consent defaults to no everywhere else here, and re-granting
  is one `amont trust`, which shows you the file again. `hook.skip` and
  `amont.severity` stay untouched, as always, and so does uncommitted work
  — parked changes in `$GIT_DIR/amont-held` survive any uninstall, because
  `amont restore` has to keep working after the hooks are gone.

## v1.12.0 — 2026-08-20

## v1.11.1 — 2026-08-20

- **`amont attest covered` found no signers from a subdirectory, and said
  nothing about it.** It resolved `.forgejo/allowed_signers` relative to the
  working directory, so a workflow step carrying a `working-directory` — a
  monorepo running its matrix inside `packages/<x>` — found no signers file,
  printed nothing, and fail-opened *forever*: the suite still ran, CI stayed
  green, and no output anywhere said the gate was dead. The path is now
  anchored to the repository root. Fail-open is the right behaviour for a
  failure; it is the wrong behaviour for a permanent misconfiguration, and
  silence made the two indistinguishable.

## v1.11.0 — 2026-08-20

- **A repository can say which amont it means.** `set minVersion 1.11.0`
  in a trusted `amont.conf` (or plain `amont.minVersion` config) puts a
  version floor in the repository itself: a binary older than the floor
  says so once per stage, naming both versions. Warn-only — a binary one
  release behind used to lack the team's newest checks *silently*, and
  nothing committed could even say so.
- **The push path's network verbs have deadlines.** `pull-rebase`'s
  reachability probe and `branch-pattern`'s initial-push check are killed
  at min(`amont.timeout`, 30s); the sync itself and `kustomize build` run
  under the full `amont.timeout`. Offline now reads as "could not reach
  the remote — skipping sync" instead of the wrong diagnosis "upstream no
  longer exists" (ls-remote's exit codes 2 and 128 are finally kept
  apart), and a remote that accepts the connection and then says nothing
  can no longer hold a push hostage. `amont.timeout 0` still disables
  every deadline.
- **Windows gets the safety-critical paths.** `amont trust` can be granted
  interactively (the prompt reads `CONIN$`, the console's `/dev/tty` —
  before this, Windows always declined, so declared checks stayed
  politely disabled), and Ctrl-C mid-check restores parked unstaged
  changes before dying, via `SetConsoleCtrlHandler` — same contract as
  the unix signal handler, same lock against the mid-park race.

## v1.10.0 — 2026-08-20

- **An attestation says where it ran, and a matrix leg only skips its own
  platform's work.** The note gained a `platform` line (`aarch64-macos`,
  `x86_64-linux`, …) and the format is now `amont-attest-v2`;
  `amont attest covered` defaults to requiring it to match the verifier's
  own platform, so a laptop's `cargo test` retires the macOS leg of a
  matrix and leaves the Linux and Windows legs untouched — with no per-leg
  configuration, since every leg runs the same one-liner. A suite whose
  result really is platform-independent says so in the committed workflow
  with `--platform any`, which is a claim about the suite and therefore
  belongs where the suite is defined, not on the machine holding the key.
  The version bump is the safety: a v1 verifier cannot know a v2 note ran
  somewhere else, so it reads it as no attestation and runs the tests.
- **`amont attest covered` — the attestation's verifying side as a CLI
  verb.** CI's step collapses from a 30-line sh block to
  `echo "covered=$(amont attest covered)" >> "$GITHUB_OUTPUT"`: fetch the
  notes ref, find the note on `HEAD` (or `HEAD^2` for a PR merge
  checkout), insist on the format version and on tree equality with the
  checkout, verify via `ssh-keygen -Y verify`, print the covered gate
  names. Empty output and exit 0 on every failure — fail-open is the
  contract, not the workflow author's option. `--signers`/`--principal`
  default to the committed `allowed_signers` (`.forgejo/` then `.github/`)
  and the first principal it names. The public templates keep the portable
  sh and mention the one-liner.

## v1.9.0 — 2026-08-20

- **The manifest carries the team's policy.** A trusted `amont.conf` can now
  say `severity clippy warn`, `skip yamllint`, and `set commit.subjectMax 50`
  — committed, reviewed lines instead of sixty people running the same
  `git config` incantation. Trust-gated like everything else in that file:
  untrusted policy is inert and says so once per run, and the trust prompt
  shows the policy you are consenting to. Precedence is a specificity
  ladder per key — default < system < global < **policy** < local <
  worktree < command — so the team's decision beats your global
  preferences and your local config in that repository still beats the
  team. Skips union across all sources and announce their origin
  separately ("by hook.skip" vs "by amont.conf"). `set` reaches an
  allowlist only (thresholds, commit style, `autoRebase`, `timeout`,
  `testPushedTree`) — never `amont.fix` or the trust decision itself,
  because a committed file must not change what already-approved commands
  may DO. `amont list` (and `--json`, additively) now names each
  severity's source; the fleet dashboard shows policy rows with origin
  `amont.conf`, folded into the same ladder the dispatcher uses. On a git
  too old for `--show-scope` (< 2.26) precedence degrades fail-safe: all
  git config beats policy.

## v1.8.0 — 2026-08-19

- **Signed test attestations: CI can skip what pre-push already proved.**
  With `git config amont.attest true`, a push whose block gates all passed
  leaves a signed note (`refs/notes/amont-attest`) on each pushed tip and
  sends the ref along to the remote. The note binds the **tree hash** (the
  content the gates actually ran against — a reword or tree-preserving
  rebase keeps its attestation, a single changed byte loses it), the names
  of the gates that PASSED (`Warned`/`Unavailable` never appear — "could
  not run" is not "passed"), and the amont version, signed with
  `ssh-keygen -Y sign` (key: `amont.attestKey`, default
  `~/.ssh/amont-attest`). CI verifies with stock git + ssh-keygen against a
  committed `allowed_signers` file and skips a test step only when the
  attested tree is exactly the tree it checked out AND the step's gate is
  named — amont itself still does not run in CI. Fail-open in one direction
  only, same doctrine as the gate stamps: any failure anywhere means CI
  runs the tests; an attestation can only ever save a redundant run. The
  four Forgejo CI templates now carry the verify step. Off by default.

## v1.7.4 — 2026-08-19

- **v1.7.3, delivered** (that tag's builds died on the runners' apt mirror
  three times; like v1.7.0 it published nothing). The install step now
  rewrites `/etc/apt/apt-mirrors.txt` — the runner routes apt through
  `mirror+file:`, so v1.7.2's rewrite of the sources files alone still let
  every fetch start at the Azure mirror.

## v1.7.3 — 2026-08-19

- **Linters run at zero warnings.** eslint exits 0 under any number of
  warning-level findings, yamllint under warning-level rules, pyright under
  warning diagnostics — so those findings accumulated into a list nobody
  was forced to read: a human scrolls past it, an agent reads "passed" and
  moves on. The three now get `--max-warnings 0`, `--strict` and
  `--warnings` respectively (clippy always had `-D warnings`), the CI
  templates say the same thing, and a repository that wants the old
  behaviour has the existing per-check downgrade:
  `git config amont.severity.lint-js warn`.

## v1.7.2 — 2026-08-19

- **The CI backstop, stated and shipped.** amont deliberately does not run
  in CI — CI wants the real tools, called directly. Eight copyable
  workflow templates (`templates/ci/{github,forgejo}/{rust,js,python,go}.yaml`)
  express amont's local policy in native steps, each annotated with the
  check it mirrors, audits keeping their branch-warns/release-blocks
  split. The amont-only checks (ban-terms, secrets, large-files,
  conventions) stay deliberately local. New docs chapter: *The CI
  backstop*.
- **`amont run` accepts short names.** `amont run ban-terms` now resolves
  exactly as `hook.skip ban-terms` always did; an ambiguous short name
  (`branch-pattern` names two checks) lists both full ids instead of
  guessing, and a short-named pre-push check still gets its synthetic
  refs.
- Two messages lost their accidental mid-line whitespace runs (the
  conventions held-back line and the gate-pair "running it here" line).

## v1.7.1 — 2026-08-19

- **v1.7.0, delivered.** Same features as below; the v1.7.0 tag never
  shipped because the release runners' apt mirror hung the Linux
  cross-builds for their whole 30-minute budget, three attempts in a row.
  The install step now uses the canonical archive, bounds and retries every
  fetch, and carries its own five-minute budget — a mirror outage costs a
  cheap rerun, not the release.

## v1.7.0 — 2026-08-19

- **The team-rollout story.** Hooks only protect the machines that
  installed them, and only npm repositories could self-install on clone.
  Three pieces close the gap:
  - **`amont enroll`** — the machine-level standing grant as one command:
    binary, template directory, and `init.templateDir`, refusing to
    overwrite a template dir that already belongs to something else, and
    idempotent. Every future `git clone` and `git init` arrives with the
    hooks; onboarding is two lines, once per machine.
  - **`amont.conventions declared`** — what makes that grant safe on a
    machine that also clones other people's projects. The house rules
    (commit shapes, branch names, lint/format gates, suites, audits,
    auto-rebase, the `commit-msg`/`prepare-commit-msg` hooks) run only in
    repositories that commit an `amont.conf` — an empty file declares,
    and presence executes nothing, so no trust decision is needed. The
    safety net (`merge-conflict`, `secrets` at both stages,
    `large-files`, `ban-terms`) runs everywhere. Held-back stages say so
    in one line; `amont list` reports the state, `--json` carries
    `"conventions_apply"`. The default, `everywhere`, changes nothing.
  - **[Rolling out to a team](https://fredericrous.github.io/amont/team-rollout.html)**
    — the doc chapter: the recipe, and honesty about what stays
    unsolved (hooks are advisory; the backstop belongs in CI).
- `amont.largeFileWarn` / `amont.largeFileBlock` are now in the
  configuration page's key table, where the other keys already were.

## v1.6.10 — 2026-08-19

- **The Go language track.** Four new checks give Go repositories the same
  lane Rust has had: `pre-commit-gofmt` (handed exactly the staged files;
  repairs and re-stages under `amont.fix true`), `pre-commit-go-vet`
  (`go vet ./...` per touched module — a `go.mod` dependency bump alone
  still vets), `pre-push-go-test` (`go test ./...` per touched module,
  against the pushed tree, per ref), and `pre-push-audit-go`
  (`govulncheck`, opted in by a `go.sum`; informational findings warn,
  code-affecting vulnerabilities block a `v*` tag push like the other
  audits). All scoped: a repo without Go never invokes the toolchain.
  Thirty-two built-in checks.

## v1.6.9 — 2026-08-19

- **Debug leftovers are caught in every language, not just JS.**
  `pre-commit-ban-terms` — until now JS/TS-only (`describe.only`, `fit(`,
  `debugger`) — learns Rust and Python: a staged `dbg!(…)`, `breakpoint()`,
  `pdb.set_trace()` or `ipdb.set_trace()` refuses the commit the same way.
  Each language gets its own comment/string blanker, so a term named in a
  doc comment, a raw string `r#"…"#`, or a docstring stays discussion,
  while a call inside an f-string interpolation (`f"{breakpoint()}"`) is
  still code and still caught. Same check, same `hook.skip ban-terms`
  escape, no new configuration.

## v1.6.8 — 2026-08-16

- **The large-file guard.** Git history never forgets a megabyte: a staged
  file over `amont.largeFileWarn` MB (default 10) is named at commit — a
  large asset can be deliberate, and this is the moment to decide — and
  one over `amont.largeFileBlock` MB (default 100, GitHub's own refusal
  line) blocks with the remedy named: git-lfs, or keep it out of history,
  because deletion later does not remove the bytes.
- **The Python test gate.** `pre-push-pytest` closes the parity gap: JS
  had a push-time suite from the start, Rust gained `cargo-test`, and now
  a repository declaring a pytest setup (`pytest.ini` or `conftest.py`)
  runs its suite against the pushed tree, per ref, for pushes that change
  Python. Twenty-eight built-in checks.

## v1.6.7 — 2026-08-16

- **Secrets never leave the machine.** Two new checks, one leak, both
  moments: `pre-commit-secrets` blocks a staged credential (private key
  headers, AWS access key ids, GitHub/Slack/Google/Stripe-live/npm/
  OpenAI/Anthropic token shapes) while it is still a ten-second fix, and
  `pre-push-secrets` scans every line every pushed commit adds — a
  `--no-verify` commit, another tool's commit, a secret added and removed
  within the pushed range — because the push is the last moment a secret
  is recoverable and after it the remedy is rotation, not history editing.
  Curated token shapes, no entropy heuristics, no network; per-line
  `amont:allow-secret` pragma for legitimate fixtures; reports are
  redacted (the kind and the place, never the matched text). Twenty-six
  built-in checks now.

## v1.6.6 — 2026-08-16

- **Dependency audits, with the severity the push deserves.** Three new
  pre-push checks — `audit-rust` (`cargo audit`, opted in by `Cargo.lock`),
  `audit-js` (`npm audit`, by `package-lock.json`), `audit-python`
  (`pip-audit`, by `requirements.txt`) — bring the release workflow's
  policy to the machine where the push starts: known vulnerabilities are a
  named WARNING on a branch push and a REFUSAL on a push carrying a `v*`
  tag, because a tag is a release and immutable registries take nothing
  back. Warning-class advisories (unmaintained/unsound) are named and never
  block; a missing tool or unreachable advisory database is loud and never
  blocks — the offline case must not teach `--no-verify`. The tools' output
  decides, never the exit code alone. Twenty-four built-in checks now.

## v1.6.5 — 2026-08-16

- **The fleet's defaults stopped being one person's laptop.** `--binary`
  defaulted to `~/.local/bin/amont` — on a machine whose amont had moved to
  homebrew's prefix, every correctly-baked shim read as a stale bake. It now
  defaults to the `amont` on PATH (what a shim's own fallback would
  execute — probing reality, not guessing), with the install path kept as
  fallback. `--root` still defaults to `~/Developer` when it exists; a home
  without one is refused with the remedy named (`--root <dir>`) instead of
  presenting one machine's layout as a fact about every machine.

- **A gate is a name, not an npm script.** The commit-time gate was keyed to
  the push gate's three package.json scripts (`typecheck`, `test:unit`,
  `test`) — the moment a Rust or Python repository wanted a gated
  `cargo test`, the seam showed. Now ANY name declared at both stages pairs:
  the `pre-commit … block` side earns per-commit stamps, and the same-named
  `pre-push` declaration is skipped for fully-stamped pushes (`✓ test gated
  at commit instead`), runs for unstamped ones with the same warning the npm
  gate gives, and its dodges land in the bypass ledger under their own name.
  Warn-severity, skipped, or scope-uncovered pairs vouch for nothing,
  exactly as before. The npm push gate keeps its vocabulary; it is now one
  consumer of the machinery instead of its definition.

## v1.6.4 — 2026-08-15

- **The release publishes its own homebrew tap.** The formula bump was the
  one manual step left in a release — copy a version, copy four sha256
  lines, push — done by hand four times in three days, each a transcription
  error waiting for its moment. A new `publish-tap` job reads the published
  release's own SHA256SUMS, rewrites the formula through an assert-heavy
  script (`scripts/bump-tap.py` — wrong-version checksums, missing targets,
  and stale version strings all refuse loudly; `ruby -c` proves the result
  is a formula before anything moves), and pushes to the tap with a deploy
  key that can write to that one repository and nothing else. Idempotent,
  so a resumed release run no-ops. This release exists to prove the loop:
  if you installed it with `brew upgrade`, no human touched the formula.

## v1.6.3 — 2026-08-15

- **A shim newer than its binary is absorbed, not erred.** Shims and binary
  upgrade separately: a refreshed template bakes its full shim set into
  every `git init`, while the binary on PATH can lag releases behind — and
  the day `post-commit` shipped, every machine with an older binary printed
  `unknown hook` at exit 2 on every commit. In hook mode an unknown name is
  a shim passing its own filename, so it now reads as what it is — a
  message from a newer template: warn once per binary version per
  repository (a versioned `$GIT_DIR/amont-skew` marker, removed by
  `uninstall`), name the fix, exit 0. A hook the binary does not know is a
  hook that does not exist yet; fail-open here is the gate's own safe
  direction, and blocking commits over binary age would teach exactly the
  `--no-verify` habit this project exists to unteach. An unknown verb
  typed interactively stays a loud usage error.

## v1.6.2 — 2026-08-14

- **The apply report earns its scrollback.** `amont-fleet fix --apply` used
  to print one `-0 +5` line per repository — eighty-six identical successes
  burying the one FAILED line at equal weight — then spell out each husky
  refusal as its own four-line paragraph, twice. On a terminal, successes
  now collapse into the live `applying n/m` counter and scrollback keeps
  only the exceptional; redirected-hooks refusals group by owner (the cause
  once, the repositories packed onto wrapped lines, the remedy once);
  failures repeat just above the summary — the place the eye lands — with
  the path said once instead of three times; and the final counts lead with
  a verdict glyph (`✓`/`!`/`✗`). Piped and CI output keeps the stable
  line-per-repo stream, and `--json` is untouched.

## v1.6.1 — 2026-08-14

- **The planning pass shows its work.** `amont-fleet fix` and `install`
  follow the scan with a per-repository planning pass — git is asked whether
  each hook path is tracked before anything may be written — and on a fleet
  that was several seconds of dead air between the scan report and the first
  apply line: silence indistinguishable from a hang, one phase after the
  scan bar fixed exactly that. The pass now draws the same in-place status
  line with the one thing the scan cannot have — a denominator:
  `⠹ planning 42/185 · 1.2s  Perso/some/repo`. Same terminal gate, same
  width and escaping guarantees, gone without a trace when the phase ends.

## v1.6.0 — 2026-08-13

- **The fleet sees the bypasses.** `amont-fleet` reads each repository's
  bypass ledger (through the runtime's own parser — the dashboard cannot
  form its own opinion) and shows it: a `BYPASS` column in the overview,
  tinted only when the newest event is recent; a per-script block in the
  repo detail pane; a fleet aggregate in the header and the text report,
  spoken only when non-zero. `scan --json` carries the per-repo objects and
  the fleet totals.

- **A dodged gate is counted.** post-commit always knew when a commit
  arrived without its commit-time gate having run — `--no-verify`, a blocked
  attempt retried with it, a missing tool — and discarded the signal on the
  spot. It now appends one line per dodged script to a local ledger
  (`$GIT_COMMON_DIR/amont-bypasses`, versioned `amont-bypass-v1`), silently,
  and `amont list` reports the tally as "unverified commits" (with a
  `bypasses` object in `--json`). Local-only, never pushed, no telemetry;
  `amont.recordBypasses false` opts out, `amont uninstall` erases it. An
  ungated repository pays zero extra git spawns — pinned by a budget test.

- **The hooks show their work.** While the concurrent stage runs, an
  interactive terminal now gets a live region under the finished blocks:
  one line per running check — braille spinner, name, elapsed seconds —
  repainted ten times a second, shrinking as checks finish, erased without
  a trace when the stage ends. Pre-push gets the same treatment, so a long
  `cargo test` is a ticking clock instead of a frozen prompt. Std-only, on
  stderr, strictly TTY-gated (pipes, redirects, `TERM=dumb`, and CI logs
  never see a control code); captured tools get `FORCE_COLOR`/
  `CLICOLOR_FORCE`/`CARGO_TERM_COLOR` so their blocks keep their colors.
  `amont.progress false` turns it off with the rest of the machinery.

- **One check, one block.** Twenty concurrent pre-commit checks used to
  print straight to the shared terminal from their own threads, shuffling
  two failing linters' lines together — the dispatcher's roll-up existed
  partly to apologise for it. Every check now writes into its own buffer
  (its helper lines and its tools' captured stdout/stderr alike) and lands
  on the terminal as one contiguous block when it finishes, in completion
  order. `amont.progress false` restores raw streaming. Two small side
  effects: ban-terms' header moved from stderr to stdout with the rest of
  its report, and a captured tool sees a pipe instead of a terminal (the
  live display in the next change re-enables tool colors where it matters).

- **A declared check's scope can name files, not only extensions.**
  `pre-commit lockcheck package.json block ./check-lock.sh` finally parses:
  bare tokens in the scope column are exact filenames, basename-matched (so
  `not-package.json` cannot counterfeit them), mixing freely with `*.<ext>`.
  Directories stay out; the grammar has no globs to mis-guess. The `files`
  marker, `$AMONT_FILES`, the push-gate coverage rule and the dashboard's
  "fires when" column all understand the new tokens.
- **The manifest stopped being a process global.** `externals()` and
  `tool_pins()` were `OnceLock`s keyed on the working directory at first
  call — safe in a hook, which handles one repository and exits, and a
  documented trap for anything that walks many. They are now one owned
  `Manifest`, parsed and trust-gated once by each entrypoint with the
  repository named explicitly, and lent down through `Ctx` exactly like the
  push refs. The `pub(crate)` quarantine and its warning comments retire
  with the trap; behavior is unchanged.

- **`pre-push-pull-rebase` stopped testing ghosts, and learned to stand
  down.** A successful auto-rebase used to fall through to the test suite —
  which then judged packages selected from the pre-rebase oids git handed the
  hook, at a HEAD those oids no longer describe, before the server refused
  the stale objects anyway. A successful sync now stops the push immediately
  and asks for a second one. And `amont.autoRebase false` turns the check
  into a pure advisor: no network round-trips on the push path, no rebase you
  did not type — a behind branch stops the push with the command to run.
- **`amont.conf` can pin tool versions.** `tool ruff 0.6.` checks — once per
  hook run, at both stages — that `ruff --version`'s first line contains the
  substring, and warns naming both sides when it does not (or when the tool
  will not run at all). Warn-only, always: skew never blocks a commit, it
  just stops masquerading as a flaky hook. Pins are trust-gated like every
  declaration — verifying one executes a program the repository named — and
  a malformed pin nags like any other broken line.

- **An editor save that lands while the checks run is no longer destroyed.**
  The index-fidelity restore used to write the held (pre-commit) bytes over
  whatever was on disk — including a save you made mid-check, silently. The
  restore now compares each held file against what its own checkout put
  there: a file that changed mid-run is kept, and the held version is parked
  in `$GIT_DIR/amont-preserved/` with a printed pointer. With `amont.fix` on
  the guard stands down — a repo-wide fixer rewrites held files as its job,
  and that opt-in's contract ("the tree returns to your unstaged version")
  holds unchanged. The hold's checkout is also scoped to exactly the held
  paths (`:(literal)` — a file named `*.rs` is a name, not a glob), so a
  dirty commit costs a walk of the changed files, not the whole tree — and
  file watchers stop seeing untouched files flap.

- **A git failure while listing staged files is announced, not read as
  "nothing staged".** The stage still proceeds — its plumbing must never
  block a commit — but it says it judged an empty, unverified set. Third
  member of the `repo_hooks` / push-gates bug family, closed the same way.
- **The suite now tests its own claims.** kube-linter and kubeconform —
  blocking checks that had never executed their tools in any test
  environment — run for real in CI on both platforms (pinned installs;
  kustomize rides along), with block-and-pass cases each. The gate stamp's
  linked-worktree and `git commit -a` behaviors, previously comments, are
  pinned by tests — both claims held. And the Ctrl-C restore test is
  marker-driven instead of guessing with a 300ms sleep that flaked under
  load twice in one day.

- **Every command a check spawns now runs under a wall-clock budget.**
  `amont.timeout` (seconds; default 600, `0` disables). One hung tool — a
  linter deadlocked on a lock file, a plugin doing network I/O — used to
  block the commit FOREVER, inside the index-fidelity hold, with your
  unstaged changes parked out of the tree; the learned response to that is
  `--no-verify`, permanently. At the deadline the command is killed and the
  check fails, loudly, naming the config key. The kill reaches the direct
  child; a detached grandchild may survive, orphaned, but the commit is no
  longer hostage to it.
- **The commit path spawns half the subprocesses, and a test now guards the
  number.** The staged file list and the repo root are read from git once
  per stage and lent to every check (the `PushRefs`/`Overrides` pattern,
  applied to the two hottest questions), and `usual-name` stopped running
  `git shortlog --all` — a full history walk — on every commit: an identity
  seen once is memoized in `amont.knownIdentity` (local; removed by
  uninstall). A PATH-shimmed git that counts its own invocations pins a
  one-file commit to a spawn budget, so the o(checks) regression class the
  repo's founding argument is about can no longer land silently.

- **The gate-stamp evidence chain is now adversarially tested**: a
  hand-written wrong-format marker stamps nothing, a foreign note in our
  notes ref is not a stamp, and a BLOCKED commit attempt cannot vouch for a
  `--no-verify` retry of the same tree (pinned end to end through real
  hooks).

- **A declared check is now a real check API, not a cron line.** The command
  runs through the same program resolution built-ins use (so `npx` works on
  Windows, where a bare spawn cannot start a `.cmd` and the check silently
  never ran), and it receives the file list its scope matched: `$AMONT_FILES`
  always (newline-separated, the exact set the gate judged), and appended to
  the argv when the command carries the new `files` marker —
  `pre-commit shellcheck *.sh block files shellcheck` finally does what it
  looks like it does. With `files`, an empty matched set runs nothing instead
  of handing a linter an empty argv. The docs now also state the `amont.fix`
  cliff in bold: a `fix`-declared check does not run AT ALL for members who
  have not set `amont.fix`.
- **The CLI meets the standard its refusals set.** `--help` and `--version`
  are inert in any position and answered with exit 0 — `amont install --help`
  used to RUN THE INSTALLER, and there was no `--version` at all in a binary
  shipped through six channels. Every verb now rejects a flag it does not
  know as a usage error naming it — `amont trust --revok` used to fall
  through the `--revoke` test and grant trust with no prompt. `amont --help`
  finally describes each verb instead of listing nine syntax lines, and
  `amont-fleet` answers `--help`/`--version` with exit 0 too. The shim's
  binary-not-found message now names the reinstall route for each install
  channel instead of a `make install` only contributors can run (the shim
  text changed, so the fleet will show installed repos as drifted until
  `amont-fleet fix --apply` re-bakes them).

- **Messages git itself writes now pass `commit-msg` unjudged.** `git merge`
  invokes the hook (githooks(5)), and "Merge branch '…'", `Revert "…"`,
  `Reapply "…"` and the autosquash shapes `fixup!`/`squash!`/`amend!` all
  carry no conventional type by design — the hook blocked the porcelain that
  produced them, and the workaround it trained was `--no-verify`, which turns
  off everything else too. Exact prefixes only: "Merges: cleanup" and "fixup
  the parser" are still judged.

- **The docs stopped denying `--no-verify`.** Four pages (and the generated
  AGENTS.md block) claimed commit-msg "cannot be bypassed with --no-verify" —
  githooks(5) says the opposite, and the pages inverted prepare-commit-msg,
  the hook the flag genuinely does not skip. Corrected everywhere; the
  "adjustable in itself" argument now stands on the true leg (`hook.skip`
  and `amont.severity` really cannot reach it).

- **The push gates stopped reporting green when git itself fails.**
  `run-tests-js` and `cargo-test` returned `Passed` when `rev-parse` or
  `ls-files` failed — silently, nothing run, nothing said. They now report
  `Unavailable` with a "git would not answer — the gate did NOT run" line:
  loud, and non-blocking, the same split `init` got in v1.5.1.

- **`defines_script` reads the top-level `"scripts"` object, not the first
  `"scripts"` substring.** `{"files":["scripts"],"dependencies":{"test":…}}`
  used to hand the brace-matcher the dependencies object — a dependency named
  `test` answered as a script (blocking the push) while the real scripts
  object was never read. The scan now tracks depth and requires a real
  top-level key. Also: the `gated_at_commit` rustdoc example is now a
  manifest line the parser accepts, and ban-terms' most-printed line learned
  to spell "were".

- **A fifth hook, `post-commit`, closes the `--no-verify` hole in commit-time
  gating.** Moving a gate entry to commit time (v1.5.0) skipped the push-time
  script on the strength of a declaration — a promise on paper. A commit made
  with `git commit --no-verify`, from a client that runs no hooks (libgit2
  IDEs), or on a machine without amont was never judged by the moved check,
  and the push gate waved it through with a green "gated at commit instead"
  line anyway.

  Now the event is recorded, not assumed: when the moved check runs at
  commit time, `post-commit` — which `--no-verify` does NOT skip — stamps the
  commit in a local notes ref (`refs/notes/amont-gate`; never pushed,
  invisible to `git log`, GC'd with its commits, removed by `amont
  uninstall`). The push gate skips a script only when every pushed commit in
  the declaration's scope carries its stamp; an unstamped commit brings the
  gate back with a line saying why. Every failure mode points the same
  direction — no marker, a rewritten hash, a merge or cherry-pick (which run
  no `post-commit`) — all mean "no stamp", and a missing stamp only costs a
  redundant run, never a skipped check. Moving a gate entry earlier now
  trades latency, never safety.

  Installing the fifth shim needs one `amont install` (or the next
  `npm install` via `prepare`, or `amont-fleet fix --apply`) per repository;
  until then pushes simply stop trusting commit-time gating and run the gate,
  announcing it. **Upgrade `amont-fleet` together with `amont`**: a fleet
  binary older than this release reads the new `post-commit` shim as a
  retired stale file and removes it.

## v1.5.1 — 2026-08-10

- **The push gate only defers to a commit check that covers the push.** Moving
  a gate entry to commit time (v1.5.0) skipped the push-time script whenever a
  matching `pre-commit` declaration existed — on its name alone. Four ways
  that let a push report a green gate having run nothing are closed: a `warn`
  declaration (or one downgraded by `amont.severity.*`) no longer stands in
  for a blocking push check; a push whose JS changes fall even partly outside
  the declaration's scope runs the full gate for that ref; a monorepo
  sub-package's gate is never skipped on the root declaration's account; and
  the `✓ … gated at commit instead` line is only printed when the skip is
  actually applied. The one gap that cannot be closed from push time —
  `git commit --no-verify` — is now stated plainly in the docs instead of
  being implied away.

- **A deliberate `core.hooksPath` is serviced again, end to end.** The
  husky-refusal work (v1.5.0/v1.5.1) over-reached in four places, each now
  fixed: stale leftover shims in `.git/hooks` no longer mark a repository
  hostile when our shims also sit at the redirect destination (amont is
  running there — the leftovers are history, not a takeover, and the stranded
  case's message now offers `amont uninstall` alongside unsetting the
  redirect); `amont-fleet fix --apply` no longer refuses at apply time the
  benign-redirect plans it had just previewed; a repair no longer skips
  restoring a missing or drifted dispatcher in an npm-managed repository — it
  restores it into the repository's own `node_modules` bake instead of either
  hijacking or ignoring it; and a husky repository that never ran amont is
  filed as unmanaged (silence) under repair instead of being printed with a
  `git config --unset core.hooksPath` remedy that would disable the hooks it
  intends.

- **`amont init` is silent only where git itself says "not a git repository".**
  Every other git failure — dubious ownership in a container bind mount, an
  unreadable `.git/config`, a corrupt gitfile — used to take the same silent
  exit 0, so `npm install` logged success while no hooks were written and
  commits from that environment ran no checks. Those now fail loudly with
  git's own reason; `install` does the same, and `uninstall` stays forgiving
  (loud, but still able to finish its cleanup).

- **`amont-fleet fix` no longer offers to undo an npm install.** A repository
  that carries `amont` as a dev dependency has its binary baked inside
  `node_modules`, by `amont init`, from its own `prepare` script — deliberate,
  and the whole point of that route. The fleet read the path as `stale`
  ("points somewhere other than the binary we install", which is literally
  true) and planned to rewrite all four shims.

  On the machine that installed amont normally that is only churn: the fleet
  re-bakes one way, the next `npm install` the other, forever. On a
  teammate's machine it is a break — the npm route exists precisely so nothing
  has to be installed first, so `~/.local/bin/amont` is the one path that is
  not there, and a fleet-wide repair would leave those repositories with shims
  resolving nothing.

  There is now a `self_managed` bake state, reported as `npm` in the dashboard,
  and repair leaves it alone. Activation is unaffected: turning hooks on in a
  repository still writes the first shims. Nothing else changes state — across
  a 163-repository sweep, every entry that had been reading `stale` was one of
  these.

- A comment in `install::init` claimed that using `absolute()` rather than
  `canonicalize()` kept the baked path off pnpm's versioned store directory.
  It does not, and never did: the JS wrapper resolves the binary through
  `require.resolve` before this code runs. It is harmless — `prepare` re-bakes
  on every install, and the shim falls through to `~/.local/bin` and then
  `PATH`, failing loudly rather than skipping a check — but the comment said
  otherwise, which is worse than saying nothing.

- **The npm wrapper tries the next binary when a spawn fails.** With a package
  manager that ignores the `libc` field — yarn classic, or the wrapper's own
  suggested `npm install --force` — both linux-x64 packages get installed, and
  on a musl host the glibc build was picked because it merely *existed*, then
  failed at exec on the missing loader, taking every hook with it while the
  right binary sat installed one candidate over. The wrapper now loops over
  spawns rather than paths: only a spawn-level failure falls through, a real
  exit code is forwarded as before (a binary that ran has answered), and the
  no-candidate message names what it tried.

## v1.5.0 — 2026-08-10

- **A gate entry can be moved to commit time.** `typecheck` sits in
  `pre-push-run-tests-js`'s gate because nothing checks it sooner, and for
  some repositories that is too late: a type error is cheapest to hear about
  at the commit that caused it, not an hour later at push. Declare it in
  `amont.conf` under the name of the script —
  `pre-commit  typecheck  *.ts,*.tsx  block  npm run typecheck` — and the push
  gate drops it, saying `✓ typecheck gated at commit instead`. The same
  argument already keeps `lint` out of that gate, so this is that rule made
  available rather than a new one, and it is not `typecheck`-specific.

  Only a declaration that **would actually run** counts: an untrusted
  manifest, an unusable line, a `hook.skip`, or a declaration on the wrong
  stage all leave the push gate exactly as it was. The failure that shapes
  the test suite is the one where a repository declares `pre-commit
  typecheck`, is never trusted, and has types checked at neither end while
  both ends report green.

## v1.4.1 — 2026-08-07

- **The npm packages v1.4.0 promised.** That release published to GitHub and
  crates.io and then failed one step short of npm, in its own verification:
  `tar tzf … | grep -q` looks harmless, but `grep -q` exits at the first
  match and closes the pipe, tar takes EPIPE, and under `set -o pipefail`
  that is a failed step — reported, unhelpfully, as `tar: stdout: write
  error`. BSD tar tolerates the closed pipe and GNU tar does not, so it
  passed on a Mac and failed on the ubuntu runner. Nothing was published, so
  npm starts cleanly at this version; `^1.4.0` resolves to it.

- `prepare` also runs on `npm ci --omit=dev` — the usual second stage of a
  Dockerfile — where a **dev** dependency is by definition absent, so the
  command does not exist and the install fails with it. The npm section now
  says to write `"prepare": "amont init || true"` where a project installs
  that way, and says where not to: a repository with no production-install
  path keeps the bare form, so a hook it may not overwrite stays loud rather
  than swallowed.

## v1.4.0 — 2026-08-07

- **A repository whose hooks another tool owns is now refused, by name.**
  `git rev-parse --git-path hooks` honours `core.hooksPath`, so in a
  repository running husky it answers `.husky/_` — inside the repository,
  plausible, and wrong. `install` baked four shims there, husky's own
  `prepare` regenerated the directory on the next `npm install`, and the
  repository went back to running nothing. Eleven repositories on the
  author's machine were in that state; the fleet called them "drifted", and
  a direct push to a protected branch went through unchallenged for as long
  as it lasted. Both `install` and `amont-fleet` now say what happened and
  what to type, and `--force` does not move it. This is not a blanket
  objection to `core.hooksPath`: a repository deliberately keeping its hooks
  in `tooling/hooks` is installed into exactly as before. The refusal needs
  evidence — a destination belonging to a manager that regenerates it, or
  our shims already sitting in the repository's own hooks directory.
  `uninstall` deliberately does not refuse, since it has to reach shims
  earlier versions put there.

- **`npm i -D amont` + `"prepare": "amont init"`.** For a JavaScript project
  the binary can now travel with the repository rather than with the
  machine, so a teammate who clones it and runs `npm install` gets the hooks
  with no install step of their own. Six prebuilt platform packages are
  declared as `optionalDependencies` with `os`/`cpu`/`libc`, and there is no
  `postinstall` — this survives `npm ci --ignore-scripts`, an offline cache
  and a pull-through registry. The binaries are the ones this release
  already publishes and checksums.

- **A new verb, `amont init`**, is what that `prepare` calls: it wires up one
  repository and does nothing else. `install` could not serve — it copies a
  binary into `~/.local/bin`, populates the XDG template directory, and
  prompts through `/dev/tty`, which in a terminal would hang `npm install`
  on a question about a manifest nobody has read. `init` never prompts,
  writes nothing outside the repository, and exits 0 in silence where there
  is no `.git`, because `npm install` legitimately runs from a tarball and
  inside a Docker build.

- A refusal that had never rendered. `Refusal::explain` was sanitized as one
  assembled string, which escapes `\n`, so `TrackedUnknown` printed its
  `git config --add safe.directory` remedy as a literal `\x0a` — the one
  refusal whose entire purpose is telling you what to type.

## v1.3.1 — 2026-08-07

- `amont-fleet` now shows the walk while it walks. `scan`, `fix`, `install`
  and `uninstall` all begin with the same pass over every repository under
  the root; it announced itself once and then went quiet for the whole seven
  seconds, which from the outside is indistinguishable from a hang. There is
  now a live line: the clock, how many directories and repositories have been
  counted, and the path being looked at right now — the last being the one
  that matters when a scan stalls, since a frozen count says something is
  slow and only the path says what. It erases itself before the report
  prints, and it appears only when stderr is a terminal, so piped, redirected
  and CI runs emit exactly the bytes they did before.

## v1.3.0 — 2026-08-04

- The branch contract is now knowable BEFORE the branch exists, three ways.
  `amont list --json` carries `branch_style` (shape, pattern, prefixes)
  beside `commit_style`; the AGENTS.md block renders the same contract so a
  coding agent reads it before its first `git checkout -b`; and a new
  twenty-first check, `pre-commit-branch-pattern`, says at the FIRST commit
  what pre-push would refuse at the last - with the `git branch -m` fix,
  while renaming costs nothing. A warning, never a block, and quiet on a
  detached head, in a remoteless repository, and on any branch a remote
  already has. All three render from the same `BRANCH_PREFIXES` table the
  push check enforces: there is no second copy to drift. Re-run
  `amont agents-md` to refresh committed blocks.

## v1.2.1 — 2026-08-04

- `amont-fleet` says what it is doing while it does it. The scan announces
  itself the moment it starts (on stderr, only when a person is watching),
  and `install`/`fix --apply` print each repository's line as it is
  applied instead of holding every line until the end - a fleet-sized run
  used to be silent for its whole duration, which read as a hang.

## v1.2.0 — 2026-08-04

- The committed manifest is now **`amont.conf`**, undotted. A file whose
  whole story is "review me before you trust me" should not be hidden by
  the shell's dotfile convention. If you created a `.amont.conf` under
  v1.1.0, rename it — the trust record is keyed on content, so an
  already-trusted manifest stays trusted under its new name.
- The AGENTS.md block written by `amont agents-md` now warns coding agents
  that `git commit` and `git push` run their checks first and can
  legitimately take minutes — pre-commit can mean clippy building a
  workspace, pre-push a test suite — so a shell tool's default two-minute
  timeout kills the command mid-check and reads its own impatience as
  failure. Re-run `amont agents-md` to refresh the block.
- `amont list | head` no longer panics with a backtrace when the pipe
  closes early: both binaries restore SIGPIPE's default disposition and
  die quietly, like every other Unix filter.

## v1.1.0 — 2026-08-04

**The project is now amont** — French for upstream: catch it *en amont*,
before it flows downstream. The old name, githooks, lost every search it
entered to the githooks(5) man page, git's own documentation and half a
dozen namesakes.

This is a clean rename, deliberately without a compatibility layer:

- Binaries: `amont` and `amont-fleet`; crates `amont`, `amont-runtime`,
  `amont-fleet` on crates.io. The `githooks` crates stay at 1.0.2 and get
  no further releases.
- Config keys: `amont.severity.*`, `amont.commit.*`, `amont.testPushedTree`,
  `amont.trusted`. Old `githooks.*` keys are not read — re-state what you
  had tuned. `hook.skip` is unchanged.
- The committed manifest is `.amont.conf` (rename yours, then re-run
  `amont trust` — the record moved with the key). The `agents-md` span
  markers are `<!-- amont:start/end -->`.
- Installer env vars: `AMONT_BIN_DIR`, `AMONT_VERSION`. The runtime
  override `GIT_HOOKS_BIN` keeps its name.
- Installed shims from the githooks era resolve the old binary, not the
  new one: re-run `amont install` per repo, or `amont-fleet fix --apply`
  across a tree.

Also in this release:

- The README opens with the argument instead of an essay, states outright
  that the binary makes no network calls, and finally *shows* the fleet
  dashboard — `docs/assets/fleet-demo.sh` rebuilds the recording against a
  synthetic fleet, real binaries throughout.
- A test now ties the "twenty checks" prose on every user-facing page to
  `registry::CHECKS`, so the twenty-first check cannot ship with the pages
  quietly understating the count.
- Questions and ideas have a home: GitHub Discussions is on, and the issue
  templates point there.

## v1.0.2 — 2026-08-03

These releases shipped under the project's original name, githooks; the
entries keep it, because they describe what was actually released.

- `cargo install githooks` and the Homebrew tap
  (`brew install fredericrous/tap/githooks`) are live and documented.
- `githooks install` no longer copies a binary a package manager owns. A
  brew-, cargo- or distro-installed binary is baked where `PATH` exposes it
  and copied nowhere, so `brew upgrade` now reaches every repository instead
  of refreshing one file while the shims stay pointed at a frozen copy. A
  build directory on `PATH` is still copied — `cargo clean` makes it the one
  path guaranteed not to be there tomorrow.
- `githooks install` now warns when an unbaked-template setup
  (`init.templateDir` pointing at a checkout) is combined with a custom
  `$GITHOOKS_BIN_DIR`: the shims would look in `~/.local/bin` and the binary
  went elsewhere. Install names both paths and the two ways out.
- The release workflow no longer polls the crates.io index that cargo
  already waits on — the rate-limited poll could fail a publish that had
  succeeded.

## v1.0.1 — 2026-08-03

- Publishing to crates.io happens from the tag, in CI, in dependency order,
  with the credential held by the repository.
- Windows has a first-class install path: `install.ps1`, exercised in CI on
  a real Windows runner against a real published release.
- The documentation became a published book, and the repository grew its
  community files: issue templates, a PR template, a code of conduct, and a
  security policy.

## v1.0.0 — 2026-08-03

The first Rust release, ending the zsh era.

- Twenty built-in checks across four git hooks, in one std-only binary —
  commit-message conventions, merge-conflict markers, per-language linters
  and formatters, branch rules, test gates — each inert until the repository
  carries the files its tool keys on.
- The trust model: a repository declares its own checks in a committed
  `.githooks.conf`, and a clone's declarations are inert until reviewed and
  accepted with `githooks trust`, keyed on content rather than path.
- `githooks-fleet`: bulk install, fleet report, fix planning with a
  dry-run/`--apply` split, and the TUI dashboard.
- `hook.skip` and `githooks.severity` with exact matching — and every
  skipped check announced on every commit.
- An installer that verifies its download against published checksums, and
  an uninstall that removes exactly the four shims it wrote.
- v1.0.0 followed a full security review; every finding landed with a
  committed reproduction.
