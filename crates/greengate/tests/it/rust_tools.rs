//! The three Rust checks, driven through the real binary against real cargo.
//!
//! These compile a tiny crate, so they are the slowest tests here. They earn it:
//! everything interesting is in the seam between git's staged set and cargo's
//! notion of a workspace, and nothing about that seam is exercised by a unit
//! test on path strings. What the Rust checks promise like every other tool
//! (scoping, a clean and a broken crate, fixing on and off, a manifest-only
//! change, a docs-only push) are rows of the tables in `tool_tables`.

use crate::common;
use common::{missing, Repo};
use std::io::Write;
use std::process::Stdio;

/// A minimal, dependency-free crate — no network, no registry.
fn crate_files(r: &Repo, body: &str) {
    r.stage(
        "Cargo.toml",
        "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n",
    );
    r.stage("src/main.rs", body);
}

fn pre_push(r: &Repo, from: &str, to: &str) -> i32 {
    let line = format!("refs/heads/feat/x {to} refs/heads/feat/x {from}\n");
    let mut child = crate::common::amont_cmd()
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-push-cargo-test")
        .current_dir(&r.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(line.as_bytes())
        .expect("write");
    child.wait().expect("wait").code().unwrap_or(-1)
}

fn head(r: &Repo) -> String {
    String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string()
}

/// The guard that makes the repair safe: `cargo fmt --all` formats the WHOLE
/// workspace, but `restage` is handed only the staged `.rs` list. A second,
/// unformatted, UNSTAGED file in the same crate is reformatted on disk (cargo's
/// doing) and must NOT appear in the index.
#[test]
fn a_repair_does_not_stage_an_unrelated_unformatted_file() {
    if missing("cargo") {
        return;
    }
    let r = Repo::new();
    r.git(&["config", "amont.fix", "true"]);
    // A clean, committed baseline for both files.
    crate_files(&r, "mod other;\nfn main() {}\n");
    r.stage("src/other.rs", "pub fn q() {}\n");
    r.commit("chore: seed");

    // The change being committed: staged and unformatted.
    r.stage("src/main.rs", "mod other;\nfn main(){let _x=1;}\n");
    // A second unformatted file in the same crate, deliberately KEPT BACK.
    r.write("src/other.rs", "pub fn q(){let _y=2;}\n");

    let run = r.hook("pre-commit-cargo-fmt", &[]);
    assert!(run.passed(), "{}", run.stdout);

    let staged = r.git(&["diff", "--cached", "--name-only"]);
    let staged = String::from_utf8_lossy(&staged.stdout);
    assert!(staged.contains("src/main.rs"), "{staged}");
    assert!(
        !staged.contains("src/other.rs"),
        "the repair swept in work the author kept back: {staged}"
    );
}

/// The gate runs a project's test suite, and git hands every hook a GIT_DIR
/// pointing at the REAL repository. Those variables beat `current_dir`, so a
/// suite that shells out to git operates on the wrong repo entirely.
///
/// This is not theoretical: running the gate on this very repo let a fixture's
/// `git commit` land a stray commit on a real branch, authored by the test
/// user. The suite must see no git environment at all.
#[test]
fn the_test_gate_does_not_leak_git_env_to_the_suite() {
    if missing("cargo") {
        return;
    }
    let r = Repo::new();
    // A test that fails IF it can see git's environment.
    crate_files(
        &r,
        "fn main() {}\n\n#[test]\nfn no_git_env() {\n    \
         for (k, _) in std::env::vars() {\n        \
         assert!(!k.starts_with(\"GIT_\"), \"leaked {k}\");\n    }\n}\n",
    );
    r.commit("feat: base");
    let base = head(&r);
    r.stage("src/main.rs", "fn main() {}\n\n#[test]\nfn no_git_env() {\n    for (k, _) in std::env::vars() {\n        assert!(!k.starts_with(\"GIT_\"), \"leaked {k}\");\n    }\n}\n// touch\n");
    r.commit("feat: touch");

    // Run the gate with git's variables set, as git itself would.
    let line = format!("refs/heads/feat/x {} refs/heads/feat/x {base}\n", head(&r));
    let mut child = crate::common::amont_cmd()
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-push-cargo-test")
        .current_dir(&r.dir)
        .env("GIT_DIR", r.path(".git"))
        .env("GIT_INDEX_FILE", r.path(".git/index"))
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(line.as_bytes())
        .expect("write");
    let code = child.wait().expect("wait").code().unwrap_or(-1);
    assert_eq!(code, 0, "the suite saw git's environment");
}

/// A brand-new branch's changed-file set must cover EVERY new commit, not
/// just the tip. For a new ref (`remote_oid` all-zero), the range logic used
/// to diff only the tip commit against its parent — a branch that added
/// `Cargo.toml`/`src/main.rs` two commits back and then a docs-only commit on
/// top would report only the docs file as changed, so `cargo_roots` found no
/// Rust and `pre-push-cargo-test` silently passed without ever running the
/// (here, deliberately failing) suite.
#[test]
fn a_new_branch_with_rust_several_commits_back_still_runs_the_suite() {
    if missing("cargo") {
        return;
    }
    let r = Repo::new();
    r.stage("README.md", "start\n");
    r.commit("chore: base");

    // The Rust project, added on the SECOND commit — not the tip.
    crate_files(
        &r,
        "fn main() {}\n\n#[test]\nfn t() {\n    assert!(false);\n}\n",
    );
    r.commit("feat: add a failing crate");

    // The tip: a docs-only commit that touches no Rust file at all.
    r.stage("README.md", "start\nand more\n");
    r.commit("docs: expand readme");

    let zero = "0".repeat(40);
    let code = pre_push(&r, &zero, &head(&r));
    assert_ne!(
        code, 0,
        "a new branch's failing Rust suite, added before the tip commit, was silently skipped"
    );
}

/// Two refs pushed at once (`git push origin a b`) must each be tested
/// against their OWN tip. A single worktree shared across the whole push used
/// to be checked out at only the first ref's commit, so the second ref's
/// suite ran against the first ref's tree — a real failure unique to the
/// second branch was invisible because its own content was never checked out
/// anywhere.
#[test]
fn each_ref_in_a_multi_ref_push_is_tested_against_its_own_tip() {
    if missing("cargo") {
        return;
    }
    let r = Repo::new();
    r.git(&["config", "amont.testPushedTree", "true"]);
    r.stage("README.md", "start\n");
    r.commit("chore: base");
    let base = head(&r);

    // Branch "a": a crate at `crate/` whose test PASSES.
    r.git(&["checkout", "-q", "-b", "a"]);
    r.stage(
        "crate/Cargo.toml",
        "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n",
    );
    r.stage(
        "crate/src/main.rs",
        "fn main() {}\n\n#[test]\nfn t() {\n    assert!(true);\n}\n",
    );
    r.commit("feat(a): passing crate");
    let tip_a = head(&r);

    // Branch "b": the SAME crate path, from the SAME base, but its test
    // FAILS. If the worktree used for "b" is actually "a"'s, this never
    // shows up.
    r.git(&["checkout", "-q", &base]);
    r.git(&["checkout", "-q", "-b", "b"]);
    r.stage(
        "crate/Cargo.toml",
        "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n",
    );
    r.stage(
        "crate/src/main.rs",
        "fn main() {}\n\n#[test]\nfn t() {\n    assert!(false);\n}\n",
    );
    r.commit("feat(b): failing crate");
    let tip_b = head(&r);

    let zero = "0".repeat(40);
    let lines = format!(
        "refs/heads/a {tip_a} refs/heads/a {zero}\n\
         refs/heads/b {tip_b} refs/heads/b {zero}\n"
    );
    let mut child = crate::common::amont_cmd()
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-push-cargo-test")
        .current_dir(&r.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(lines.as_bytes())
        .expect("write");
    let code = child.wait().expect("wait").code().unwrap_or(-1);
    assert_ne!(
        code, 0,
        "branch b's failing crate was tested against branch a's tree instead of its own"
    );
}
