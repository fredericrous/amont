//! Every name the tool says about itself, in one place (ADR-0064
//! `harness.renames`, plan "amont ships as greengate").
//!
//! Messages, usage text, error prefixes, `--version` lines and command hints
//! are built from these, never from a literal: a later name is one change
//! here. The names test (`crates/greengate/tests/it/names.rs`) fails on a new
//! literal `"amont "`, `"amont: "` or `"amont-fleet"` in any other source.
//!
//! The names the tool READS and WRITES are here too since greengate 4.0
//! (plan "greengate 4.0 renames its data names", a hard rename): the
//! `GREENGATE_` environment, the `greengate.` config keys, the state under
//! `$GIT_DIR` and the common dir, and the ledgers' headers. greengate 3.x's
//! spellings live only in `migrate.rs`, which moves them once; the names
//! test fails on an old data name anywhere else. Kept on purpose: the notes
//! namespaces (`refs/notes/amont-*`, plan 3a-3), the shim placeholder, and
//! the binary aliases above.
//!
//! In a format string, name the constant inline: `eprintln!("{BIN}: …")`.
//! For a `&'static str` built at compile time, use the [`bin!`] macro with
//! `concat!`.

/// The hook binary's name as a literal, for `concat!` in a `&'static str`.
#[doc(hidden)]
#[macro_export]
macro_rules! bin {
    () => {
        "greengate"
    };
}

/// The fleet binary's name as a literal, for `concat!` in a `&'static str`.
#[doc(hidden)]
#[macro_export]
macro_rules! fleet_bin {
    () => {
        "greengate-fleet"
    };
}

/// The product's name, as it introduces itself.
pub const PRODUCT: &str = "greengate";

/// The hook binary.
pub const BIN: &str = bin!();

/// The fleet binary.
pub const FLEET_BIN: &str = fleet_bin!();

/// The hook binary's old name, still installed as an alias (a copy).
pub const OLD_BIN: &str = "amont";

/// The fleet binary's old name, still installed as an alias (a copy).
pub const OLD_FLEET_BIN: &str = "amont-fleet";

/// The repository the releases come from.
pub const REPO: &str = "greenharness/greengate";

/// The one-line installer for Linux and macOS.
pub const INSTALLER_URL: &str =
    "https://raw.githubusercontent.com/greenharness/greengate/main/install/install.sh";

/// The crate `cargo install` takes.
pub const CRATE: &str = "greengate";

/// The npm package.
pub const NPM_PACKAGE: &str = "@greenharness/greengate";

/// The Homebrew formula, tap-qualified.
pub const BREW_FORMULA: &str = "fredericrous/tap/greengate";

/// The git config namespace every key lives in (`greengate.timeout`).
pub const CONFIG_PREFIX: &str = "greengate.";

/// The environment namespace a host key answers to (`GREENGATE_TIMEOUT`).
pub const ENV_PREFIX: &str = "GREENGATE";

/// The folder of state under `$GIT_DIR` and the common dir (logs, land
/// receipts, pending detached runs).
pub const STATE_DIR: &str = "greengate";

/// The tree cache under `$GIT_DIR`.
pub const TREE_CACHE: &str = "greengate-cache";

/// Where the unstaged changes are parked during a check.
pub const HELD_STORE: &str = "greengate-held";

/// A held copy a mid-check edit displaced.
pub const PRESERVED_STORE: &str = "greengate-preserved";

/// The bypass ledger, and its header.
pub const BYPASS_LEDGER: &str = "greengate-bypasses";
pub const BYPASS_FORMAT: &str = "greengate-bypass-v1";

/// The downgrade ledger, and its header.
pub const DOWNGRADE_LEDGER: &str = "greengate-downgrades";
pub const DOWNGRADE_FORMAT: &str = "greengate-downgrade-v1";

/// The waves ledger, and its header.
pub const WAVES_LEDGER: &str = "greengate-waves";
pub const WAVES_FORMAT: &str = "greengate-waves-v1";

/// A rehearsal's state file, its log, and the state's header.
pub const REHEARSAL_STATE: &str = "greengate-rehearsal";
pub const REHEARSAL_LOG: &str = "greengate-rehearsal.log";
pub const REHEARSAL_FORMAT: &str = "greengate-rehearsal-v1";

/// The cold-cache warm worker's log under `$GIT_DIR`.
pub const WARM_LOG: &str = "greengate-warm.log";

/// The version-skew marker, and its header.
pub const SKEW_MARKER: &str = "greengate-skew";
pub const SKEW_FORMAT: &str = "greengate-skew-v1";

/// The held store's index header.
pub const HELD_FORMAT: &str = "greengate-held-v1";

/// The pre-commit → post-commit gate marker under `$GIT_DIR`. Its content
/// keeps the notes format (`amont-gate-v1`), which plan 3a-3 renames.
pub const GATE_MARKER: &str = "greengate-gate";

/// The `list --json` envelope. greenpiste and relais read it and the old
/// `amont-list-v1` alike, from greenpiste 3.30.0 and relais 0.54.0.
pub const LIST_FORMAT: &str = "greengate-list-v1";

/// The migration's lock and its done-marker, in the common dir.
pub const MIGRATE_LOCK: &str = "greengate-migrate.lock";
pub const MIGRATED_MARKER: &str = "greengate-migrated";

/// The name this tool holds a host slot under (`greengate slots`).
pub const SLOT_HOLDER: &str = "greengate";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_macros_and_the_constants_agree() {
        assert_eq!(BIN, PRODUCT);
        assert_eq!(FLEET_BIN, concat!(bin!(), "-fleet"));
        assert_eq!(CRATE, BIN);
        assert!(NPM_PACKAGE.ends_with(BIN));
        assert!(BREW_FORMULA.ends_with(BIN));
        assert!(INSTALLER_URL.contains(REPO));
    }
}
