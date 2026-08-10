# Changelog

What an upgrader gets, in sentences. Each [GitHub
release](https://github.com/fredericrous/amont/releases) carries the
mechanical pull-request list too, generated; this file is the part a human
wrote, and the release workflow refuses to tag a version whose section is
missing here.

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
