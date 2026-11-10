//! Every name the tool says about itself, in one place (ADR-0064
//! `harness.renames`, plan "amont ships as greengate").
//!
//! Messages, usage text, error prefixes, `--version` lines and command hints
//! are built from these, never from a literal: a later name is one change
//! here. The names test (`crates/greengate/tests/it/names.rs`) fails on a new
//! literal `"amont "`, `"amont: "` or `"amont-fleet"` in any other source.
//!
//! Not here: the names the tool READS (env vars, git config keys,
//! `amont.conf`, ledgers, state paths, markers, the slots contract). Those
//! keep their old spelling until plan 3a-2 reads both.
//!
//! In a format string, name the constant inline: `eprintln!("{BIN}: …")`.
//! For a `&'static str` built at compile time, use the [`bin!`] macro with
//! `concat!`.

/// The hook binary's name as a literal, for `concat!` in a `&'static str`.
#[macro_export]
macro_rules! bin {
    () => {
        "greengate"
    };
}

/// The fleet binary's name as a literal, for `concat!` in a `&'static str`.
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
