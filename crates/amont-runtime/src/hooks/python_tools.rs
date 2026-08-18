//! pre-commit-ruff and pre-commit-pyright — the two Python hooks.
//!
//! Both are scoped: they only fire when the repo opts in (a ruff/pyright config
//! or the matching `[tool.*]` table), so a JS repo never pulls a Python tool.
//!
//! Both prefer the repo's PINNED tool over an ambient latest, in the same order
//! the shell versions established: `uv run --no-sync` (the lockfile-pinned one
//! CI runs) → the worktree's .venv → the MAIN worktree's .venv (a linked
//! worktree has none of its own) → PATH → uvx (unpinned LATEST, with a warning,
//! because it flags issues the CI-pinned version does not — phantom failures).

use super::common::{
    fail, fixing_enabled, hl, ok, repo_root, restage, run as run_tool, run_quiet, staged_files,
    warn, which, Restaged,
};
use crate::check::Outcome;
use crate::git;
use std::path::Path;
use std::process::{Command, Stdio};

/// The extensions both Python checks consume. Exported so `registry.rs`
/// declares the scope from the same constant — see `lint_json_yaml::EXTS` for
/// the drift this prevents.
pub const EXTS: &[&str] = &[".py", ".pyi"];

fn tool_runs(root: &str, argv: &[String]) -> bool {
    let Some((p, rest)) = argv.split_first() else {
        return false;
    };
    Command::new(p)
        .args(rest)
        .arg("--version")
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn main_worktree_venv(tool: &str) -> Option<String> {
    let common = git::stdout(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    let p = Path::new(&common).parent()?.join(".venv/bin").join(tool);
    p.is_file().then(|| p.to_string_lossy().into_owned())
}

/// Returns (argv, warned_about_unpinned).
///
/// Every branch yields a RESOLVED path, never a bare name. `Command::new` does
/// no PATHEXT resolution, so a bare `uv`/`uvx`/`ruff` cannot execute
/// `uv.exe`/`ruff.cmd` on Windows: the spawn fails with "program not found" and
/// a `Severity::Block` check reports an installed tool as broken. That is the
/// incident `common::program` was written for, and these were the last places
/// still handing `Command` a bare name — three of them AFTER `which()` had
/// already succeeded and thrown the answer away.
fn resolve_python_tool(root: &str, tool: &str) -> Option<(Vec<String>, bool)> {
    if let Some(uv) = which("uv") {
        let argv = vec![uv, "run".into(), "--no-sync".into(), tool.into()];
        if tool_runs(root, &argv) {
            return Some((argv, false));
        }
    }
    let local = format!("{root}/.venv/bin/{tool}");
    if Path::new(&local).is_file() {
        return Some((vec![local], false));
    }
    if let Some(v) = main_worktree_venv(tool) {
        return Some((vec![v], false));
    }
    // The path `which` already resolved, rather than probing and discarding it.
    if let Some(found) = which(tool) {
        return Some((vec![found], false));
    }
    if let Some(uvx) = which("uvx") {
        return Some((vec![uvx, tool.into()], true));
    }
    None
}

fn opts_in(root: &str, configs: &[&str], table: &str) -> bool {
    if configs.iter().any(|c| Path::new(root).join(c).is_file()) {
        return true;
    }
    std::fs::read_to_string(Path::new(root).join("pyproject.toml"))
        .map(|t| t.lines().any(|l| l.trim_start().starts_with(table)))
        .unwrap_or(false)
}

pub fn ruff(_args: &[std::ffi::OsString]) -> Outcome {
    let files = staged_files(EXTS);
    if files.is_empty() {
        return Outcome::Passed;
    }
    let root = repo_root();
    if !opts_in(&root, &["ruff.toml", ".ruff.toml"], "[tool.ruff") {
        return Outcome::Passed;
    }
    let Some((argv, unpinned)) = resolve_python_tool(&root, "ruff") else {
        warn("ruff config found but no ruff/uvx binary. Install ruff or uv.");
        return Outcome::Unavailable;
    };
    if unpinned {
        warn(&format!(
            "No pinned ruff found (.venv); using {} (latest) — may flag issues the CI-pinned ruff doesn't.",
            hl("uvx ruff")
        ));
    }

    // The registry has always declared `Fix::Rewrite` for this check, and
    // `amont list --json` reported `"fix":"rewrite"` — which `agents_md`
    // explicitly tells agents to trust — while no fixing code existed
    // anywhere. Only prettier and the manifest's externals ever called
    // `restage`. Rather than downgrade the declaration, the fixing is now
    // real.
    //
    // Repair FIRST and QUIETLY, then let the check passes below decide. Ruff
    // legitimately leaves findings it cannot fix, unlike `cargo fmt`, so the
    // repair pass's own exit code says nothing about the verdict — and running
    // it loudly would print the surviving offenders twice.
    let mut repaired = false;
    if fixing_enabled() {
        let _ = run_quiet(&root, &argv, &with_files(&["check", "--fix"], &files));
        let _ = run_quiet(&root, &argv, &with_files(&["format"], &files));
        match restage(&files) {
            Restaged::Staged => repaired = true,
            Restaged::Failed(stuck) => {
                fail(&format!(
                    "ruff rewrote these files but {} failed — the index still holds the OLD \
                     content: {}",
                    hl("git add"),
                    stuck.join(", ")
                ));
                return Outcome::Failed;
            }
            Restaged::Nothing => {}
        }
    }

    let mut failed = false;
    if !run_tool(&root, &argv, &with_files(&["check"], &files)) {
        fail(&format!(
            "Ruff lint issues. Run {}. Offenders above.",
            hl("ruff check --fix")
        ));
        failed = true;
    }
    if !run_tool(&root, &argv, &with_files(&["format", "--check"], &files)) {
        fail(&format!(
            "Ruff found unformatted files. Run {} on the files listed above.",
            hl("ruff format")
        ));
        failed = true;
    }

    if failed {
        // A repair that could not finish the job still blocks — and whatever it
        // DID fix is already staged, so the next attempt starts from there.
        return Outcome::Failed;
    }
    if repaired {
        ok("Ruff fixed and re-staged");
        return Outcome::Fixed;
    }
    // An unpinned ruff RAN, and its verdict was clean — that is a pass, not a
    // gap. The caveat above is advice about which ruff spoke, not a claim that
    // none did.
    ok("Ruff passed");
    Outcome::Passed
}

/// `<sub…> --force-exclude -- <files>`.
///
/// `--force-exclude` on every pass so ruff honours the project's `exclude`
/// even though the paths are handed to it explicitly. `--` before the file
/// list because a staged file named e.g. `-x.py` would otherwise be read as a
/// flag by ruff's own parser.
fn with_files(sub: &[&str], files: &[String]) -> Vec<String> {
    let mut argv: Vec<String> = sub.iter().map(|s| (*s).to_string()).collect();
    argv.push("--force-exclude".into());
    argv.push("--".into());
    argv.extend(files.iter().cloned());
    argv
}

pub fn pyright(_args: &[std::ffi::OsString]) -> Outcome {
    let files = staged_files(EXTS);
    if files.is_empty() {
        return Outcome::Passed;
    }
    let root = repo_root();
    if !opts_in(
        &root,
        &["pyrightconfig.json", "pyrightconfig.jsonc"],
        "[tool.pyright",
    ) {
        return Outcome::Passed;
    }
    let Some((argv, _)) = resolve_python_tool(&root, "pyright") else {
        warn("pyright config found but no pyright binary. Install pyright or uv.");
        return Outcome::Unavailable;
    };
    // Scoped to the STAGED files, not the whole tree: fast enough for a
    // pre-commit hook while catching the per-file errors that are the usual
    // local-vs-CI gap. Pyright still resolves the whole workspace for imports,
    // so inference is unchanged; only the reported set is scoped. CI's
    // whole-tree run stays the authority for cross-file-only errors.
    // PYRIGHT HAS NO `--` SEPARATOR, and this check used to pass one.
    //
    // Every other tool driven from this repository takes `--` to mean "flags
    // are over" — ruff, prettier, yamllint, node, yq all do, and each has a
    // regression test for a staged file named like a flag. Pyright does not.
    // It has a hand-rolled argument parser that treats `--` as an ordinary
    // path, so `pyright -- a.py` reports
    //
    //     File or directory ".../--" does not exist
    //
    // and exits 4. `run_tool` sees a non-zero status, and a check declared
    // `Severity::Block` blocked the commit. Not on bad code — on ALL code. Any
    // repository with a `pyrightconfig.json` or a `[tool.pyright]` table could
    // not commit a `.py` file at all while pyright was installed, and the
    // message it got named a file nobody had written.
    //
    // It survived because the check's only test asserted that it does NOTHING
    // in a repo with no config, which is the one path that never reaches this
    // line. `linters.rs` now has both halves — a type error must block, and
    // clean code must not — and it was the second of those that caught this in
    // its first run. A check that fails on everything satisfies "reports a type
    // error" perfectly.
    //
    // `./` in place of `--` keeps the protection that was intended. `./-p.py`
    // begins with a dot, so no argument parser can mistake it for a flag, and
    // pyright resolves it against the working directory `run_tool` already
    // sets. Git reports staged paths relative to the repository root with
    // forward slashes on every platform, so this is safe to prepend blindly.
    let with_files: Vec<String> = files.iter().map(|f| format!("./{f}")).collect();
    if !run_tool(&root, &argv, &with_files) {
        fail("Pyright type errors. Please fix");
        return Outcome::Failed;
    }
    ok("pyright passed");
    Outcome::Passed
}

/// The Python test gate — `pre-push-pytest`, `cargo-test`'s shape for the
/// third ecosystem. JS had a push-time suite from the start and Rust
/// gained one; Python repositories declaring a pytest setup (a
/// `pytest.ini` or a `conftest.py` — a bare `pyproject.toml` is not a
/// promise to test) ran nothing at push until now.
///
/// Same contract as the other suites: runs against the PUSHED tree per
/// ref (see `pushed_tree`), `Unavailable` — loudly, never green — when
/// git will not answer or pytest is not installed, and fail-fast on a red
/// suite.
pub fn pytest(refs: &[crate::pushrefs::PushRef]) -> Outcome {
    let Some(root) = crate::git::stdout(&["rev-parse", "--show-toplevel"]) else {
        super::common::warn("pytest: git would not answer — the gate did NOT run");
        return Outcome::Unavailable;
    };
    let zero = crate::git::stdout(&["hash-object", "--stdin"])
        .map(|h| "0".repeat(h.len()))
        .unwrap_or_else(|| "0".repeat(40));
    let mut ran_any = false;
    for r in refs {
        let changed = crate::pushrefs::changed_files_for(r, &zero);
        if !changed.iter().any(|f| EXTS.iter().any(|e| f.ends_with(e))) {
            continue; // this ref pushes no Python
        }
        let Some(pytest) = super::common::which("pytest") else {
            super::common::warn(
                "Python changed but pytest is not installed — the gate did NOT run",
            );
            return Outcome::Unavailable;
        };
        // Where THIS ref's suite runs decides what it is answering about —
        // the pushed commits, not whatever is open in the editor.
        let (where_, _guard) = crate::pushed_tree::where_to_run(&r.local_oid, &root);
        let mut cmd = std::process::Command::new(&pytest);
        cmd.current_dir(&where_).stdin(std::process::Stdio::null());
        super::common::strip_git_env(&mut cmd);
        if !super::common::bounded_success(&mut cmd, "pytest") {
            super::common::fail("Python tests failed. Push aborted.");
            return Outcome::Failed;
        }
        ran_any = true;
    }
    if ran_any {
        super::common::ok("Python tests passed");
    }
    Outcome::Passed
}
