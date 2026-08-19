//! The three Go hooks: `gofmt`, `go vet`, `go test` — the Rust lane's shape,
//! spoken in Go.
//!
//! Scoped like every other language hook: they fire only when the commit (or
//! push) touches Go, and only under a directory that actually has a `go.mod`.
//! A Python repo never invokes the Go toolchain.
//!
//! Split across the two dispatchers by COST, as the other languages are:
//! `gofmt` and `go vet` are pre-commit (as cargo-fmt and clippy are), `test`
//! is pre-push (as cargo-test and pytest are). Each is a separate check so
//! `hook.skip` can disable them individually.
//!
//! One deliberate difference from cargo: `gofmt` takes FILES, not a manifest
//! root — so the format check is handed exactly the staged `.go` list and
//! never wanders into files this commit did not touch. `vet` and `test` need
//! a module to make sense of imports, so they run per `go.mod` root like
//! cargo runs per `Cargo.toml`.

use super::common::{
    fail, fixing_enabled, hl, ok, repo_root, restage, run as run_tool, staged_files, warn, which,
    Restaged,
};
use crate::check::Outcome;
use crate::git;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Files that mean "this touches Go". `go.mod` and `go.sum` count: a
/// dependency bump compiles differently without a single `.go` edit, and that
/// is exactly when `go vet` and the test suite earn their keep.
///
/// Exported for the registry's drift guard, like `rust_tools::RUST_PATHS`.
pub const GO_PATHS: &[&str] = &[".go", "go.mod", "go.sum"];

/// What `gofmt` is handed. Exported so `registry.rs` declares the scope from
/// the same constant — see `lint_json_yaml::EXTS` for the drift this prevents.
pub const EXTS: &[&str] = &[".go"];

fn is_go_path(f: &str) -> bool {
    let name = f.rsplit('/').next().unwrap_or(f);
    GO_PATHS.iter().any(|pattern| {
        if pattern.starts_with('.') {
            name.ends_with(pattern)
        } else {
            name == *pattern
        }
    })
}

/// The nearest ancestor of `file` holding a `go.mod`, bounded by the repo —
/// the same walk as `rust_tools::cargo_root_for`, for the same reason: a Go
/// service can live in a subdirectory next to other languages, and the
/// toolchain must run where the module is. `./...` then covers every package
/// from that point.
fn module_root_for(root: &str, file: &str) -> Option<PathBuf> {
    let mut dir = Path::new(root).join(file);
    dir.pop();
    loop {
        if dir.join("go.mod").is_file() {
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

fn module_roots<'a>(root: &str, files: impl Iterator<Item = &'a str>) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    for f in files.filter(|f| is_go_path(f)) {
        if let Some(d) = module_root_for(root, f) {
            seen.insert(d);
        }
    }
    seen.into_iter().collect()
}

/// Run one `go`/`gofmt` invocation in every module root. True when all
/// succeeded.
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

/// `gofmt -l` over exactly the staged `.go` files: it PRINTS the unformatted
/// ones and exits 0 either way, so the listing decides, not the exit code.
/// A non-zero exit is a parse error in a staged file — the commit has bigger
/// problems, and hiding them behind "formatting is clean" would be a lie.
fn unformatted(root: &str, gofmt: &str, files: &[String]) -> Option<Vec<String>> {
    let mut cmd = std::process::Command::new(gofmt);
    cmd.arg("-l")
        .args(files)
        .current_dir(root)
        .stdin(std::process::Stdio::null());
    super::common::strip_git_env(&mut cmd);
    let (ran, out) = super::common::capture_within(&mut cmd)?;
    match ran {
        super::common::Ran::TimedOut(budget) => {
            super::common::say_timed_out(gofmt, budget);
            None
        }
        super::common::Ran::Status(s) if !s.success() => {
            // gofmt refused to parse something; its own message says which.
            for line in out.lines() {
                crate::say!("{line}");
            }
            Some(files.to_vec())
        }
        super::common::Ran::Status(_) => Some(
            out.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect(),
        ),
    }
}

pub fn fmt(_args: &[std::ffi::OsString]) -> Outcome {
    let files = staged_files(EXTS);
    if files.is_empty() {
        return Outcome::Passed;
    }
    let root = repo_root();
    if module_roots(&root, files.iter().map(String::as_str)).is_empty() {
        return Outcome::Passed;
    }
    let Some(gofmt) = which("gofmt") else {
        warn("Go staged but gofmt is not installed — install the Go toolchain.");
        return Outcome::Unavailable;
    };
    let Some(dirty) = unformatted(&root, &gofmt, &files) else {
        return Outcome::Unavailable;
    };
    if dirty.is_empty() {
        ok("Go formatting is clean");
        return Outcome::Passed;
    }

    // `gofmt -w` rewrites only the files `-l` named — never the whole module,
    // so `restage` never stages anything the author did not stage.
    if fixing_enabled() {
        let argv = vec![gofmt.clone()];
        let mut args: Vec<&str> = vec!["-w"];
        args.extend(dirty.iter().map(String::as_str));
        if run_in_roots(&[PathBuf::from(&root)], &argv, &args) {
            match restage(&files) {
                Restaged::Staged => {
                    ok("Go reformatted and re-staged");
                    return Outcome::Fixed;
                }
                Restaged::Failed(stuck) => {
                    fail(&format!(
                        "gofmt rewrote these files but {} failed — the index still holds the \
                         UNFORMATTED content: {}",
                        hl("git add"),
                        stuck.join(", ")
                    ));
                    return Outcome::Failed;
                }
                Restaged::Nothing => {}
            }
        }
    }

    fail(&format!(
        "Unformatted Go: {}. Run {}.",
        dirty.join(", "),
        hl("gofmt -w .")
    ));
    Outcome::Failed
}

pub fn vet(_args: &[std::ffi::OsString]) -> Outcome {
    // Basename-matched like `is_rust_path`, so `vendor/Notgo.mod` is not a
    // module marker.
    let files: Vec<String> = staged_files(&[])
        .into_iter()
        .filter(|f| is_go_path(f))
        .collect();
    if files.is_empty() {
        return Outcome::Passed;
    }
    let root = repo_root();
    let roots = module_roots(&root, files.iter().map(String::as_str));
    if roots.is_empty() {
        return Outcome::Passed;
    }
    let Some(go) = which("go") else {
        warn("Go staged but the go toolchain is not installed.");
        return Outcome::Unavailable;
    };
    if run_in_roots(&roots, &[go], &["vet", "./..."]) {
        ok("go vet passed");
        Outcome::Passed
    } else {
        fail(&format!("{} found problems.", hl("go vet")));
        Outcome::Failed
    }
}

/// pre-push. Mirrors `cargo-test`: the range actually being pushed decides
/// whether the suite runs, per ref, against the pushed tree — a docs-only
/// push costs nothing, and a multi-ref push tests each tip in its own
/// worktree.
pub fn test(refs: &[crate::pushrefs::PushRef]) -> Outcome {
    let Some(root) = git::stdout(&["rev-parse", "--show-toplevel"]) else {
        warn("go-test: git would not answer — the gate did NOT run");
        return Outcome::Unavailable;
    };
    let zero = git::stdout(&["hash-object", "--stdin"])
        .map(|h| "0".repeat(h.len()))
        .unwrap_or_else(|| "0".repeat(40));
    let mut ran_any = false;
    for r in refs {
        let changed = crate::pushrefs::changed_files_for(r, &zero);
        let roots = module_roots(&root, changed.iter().map(String::as_str));
        if roots.is_empty() {
            continue;
        }
        let Some(go) = which("go") else {
            warn("Go changed but the go toolchain is not installed — the gate did NOT run");
            return Outcome::Unavailable;
        };
        // Where THIS ref's suite runs decides what it is answering about —
        // the pushed commits, not whatever is open in the editor.
        let (where_, _guard) = crate::pushed_tree::where_to_run(&r.local_oid, &root);
        let roots: Vec<PathBuf> = roots
            .iter()
            .map(|rt| {
                rt.strip_prefix(&root)
                    .map(|rel| where_.join(rel))
                    .unwrap_or_else(|_| rt.clone())
            })
            .collect();
        if !run_in_roots(&roots, std::slice::from_ref(&go), &["test", "./..."]) {
            fail("Go tests failed. Push aborted.");
            return Outcome::Failed;
        }
        ran_any = true;
    }
    if ran_any {
        ok("Go tests passed");
    }
    Outcome::Passed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_go_paths() {
        assert!(is_go_path("cmd/api/main.go"));
        assert!(is_go_path("go.mod"));
        assert!(is_go_path("services/auth/go.sum"));
        assert!(!is_go_path("README.md"));
        assert!(!is_go_path("main.gone"));
        // A file merely CONTAINING the name is not the module file.
        assert!(!is_go_path("docs/go.mod.md"));
        assert!(!is_go_path("vendor/Notgo.mod"));
    }

    #[test]
    fn finds_the_nearest_module_not_the_repo_root() {
        let tmp = std::env::temp_dir().join("amont-go-roots");
        let _ = std::fs::remove_dir_all(&tmp);
        let nested = tmp.join("services/api");
        std::fs::create_dir_all(nested.join("cmd")).unwrap();
        std::fs::write(nested.join("go.mod"), "module api\n").unwrap();
        let root = tmp.to_string_lossy().into_owned();

        let got = module_roots(&root, ["services/api/cmd/main.go"].into_iter());
        assert_eq!(got, vec![nested.clone()], "should find the nested module");

        // A Go file with no go.mod anywhere above it is not a module.
        std::fs::create_dir_all(tmp.join("scripts")).unwrap();
        let none = module_roots(&root, ["scripts/loose.go"].into_iter());
        assert!(none.is_empty(), "no module above it: {none:?}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn several_files_in_one_module_yield_one_root() {
        let tmp = std::env::temp_dir().join("amont-go-dedupe");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("pkg")).unwrap();
        std::fs::write(tmp.join("go.mod"), "module x\n").unwrap();
        let root = tmp.to_string_lossy().into_owned();
        let got = module_roots(&root, ["pkg/a.go", "pkg/b.go", "go.mod"].into_iter());
        assert_eq!(got.len(), 1, "one go invocation, not three: {got:?}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn non_go_files_select_nothing() {
        let got = module_roots("/tmp", ["README.md", "a.rs"].into_iter());
        assert!(got.is_empty());
    }
}
