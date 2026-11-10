# Hook architecture: one `Check` trait

Status: **shipped**. PRs 1–3 below are done, and `docs/rust-migration.md`
carried the result the rest of the way; what came after this document is
[index-fidelity-and-run-modes.md](index-fidelity-and-run-modes.md). Kept as
the design record — the "what is wrong today" section describes the state
before this landed, not the state now.

## What is wrong today

**Four tables keyed by check name**, kept in step by reconciliation tests:

| table | where | holds |
|---|---|---|
| `REGISTRY` | `greengate-runtime/registry.rs` | name → fn pointer |
| `PRE_COMMIT_CHECKS` | same file | order, pre-commit |
| `PRE_PUSH_CHECKS` | same file | order, pre-push |
| `LANGUAGES` | `greengate-fleet/checks.rs` | name → language scope |

Adding a check means editing three of them and writing a module. The tests that
keep them aligned are good tests, but they exist to police a shape that should
not be splittable in the first place.

**Three entry-point signatures and eight differently-named entry points**,
reconciled by closures in the registry. The signatures are
`(&[OsString])`, `(&str, &[OsString])` and `(&[PushRef])`; the eight functions
not called `run` are `ruff`, `pyright`, `argo_lint`, `kube_linter`,
`kubeconform`, `fmt`, `clippy` and `test`.

Three shapes is not itself scandalous — the closures adapt them fine. It matters
because a uniform signature is what lets a check be a value rather than a
special case, and that is what makes an external check indistinguishable from a
built-in to the dispatcher.

**Two models of "does this apply here".** Each check scopes itself internally
from staged files and the nearest manifest; the dashboard *separately* infers
applicability from root manifests, and `checks.rs` documents its own answer as
an approximation. Two implementations of one question, one of which admits it
is guessing.

**No extension point at all.** File-discovered sub-hooks were removed when
checks moved in-process (they had two users in 96 repos). A third party now has
no way to add a check without recompiling the binary.

**Severity is implicit in a return value.** Fifteen sites warn and then return
0. Those collapse two different situations, which is the finding that most
changes this design — see below.

## The pattern

One trait, two implementors. Strategy, with the metadata attached to the
strategy rather than kept in a parallel table.

```rust
pub trait Check {
    fn name(&self) -> &str;
    fn stage(&self) -> Stage;          // PreCommit | PrePush
    fn scope(&self) -> Scope;          // declarative; see below
    fn severity(&self) -> Severity;    // Block | Warn
    fn run(&self, ctx: &Ctx) -> Outcome;
}
```

- **`Builtin`** wraps a fn pointer. One `const` descriptor per check carries
  name, stage, scope and severity beside the function.
- **`External`** runs a command declared in a committed manifest.

The dispatcher holds `Vec<Box<dyn Check>>` — built-ins in declared order, then
externals — and stops caring which is which.

What this removes: four tables become one declaration per check, and the
reconciliation tests become **unnecessary rather than merely passing**. That is
the win. Three signatures and eight entry-point names become one shape. `scope()` becomes authoritative, so the
dashboard asks the check instead of guessing, and the approximation caveat can
be deleted rather than documented.

## `Outcome` distinguishes three things a check can mean

```rust
pub enum Outcome {
    Passed,
    Failed,       // ran, found a problem
    Warned,       // ran, found something non-blocking
    Fixed,        // ran, found a problem, REPAIRED it
    Unavailable,  // COULD NOT RUN
}
```

There are FIVE. `Fixed` arrived with `Fix::Rewrite` (see
`docs/index-fidelity-and-run-modes.md` §2): the check ran, found a problem, and
repaired it, and the commit proceeds with the repair staged. That is neither
`Passed` — something happened and the author's files changed, which they should
be told — nor `Failed`, since nothing is blocking. Reachable only from a
`Stage::PreCommit` declaration, which the compiler enforces: a `pre-push` hook
must not modify the worktree or index, or the pushed commit would differ from
the tree the developer is looking at.

Shipped without the `detail` / `reason` payloads the sketch carried. Every check
already prints its own diagnosis at the moment it has the context to phrase it;
threading the same string back for the dispatcher to print again produced two
messages about one problem. The variant is the whole signal.

**`Failed` is NOT `Default` — there is no `Default`, deliberately.** It was
`Failed`, to fill the slot of a check whose thread died, and `check.rs` records
why that was removed:

> Deliberately NO `Default`. It used to be `Failed`, to fill the slot of a check
> whose thread died — a real rule, but `Default` means "the neutral value" to
> every reader and to every `#[derive(Default)]` that might later contain one.
> The rule is now written where it applies, in the runner.

So the rule survives and only its location changed: `dispatch::run_stage` passes
`Outcome::Failed` explicitly as the fill value, under a comment saying "a check
whose thread died has not passed". Stated where the slot is filled, rather than
hidden in a trait impl that any future `#[derive(Default)]` would silently
inherit.

`Unavailable` is the important addition. Today `ruff config found but no
ruff/uvx binary` prints a warning and returns 0, which is indistinguishable
from ruff running clean — to the dispatcher, and to the dashboard. A repo where
a check has silently never executed reads as a repo where it passes.

That is the same failure that `hook.skip` had before the dispatcher announced
skipped checks, and it cost three PRs to notice there. Modelling it means the
dashboard can show *ran clean* separately from *never ran*, which is the
difference between a green fleet and an unverified one.

Classifying the fifteen sites turned up an ordering bug it would not otherwise
have found. Three checks — yamllint, kube-linter, kubeconform — tested for their
binary BEFORE testing whether the repo had opted in, so a repo that never wanted
yamllint was told to install it, and under `Outcome` would have reported a gap it
did not have. One repo in the fleet configures yamllint; the nag reached the
other ninety-five. All three now test the opt-in first, and `Unavailable` means
what it says: this repo asked for the check and the tool was missing.

One site was reclassified in the other direction. An unpinned `uvx ruff` prints a
caveat about which ruff spoke — but it RAN, and a clean verdict from it is a
pass. The caveat is advice, not a gap.

## `Severity` is declared, and choosable

`Block` fails the stage; `Warn` reports and continues. It lives on the check, so
a built-in and an external are governed the same way.

Two consequences worth stating:

**Fail-fast applies only to `Block`.** `pre-push` stops at the first failure
because later steps are expensive and their preconditions are gone. A `Warn`
check that finds something must not stop the chain — it has not invalidated
anything.

**A severity override is a better escape hatch than `hook.skip`.**
`git config amont.severity.<check> warn` downgrades a check instead of
disabling it. `hook.skip` is all-or-nothing and, as measured, invisible enough
that a one-line config edit could disable everything unnoticed. A downgrade keeps
the signal
and removes only the block, which is what people usually want when they reach
for `--no-verify`. I would ship this alongside, and expect it to become the
common case.

`Severity::parse` and `Severity::as_str` are the ONE mapping between the
configured words and the enum, and `registry::effective_override` is the one
answer to "what will the dispatcher apply here". There were four of the former
and two of the latter — the dashboard's copies being its PREDICTION of the
dispatcher, which is the one thing it must never get wrong. It did: the
dispatcher asks `--get` (last entry wins), the dashboard listed every entry from
`--get-regexp` and treated each as authoritative, so a global `warn` overridden
by a local `block` was reported as a live downgrade.

Shipped, with one property the sketch did not state: an unrecognised value falls
back to the declared severity rather than to `warn`. Git validates nothing here,
so a typo would otherwise be a silent disable — the exact failure this feature
exists to replace.

**And the dashboard has to show it.** A downgrade is quieter than a skip: the
check runs, prints its failure in red, and the commit passes. Nothing on screen
distinguishes it from enforcement, so the fleet view carries a `WARN` column and
a per-repo `amont.severity` block. That block separates three cases a config
line cannot: a real downgrade, an explicit `block` (the default written out), and
a line that changes nothing because the check name or the value is misspelt.

## Scope, declared rather than reimplemented

Scoping is a **conjunction**, not a choice between alternatives:

```
ruff       .py/.pyi     AND  ruff.toml | .ruff.toml | pyproject [tool.ruff]
yamllint   .yaml/.yml   AND  .yamllint.yaml | .yamllint.yml | .yamllint
prettier   js-ish       AND  .prettierrc | .prettierrc.json | …
clippy     .rs          AND  Cargo.toml
```

So it is a struct, not an enum:

```rust
pub struct Scope {
    /// Extensions that trigger it. Empty = any change.
    files: &'static [&'static str],
    /// Config paths that opt the repo in. Empty = always on.
    opt_in: &'static [&'static str],
}
```

An earlier draft made these alternatives — `StagedFiles(..)` **or**
`Manifest(..)` — plus a `Custom` escape hatch for anything that fitted neither.
That was wrong twice over. Neither variant expresses "both", so every check with
an opt-in config would have fallen into `Custom`; an escape hatch that absorbs
most of the set leaves the dashboard knowing nothing, which is precisely the
guessing this trait exists to remove.

All twenty checks fit the struct. `merge-conflict` is `files: [], opt_in: []`.
`package-lock` is `files: [], opt_in: ["package.json"]`. `kube-linter` — one of
the cases `Custom` was invented for — reads repo-root `.kube-linter*.yaml`,
which is just an `opt_in` entry.

### Coarse declaration, precise execution

`rust_tools` resolves the NEAREST ancestor `Cargo.toml`, which no static
declaration captures. Its `Scope` therefore says `opt_in: ["Cargo.toml"]`,
meaning "somewhere in this repo" — coarser than what the check enforces when it
runs, and deliberately so.

That is safe because the two readers ask different questions. The dispatcher
asks "does this apply to the staged files of this commit" and the check answers
precisely, as it does today. The dashboard asks "would this ever fire here", for
which the coarse answer is correct. One declaration, evaluated against staged
files in one case and tracked files in the other.

Over-approximating is also the safe direction: showing a check as applicable
when it happens not to fire for a given commit is a small inaccuracy, while
`Custom` meant the dashboard could not answer at all.

## External checks

A third party cannot add a Rust module without rebuilding the binary, so
extension means declared commands.

The manifest is **committed at the repo root**, which is the point.
`.git/hooks` is not committed, so a team could never share a custom hook — a
worse flaw than the lexicographic ordering usually cited against the old
filename-prefix mechanism.

```
# amont.conf — stage  name        scope     severity  command
pre-commit        shellcheck  *.sh      block     scripts/lint-shell.sh
pre-push          smoke       *         warn      make smoke
```

Whitespace-delimited, order of file, ~20 lines of std parsing. Full reference in
[custom-checks.md](custom-checks.md).

Three rules the sketch left open, all decided the same way — by asking what a
committed text file should be able to do to a hook chain:

- **A built-in's name is refused.** An external calling itself
  `pre-push-branch-protect` would either shadow the built-in or silently lose to
  it.
- **A duplicate ID is refused** — the same name on two triggers is two checks
  and both run; the same name twice on one trigger is the clash. It could be
  addressed by neither `hook.skip`
  nor a severity override, so it would run anonymously.
- **A line that cannot be parsed is not skipped.** It becomes a check that runs
  to `Unavailable` and says which line and why, appearing in the same "could not
  run" roll-up as a missing binary. Dropping it silently would mean a check
  somebody committed months ago has never run and nothing ever said so.

**On the format**: this is a judgement, not a constraint. The dependency guard
(`scripts/check-no-deps.sh`) is a strong default about the commit path's supply
chain, not a prohibition — see its comment. TOML would be nicer to write and
costs a dependency tree running on every commit in 96 repos. For four fields I
take the twenty lines; for a genuinely rich format the trade is worth reopening.

Externals run **after** built-ins, and cannot be reordered ahead of them: a
third-party command should not be able to delay `branch-protect`.

## Developer experience

- `greengate list` — every check, stage, scope, severity, and whether it would
  run *here*. Externals are listed too, marked `(declared)`, and a line that
  could not be parsed gets its own glyph: correctly-inert, disabled, and
  unusable are three different things and none may look like another.
- Adding a built-in: one module plus one descriptor; the compiler names what is
  missing.
- Adding an external: edit a committed file, no rebuild.
- `greengate-fleet` gains third-party checks in its views, which it cannot see
  today at all.

### Named future: `greengate explain <check>`

Not built. It was in the list above, among shipped DX, which made "why didn't
prettier run" look like a question the tool answers.

Half of it exists: `greengate list` says whether a check would run HERE and why
not — inert, skipped, or an unusable declaration, as three distinct glyphs. What
is missing is the other half, the retrospective one: why a check did or did not
fire on the commit you just made. Today that is a code-reading exercise.

## Migration

**PR 1 — the trait, no behaviour change.** All 19 modules and the dispatcher.
High mechanical risk, no user-visible payoff, so it lands alone, proved inert
by a differential over hook output rather than by the test suite (tests will
legitimately change shape).

**PR 2 — `Outcome` and `Severity`.** DONE. The 15 warn-and-return-0 sites are
classified one at a time: 12 `Unavailable`, 2 `Warned` (a waived-past lockfile, a
first-time author identity), 1 left `Passed` (unpinned ruff, which ran). Config
override shipped. Dashboard shows downgrades apart from enforcement.

**PR 3 — external checks.** DONE. Manifest, parser, `External`, `greengate list`,
and the fleet views. Two things the sketch did not anticipate:

*`Scope` had to gate externals at RUN time.* For a built-in it is a declaration
the dashboard reads, because the check enforces its own scope in its first three
lines. A declared command cannot — it has no idea what was staged — so without a
gate here `*.sh` would have run on every commit and the column would have been
decoration. Which files it is judged against depends on the stage: what is staged
for a commit, what is in the range being pushed for a push.

*Parsing had to be split in two.* `External` holds a `Scope`, whose `&'static`
slices are leaked; that is fine in a hook process which reads one manifest and
exits, and wrong in a dashboard that reads ninety-six and may re-read them on
every refresh. `parse_lines` returns owned `Line`s and only `External::from`
leaks, with a test pinning the two to the same answers.

## What came next

Built, in `docs/`: this document's plan shipped as #57–#67. The comparison
against `pre-commit`, `lefthook` and `husky` that followed is
[index-fidelity-and-run-modes.md](index-fidelity-and-run-modes.md) — four ideas
worth taking, one refused on the record, and one correctness gap this document
did not notice.

## Open decisions

1. ~~**Does `Custom` scope survive?**~~ RESOLVED: no, and the question was
   better than it looked. Checking what the checks actually key on showed the
   `Scope` enum modelled alternatives where the truth is a conjunction, so
   `Custom` would have swallowed most of the set rather than the one case it was
   written for. `Scope` is a struct now and every check fits it.
2. **Can an external check be `Block` at all?** Decided: yes, severity is on the
   trait and the author chooses. Worth revisiting if a repo ever ships a hostile
   or flaky one.
2b. ~~**Can an external run before a built-in?**~~ RESOLVED: no, and not
   configurably. Externals are appended to each stage.
3. **Does `greengate list` belong in the hook binary or the fleet tool?** The
   fleet tool has the nicer output; the hook binary is what is installed
   everywhere.
