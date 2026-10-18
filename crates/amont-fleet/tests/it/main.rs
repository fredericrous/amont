//! amont-fleet's integration tests, as ONE binary.
//!
//! `parity`, `scan`, `uninstall` and `write_safety` were four test binaries,
//! each linking amont-fleet's dependencies again and each a separate link for
//! every mutant a mutation run builds (decisions plan
//! 2026-10-17-amont-tests-follow-user-flows; amont's own `tests/it` did the
//! same in amont#351). As modules of one crate they link once. Test names
//! carry the old file as their module path, so `cargo test --test it scan::`
//! runs what `cargo test --test scan` did.
//!
//! `flows` walks the fleet journey end to end (scan, fix, apply, install,
//! uninstall); the other modules keep the cases with fixtures of their own.
//!
//! Every module builds its fixtures under a temp directory named after the
//! module and the test, so tests that used to run in separate processes can
//! share one without sharing a directory.

mod common;

mod flows;
mod parity;
mod scan;
mod uninstall;
mod write_safety;
