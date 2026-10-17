//! amont's integration tests, as ONE binary.
//!
//! Each file here was its own test binary, and each binary linked
//! amont-runtime again: 69 links, about 50 s to rebuild after a one-line
//! change, and the same cost for every mutant a mutation run builds
//! (decisions plan 2026-10-17-amont-tests-follow-user-flows). As modules of
//! one crate they link once. Test names now carry the old file as their
//! module path, so `cargo test --test it land::` runs what
//! `cargo test --test land` did.
//!
//! `timing.rs` stays a binary of its own: its tests assert elapsed time and
//! need the machine to themselves (its header says why).

#[path = "../common/mod.rs"]
mod common;

mod agents_md;
mod agents_md_drift;
mod attest_check_inputs;
mod audit;
mod ban_terms;
mod branch_pattern;
mod branch_pattern_early;
mod branch_protect_early;
mod branch_protect_push;
mod check_verb;
mod ci_attest_scripts;
mod cli_dispatch;
mod commit_msg;
mod commit_stamps;
mod commit_style;
mod dispatchers;
mod docs_counts;
mod downgrades;
mod enroll;
mod excerpt_golden;
mod external;
mod fixing;
mod gate_evidence;
mod gate_pairs;
mod git_state;
mod go_tools;
mod hadolint;
mod helm;
mod index_fidelity;
mod init;
mod install;
mod json_contract;
mod k8s;
mod land;
mod large_files;
mod linters;
mod merge_conflict;
mod npm_packaging;
mod pack_add;
mod package_lock;
mod policy;
mod prepare_commit_msg;
mod pull_rebase;
mod push_stamps;
mod pushed_tree;
mod pytest_gate;
mod quiet_run;
mod reach;
mod rehearsal;
mod release_profile;
mod run_mode;
mod run_tests_js;
mod rust_toolchain;
mod rust_tools;
mod scope_dirs;
mod scope_opt_in;
mod secrets;
mod severity;
mod shellcheck;
mod shim_resolution;
mod sigpipe;
mod snapshot_deps;
mod spawn_budget;
mod tree_gates;
mod trust_display;
mod usual_name;
mod worktrees;
mod zero_warnings;
