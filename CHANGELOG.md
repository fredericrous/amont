# Changelog

What an upgrader gets, in sentences. Each [GitHub
release](https://github.com/fredericrous/amont/releases) carries the
mechanical pull-request list too, generated; this file is the part a human
wrote, and the release workflow refuses to tag a version whose section is
missing here.

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
