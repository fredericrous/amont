//! pre-commit-yamllint — strict YAML lint, but only where a repo has opted in.

use super::common::run as run_tool;
use super::common::{fail, first_existing, hl, ok, repo_root, staged_files, warn, which};
use crate::check::Outcome;

/// The extensions this check consumes. Exported so `registry.rs` declares the
/// scope from the same constant — see `lint_json_yaml::EXTS` for the drift this
/// prevents.
pub const EXTS: &[&str] = &[".yaml", ".yml"];

pub fn run(_args: &[std::ffi::OsString]) -> Outcome {
    let files = staged_files(EXTS);
    if files.is_empty() {
        return Outcome::Passed;
    }
    let root = repo_root();
    // yamllint's stock rules are too noisy to enforce generically, so a
    // repo-local config is the opt-in signal. Skip silently without one.
    //
    // The opt-in is tested BEFORE the binary: one repo in the fleet has this
    // config, so asking the other ninety-five to install a tool they never
    // wanted was ninety-five nags, and — since Outcome exists — ninety-five
    // dashboard rows reading "could not run" for a check that had nothing
    // to run.
    let Some(config) = first_existing(&root, &[".yamllint.yaml", ".yamllint.yml", ".yamllint"])
    else {
        return Outcome::Passed;
    };
    // The RESOLVED path, not the bare name, and not a second probe either.
    // `Command::new` does no PATHEXT resolution, so a bare `yamllint` cannot
    // execute `yamllint.exe`/`.cmd` on Windows — the spawn fails with "program
    // not found" and a `Severity::Block` check reports an installed tool as
    // broken. Same incident `common::program` documents for `npm.cmd`.
    let Some(bin) = which("yamllint") else {
        warn(&format!(
            "This repo configures yamllint but it is not installed. Install {}",
            hl("yamllint")
        ));
        return Outcome::Unavailable;
    };
    // `--strict`: warning-level findings fail the run too. Without it
    // yamllint exits 0 under any number of warnings — see lint_js for why an
    // ever-growing warn list is worse than a block.
    let argv = vec![bin, "--strict".to_string(), "-c".to_string(), config];
    // `--` before the file list: a staged file named e.g. `-x.yaml` would
    // otherwise be read as a flag by yamllint's own (argparse) parser.
    let mut with_files = vec!["--".to_string()];
    with_files.extend(files);
    if !run_tool(&root, &argv, &with_files) {
        fail("yamllint found issues. Please fix");
        return Outcome::Failed;
    }
    ok("yamllint passed");
    Outcome::Passed
}
