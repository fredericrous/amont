//! The hook registry — one table, one signature.
//!
//! Before this, dispatch was a 20-arm `match` in main.rs and handlers had four
//! different signatures (`run(&args)`, `run(&hook, &args)`, `argo_lint(&args)`,
//! …). Two costs, one of them real:
//!
//!   - adding a hook meant touching a match arm, a module, and remembering
//!     which signature that one used;
//!   - the hook NAME was written twice — in the arm and as the shim's filename
//!     — with nothing checking they agree. A shim the binary does not recognise
//!     exits 2 and blocks the commit; a handler with no shim is dead code. The
//!     consistency test below turns that pairing into something enforced.

use std::ffi::OsString;
use std::path::Path;

use crate::check::{
    Builtin, Check, Fix, GitState, Outcome, Reach, Scope, Severity, Stage, Verdict,
};
use crate::pushrefs::PushRefs;
use crate::{dispatch, hooks};

/// Everything a hook is given. One shape for all of them, so a handler that
/// needs the invoked name (ban-terms excludes its own source by it) or the
/// hooks directory (the dispatchers glob it) does not need its own signature.
pub struct Ctx<'a> {
    /// The hook name as invoked.
    pub name: &'a str,
    /// Arguments git passed the hook.
    pub args: &'a [OsString],
    /// Directory the shim lives in. Only foreign sub-hooks are found here now;
    /// our own checks are functions in this binary.
    pub hooks_dir: &'a Path,
    /// The pre-push ref list, read from stdin at most once and lent to every
    /// check that asks. See `pushrefs`.
    pub push: &'a PushRefs,
    /// What this repository's manifest declares — parsed and trust-gated once
    /// by the entrypoint, lent to everything downstream. The same shape as
    /// `push`: read once, owned high, borrowed everywhere.
    pub manifest: &'a crate::manifest::Manifest,
}

pub type HookFn = fn(&Ctx) -> Verdict;

/// name → handler. The single place a hook is registered.
/// The five hook names git itself invokes. Everything else is a `Check`.
pub const ENTRYPOINTS: &[(&str, HookFn)] = &[
    ("pre-commit", dispatch::pre_commit),
    ("pre-push", dispatch::pre_push),
    // The message hooks are pure convention — a subject shape and a decoration
    // — so under `amont.conventions declared` they quietly stand down in a
    // repository that never subscribed. Quietly: the pre-commit stage has
    // already said, once, that the conventions are held back here; a second
    // and third line every commit would be the noise that gets amont
    // uninstalled. post-commit stays: it records, never opines.
    ("commit-msg", |ctx| {
        if !dispatch::conventions_apply(ctx.manifest) {
            return Verdict::Proceed;
        }
        hooks::commit_msg::run(ctx.args)
    }),
    ("prepare-commit-msg", |ctx| {
        if !dispatch::conventions_apply(ctx.manifest) {
            return Verdict::Proceed;
        }
        hooks::prepare_commit_msg::run(ctx.args)
    }),
    ("post-commit", |ctx| hooks::post_commit::run(ctx)),
];

/// Every check, in the order its stage runs them.
///
/// ONE declaration each: name, stage, scope and function together. This
/// replaced `REGISTRY` plus `PRE_COMMIT_CHECKS` plus `PRE_PUSH_CHECKS` plus the
/// fleet crate's `LANGUAGES` — four tables keyed by the same string, kept in
/// step by reconciliation tests that are now unnecessary rather than passing.
///
/// pre-push order is the cost order: refuse a forbidden push before validating
/// a name, and validate everything structural before paying for a test suite.
/// Operations during which a content check cannot say anything useful: half the
/// tree is somebody else's work, and you cannot fix it from inside the
/// operation anyway.
///
/// NOT applied to `merge-conflict` or `ban-terms`. Those are exactly the checks
/// you want during a resolution commit — leaving a conflict marker in the
/// commit that RESOLVES a merge is the bug, and importing a banned term from
/// the other branch is the other one. The old behaviour skipped the whole
/// pre-commit stage during a cherry-pick, which silenced both.
const MID_OPERATION: &[GitState] = &[
    GitState::Merge,
    GitState::Rebase,
    GitState::CherryPick,
    GitState::Revert,
];

pub const CHECKS: &[Builtin] = &[
    // ---- pre-commit ----
    Builtin {
        name: "pre-commit-argo-lint",
        stage: Stage::PreCommit,
        scope: Scope::new(
            hooks::k8s::EXTS,
            &["kustomization.yaml", "kustomization.yml"],
        )
        .not_during(MID_OPERATION),
        severity: Severity::Block,
        fix: Fix::None,
        reach: Reach::Convention,
        run: |ctx| hooks::k8s::argo_lint(ctx.args),
    },
    Builtin {
        name: "pre-commit-ban-terms",
        stage: Stage::PreCommit,
        scope: Scope::files(hooks::ban_terms::EXTS),
        severity: Severity::Block,
        fix: Fix::None,
        reach: Reach::Safety,
        run: |ctx| hooks::ban_terms::run(ctx.name, ctx.args),
    },
    // The push-time contract, said at the first commit — when renaming the
    // branch costs one command and zero rework. Same short name as the
    // pre-push check on purpose: `hook.skip branch-pattern` silences the
    // rule, not one of its two voices.
    Builtin {
        name: "pre-commit-branch-pattern",
        stage: Stage::PreCommit,
        scope: Scope::ALWAYS,
        severity: Severity::Warn,
        fix: Fix::None,
        reach: Reach::Convention,
        run: |_ctx| hooks::branch_pattern::early(),
    },
    Builtin {
        name: "pre-commit-cargo-fmt",
        stage: Stage::PreCommit,
        scope: Scope::new(hooks::rust_tools::EXTS, &["Cargo.toml"]).not_during(MID_OPERATION),
        severity: Severity::Block,
        fix: Fix::Rewrite,
        reach: Reach::Convention,
        run: |ctx| hooks::rust_tools::fmt(ctx.args),
    },
    Builtin {
        name: "pre-commit-clippy",
        stage: Stage::PreCommit,
        scope: Scope::new(hooks::rust_tools::EXTS, &["Cargo.toml"]).not_during(MID_OPERATION),
        severity: Severity::Block,
        fix: Fix::None,
        reach: Reach::Convention,
        run: |ctx| hooks::rust_tools::clippy(ctx.args),
    },
    Builtin {
        name: "pre-commit-go-vet",
        stage: Stage::PreCommit,
        scope: Scope::new(hooks::go_tools::EXTS, &["go.mod"]).not_during(MID_OPERATION),
        severity: Severity::Block,
        fix: Fix::None,
        reach: Reach::Convention,
        run: |ctx| hooks::go_tools::vet(ctx.args),
    },
    Builtin {
        name: "pre-commit-gofmt",
        stage: Stage::PreCommit,
        scope: Scope::new(hooks::go_tools::EXTS, &["go.mod"]).not_during(MID_OPERATION),
        severity: Severity::Block,
        fix: Fix::Rewrite,
        reach: Reach::Convention,
        run: |ctx| hooks::go_tools::fmt(ctx.args),
    },
    Builtin {
        name: "pre-commit-kube-linter",
        stage: Stage::PreCommit,
        scope: Scope::new(
            hooks::k8s::EXTS,
            &[".kube-linter*.yaml", ".kube-linter*.yml"],
        )
        .not_during(MID_OPERATION),
        severity: Severity::Block,
        fix: Fix::None,
        reach: Reach::Convention,
        run: |ctx| hooks::k8s::kube_linter(ctx.args),
    },
    Builtin {
        name: "pre-commit-kubeconform",
        stage: Stage::PreCommit,
        scope: Scope::new(
            hooks::k8s::EXTS,
            &["kustomization.yaml", "kustomization.yml"],
        )
        .not_during(MID_OPERATION),
        severity: Severity::Block,
        fix: Fix::None,
        reach: Reach::Convention,
        run: |ctx| hooks::k8s::kubeconform(ctx.args),
    },
    Builtin {
        name: "pre-commit-large-files",
        stage: Stage::PreCommit,
        scope: Scope::ALWAYS,
        severity: Severity::Block,
        fix: Fix::None,
        reach: Reach::Safety,
        run: |_ctx| hooks::large_files::staged(),
    },
    Builtin {
        name: "pre-commit-lint-js",
        stage: Stage::PreCommit,
        scope: Scope::new(hooks::lint_js::EXTS, &["package.json"]).not_during(MID_OPERATION),
        severity: Severity::Block,
        fix: Fix::None,
        reach: Reach::Convention,
        run: |ctx| hooks::lint_js::run(ctx.args),
    },
    Builtin {
        name: "pre-commit-lint-json-yaml",
        stage: Stage::PreCommit,
        scope: Scope::files(hooks::lint_json_yaml::EXTS).not_during(MID_OPERATION),
        severity: Severity::Block,
        fix: Fix::None,
        reach: Reach::Convention,
        run: |ctx| hooks::lint_json_yaml::run(ctx.args),
    },
    Builtin {
        name: "pre-commit-merge-conflict",
        stage: Stage::PreCommit,
        scope: Scope::ALWAYS,
        severity: Severity::Block,
        fix: Fix::None,
        reach: Reach::Safety,
        run: |ctx| hooks::merge_conflict::run(ctx.name, ctx.args),
    },
    Builtin {
        name: "pre-commit-package-lock",
        stage: Stage::PreCommit,
        scope: Scope::new(&[], &["package.json"]),
        severity: Severity::Block,
        fix: Fix::None,
        reach: Reach::Convention,
        run: |ctx| hooks::package_lock::run(ctx.args),
    },
    Builtin {
        name: "pre-commit-prettier",
        stage: Stage::PreCommit,
        scope: Scope::new(
            &[],
            &[
                ".prettierrc",
                ".prettierrc.json",
                ".prettierrc.yml",
                ".prettierrc.yaml",
                ".prettierrc.js",
                "prettier.config.js",
            ],
        )
        .not_during(MID_OPERATION),
        severity: Severity::Block,
        fix: Fix::Rewrite,
        reach: Reach::Convention,
        run: |ctx| hooks::prettier::run(ctx.args),
    },
    Builtin {
        name: "pre-commit-pyright",
        stage: Stage::PreCommit,
        scope: Scope::new(
            hooks::python_tools::EXTS,
            &[
                "pyrightconfig.json",
                "pyrightconfig.jsonc",
                "pyproject.toml",
            ],
        )
        .not_during(MID_OPERATION),
        severity: Severity::Block,
        fix: Fix::None,
        reach: Reach::Convention,
        run: |ctx| hooks::python_tools::pyright(ctx.args),
    },
    Builtin {
        name: "pre-commit-ruff",
        stage: Stage::PreCommit,
        scope: Scope::new(
            hooks::python_tools::EXTS,
            &["ruff.toml", ".ruff.toml", "pyproject.toml"],
        )
        .not_during(MID_OPERATION),
        severity: Severity::Block,
        fix: Fix::Rewrite,
        reach: Reach::Convention,
        run: |ctx| hooks::python_tools::ruff(ctx.args),
    },
    // A staged credential is a ten-second fix; a pushed one is an
    // incident. Both halves of that sentence are checks — see
    // `hooks::secrets` for why the push half exists at all.
    Builtin {
        name: "pre-commit-secrets",
        stage: Stage::PreCommit,
        scope: Scope::ALWAYS,
        severity: Severity::Block,
        fix: Fix::None,
        reach: Reach::Safety,
        run: |_ctx| hooks::secrets::staged(),
    },
    Builtin {
        name: "pre-commit-usual-name",
        stage: Stage::PreCommit,
        scope: Scope::ALWAYS,
        severity: Severity::Block,
        fix: Fix::None,
        reach: Reach::Convention,
        run: |ctx| hooks::usual_name::run(ctx.args),
    },
    Builtin {
        name: "pre-commit-yamllint",
        stage: Stage::PreCommit,
        scope: Scope::new(
            hooks::yamllint::EXTS,
            &[".yamllint.yaml", ".yamllint.yml", ".yamllint"],
        )
        .not_during(MID_OPERATION),
        severity: Severity::Block,
        fix: Fix::None,
        reach: Reach::Convention,
        run: |ctx| hooks::yamllint::run(ctx.args),
    },
    // ---- pre-push, cheapest and most decisive first ----
    Builtin {
        name: "pre-push-branch-protect",
        stage: Stage::PrePush,
        scope: Scope::ALWAYS,
        severity: Severity::Block,
        fix: Fix::None,
        reach: Reach::Convention,
        run: |ctx| hooks::branch_protect::run(ctx.push.get()),
    },
    Builtin {
        name: "pre-push-branch-pattern",
        stage: Stage::PrePush,
        scope: Scope::ALWAYS,
        severity: Severity::Block,
        fix: Fix::None,
        reach: Reach::Convention,
        run: |ctx| hooks::branch_pattern::run(ctx.push.get(), ctx.args),
    },
    Builtin {
        name: "pre-push-secrets",
        stage: Stage::PrePush,
        scope: Scope::ALWAYS,
        severity: Severity::Block,
        fix: Fix::None,
        reach: Reach::Safety,
        run: |ctx| hooks::secrets::pushed(ctx.push.get()),
    },
    Builtin {
        name: "pre-push-pull-rebase",
        stage: Stage::PrePush,
        scope: Scope::ALWAYS.not_during(&[GitState::Rebase, GitState::Merge]),
        severity: Severity::Block,
        fix: Fix::None,
        reach: Reach::Convention,
        run: |ctx| hooks::pull_rebase::run(ctx.args),
    },
    // The four dependency audits sit between the structural checks and the
    // test suites: network-bound but seconds, where a suite is minutes —
    // and when a v* tag is in the push, failing here saves the suite's
    // whole cost. Each opts in by the LOCKFILE its tool audits: an audit
    // without a resolved tree audits a guess. On a branch push they only
    // warn; the blocking case is the release tag — see `hooks::audit`.
    Builtin {
        name: "pre-push-audit-go",
        stage: Stage::PrePush,
        scope: Scope::new(&[], &["go.sum"]),
        severity: Severity::Block,
        fix: Fix::None,
        reach: Reach::Convention,
        run: |ctx| hooks::audit::go(ctx.push.get()),
    },
    Builtin {
        name: "pre-push-audit-js",
        stage: Stage::PrePush,
        scope: Scope::new(&[], &["package-lock.json"]),
        severity: Severity::Block,
        fix: Fix::None,
        reach: Reach::Convention,
        run: |ctx| hooks::audit::js(ctx.push.get()),
    },
    Builtin {
        name: "pre-push-audit-python",
        stage: Stage::PrePush,
        scope: Scope::new(&[], &["requirements.txt"]),
        severity: Severity::Block,
        fix: Fix::None,
        reach: Reach::Convention,
        run: |ctx| hooks::audit::python(ctx.push.get()),
    },
    Builtin {
        name: "pre-push-audit-rust",
        stage: Stage::PrePush,
        scope: Scope::new(&[], &["Cargo.lock"]),
        severity: Severity::Block,
        fix: Fix::None,
        reach: Reach::Convention,
        run: |ctx| hooks::audit::rust(ctx.push.get()),
    },
    Builtin {
        name: "pre-push-run-tests-js",
        stage: Stage::PrePush,
        scope: Scope::new(hooks::run_tests::JS_EXTS, &["package.json"])
            .not_during(&[GitState::Bisect, GitState::Rebase]),
        severity: Severity::Block,
        fix: Fix::None,
        reach: Reach::Convention,
        run: |ctx| hooks::run_tests::run(ctx.push.get(), &ctx.manifest.externals),
    },
    Builtin {
        name: "pre-push-cargo-test",
        stage: Stage::PrePush,
        scope: Scope::new(hooks::rust_tools::EXTS, &["Cargo.toml"])
            .not_during(&[GitState::Bisect, GitState::Rebase]),
        severity: Severity::Block,
        fix: Fix::None,
        reach: Reach::Convention,
        run: |ctx| hooks::rust_tools::test(ctx.push.get()),
    },
    Builtin {
        name: "pre-push-go-test",
        stage: Stage::PrePush,
        scope: Scope::new(hooks::go_tools::EXTS, &["go.mod"])
            .not_during(&[GitState::Bisect, GitState::Rebase]),
        severity: Severity::Block,
        fix: Fix::None,
        reach: Reach::Convention,
        run: |ctx| hooks::go_tools::test(ctx.push.get()),
    },
    Builtin {
        name: "pre-push-pytest",
        stage: Stage::PrePush,
        scope: Scope::new(hooks::python_tools::EXTS, &["pytest.ini", "conftest.py"])
            .not_during(&[GitState::Bisect, GitState::Rebase]),
        severity: Severity::Block,
        fix: Fix::None,
        reach: Reach::Convention,
        run: |ctx| hooks::python_tools::pytest(ctx.push.get()),
    },
];

/// A check's severity, after any per-repository override.
///
/// `git config amont.severity.<check> warn` downgrades a blocking check to a
/// warning. Unlike `hook.skip` it keeps the signal: the check still runs and
/// still reports, it just stops failing the commit.
pub fn severity_of(check: &dyn Check) -> Severity {
    effective_override(None, check.name()).unwrap_or_else(|| check.severity())
}

/// The config key a severity override lives under.
pub fn severity_key(check: &str) -> String {
    format!("amont.severity.{check}")
}

/// Every severity override visible here, resolved the way `--get` resolves it.
///
/// ONE subprocess for the whole stage, and — more importantly — a VALUE. The
/// dispatcher used to call `severity_of` inside the loop that classifies
/// outcomes, which put a `git` spawn in the middle of a fold and made the fold
/// impossible to test without a repository.
///
/// `--get-regexp` emits entries in precedence order, so folding with overwrite
/// lands on the same answer `--get` gives. That equivalence is not assumed:
/// `the_batch_agrees_with_the_authority` pins it against `effective_override`.
#[derive(Debug, Default, Clone)]
pub struct Overrides(std::collections::BTreeMap<String, Severity>);

impl Overrides {
    pub fn read() -> Overrides {
        Overrides::from_config(crate::git::stdout(&[
            "config",
            "--get-regexp",
            r"^amont\.severity\.",
        ]))
    }

    /// Built from `--get-regexp`-shaped text (`amont.severity.<check>
    /// <value>` per line, in git's own precedence order — system, then
    /// global, then local, then includes). `pub` so a reader that has
    /// already fetched those lines for its own reasons (the fleet dashboard
    /// needs the origin of each, which `Overrides` does not track) can still
    /// ask this — the one place that must not get precedence wrong — rather
    /// than re-deriving it from the lines by hand.
    pub fn from_config(out: Option<String>) -> Overrides {
        let mut map = std::collections::BTreeMap::new();
        for line in out.as_deref().unwrap_or_default().lines() {
            let Some((key, value)) = line.split_once(' ') else {
                continue;
            };
            let Some(check) = key.strip_prefix("amont.severity.") else {
                continue;
            };
            // Later entries overwrite earlier ones — git's own precedence,
            // observed rather than reimplemented.
            match Severity::parse(value.trim()) {
                Some(s) => {
                    map.insert(check.to_string(), s);
                }
                // An unrecognised value is not an override at all, and must not
                // shadow a valid earlier one either.
                None => {
                    map.remove(check);
                }
            }
        }
        Overrides(map)
    }

    /// The configured key that applies to `check`, and what it says.
    ///
    /// Several keys can name one check — `pre-commit` and `clippy` and
    /// `pre-commit-clippy` all reach `pre-commit-clippy`. The most specific
    /// wins, which is the rule anybody would guess and the only one that lets
    /// you downgrade a whole trigger and then exempt one check from it.
    pub fn applied_to(&self, check: &str) -> Option<(&str, Severity)> {
        self.0
            .iter()
            .filter_map(|(pattern, severity)| {
                crate::names_check(check, pattern).map(|m| (m, pattern.as_str(), *severity))
            })
            .max_by_key(|(m, _, _)| *m)
            .map(|(_, pattern, severity)| (pattern, severity))
    }

    /// The severity to apply to `check`, override or declared.
    pub fn of(&self, check: &dyn Check) -> Severity {
        self.applied_to(check.name())
            .map(|(_, severity)| severity)
            .unwrap_or_else(|| check.severity())
    }
}

#[cfg(test)]
mod precedence {
    use super::{Overrides, Severity};

    fn overrides(lines: &[&str]) -> Overrides {
        let text = lines
            .iter()
            .map(|l| format!("amont.severity.{l}\n"))
            .collect::<String>();
        Overrides::from_config(Some(text))
    }

    /// The whole reason triggers and short names are allowed as keys: downgrade
    /// a trigger wholesale, then say something different about one check. If the
    /// broader key won instead, the exemption would be unwritable.
    #[test]
    fn the_more_specific_key_wins() {
        let both = overrides(&["pre-commit warn", "pre-commit-clippy block"]);
        assert_eq!(
            both.applied_to("pre-commit-clippy"),
            Some(("pre-commit-clippy", Severity::Block)),
            "a full id beats its trigger"
        );
        assert_eq!(
            both.applied_to("pre-commit-shellcheck"),
            Some(("pre-commit", Severity::Warn)),
            "and the trigger still governs every check it did not exempt"
        );
    }

    /// Full id > short name > trigger, all three at once, so the ordering is
    /// pinned end to end rather than one pair at a time.
    #[test]
    fn the_three_ways_to_name_a_check_are_ranked() {
        let all = overrides(&["pre-commit warn", "clippy block", "pre-commit-clippy warn"]);
        assert_eq!(
            all.applied_to("pre-commit-clippy"),
            Some(("pre-commit-clippy", Severity::Warn))
        );

        let no_full = overrides(&["pre-commit warn", "clippy block"]);
        assert_eq!(
            no_full.applied_to("pre-commit-clippy"),
            Some(("clippy", Severity::Block)),
            "a short name beats a trigger"
        );
    }

    /// A key naming nothing must not become the answer by being the only one
    /// there — that is how a repo believes it downgraded a check it never named.
    #[test]
    fn a_key_that_names_no_check_applies_to_nothing() {
        let typo = overrides(&["clipy warn", "e warn", " warn"]);
        assert_eq!(typo.applied_to("pre-commit-clippy"), None);
    }
}

/// The override git would actually apply for `check`, or `None` if there is
/// none (or the value is not one this understands).
///
/// `--get`, NOT `--get-regexp`: git returns the LAST value, so a local `block`
/// beats a global `warn`. A reader that listed every entry instead and treated
/// each as authoritative would report a downgrade that the dispatcher does not
/// apply — which is exactly what the dashboard used to do.
///
/// `repo` is `None` for the current directory, which is where a hook runs.
pub fn effective_override(repo: Option<&Path>, check: &str) -> Option<Severity> {
    overrides_in(repo).applied_to(check).map(|(_, s)| s)
}

/// Which configured KEY applies to `check` here, if any.
///
/// The dashboard needs the key rather than the value: with three ways to name a
/// check, "which of these lines is the one doing something" is the question a
/// reader actually has.
pub fn effective_key(repo: Option<&Path>, check: &str) -> Option<String> {
    overrides_in(repo)
        .applied_to(check)
        .map(|(pattern, _)| pattern.to_string())
}

fn overrides_in(repo: Option<&Path>) -> Overrides {
    let args = ["config", "--get-regexp", r"^amont\.severity\."];
    Overrides::from_config(match repo {
        None => crate::git::stdout(&args),
        Some(dir) => crate::git::stdout_in(dir, &args),
    })
}

/// Built-in checks for one stage, in declared order.
pub fn stage_checks(stage: Stage) -> impl Iterator<Item = &'static Builtin> {
    CHECKS.iter().filter(move |check| check.stage == stage)
}

/// Every check for one stage — built-ins first, then whatever this repository
/// declares in `amont.conf`.
///
/// The order is not negotiable and not configurable. A third-party command must
/// not be able to delay `pre-push-branch-protect`, and appending is the only
/// arrangement in which it cannot.
pub fn all_stage_checks<'a>(
    stage: Stage,
    manifest: &'a crate::manifest::Manifest,
) -> Vec<&'a dyn Check> {
    let mut out: Vec<&'a dyn Check> = stage_checks(stage)
        .map(|check| check as &dyn Check)
        .collect();
    out.extend(
        manifest
            .externals
            .iter()
            .filter(|external| external.stage == stage)
            .map(|external| external as &dyn Check),
    );
    out
}

pub fn lookup(name: &str, manifest: &crate::manifest::Manifest) -> Option<HookFn> {
    if let Some((_, f)) = ENTRYPOINTS.iter().find(|(n, _)| *n == name) {
        return Some(*f);
    }
    // A check invoked directly by name — how the tests drive individual checks,
    // and how `amont <check>` works from a shell. Its Outcome collapses to an
    // exit code here, honouring severity, so a `warn` check invoked directly
    // reports without failing exactly as it does inside a dispatcher. The
    // closure re-resolves through the Ctx it is handed — a plain fn pointer
    // captures nothing, and the manifest travels ON the Ctx.
    if CHECKS.iter().any(|check| check.name == name)
        || manifest
            .externals
            .iter()
            .any(|external| external.id == name)
    {
        return Some(|ctx: &Ctx| {
            let check = one_named(ctx.name, ctx.manifest).expect("checked above");
            Verdict::blocking(matches!(
                (check.run(ctx), severity_of(check)),
                (Outcome::Failed, Severity::Block)
            ))
        });
    }
    None
}

/// One check by name, whichever kind it is. Built-ins are searched first, which
/// costs nothing because `manifest::parse` refuses a name a built-in already
/// holds — the two guards together mean neither kind can shadow the other.
pub fn one_named<'a>(name: &str, manifest: &'a crate::manifest::Manifest) -> Option<&'a dyn Check> {
    if let Some(builtin) = CHECKS.iter().find(|check| check.name == name) {
        return Some(builtin);
    }
    manifest
        .externals
        .iter()
        .find(|external| external.id == name)
        .map(|external| external as &dyn Check)
}

#[cfg(test)]
mod tests {
    use super::{lookup, Overrides, Reach, Severity, Stage, CHECKS, ENTRYPOINTS};
    use std::collections::BTreeSet;

    /// The batch reader must land on the same answer as the authority.
    ///
    /// `Overrides` folds `--get-regexp` output with overwrite; `effective_override`
    /// asks `--get`. They agree only because git emits entries in precedence
    /// order — an assumption, so it is asserted against real git rather than
    /// trusted.
    #[test]
    fn the_batch_agrees_with_the_authority() {
        let d = std::env::temp_dir().join(format!("ov-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&d)
                .output()
                .expect("git");
        };
        git(&["init", "-q", "--template=", "."]);
        let key = "amont.severity.pre-commit-merge-conflict";
        git(&["config", "--add", key, "warn"]);
        git(&["config", "--add", key, "block"]);

        let raw = std::process::Command::new("git")
            .args(["config", "--get-regexp", r"^amont\.severity\."])
            .current_dir(&d)
            .output()
            .expect("git");
        let batch = Overrides::from_config(Some(
            String::from_utf8_lossy(&raw.stdout).trim().to_string(),
        ));
        let authority =
            crate::git::stdout_in(&d, &["config", "--get", key]).and_then(|v| Severity::parse(&v));
        let _ = std::fs::remove_dir_all(&d);

        assert_eq!(
            authority,
            Some(Severity::Block),
            "git applies the last entry"
        );
        assert_eq!(
            batch.0.get("pre-commit-merge-conflict").copied(),
            authority,
            "the batch reader disagreed with `--get`"
        );
    }

    /// An unrecognised value is not an override, and must not shadow a valid
    /// earlier one either — a typo would otherwise silently restore the
    /// declared severity in a way nobody could see.
    #[test]
    fn an_unrecognised_value_clears_rather_than_overrides() {
        let o = Overrides::from_config(Some(
            "amont.severity.a warn\namont.severity.a advisory\namont.severity.b warn".to_string(),
        ));
        assert_eq!(o.0.get("a"), None, "a typo must not leave `warn` standing");
        assert_eq!(o.0.get("b").copied(), Some(Severity::Warn));
    }

    #[test]
    fn names_are_unique_across_entrypoints_and_checks() {
        let mut seen = BTreeSet::new();
        for n in ENTRYPOINTS
            .iter()
            .map(|(n, _)| *n)
            .chain(CHECKS.iter().map(|check| check.name))
        {
            assert!(seen.insert(n), "duplicate registration: {n}");
        }
    }

    /// Only FIVE files ship, and they are exactly the hook names git invokes.
    #[test]
    fn the_shipped_shims_are_exactly_the_git_invoked_hooks() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../templates/hooks");
        let mut shipped: Vec<String> = std::fs::read_dir(dir)
            .expect("templates/hooks")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        shipped.sort();
        assert_eq!(
            shipped,
            vec![
                "commit-msg",
                "post-commit",
                "pre-commit",
                "pre-push",
                "prepare-commit-msg"
            ]
        );
        let none = crate::manifest::Manifest::default();
        for name in &shipped {
            assert!(
                lookup(name, &none).is_some(),
                "shipped shim {name:?} has no handler"
            );
        }
    }

    /// Every check is reachable by name, which is how a shell — and the tests —
    /// invoke one directly.
    #[test]
    fn every_check_is_reachable_by_name() {
        let none = crate::manifest::Manifest::default();
        for check in CHECKS {
            assert!(
                lookup(check.name, &none).is_some(),
                "{} not reachable",
                check.name
            );
        }
        assert!(lookup("pre-commit-not-a-check", &none).is_none());
    }

    /// pre-push is serial and fail-fast, so declaration order IS cost order.
    #[test]
    fn pre_push_runs_cheapest_first() {
        let order: Vec<&str> = super::stage_checks(Stage::PrePush)
            .map(|check| check.name)
            .collect();
        assert_eq!(
            order,
            vec![
                "pre-push-branch-protect",
                "pre-push-branch-pattern",
                "pre-push-secrets",
                "pre-push-pull-rebase",
                "pre-push-audit-go",
                "pre-push-audit-js",
                "pre-push-audit-python",
                "pre-push-audit-rust",
                "pre-push-run-tests-js",
                "pre-push-cargo-test",
                "pre-push-go-test",
                "pre-push-pytest",
            ]
        );
    }

    /// What a check actually looks at, as opposed to what it DECLARES.
    ///
    /// `All` is a check that reads every staged path regardless of the
    /// extensions in its scope (ban-terms greps them all); `Exts(e)` is a check
    /// that filters by suffix, and `e` is the list it filters WITH.
    enum Consumes {
        All,
        Exts(&'static [&'static str]),
    }

    /// Every check, and the file set it consumes. The table exists so a NEW
    /// check cannot be added without somebody stating the answer.
    ///
    /// The incident: `pre-commit-lint-json-yaml` declared
    /// `[".json", ".yaml", ".yml"]` in the registry while `lint_json_yaml::run`
    /// asked `staged_files` for `[".yaml"]`. `amont list` reported the check
    /// as covering `.yml`, the fleet dashboard agreed, and a staged, broken
    /// `x.yml` returned `Outcome::Passed` with no output whatsoever. Nothing
    /// connected the two lists, so nothing could notice.
    ///
    /// Each entry now names the module constant the check itself filters with,
    /// which is the same constant the registry declares its scope from — so
    /// this table cannot silently agree with a stale copy.
    const CONSUMED: &[(&str, Consumes)] = &[
        (
            "pre-commit-argo-lint",
            Consumes::Exts(crate::hooks::k8s::EXTS),
        ),
        // Each term filters candidates against its own language's extensions,
        // and `EXTS` is pinned to be exactly their union — so this names the
        // module constant the check itself filters with.
        (
            "pre-commit-ban-terms",
            Consumes::Exts(crate::hooks::ban_terms::EXTS),
        ),
        (
            "pre-commit-cargo-fmt",
            Consumes::Exts(crate::hooks::rust_tools::EXTS),
        ),
        (
            "pre-commit-clippy",
            Consumes::Exts(crate::hooks::rust_tools::RUST_PATHS),
        ),
        (
            "pre-commit-go-vet",
            Consumes::Exts(crate::hooks::go_tools::GO_PATHS),
        ),
        (
            "pre-commit-gofmt",
            Consumes::Exts(crate::hooks::go_tools::EXTS),
        ),
        (
            "pre-commit-kube-linter",
            Consumes::Exts(crate::hooks::k8s::EXTS),
        ),
        (
            "pre-commit-kubeconform",
            Consumes::Exts(crate::hooks::k8s::EXTS),
        ),
        (
            "pre-commit-lint-js",
            Consumes::Exts(crate::hooks::lint_js::EXTS),
        ),
        (
            "pre-commit-lint-json-yaml",
            Consumes::Exts(crate::hooks::lint_json_yaml::EXTS),
        ),
        ("pre-commit-merge-conflict", Consumes::All),
        ("pre-commit-package-lock", Consumes::All),
        // Declares `files: &[]` — it is opt-in by CONFIG, not by file type —
        // while consuming seventeen extensions. The other reason the assertion
        // is a subset check rather than an equality.
        (
            "pre-commit-prettier",
            Consumes::Exts(crate::hooks::prettier::EXTS),
        ),
        (
            "pre-commit-pyright",
            Consumes::Exts(crate::hooks::python_tools::EXTS),
        ),
        (
            "pre-commit-ruff",
            Consumes::Exts(crate::hooks::python_tools::EXTS),
        ),
        ("pre-commit-usual-name", Consumes::All),
        (
            "pre-commit-yamllint",
            Consumes::Exts(crate::hooks::yamllint::EXTS),
        ),
        ("pre-commit-large-files", Consumes::All),
        ("pre-commit-secrets", Consumes::All),
        ("pre-push-secrets", Consumes::All),
        ("pre-push-branch-protect", Consumes::All),
        ("pre-push-branch-pattern", Consumes::All),
        ("pre-push-pull-rebase", Consumes::All),
        (
            "pre-push-run-tests-js",
            Consumes::Exts(crate::hooks::run_tests::JS_EXTS),
        ),
        (
            "pre-push-pytest",
            Consumes::Exts(crate::hooks::python_tools::EXTS),
        ),
        (
            "pre-push-cargo-test",
            Consumes::Exts(crate::hooks::rust_tools::RUST_PATHS),
        ),
        (
            "pre-push-go-test",
            Consumes::Exts(crate::hooks::go_tools::GO_PATHS),
        ),
    ];

    /// A declared scope must never promise more than the check consumes.
    ///
    /// Two halves, and the first is the one that earns its keep: a check with a
    /// non-empty `scope.files` that nobody has entered in `CONSUMED` fails the
    /// build, so adding a check forces somebody to state what it actually
    /// reads. The second half then pins that `scope.files ⊆ consumed`.
    ///
    /// SUBSET, not equality, deliberately: `prettier` declares `files: &[]` —
    /// it is opt-in by CONFIG, not by file type — while consuming seventeen
    /// extensions. Equality would forbid it.
    #[test]
    fn no_check_declares_a_file_type_it_does_not_consume() {
        for (name, _) in CONSUMED {
            assert!(
                CHECKS.iter().any(|check| check.name == *name),
                "CONSUMED names {name:?}, which is not a check"
            );
        }
        for check in CHECKS {
            let entry = CONSUMED.iter().find(|(name, _)| *name == check.name);
            if check.scope.files.is_empty() && entry.is_none() {
                continue;
            }
            let Some((_, consumes)) = entry else {
                panic!(
                    "{} declares scope.files {:?} but is missing from CONSUMED — \
                     say what it actually reads",
                    check.name, check.scope.files
                );
            };
            let Consumes::Exts(consumed) = consumes else {
                continue; // `All` consumes everything, so any declaration fits
            };
            for ext in check.scope.files {
                assert!(
                    consumed.contains(ext),
                    "{} declares {ext:?} in its scope but never asks for it — \
                     `amont list` would report a coverage the check does not have",
                    check.name
                );
            }
        }
    }

    /// Which checks contain code that repairs and re-stages, stated by hand.
    ///
    /// `pre-commit-cargo-fmt` and `pre-commit-ruff` both declared
    /// `Fix::Rewrite` with NO fixing code anywhere — only `prettier.rs` and
    /// `manifest.rs` ever called `restage`/`fixing_enabled`. `amont list
    /// --json` reported `"fix":"rewrite"` for them regardless, and `agents_md`
    /// explicitly directs agents to trust that JSON, so an agent would set
    /// `amont.fix true` and wait for a repair that could never arrive.
    const HAS_FIXING_CODE: &[(&str, bool)] = &[
        ("pre-commit-argo-lint", false),
        ("pre-commit-ban-terms", false),
        ("pre-commit-branch-pattern", false),
        ("pre-commit-cargo-fmt", true),
        ("pre-commit-clippy", false),
        ("pre-commit-go-vet", false),
        ("pre-commit-gofmt", true),
        ("pre-commit-kube-linter", false),
        ("pre-commit-kubeconform", false),
        ("pre-commit-lint-js", false),
        ("pre-commit-lint-json-yaml", false),
        ("pre-commit-merge-conflict", false),
        ("pre-commit-package-lock", false),
        ("pre-commit-prettier", true),
        ("pre-commit-pyright", false),
        ("pre-commit-ruff", true),
        ("pre-commit-usual-name", false),
        ("pre-commit-yamllint", false),
        ("pre-commit-large-files", false),
        ("pre-commit-secrets", false),
        ("pre-push-secrets", false),
        ("pre-push-branch-protect", false),
        ("pre-push-branch-pattern", false),
        ("pre-push-pull-rebase", false),
        ("pre-push-audit-go", false),
        ("pre-push-audit-js", false),
        ("pre-push-audit-python", false),
        ("pre-push-audit-rust", false),
        ("pre-push-run-tests-js", false),
        ("pre-push-cargo-test", false),
        ("pre-push-go-test", false),
        ("pre-push-pytest", false),
    ];

    /// A `Fix::Rewrite` declaration is a PROMISE, and the set of checks that
    /// keep it must equal the set that make it.
    #[test]
    fn every_rewrite_declaration_has_a_fixer() {
        let declared: BTreeSet<&str> = CHECKS
            .iter()
            .filter(|check| check.fix == super::Fix::Rewrite)
            .map(|check| check.name)
            .collect();
        let implemented: BTreeSet<&str> = HAS_FIXING_CODE
            .iter()
            .filter(|(_, has)| *has)
            .map(|(name, _)| *name)
            .collect();
        assert_eq!(
            declared, implemented,
            "a check declaring Fix::Rewrite with no fixer lies to `amont list --json`, \
             and a check with a fixer that does not declare it can never be reached"
        );

        // …and the table must cover every check, so a new one cannot be added
        // without somebody answering the question.
        let listed: BTreeSet<&str> = HAS_FIXING_CODE.iter().map(|(name, _)| *name).collect();
        let all: BTreeSet<&str> = CHECKS.iter().map(|check| check.name).collect();
        assert_eq!(listed, all, "HAS_FIXING_CODE does not cover CHECKS");
    }

    /// The reconciliation tests that used to live here are gone, and that is
    /// the point of the refactor: there is no second table to disagree with.
    /// The safety net is exactly the checks whose findings are mistakes in
    /// ANY codebase, with near-zero false positives — a conflict marker, an
    /// oversized blob, a leaked credential, a debug leftover in YOUR diff.
    /// Everything else is a house rule, and a house rule must not fire in a
    /// repository that never subscribed. Growing this list is a decision,
    /// not a default: lint-json-yaml stays OUT because Helm templates are
    /// invalid YAML and blocking a contribution to somebody else's chart
    /// repo is exactly the false positive this split exists to prevent.
    #[test]
    fn the_safety_net_is_exactly_the_low_false_positive_set() {
        let safety: Vec<&str> = CHECKS
            .iter()
            .filter(|c| c.reach == Reach::Safety)
            .map(|c| c.name)
            .collect();
        assert_eq!(
            safety,
            vec![
                "pre-commit-ban-terms",
                "pre-commit-large-files",
                "pre-commit-merge-conflict",
                "pre-commit-secrets",
                "pre-push-secrets",
            ]
        );
    }

    #[test]
    fn every_check_declares_a_stage_and_a_scope() {
        assert_eq!(CHECKS.len(), 32);
        let pre_commit = super::stage_checks(Stage::PreCommit).count();
        let pre_push = super::stage_checks(Stage::PrePush).count();
        assert_eq!(
            pre_commit + pre_push,
            CHECKS.len(),
            "every check has a stage"
        );
    }
}
