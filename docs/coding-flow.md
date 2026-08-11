# Where the hooks fit in your flow

One commit, start to finish, with every place a hook steps in.

![amont catching a commit and letting the fixed one through](assets/amont-demo.gif)

## You hit `git commit`

If you set `commit.template`, the
[footer scaffold](https://github.com/fredericrous/amont/blob/main/message)
opens in your editor to help you write something meaningful. Or you are in a
hurry and write `git commit -m "Add to Cart"`, which is the interesting case,
because that is the one that gets stopped.

## `pre-commit`

Git runs it before the commit exists. All fifteen built-in checks fan out
**concurrently**, each reporting its own line, and a panic in one is isolated so
the other fourteen still report.

Most of them will say nothing, because most are inert in any given repository:
a check fires only when the commit touches files it understands *and* the
repository carries the configuration that opts into that tool. `amont list`
tells you which ones are live where you are standing.

Some checks **fix** rather than complain — `cargo fmt`, `prettier`, `ruff` —
and stage the result. What exactly they are allowed to touch, and how your
unstaged work survives it, is the subject of
[index fidelity and run modes](index-fidelity-and-run-modes.md); it is the most
carefully argued part of this codebase, because the failure it guards against
is losing work you had not committed.

If a blocking check fails, the commit is aborted and nothing has happened.

## `commit-msg`

Then git hands the message to `commit-msg`, which lints it against the
[conventions](commit-convention.md), wraps the body at 72 columns and groups
the footers.

This is the hook that rejects `Add to Cart`: no type prefix. `git commit -m
"feat: a cart the checks agree with"` passes.

`--no-verify` skips `commit-msg` along with `pre-commit` — that is git's
behaviour — but neither `hook.skip` nor a severity override names it, so
there is no way to turn it down and leave it on. So this is the one
hook whose rules are adjustable in themselves: `amont setup` sets the
subject and description limits, the body wrap, and where the type's gitmoji
goes (nowhere, by default).

## `git push`

`pre-push` runs its five checks **in sequence**, cheapest and most decisive
first — refuse a forbidden push before validating a branch name, and validate
everything structural before paying for a test suite.

It refuses a direct push to `main` or `master`; it requires a branch name of
the form `feat/3002-image-crop`, unless the branch is already on the remote; it
rebases your branch onto **its own** upstream, never onto the default branch,
and never when your tree is dirty; and then it runs the test suite of whatever
your commits actually touched.

By default that suite runs against your **working tree**, and says so — which
is fast, and is not what you are pushing. `git config
amont.testPushedTree true` runs it against a throwaway checkout of the
commits being pushed instead. See [the checks](checks.md).

Where a check can only recommend rather than act, it recommends. `pull-rebase`
warns when the default branch has moved ahead of you; it does not go and do
anything about it.

## Asking the same questions without committing

The hooks are not the only way to run the checks, and during adoption they are
the wrong way:

```sh
amont run                 # would my commit pass? (the staged set)
amont run --all-files     # does my working tree pass? (git ls-files)
amont run pre-commit-prettier
```

Those two questions differ on purpose. `--all-files` on a dirty tree reports on
content that is not committed and may never be — which is exactly what you want
when adopting a check into an existing repository, where `git add .` is not an
acceptable way to measure the mess.
