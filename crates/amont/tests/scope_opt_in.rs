//! The scope column's `+` half — what the REPOSITORY must carry.
//!
//! Its own binary rather than a few more cases in `external.rs`, and that is a
//! finding rather than tidiness: adding two tests there took the file from 28
//! to 30, and `a_chatty_check_outlives_the_idle_budget` went from occasionally
//! flaky to failing 3 runs out of 3. That test asserts on elapsed time while
//! its neighbours hold `sleep 5`, `sleep 30` and `sleep 300`, so every test
//! added to that binary competes with it for a scheduler slot. `cargo` runs
//! test BINARIES sequentially, so a separate file removes the competition
//! entirely.

mod common;

use common::Repo;
use std::process::Command;

fn trust(r: &Repo) {
    Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("trust")
        .current_dir(r.path(""))
        .output()
        .expect("amont trust");
}

fn manifest(r: &Repo, body: &str) {
    r.stage("amont.conf", body);
    trust(r);
}

/// A declared check can name what the REPOSITORY must carry, not just what the
/// commit touched — the courtesy every builtin has had and no declaration
/// could express.
///
/// This is what makes a vendored check safe to ship. Before `amont add`, every
/// line in a manifest was one somebody typed, so the manifest WAS the opt-in.
/// A packaged `rubocop` breaks that: it is in your file because you took a
/// pack, and without a condition it fires on every `.rb` in a repository that
/// never wanted rubocop and simply errors — the "ninety-five nags"
/// `hooks::yamllint` documents avoiding.
#[test]
fn an_opt_in_keeps_a_declared_check_quiet_until_the_repo_carries_its_config() {
    let r = Repo::new();
    manifest(
        &r,
        "pre-commit  rubocop  *.rb+.rubocop.yml  block  sh -c 'echo RUBOCOP-RAN; exit 1'\n",
    );
    r.stage("app.rb", "puts 'hi'\n");

    // A Ruby repository that never asked for rubocop.
    let run = r.hook("pre-commit", &[]);
    assert!(
        !run.says("RUBOCOP-RAN"),
        "no .rubocop.yml, so it must not run: {}",
        run.output()
    );
    assert!(run.passed(), "and must not block: {}", run.output());

    // …and one that did.
    r.stage(".rubocop.yml", "AllCops:\n  NewCops: enable\n");
    let run = r.hook("pre-commit", &[]);
    assert!(
        run.says("RUBOCOP-RAN"),
        "with the config present it must run: {}",
        run.output()
    );
    assert!(
        !run.passed(),
        "and its failure must block: {}",
        run.output()
    );
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
