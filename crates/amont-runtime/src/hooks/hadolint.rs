//! pre-commit-hadolint — the Dockerfile linter.

use super::common::run as run_tool;
use super::common::{fail, hl, ok, repo_root, staged_files, warn, which};
use crate::check::Outcome;

/// The filenames this check consumes. Exported so `registry.rs` declares the
/// scope from the same constant.
///
/// `Dockerfile` EXACTLY. `Dockerfile.dev` and `Dockerfile.prod` are not
/// matched: the scope column's name tokens are exact basenames, and inventing
/// a prefix match here — for one check, in Rust, where the manifest grammar
/// deliberately has none — would put two different answers to "what is a
/// filename" in the same tool. Documented in `docs/checks.md` instead.
pub const NAMES: &[&str] = &["Dockerfile"];

/// Staged paths whose BASENAME is one of `NAMES`.
///
/// `staged_files(&[])` rather than a second git spawn: that list is read once
/// per process and lent to every check, and a suffix filter cannot express
/// "exactly this filename" — `staged_files(&["Dockerfile"])` would also match
/// `my-Dockerfile`, which is the reason the manifest's scope column has bare
/// filenames as their own kind of token.
fn staged_dockerfiles() -> Vec<String> {
    staged_files(&[])
        .into_iter()
        .filter(|p| {
            let base = p.rsplit('/').next().unwrap_or(p);
            NAMES.contains(&base)
        })
        .collect()
}

pub fn run(_args: &[std::ffi::OsString]) -> Outcome {
    let files = staged_dockerfiles();
    if files.is_empty() {
        return Outcome::Passed;
    }
    let root = repo_root();
    // No opt-in file: a `Dockerfile` in the diff has already said everything a
    // marker would. Contrast `yamllint`, where `.yaml` says nothing about
    // whether the repository wanted a YAML linter.
    let Some(bin) = which("hadolint") else {
        warn(&format!(
            "A Dockerfile is staged but hadolint is not installed. Install {}",
            hl("hadolint")
        ));
        return Outcome::Unavailable;
    };
    // `--` before the file list, for the same reason every other check does it:
    // a path beginning with `-` is a flag to the tool's own parser.
    let argv = vec![bin];
    let mut with_files = vec!["--".to_string()];
    with_files.extend(files);
    if !run_tool(&root, &argv, &with_files) {
        fail("hadolint found issues. Please fix");
        return Outcome::Failed;
    }
    ok("hadolint passed");
    Outcome::Passed
}
