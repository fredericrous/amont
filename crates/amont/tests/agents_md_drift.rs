//! pre-commit-agents-md — the generated block, checked against the binary
//! that would generate it now.
//!
//! The states it stays QUIET in matter as much as the one it speaks in: a
//! repository that never opted in must never hear about a file it does not
//! have.

mod common;
use common::Repo;

const START: &str = "<!-- amont:start -->";
const END: &str = "<!-- amont:end -->";

fn stale_block(r: &Repo) {
    r.write("AGENTS.md", &format!("# P\n\n{START}\nSTALE\n{END}\n"));
    r.git(&["add", "AGENTS.md"]);
}

/// No markers, no opinion — and no `git config` spawned to find that out.
#[test]
fn a_repository_without_the_block_is_quiet() {
    let r = Repo::new();
    r.stage("a.txt", "x\n");
    let run = r.run(&["run", "pre-commit-agents-md"]);
    assert!(run.passed(), "{}", run.output());
    assert!(!run.says("AGENTS.md"), "{}", run.output());
}

/// A stale block warns, names the fix, and does NOT block the commit.
#[test]
fn a_stale_block_warns_without_blocking() {
    let r = Repo::new();
    stale_block(&r);
    let run = r.run(&["run", "pre-commit-agents-md"]);
    assert!(run.passed(), "a warning must not block: {}", run.output());
    assert!(run.says("behind what amont"), "{}", run.output());
    assert!(run.says("amont agents-md"), "{}", run.output());
}

/// With `amont.fix true` the block is regenerated AND re-staged, so the
/// commit being made carries the current guidance.
#[test]
fn with_fixing_on_the_block_is_regenerated_and_restaged() {
    let r = Repo::new();
    stale_block(&r);
    r.git(&["config", "amont.fix", "true"]);
    let run = r.run(&["run", "pre-commit-agents-md"]);
    assert!(run.passed(), "{}", run.output());
    assert!(run.says("regenerated"), "{}", run.output());
    let staged = r.git(&["show", ":AGENTS.md"]);
    let staged = String::from_utf8_lossy(&staged.stdout);
    assert!(
        !staged.contains("STALE"),
        "the index still holds the stale block"
    );
    assert!(staged.contains("amont list --json"), "{staged}");
}

/// A block this binary just wrote is current: quiet.
#[test]
fn a_current_block_passes() {
    let r = Repo::new();
    r.commit("init");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("agents-md")
        .current_dir(&r.dir)
        .output()
        .expect("agents-md");
    assert!(out.status.success());
    r.git(&["add", "AGENTS.md", "CLAUDE.md"]);
    let run = r.run(&["run", "pre-commit-agents-md"]);
    assert!(run.passed(), "{}", run.output());
    assert!(run.says("matches"), "{}", run.output());
}
