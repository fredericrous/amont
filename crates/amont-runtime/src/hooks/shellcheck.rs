//! pre-commit-shellcheck — the shell linter, on the shell this repo stages.

use super::common::run as run_tool;
use super::common::{fail, hl, ok, repo_root, staged_files, warn, which};
use crate::check::Outcome;

/// The extensions this check consumes. Exported so `registry.rs` declares the
/// scope from the same constant — see `lint_json_yaml::EXTS` for the drift this
/// prevents.
///
/// Extensions only, deliberately: a `#!/bin/sh` file with no extension is not
/// matched. Reading shebangs is its own change with its own risks, and it is
/// already written down as one — see `docs/index-fidelity-and-run-modes.md` §5.
pub const EXTS: &[&str] = &[".sh", ".bash"];

pub fn run(_args: &[std::ffi::OsString]) -> Outcome {
    let files = staged_files(EXTS);
    if files.is_empty() {
        return Outcome::Passed;
    }
    let root = repo_root();
    // NO opt-in file, unlike `yamllint`. That check gates on a config because
    // its stock rules are too noisy to enforce generically; shellcheck's
    // defaults are the reason people run it at all. Gating on a `.shellcheckrc`
    // would leave this inert in every repository that has not written one,
    // which is a check that does nothing dressed as a check that is careful.
    //
    // The RESOLVED path, not the bare name: `Command::new` does no PATHEXT
    // resolution, so a bare `shellcheck` cannot execute `shellcheck.exe` on
    // Windows and a `Severity::Block` check would report an installed tool as
    // broken. Same incident `common::program` documents for `npm.cmd`.
    let Some(bin) = which("shellcheck") else {
        warn(&format!(
            "Shell files are staged but shellcheck is not installed. Install {}",
            hl("shellcheck")
        ));
        return Outcome::Unavailable;
    };
    // `--` before the file list: a staged file named e.g. `-x.sh` would
    // otherwise be read as a flag by shellcheck's own parser.
    let argv = vec![bin];
    let mut with_files = vec!["--".to_string()];
    with_files.extend(files);
    if !run_tool(&root, &argv, &with_files) {
        fail("shellcheck found issues. Please fix");
        return Outcome::Failed;
    }
    ok("shellcheck passed");
    Outcome::Passed
}
