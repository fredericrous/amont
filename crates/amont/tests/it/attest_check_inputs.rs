//! `amont attest check-inputs` — do the named gates declare, completely,
//! everything the tree tracks?
//!
//! Declared inputs let a result be reused on a different tree, so the day a
//! gate's declaration is missing something it reads, a change there is skipped
//! wrongly. These tests pin the ways a declaration can look complete and not
//! be: a subdirectory standing for its parent, one gate rescued by another, a
//! gate not declared at all, a name outside the grammar.

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
fn a_path_omitted_from_one_gate_is_not_rescued_by_another() {
    let repo = tracked(
        Some("full a.txt b.txt\npartial a.txt\n"),
        &[("a.txt", "a"), ("b.txt", "b")],
    );
    let run = check(&repo, &["full", "partial"]);
    assert_eq!(run.code, 1, "{}", run.output());
    assert!(run.says("partial: not covered:"), "{}", run.output());
    assert!(run.says("  b.txt"), "{}", run.output());
    assert!(!run.says("full: not covered"), "{}", run.output());
}

#[test]
fn a_required_gate_the_spec_does_not_declare_fails() {
    let repo = tracked(Some("g a.txt\n"), &[("a.txt", "a")]);
    let run = check(&repo, &["g", "other"]);
    assert_eq!(run.code, 1, "{}", run.output());
    assert!(
        run.says("other: not declared in the spec"),
        "{}",
        run.output()
    );
}

#[test]
fn plans_are_exempt_but_every_other_docs_child_is_not() {
    let repo = tracked(
        Some("g README.md\n"),
        &[
            ("README.md", "r"),
            ("docs/plans/p.md", "p"),
            ("docs/guide.md", "g"),
        ],
    );
    let run = check(&repo, &["g"]);
    assert_eq!(run.code, 1, "{}", run.output());
    assert!(run.says("  docs/guide.md"), "{}", run.output());
    assert!(!run.says("docs/plans"), "{}", run.output());
}

#[test]
fn the_spec_itself_and_the_root_git_files_need_no_declaration() {
    let repo = tracked(
        Some("g a.txt\n"),
        &[
            ("a.txt", "a"),
            (".gitattributes", "* text=auto\n"),
            (".gitmodules", ""),
        ],
    );
    let run = check(&repo, &["g"]);
    assert_eq!(run.code, 0, "{}", run.output());
}

#[test]
fn a_name_outside_the_grammar_is_covered_only_by_an_ascii_ancestor() {
    let spec = "g src\n";
    let under = tracked(Some(spec), &[("src/caf\u{e9}.txt", "x")]);
    assert_eq!(check(&under, &["g"]).code, 0);

    let loose = tracked(
        Some("g src\n"),
        &[("src/a.txt", "a"), ("caf\u{e9}.txt", "x")],
    );
    let run = check(&loose, &["g"]);
    assert_eq!(run.code, 1, "{}", run.output());
    assert!(run.says("caf"), "{}", run.output());
}

#[test]
fn an_optional_path_counts_as_declared_even_when_absent() {
    let repo = tracked(Some("g a.txt ?build.rs\n"), &[("a.txt", "a")]);
    let run = check(&repo, &["g"]);
    assert_eq!(run.code, 0, "{}", run.output());
}

#[test]
fn a_required_path_that_is_missing_cannot_be_fingerprinted() {
    let repo = tracked(Some("g a.txt nope\n"), &[("a.txt", "a")]);
    let run = check(&repo, &["g"]);
    assert_eq!(run.code, 1, "{}", run.output());
    assert!(run.says("g: cannot be fingerprinted"), "{}", run.output());
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

#[test]
fn the_spec_is_read_from_the_tree_not_the_working_copy() {
    let repo = tracked(Some("g a.txt\n"), &[("a.txt", "a")]);
    // The working copy now claims less; the committed spec is what counts.
    repo.write(SPEC, "g\n");
    let run = check(&repo, &["g"]);
    assert_eq!(run.code, 0, "{}", run.output());
}
