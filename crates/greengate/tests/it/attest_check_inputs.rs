//! `amont attest check-inputs` — do the named gates declare, completely,
//! everything the tree tracks?
//!
//! The proof is attest's (`git_attest::spec::check_coverage`, decisions
//! ADR-0037), and attest's own table pins every way a declaration can look
//! complete and not be. What stays here is amont's wiring: each of the three
//! answers reaches the terminal with its exit code, and a missing
//! `--require` is a usage error.

use crate::common;

use common::{HookRun, Repo};

const SPEC: &str = ".github/attest-inputs";

/// A repository tracking `files`, plus the spec text, committed.
fn tracked(spec: Option<&str>, files: &[(&str, &str)]) -> Repo {
    let repo = Repo::new();
    for (path, body) in files {
        repo.stage(path, body);
    }
    if let Some(spec) = spec {
        repo.stage(SPEC, spec);
    }
    repo.commit("fixture");
    repo
}

fn check(repo: &Repo, gates: &[&str]) -> HookRun {
    let mut args = vec!["attest", "check-inputs", "--require"];
    args.extend_from_slice(gates);
    repo.run(&args)
}

#[test]
fn a_gate_declaring_every_tracked_file_is_covered() {
    let repo = tracked(
        Some("g crates README.md\n"),
        &[("crates/a/x.rs", "x"), ("README.md", "r")],
    );
    let run = check(&repo, &["g"]);
    assert_eq!(run.code, 0, "{}", run.output());
    assert!(run.says("check-attest-inputs: ok"), "{}", run.output());
}

#[test]
fn a_subdirectory_does_not_stand_for_its_parent() {
    let repo = tracked(
        Some("g crates/foo\n"),
        &[("crates/foo/x.rs", "x"), ("crates/bar/y.rs", "y")],
    );
    let run = check(&repo, &["g"]);
    assert_eq!(run.code, 1, "{}", run.output());
    assert!(run.says("g: not covered:"), "{}", run.output());
    assert!(run.says("  crates/bar/y.rs"), "{}", run.output());
    assert!(!run.says("  crates/foo/x.rs"), "{}", run.output());
}

#[test]
fn an_unusable_spec_exits_two_and_says_which() {
    let none = tracked(None, &[("a.txt", "a")]);
    let run = check(&none, &["g"]);
    assert_eq!(run.code, 2, "{}", run.output());
    assert!(run.says("no .forgejo/attest-inputs or"), "{}", run.output());

    // A glob invalidates the whole spec. With no allowed_signers anywhere,
    // `verify.sh` would have answered "nothing covered" and exited 0 before
    // ever reading it; this guard has to say invalid.
    let glob = tracked(Some("g src/*\n"), &[("a.txt", "a")]);
    let run = check(&glob, &["g"]);
    assert_eq!(run.code, 2, "{}", run.output());
    assert!(run.says("is invalid"), "{}", run.output());

    let both = tracked(Some("g a.txt\n"), &[("a.txt", "a")]);
    both.stage(".forgejo/attest-inputs", "g a.txt\n");
    both.commit("second spec");
    let run = check(&both, &["g"]);
    assert_eq!(run.code, 2, "{}", run.output());
    assert!(run.says("both"), "{}", run.output());
}

#[test]
fn without_a_gate_to_require_it_is_a_usage_error() {
    let repo = tracked(Some("g a.txt\n"), &[("a.txt", "a")]);
    let run = repo.run(&["attest", "check-inputs"]);
    assert_eq!(run.code, 2, "{}", run.output());
    assert!(run.says("needs --require"), "{}", run.output());
}
