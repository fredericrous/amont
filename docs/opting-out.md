# Opting out

Four different things get called "turning it off". They are listed here
smallest first, because the right answer is usually the smallest one.

## 0. Tune it instead of turning it off

If what you want gone is a *rule* rather than a *check* — the gitmoji in every
subject, the 50-character description budget, the body wrapping — none of the
four answers below is the right one. Those are settings:

```sh
amont setup      # walks you through them, with the current values
```

This matters most for `commit-msg`, which is the one hook `hook.skip`
genuinely cannot reach (see below). Changing what it asks for is
the only lever there is, and it is a real one. See
[commit conventions](commit-convention.md#if-the-defaults-do-not-fit).

## 1. One command: `--no-verify`

```sh
git commit --no-verify
git push --no-verify
```

`git commit --no-verify` skips `pre-commit` **and** `commit-msg` for that one
commit; `git push --no-verify` skips the whole `pre-push` stage.
`prepare-commit-msg` and `post-commit` still run — that is git's behaviour,
not ours, and it is why a bypassed commit still gets its message prepared and
still fails to earn a [gate stamp](checks.md#moving-a-gate-entry-earlier).

Neither `commit-msg` nor `prepare-commit-msg` takes `hook.skip` or a severity
override: they are entrypoints rather than checks, so the keys below do not
name them. To get a message past `commit-msg` without bypassing everything
else, fix the message — or change what it asks for, which is what §0 is
about.

## 2. One run, one check: `-c`

```sh
git -c hook.skip=clippy commit -m "fix: …"
git -c hook.skip=clippy -c hook.skip=prettier commit -m "fix: …"
```

Nothing is written to config, so there is nothing to remember to undo.

## 3. Permanently, in this repository

```sh
git config --add hook.skip pre-commit-clippy   # that one check
git config --add hook.skip clippy              # that check, either trigger
git config --add hook.skip pre-commit          # every pre-commit check
```

**These are exact names, not globs.** `hook.skip e` reaches nothing at all, and
skipping `lint-js` leaves `lint-json-yaml` alone — a skip can never silently
couple two checks whose names happen to share a prefix.

To see and undo:

```sh
git config --get-all hook.skip
git config --unset-all hook.skip
```

A skipped check is **announced on every commit**, on purpose. A `hook.skip`
line nobody remembers writing is exactly how a repository ends up with a check
everyone believes is running.

### Usually better: warn instead of skip

```sh
git config amont.severity.clippy warn
git config amont.severity.pre-commit warn
```

The check still runs and still reports; it just stops failing the commit. You
keep the signal, which is the thing `hook.skip` throws away. This is the right
first move when adopting a check into an existing repository with a backlog.

## 4. Remove the hooks entirely

```sh
amont uninstall              # this repository
amont uninstall --binary     # …and the binary from ~/.local/bin
amont-fleet uninstall --root ~/Developer
```

This removes **our five shims and nothing else**. A hook you wrote yourself is
left alone and named in the output, whatever it is; a hook it cannot even read
is named too, rather than passed over in silence. `hook.skip` and
`amont.severity` are never touched — those are your statements about your
repository.

If `init.templateDir` is still set, uninstall says so loudly and gives you the
command to unset it. Without that, an uninstall you believed had finished would
leave every future `git clone` re-installing the hooks:

```sh
git config --global --unset init.templateDir
```

### Do not use `rm .git/hooks/*`

That glob deletes **every** hook in the
directory — including ones other tools installed and ones you wrote — in order
to remove five files that belong to us. `amont uninstall` exists precisely
so that removing our hooks never means removing yours.

## A repository is asking to run its own checks

If `amont list` shows a check with:

```
declared in an untrusted amont.conf — review it, then `amont trust`
```

then that repository has declared checks and they are **already not running**.
There is nothing to opt out of. Opting *in* is `amont trust`, after reading
the file. See [the trust model](trust.md).
