//! What every module of this binary needs: the binary under test and the
//! shim the installer writes.

use std::path::PathBuf;

/// The `amont-fleet` binary cargo built for this test run.
pub fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_amont-fleet")
}

/// The workspace root, where `templates/` lives.
pub fn repo_root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

/// The shim template, unbaked.
pub fn template() -> String {
    std::fs::read_to_string(repo_root().join("templates/hooks/pre-commit")).expect("template")
}

/// The shim body, baked at `binary`, exactly as the installer writes it.
pub fn shim_for(binary: &str) -> String {
    template().replace("__AMONT_BIN__", binary)
}

/// The six dispatchers, in the order the scan reports them.
pub const DISPATCHERS: [&str; 6] = [
    "commit-msg",
    "post-commit",
    "post-rewrite",
    "pre-commit",
    "pre-push",
    "prepare-commit-msg",
];
