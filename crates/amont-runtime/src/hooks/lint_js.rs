//! pre-commit-lint-js — ESLint over staged JS/TS, scoped to repos that lint.

use super::common::{
    fail, first_existing, hl, ok, repo_root, resolve_tool, run as run_tool, staged_files, warn,
};
use crate::check::Outcome;

/// The extensions this check consumes. Exported so `registry.rs` declares the
/// scope from the same constant — see `lint_json_yaml::EXTS` for the drift this
/// prevents.
pub const EXTS: &[&str] = &[".js", ".jsx", ".ts", ".tsx", ".vue"];

pub fn run(args: &[std::ffi::OsString]) -> Outcome {
    let files = staged_files(EXTS);
    if files.is_empty() {
        return Outcome::Passed;
    }
    let root = repo_root();

    // ESLint 9+ ERRORS OUT when it finds no config ("couldn't find an
    // eslint.config file") instead of no-op'ing, which would fail commits in
    // repos that do not lint JS at all — infra and manifest repos, and this
    // templates repo. So the config is the opt-in signal.
    let has_config = first_existing(
        &root,
        &[
            "eslint.config.js",
            "eslint.config.mjs",
            "eslint.config.cjs",
            "eslint.config.ts",
            ".eslintrc",
            ".eslintrc.js",
            ".eslintrc.cjs",
            ".eslintrc.json",
            ".eslintrc.yml",
            ".eslintrc.yaml",
        ],
    )
    .is_some()
        || std::fs::read_to_string(format!("{root}/package.json"))
            .map(|p| p.contains("\"eslintConfig\""))
            .unwrap_or(false);
    if !has_config {
        ok("ESLint skipped (no eslint config)");
        return Outcome::Passed;
    }

    let Some(argv) = resolve_tool(&root, "eslint") else {
        // A non-zero exit from a MISSING eslint is not the same as lint errors,
        // so warn and skip rather than block with a false "issues found".
        warn(&format!(
            "eslint config found but no eslint binary. Run {}",
            hl("npm install")
        ));
        return Outcome::Unavailable;
    };

    // Forwarded flags first, THEN `--`, THEN the files: a staged file named
    // e.g. `-x.js` would otherwise be read as a flag by eslint's own parser,
    // and putting `--` before the forwarded args instead would not separate
    // the files from anything — it would only separate ITSELF from `args`.
    // Zero warnings, not zero errors: eslint exits 0 under any number of
    // warning-level findings, and a warn list nobody is forced to read only
    // grows — a human scrolls past it, an agent reads "passed" and moves on.
    // Ours goes FIRST, so a repository that genuinely wants a budget can
    // forward its own `--max-warnings` and have the later value win.
    let mut extra: Vec<String> = vec!["--max-warnings".to_string(), "0".to_string()];
    extra.extend(args.iter().filter_map(|a| a.to_str()).map(str::to_owned));
    extra.push("--".to_string());
    extra.extend(files);
    if !run_tool(&root, &argv, &extra) {
        fail("ESLint issues found. Please fix");
        return Outcome::Failed;
    }
    ok("ESLint passed");
    Outcome::Passed
}
