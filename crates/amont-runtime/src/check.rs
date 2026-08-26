//! What a check IS, as one value rather than four tables.
//!
//! Before this, a check was spread across `REGISTRY` (name → fn), two ordered
//! name lists, and a language table in the fleet crate — four places keyed by
//! the same string, held together by reconciliation tests. Those tests were
//! good, but they policed a shape that should not have been splittable. With
//! the metadata attached to the check, there is nothing left to reconcile.
//!
//! It also gives external checks somewhere to exist. A third party cannot add a
//! Rust module without rebuilding the binary, so extension means a declared
//! command implementing this same trait — and the dispatcher not caring which
//! kind it is holding.

use crate::registry::Ctx;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    PreCommit,
    PrePush,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::PreCommit => "pre-commit",
            Stage::PrePush => "pre-push",
        }
    }
}

/// When a check is relevant, declared rather than reimplemented by every
/// reader.
///
/// A CONJUNCTION, not a choice: ruff is `.py` files AND a ruff config; clippy
/// is `.rs` AND `Cargo.toml`. An earlier design offered these as alternatives
/// plus a `Custom` escape hatch, which would have swallowed nearly every check
/// and left the dashboard knowing nothing.
/// A git operation that is part-way through.
///
/// Detected from the marker files git writes into `$GIT_DIR`, which is how git
/// itself and every prompt-writer answers the question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitState {
    Merge,
    Rebase,
    CherryPick,
    Revert,
    Bisect,
}

impl GitState {
    /// The marker git writes. `rebase-merge` and `rebase-apply` are
    /// DIRECTORIES; the rest are files, and `Path::exists` covers both.
    pub fn markers(self) -> &'static [&'static str] {
        match self {
            GitState::Merge => &["MERGE_HEAD"],
            GitState::Rebase => &["REBASE_HEAD", "rebase-merge", "rebase-apply"],
            GitState::CherryPick => &["CHERRY_PICK_HEAD"],
            GitState::Revert => &["REVERT_HEAD"],
            GitState::Bisect => &["BISECT_LOG"],
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            GitState::Merge => "a merge",
            GitState::Rebase => "a rebase",
            GitState::CherryPick => "a cherry-pick",
            GitState::Revert => "a revert",
            GitState::Bisect => "a bisect",
        }
    }

    pub const ALL: [GitState; 5] = [
        GitState::Merge,
        GitState::Rebase,
        GitState::CherryPick,
        GitState::Revert,
        GitState::Bisect,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scope {
    /// Extensions that trigger it. Empty means any change.
    pub files: &'static [&'static str],
    /// Exact FILENAMES that trigger it — `package.json`, `Dockerfile` —
    /// matched against the path's basename, never as a suffix: an extension
    /// list cannot say "package.json" without also matching
    /// `not-package.json`. Builtins keep this empty; the manifest's scope
    /// column fills it.
    pub names: &'static [&'static str],
    /// Config paths that opt a repository in. Empty means always on.
    pub opt_in: &'static [&'static str],
    /// Git operations during which this check does not run.
    ///
    /// The other half of "when does this apply". `files` and `opt_in` say which
    /// REPOSITORIES and which CHANGES; this says which repository STATES — a
    /// question that used to be answered by one hard-coded `CHERRY_PICK_HEAD`
    /// test in one dispatcher, with the other carrying a comment admitting it
    /// had none because the shell version had none.
    pub not_during: &'static [GitState],
}

impl Scope {
    pub const ALWAYS: Scope = Scope {
        files: &[],
        names: &[],
        opt_in: &[],
        not_during: &[],
    };

    pub const fn files(files: &'static [&'static str]) -> Scope {
        Scope {
            files,
            names: &[],
            opt_in: &[],
            not_during: &[],
        }
    }

    pub const fn new(files: &'static [&'static str], opt_in: &'static [&'static str]) -> Scope {
        Scope {
            files,
            names: &[],
            opt_in,
            not_during: &[],
        }
    }

    /// The same scope, silent during these operations.
    pub const fn not_during(self, states: &'static [GitState]) -> Scope {
        Scope {
            files: self.files,
            names: self.names,
            opt_in: self.opt_in,
            not_during: states,
        }
    }

    /// No file gate at all — every change is in scope.
    pub fn is_unscoped(&self) -> bool {
        self.files.is_empty() && self.names.is_empty()
    }

    /// Does ONE path fall inside the file gate?
    pub fn covers(&self, path: &str) -> bool {
        self.files.iter().any(|ext| path.ends_with(ext)) || {
            let base = path.rsplit('/').next().unwrap_or(path);
            self.names.contains(&base)
        }
    }

    /// Did the gate see EVERY one of `paths`? All-match, where [`matches`]
    /// is any-match: the caller asking this is deciding whether a commit-time
    /// run COVERED a push, and under-approximating is the safe direction.
    pub fn covers_all(&self, paths: &[String]) -> bool {
        self.is_unscoped() || paths.iter().all(|p| self.covers(p))
    }

    /// Would this check ever fire, given the paths a repository contains?
    ///
    /// Deliberately coarse for checks that resolve an ancestor at run time —
    /// `cargo-fmt` declares `Cargo.toml` meaning "somewhere here" while
    /// enforcing "nearest above the staged file". The dispatcher asks the
    /// precise question by running the check; this answers the dashboard's
    /// question, "would it ever fire", where over-approximating is the safe
    /// direction.
    pub fn matches(&self, paths: &[String]) -> bool {
        let by_ext = self.is_unscoped() || paths.iter().any(|path| self.covers(path));
        let opted_in = self.opt_in.is_empty()
            || paths.iter().any(|p| {
                let name = p.rsplit('/').next().unwrap_or(p);
                self.opt_in.iter().any(|c| {
                    // A trailing `*` is a prefix match: `.kube-linter*.yaml`.
                    match c.split_once('*') {
                        Some((pre, suf)) => name.starts_with(pre) && name.ends_with(suf),
                        None => name == *c,
                    }
                })
            });
        by_ext && opted_in
    }
}

/// What a check meant, as opposed to what it printed.
///
/// The fourth variant is the point. Fifteen sites used to warn and return 0,
/// collapsing two different situations: "I ran and found something you should
/// know" and "I could not run at all". `ruff config found but no ruff binary`
/// was indistinguishable from ruff running clean — to the dispatcher, and to
/// the dashboard. A repository where a check has silently never executed read
/// as one where it passes.
///
/// That is the same invisibility `hook.skip` had before skipped checks were
/// announced, and it took three PRs to notice there.
///
/// The check still prints its own message; this only classifies the result.
///
/// Deliberately NO `Default`. It used to be `Failed`, to fill the slot of a
/// check whose thread died — a real rule, but `Default` means "the neutral
/// value" to every reader and to every `#[derive(Default)]` that might later
/// contain one. The rule is now written where it applies, in the runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    /// Ran, found a problem. Whether that blocks is `Severity`, not this.
    Failed,
    /// Ran, found something worth saying, which does not block.
    Warned,
    /// Ran, found a problem, and REPAIRED it. The commit proceeds with the
    /// repair staged, which is neither `Passed` (something happened, and the
    /// author should know their files changed) nor `Failed`.
    Fixed,
    /// COULD NOT RUN — a tool is missing, or the opt-in config is absent.
    Unavailable,
}

/// What a HOOK concluded — the only thing git actually reads.
///
/// Distinct from `Outcome`, which is what one CHECK concluded. Git has exactly
/// two questions to ask a hook, so this has exactly two answers, and the `i32`
/// that expresses them lives at the process boundary rather than being threaded
/// through every hook, dispatcher and handler as it used to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Proceed,
    Block,
}

impl Verdict {
    /// The exit code git reads. The ONLY place a hook result becomes a number.
    pub fn exit_code(self) -> i32 {
        match self {
            Verdict::Proceed => 0,
            Verdict::Block => 1,
        }
    }

    pub fn blocking(blocked: bool) -> Verdict {
        if blocked {
            Verdict::Block
        } else {
            Verdict::Proceed
        }
    }
}

/// Whether a failing check stops the commit or merely reports.
///
/// Declared per check and overridable per repository with
/// `git config amont.severity.<check> warn`. That is a better escape hatch
/// than `hook.skip`, which is all-or-nothing and invisible enough that
/// `hook.skip = e` disables all twenty checks: a downgrade keeps the signal and
/// removes only the block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Block,
    Warn,
}

impl Severity {
    /// The ONE mapping from configured text to severity.
    ///
    /// There were four: this key's reader, the manifest's severity column, the
    /// dashboard's copy, and the dashboard's reverse mapping for `--json`. They
    /// agreed, but nothing made them — and the dashboard's copy is its
    /// prediction of what the dispatcher will do, which is the one thing it must
    /// never get wrong.
    ///
    /// `None` for anything else, deliberately: git validates nothing here, so an
    /// unrecognised value must fall back to the declared severity rather than
    /// silently disable a check.
    pub fn parse(value: &str) -> Option<Severity> {
        match value {
            "warn" => Some(Severity::Warn),
            "block" => Some(Severity::Block),
            _ => None,
        }
    }

    /// How it is written in config and in `--json`.
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Block => "block",
            Severity::Warn => "warn",
        }
    }
}

/// Whether a check can rewrite the files it inspects.
///
/// Off by default and per check, never global: a hook that edits your files
/// without being asked is a larger surprise than one that complains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fix {
    /// Reports only. Every check, until somebody declares otherwise.
    None,
    /// Runs a command that rewrites files, and stages what it changed.
    ///
    /// Only reachable from a `Stage::PreCommit` declaration. A pre-push hook
    /// must not modify the worktree or index: the pushed commit would then
    /// differ from the tree the developer is looking at.
    Rewrite,
}

impl Fix {
    /// How it is written in `--json`. No `parse()`: nothing reads a `Fix`
    /// back out of text — the manifest's `fix` marker has its own, unrelated
    /// parsing path in `manifest.rs`.
    pub fn as_str(self) -> &'static str {
        match self {
            Fix::None => "none",
            Fix::Rewrite => "rewrite",
        }
    }
}

/// One check, whether compiled in or declared by the repository.
///
/// `Sync` because `pre-commit` hands every check to its own thread. Both
/// implementations satisfy it for free, and requiring it here is what lets the
/// dispatcher hold `&'static dyn Check` without caring which kind it has.
pub trait Check: Sync {
    fn name(&self) -> &str;
    fn stage(&self) -> Stage;
    fn scope(&self) -> Scope;
    /// The name a commit-time declaration must use to pair with this gate.
    ///
    /// Deliberately the SHORT name, and deliberately not the id: pairing is
    /// cross-stage by definition — `test` declared at `pre-commit` pairs
    /// with `test` at `pre-push` — so the id, which encodes the stage, is
    /// the one key that cannot work. `pre-push-cargo-test` would be compared
    /// against a `GateDecl.script` of `cargo-test` and never match, and the
    /// failure would be silence rather than an error: the gate simply never
    /// pairs and nobody is told why.
    ///
    /// This is the single place where comparing short names is correct,
    /// which is why it is a named method rather than a `short_name()` call
    /// at the comparison site — that function's own documentation says never
    /// to compare against it, and it is right everywhere else.
    fn pairing_name(&self) -> &str {
        crate::short_name(self.name())
    }
    fn severity(&self) -> Severity;
    /// Whether this check can repair what it finds. `None` for almost all.
    fn fix(&self) -> Fix {
        Fix::None
    }
    /// How far the check reaches when `amont.conventions` is `declared` —
    /// see [`Reach`]. Externals default to `Convention`, which costs them
    /// nothing: a declared check only exists where an `amont.conf` does,
    /// and that is exactly the declaration the mode asks for.
    fn reach(&self) -> Reach {
        Reach::Convention
    }
    fn run(&self, ctx: &Ctx) -> Outcome;
}

/// How far a check reaches into repositories that never asked for it.
///
/// With `git config --global amont.conventions declared`, hooks installed by
/// a standing grant (`init.templateDir`) split in two: `Safety` checks run in
/// EVERY repository, because their findings are mistakes in any codebase — a
/// conflict marker, a leaked credential, a hundred-megabyte blob, a
/// `debugger;` left in the diff. `Convention` checks run only where the
/// repository has committed an `amont.conf` — they are one team's house
/// rules (commit shapes, branch names, lint severities, test gates), and a
/// clone of somebody else's project did not agree to them.
///
/// The default mode is `everywhere`, where this distinction is inert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// A mistake anywhere: runs regardless of declaration.
    Safety,
    /// A house rule: runs only where the repository declares amont.
    Convention,
}

/// A check compiled into the binary.
pub struct Builtin {
    pub name: &'static str,
    pub stage: Stage,
    pub scope: Scope,
    pub severity: Severity,
    pub run: fn(&Ctx) -> Outcome,
    /// Almost always `Fix::None`; see `CHECKS`.
    pub fix: Fix,
    /// Almost always `Convention`; the exceptions are pinned in the registry.
    pub reach: Reach,
}

impl Check for Builtin {
    fn name(&self) -> &str {
        self.name
    }
    fn stage(&self) -> Stage {
        self.stage
    }
    fn scope(&self) -> Scope {
        self.scope
    }
    fn severity(&self) -> Severity {
        self.severity
    }
    fn fix(&self) -> Fix {
        self.fix
    }
    fn reach(&self) -> Reach {
        self.reach
    }
    fn run(&self, ctx: &Ctx) -> Outcome {
        (self.run)(ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trips, and refuses everything else. A `Some` for an unknown value
    /// would turn a typo into a silent disable.
    #[test]
    fn severity_parses_exactly_the_two_words_it_documents() {
        for s in [Severity::Block, Severity::Warn] {
            assert_eq!(Severity::parse(s.as_str()), Some(s));
        }
        for bad in ["", "Warn", "WARN", "advisory", "true", "1", " warn"] {
            assert_eq!(Severity::parse(bad), None, "{bad:?} must not parse");
        }
    }

    #[test]
    fn fix_says_how_it_is_written_in_json() {
        assert_eq!(Fix::None.as_str(), "none");
        assert_eq!(Fix::Rewrite.as_str(), "rewrite");
    }

    #[test]
    fn always_matches_anything() {
        assert!(Scope::ALWAYS.matches(&[]));
        assert!(Scope::ALWAYS.matches(&["README.md".into()]));
    }

    #[test]
    fn extensions_gate_on_the_file_type() {
        let s = Scope::files(&[".rs"]);
        assert!(s.matches(&["src/main.rs".into()]));
        assert!(!s.matches(&["README.md".into()]));
    }

    /// The case the enum could not express: BOTH conditions must hold.
    #[test]
    fn files_and_opt_in_are_a_conjunction() {
        let ruff = Scope::new(&[".py"], &["ruff.toml", "pyproject.toml"]);
        assert!(
            !ruff.matches(&["a.py".into()]),
            "python alone is not enough — the repo must opt in"
        );
        assert!(
            !ruff.matches(&["pyproject.toml".into()]),
            "and a config alone is not enough without python"
        );
        assert!(ruff.matches(&["a.py".into(), "pyproject.toml".into()]));
    }

    /// `.kube-linter*.yaml` is a real config name in this repo's own hooks.
    #[test]
    fn a_trailing_star_is_a_prefix_match() {
        let s = Scope::new(&[".yaml"], &[".kube-linter*.yaml"]);
        assert!(s.matches(&["k8s/x.yaml".into(), ".kube-linter-prod.yaml".into()]));
        assert!(!s.matches(&["k8s/x.yaml".into(), ".kube-lint.yaml".into()]));
    }

    /// Opt-in matches a BASENAME anywhere, which is what makes the coarse
    /// answer right for a check that resolves an ancestor when it runs.
    #[test]
    fn opt_in_matches_a_nested_manifest() {
        let cargo = Scope::new(&[".rs"], &["Cargo.toml"]);
        assert!(cargo.matches(&["crates/a/src/lib.rs".into(), "crates/a/Cargo.toml".into()]));
    }
}
