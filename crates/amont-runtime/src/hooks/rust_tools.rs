//! The three Rust hooks: `cargo fmt --check`, `cargo clippy`, `cargo test`.
//!
//! Scoped like every other language hook — they fire only when the commit (or
//! push) touches Rust, and only in a directory that actually has a `Cargo.toml`.
//! A Python repo never invokes cargo.
//!
//! Split across the two dispatchers by COST, matching what the other languages
//! already do: `fmt` and `clippy` are pre-commit (as ruff and pyright are),
//! `test` is pre-push (as `run-tests-js` is). Nobody wants to wait for a
//! workspace test run on every commit.
//!
//! Each is a separate check rather than one "rust" hook, so `hook.skip` can
//! disable them individually — `git config hook.skip clippy` when you are
//! mid-refactor, without losing the formatting gate.

use super::common::{
    fail, fixing_enabled, hl, ok, repo_root, restage, run as run_tool, staged_files, warn, which,
    Restaged,
};
use crate::check::Outcome;
use crate::git;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Files that mean "this commit touches Rust". `Cargo.toml` and `Cargo.lock`
/// count: a dependency bump compiles differently without a single `.rs` edit,
/// and that is exactly when clippy earns its keep.
///
/// Exported for the registry's drift guard: clippy and cargo-test consume this
/// whole set while declaring only `.rs` plus a `Cargo.toml` opt-in.
pub const RUST_PATHS: &[&str] = &[
    ".rs",
    "Cargo.toml",
    "Cargo.lock",
    "rustfmt.toml",
    "clippy.toml",
];

/// What `cargo fmt` is handed. Exported so `registry.rs` declares the scope
/// from the same constant — see `lint_json_yaml::EXTS`.
pub const EXTS: &[&str] = &[".rs"];

fn is_rust_path(f: &str) -> bool {
    let name = f.rsplit('/').next().unwrap_or(f);
    RUST_PATHS.iter().any(|pattern| {
        if pattern.starts_with('.') {
            name.ends_with(pattern)
        } else {
            name == *pattern
        }
    })
}

/// The nearest ancestor of `file` holding a `Cargo.toml`, bounded by the repo.
///
/// Not simply "the repo root": plenty of repos keep a Rust component in a
/// subdirectory next to services in other languages, and cargo must run where
/// the manifest is. `--workspace` then covers every member from that point, so
/// one invocation per manifest root is enough.
fn cargo_root_for(root: &str, file: &str) -> Option<PathBuf> {
    let mut dir = Path::new(root).join(file);
    dir.pop();
    loop {
        if dir.join("Cargo.toml").is_file() {
            return Some(dir);
        }
        if dir == Path::new(root) || !dir.starts_with(root) {
            return None;
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn cargo_roots<'a>(root: &str, files: impl Iterator<Item = &'a str>) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    for f in files.filter(|f| is_rust_path(f)) {
        if let Some(d) = cargo_root_for(root, f) {
            seen.insert(d);
        }
    }
    seen.into_iter().collect()
}

/// True when `cargo <component> --version` works.
///
/// Only for separately-installable COMPONENTS — rustfmt and clippy, which a
/// toolchain can legitimately lack. A missing one must warn and pass, never
/// fail a commit for a tool the developer never chose.
///
/// Do NOT probe a BUILT-IN subcommand this way: `cargo test --version` is
/// "unexpected argument '--version'", so the probe reports test as unavailable
/// and the gate silently passes — it would never have run a test anywhere.
fn component_available(dir: &Path, sub: &str) -> bool {
    let Some(cargo) = which("cargo") else {
        return false;
    };
    Command::new(cargo)
        .arg(sub)
        .arg("--version")
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn cargo_argv() -> Option<Vec<String>> {
    which("cargo").map(|c| vec![c])
}

/// Resolve cargo and verify the component, or warn and give up.
///
/// Split out from `each_root` because `fmt` now runs TWO passes — a `--check`
/// and, when repairing, a write — and the second must not re-probe rustfmt
/// (a second `cargo fmt --version` per manifest root) nor duplicate the
/// resolution it would have to get identical.
fn cargo_for(roots: &[PathBuf], component: Option<&str>, missing: &str) -> Option<Vec<String>> {
    let argv = cargo_argv().or_else(|| {
        warn(missing);
        None
    })?;
    if let Some(c) = component {
        for dir in roots {
            if !component_available(dir, c) {
                warn(missing);
                return None;
            }
        }
    }
    Some(argv)
}

/// Run one cargo invocation in every manifest root. True when all succeeded.
fn run_in_roots(roots: &[PathBuf], argv: &[String], args: &[&str]) -> bool {
    let extra: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    let mut all_ok = true;
    for dir in roots {
        let d = dir.to_string_lossy().into_owned();
        if !run_tool(&d, argv, &extra) {
            all_ok = false;
        }
    }
    all_ok
}

/// Shared shape: find the manifest roots, verify the component if there is one
/// to verify, run the command in each, report once.
///
/// `component` is `Some` only for rustfmt/clippy; `None` means a built-in
/// subcommand where cargo's own presence is the whole requirement.
fn each_root(
    roots: &[PathBuf],
    component: Option<&str>,
    args: &[&str],
    missing: &str,
) -> Option<bool> {
    let argv = cargo_for(roots, component, missing)?;
    Some(run_in_roots(roots, &argv, args))
}

pub fn fmt(_args: &[std::ffi::OsString]) -> Outcome {
    let files = staged_files(EXTS);
    if files.is_empty() {
        return Outcome::Passed;
    }
    let root = repo_root();
    let roots = cargo_roots(&root, files.iter().map(String::as_str));
    if roots.is_empty() {
        return Outcome::Passed;
    }
    const MISSING: &str =
        "Rust staged but rustfmt is not installed. `rustup component add rustfmt`.";
    let Some(argv) = cargo_for(&roots, Some("fmt"), MISSING) else {
        return Outcome::Unavailable;
    };

    // `--all -- --check` per the project convention. It inspects the working
    // TREE rather than the index — and that is now correct, because the
    // pre-commit stage holds the unstaged changes aside for the duration, so
    // the tree IS the staged content. This comment used to call that "the same
    // trade-off cargo fmt gives everyone", which was true of the observation
    // and wrong about the conclusion: see `staged_only`.
    if run_in_roots(&roots, &argv, &["fmt", "--all", "--", "--check"]) {
        ok("Rust formatting is clean");
        return Outcome::Passed;
    }

    // The registry has always declared `Fix::Rewrite` for this check, and
    // `amont list --json` reported `"fix":"rewrite"` — which `agents_md`
    // explicitly tells agents to trust — while no fixing code existed
    // anywhere. Only prettier and the manifest's externals ever called
    // `restage`. Rather than downgrade the declaration, the fixing is now
    // real.
    if fixing_enabled() && run_in_roots(&roots, &argv, &["fmt", "--all"]) {
        // The non-obvious guard: `cargo fmt --all` formats the WHOLE
        // workspace, not just the staged files — but `restage` is handed the
        // staged `.rs` list, so nothing the author did not stage is staged
        // here. The formatter's other edits stay in the working tree, exactly
        // as an unrelated unstaged change would.
        match restage(&files) {
            Restaged::Staged => {
                ok("Rust reformatted and re-staged");
                return Outcome::Fixed;
            }
            Restaged::Failed(stuck) => {
                fail(&format!(
                    "cargo fmt rewrote these files but {} failed — the index still holds the \
                     UNFORMATTED content: {}",
                    hl("git add"),
                    stuck.join(", ")
                ));
                return Outcome::Failed;
            }
            // Nothing staged differed, so whatever `--check` objected to was
            // outside the staged set. Fall through and report it.
            Restaged::Nothing => {}
        }
    }

    fail(&format!("Unformatted Rust. Run {}.", hl("cargo fmt --all")));
    Outcome::Failed
}

pub fn clippy(_args: &[std::ffi::OsString]) -> Outcome {
    // `staged_files` matches by suffix, which would also accept
    // `vendor/NotCargo.toml`. `is_rust_path` compares the basename, so let it
    // be the only filter rather than keeping two that disagree.
    let files: Vec<String> = staged_files(&[])
        .into_iter()
        .filter(|f| is_rust_path(f))
        .collect();
    if files.is_empty() {
        return Outcome::Passed;
    }
    let root = repo_root();
    let roots = cargo_roots(&root, files.iter().map(String::as_str));
    if roots.is_empty() {
        return Outcome::Passed;
    }
    match each_root(
        &roots,
        Some("clippy"),
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--",
            "-D",
            "warnings",
        ],
        "Rust staged but clippy is not installed. `rustup component add clippy`.",
    ) {
        None => Outcome::Unavailable,
        Some(true) => {
            ok("Clippy passed");
            Outcome::Passed
        }
        Some(false) => {
            fail(&format!(
                "Clippy warnings. Fix them or run {}.",
                hl("cargo clippy --fix")
            ));
            Outcome::Failed
        }
    }
}

/// pre-push. Mirrors `run-tests-js`: the range that is actually being pushed
/// decides whether the suite runs, so a docs-only push costs nothing.
///
/// PER REF, not once for the whole push: `git push origin a b` carries two
/// tips, and a single worktree checked out to the first one would run the
/// second ref's tests against the first ref's tree — a real failure in the
/// untested branch reported as a pass because nothing actually ran against
/// it. Each ref that touches Rust gets its own worktree and its own verdict.
pub fn test(refs: &[crate::pushrefs::PushRef]) -> Outcome {
    // `Unavailable`, never `Passed`, when git will not answer — same argument
    // and same wording as run-tests-js: a gate must not report green having
    // asked nothing.
    let Some(root) = git::stdout(&["rev-parse", "--show-toplevel"]) else {
        super::common::warn("cargo-test: git would not answer — the gate did NOT run");
        return Outcome::Unavailable;
    };
    let zero = git::stdout(&["hash-object", "--stdin"])
        .map(|h| "0".repeat(h.len()))
        .unwrap_or_else(|| "0".repeat(40));
    let mut ran_any = false;
    for r in refs {
        let changed = crate::pushrefs::changed_files_for(r, &zero);
        let roots = cargo_roots(&root, changed.iter().map(String::as_str));
        if roots.is_empty() {
            continue;
        }
        // Where THIS ref's suite runs decides what it is answering about.
        // `_guard` owns the checkout for the length of this ref's run;
        // dropping it removes the worktree before the next ref's begins.
        let (where_, _guard) = crate::pushed_tree::where_to_run(&r.local_oid, &root);
        let roots: Vec<PathBuf> = roots
            .iter()
            .map(|rt| {
                rt.strip_prefix(&root)
                    .map(|rel| where_.join(rel))
                    .unwrap_or_else(|_| rt.clone())
            })
            .collect();
        match each_root(
            &roots,
            None,
            &["test", "--workspace", "--all-features"],
            "Rust changed but cargo is not installed.",
        ) {
            None => return Outcome::Unavailable,
            Some(true) => ran_any = true,
            Some(false) => {
                fail("Rust tests failed. Push aborted.");
                return Outcome::Failed;
            }
        }
    }
    if ran_any {
        ok("Rust tests passed");
    }
    Outcome::Passed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_rust_paths() {
        assert!(is_rust_path("src/main.rs"));
        assert!(is_rust_path("Cargo.toml"));
        assert!(is_rust_path("crates/a/Cargo.lock"));
        assert!(is_rust_path("rustfmt.toml"));
        assert!(!is_rust_path("README.md"));
        assert!(!is_rust_path("src/main.rsx"));
        // A file merely CONTAINING the name is not the manifest.
        assert!(!is_rust_path("docs/Cargo.toml.md"));
        assert!(!is_rust_path("vendor/NotCargo.toml"));
    }

    #[test]
    fn finds_the_nearest_manifest_not_the_repo_root() {
        let tmp = std::env::temp_dir().join("amont-cargo-roots");
        let _ = std::fs::remove_dir_all(&tmp);
        let nested = tmp.join("services/engine");
        std::fs::create_dir_all(nested.join("src")).unwrap();
        std::fs::write(nested.join("Cargo.toml"), "[package]\n").unwrap();
        let root = tmp.to_string_lossy().into_owned();

        let got = cargo_roots(&root, ["services/engine/src/main.rs"].into_iter());
        assert_eq!(got, vec![nested.clone()], "should find the nested manifest");

        // A Rust file with no manifest anywhere above it is not a cargo project.
        std::fs::create_dir_all(tmp.join("scripts")).unwrap();
        let none = cargo_roots(&root, ["scripts/loose.rs"].into_iter());
        assert!(none.is_empty(), "no manifest above it: {none:?}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn several_files_in_one_crate_yield_one_root() {
        let tmp = std::env::temp_dir().join("amont-cargo-dedupe");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("src")).unwrap();
        std::fs::write(tmp.join("Cargo.toml"), "[package]\n").unwrap();
        let root = tmp.to_string_lossy().into_owned();
        let got = cargo_roots(&root, ["src/a.rs", "src/b.rs", "Cargo.toml"].into_iter());
        assert_eq!(got.len(), 1, "one cargo invocation, not three: {got:?}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn non_rust_files_select_nothing() {
        let got = cargo_roots("/tmp", ["README.md", "a.py"].into_iter());
        assert!(got.is_empty());
    }
}
