//! The scope column's `+` half — what the REPOSITORY must carry.
//!
//! The opt-in's journey (quiet until the repository carries the config, read
//! from the repository rather than the commit, and a row that is all opt-in)
//! is a step of `flows/declared.rs`. This case stays: a carried config does
//! not stand in for the trigger.
//!
//! Its own binary rather than a few more cases in `external.rs`, and that is a
//! finding rather than tidiness: adding two tests there took the file from 28
//! to 30, and `a_chatty_check_outlives_the_idle_budget` went from occasionally
//! flaky to failing 3 runs out of 3. That test asserts on elapsed time while
//! its neighbours hold `sleep 5`, `sleep 30` and `sleep 300`, so every test
//! added to that binary competes with it for a scheduler slot. `cargo` runs
//! test BINARIES sequentially, so a separate file removes the competition
//! entirely.

use crate::common;

use common::Repo;

fn trust(r: &Repo) {
    crate::common::amont_cmd()
        .arg("trust")
        .current_dir(r.path(""))
        .output()
        .expect("amont trust");
}

fn manifest(r: &Repo, body: &str) {
    r.stage("amont.conf", body);
    trust(r);
}

/// The trigger half still gates independently: carrying the config does not
/// make a check run on a commit that touches nothing it understands.
#[test]
fn an_opt_in_does_not_replace_the_trigger() {
    let r = Repo::new();
    manifest(
        &r,
        "pre-commit  rubocop  *.rb+.rubocop.yml  block  sh -c 'echo RUBOCOP-RAN; exit 1'\n",
    );
    r.stage(".rubocop.yml", "AllCops:\n");
    r.stage("notes.md", "no ruby here\n");

    let run = r.hook("pre-commit", &[]);
    assert!(
        !run.says("RUBOCOP-RAN"),
        "the commit touched no .rb: {}",
        run.output()
    );
    assert!(run.passed(), "{}", run.output());
}
