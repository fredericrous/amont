//! One module per ported hook. Phase 1 of docs/rust-migration.md moves them
//! across one at a time; each keeps a shim at its original path so the existing
//! .zsh suite exercises the Rust implementation unchanged.
pub mod audit;
pub mod ban_terms;
pub mod branch_pattern;
pub mod branch_protect;
pub mod commit_msg;
pub mod common;
pub mod k8s;
pub mod lint_js;
pub mod lint_json_yaml;
pub mod merge_conflict;
pub mod package_lock;
pub mod post_commit;
pub mod prepare_commit_msg;
pub mod prettier;
pub mod pull_rebase;
pub mod python_tools;
pub mod run_tests;
pub mod rust_tools;
pub mod usual_name;
pub mod yamllint;
