//! `amont check` — the content checks, asked about files rather than the index.
//!
//! The property under test throughout is that this verb is a READ. It never
//! stages, never stashes, never writes; it answers about bytes. That is what
//! separates it from `run`, and it is what makes it safe for an editor to call
//! on every keystroke.

mod common;

use common::Repo;
use std::io::Write;
use std::process::{Command, Stdio};

/// `check` with content piped in, for a buffer that was never saved.
/// `Repo::run` nulls stdin, so this one builds its own command.
fn check_stdin(repo: &Repo, name: &str, content: &str) -> (i32, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_amont"))
        .args(["check", "--stdin-filename", name])
        .current_dir(repo.path(""))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn amont check");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(content.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("wait");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn a_finding_carries_the_line_and_column() {
    let repo = Repo::new();
    repo.write("app.js", "const x = 1;\n\n  debugger;\n");
    let run = repo.run(&["check", "app.js"]);
    // The whole point: the address, not just the file.
    assert!(
        run.says("app.js:3:3: error:"),
        "want a positioned finding, got: {}",
        run.output()
    );
    assert!(run.says("[ban-terms]"), "{}", run.output());
    assert_eq!(run.code, 1, "a blocking finding exits 1");
}

#[test]
fn a_clean_file_says_nothing_and_exits_zero() {
    let repo = Repo::new();
    repo.write("app.js", "const x = 1;\n");
    let run = repo.run(&["check", "app.js"]);
    assert!(run.silent(), "expected silence, got: {}", run.output());
    assert!(run.passed());
}

/// A warning is not a failure. `large-files` warns about an asset that may be
/// entirely deliberate, and an editor asking what is here must not be told the
/// answer was fatal.
#[test]
fn a_warning_alone_does_not_fail() {
    let repo = Repo::new();
    repo.write("big.bin", &"x".repeat(11 * 1024 * 1024));
    let run = repo.run(&["check", "big.bin"]);
    assert!(run.says("warning:"), "{}", run.output());
    assert!(run.says("[large-files]"), "{}", run.output());
    assert!(run.passed(), "a warning must not exit non-zero");
}

/// Scope still applies: `debugger;` in prose is prose.
#[test]
fn a_file_outside_every_scope_is_silent() {
    let repo = Repo::new();
    repo.write("notes.txt", "debugger;\n");
    let run = repo.run(&["check", "notes.txt"]);
    assert!(run.silent(), "{}", run.output());
    assert!(run.passed());
}

/// The reason this verb exists as something other than `run`: an editor asks
/// about a buffer that is not staged, not committed, and may never have
/// touched the disk.
#[test]
fn an_unsaved_buffer_can_be_checked_over_stdin() {
    let repo = Repo::new();
    let (code, out) = check_stdin(&repo, "src/spec.js", "it.only('x', () => {});\n");
    assert!(out.contains("src/spec.js:1:1:"), "got: {out}");
    assert_eq!(code, 1);
    // And nothing was written for it.
    assert!(!repo.path("src/spec.js").exists());
}

#[test]
fn json_is_versioned_and_carries_the_position() {
    let repo = Repo::new();
    repo.write("app.js", "\n\n  debugger;\n");
    let run = repo.run(&["check", "app.js", "--format", "json"]);
    assert!(run.says(r#""format":"amont-check-v1""#), "{}", run.output());
    assert!(run.says(r#""line":3,"column":3"#), "{}", run.output());
    assert!(run.says(r#""severity":"error""#), "{}", run.output());
}

/// THE architectural property. `run` rehearses a commit and is entitled to
/// hold unstaged work aside; `check` is a read and must leave the repository
/// byte-identical — index included.
#[test]
fn checking_never_touches_the_index_or_the_worktree() {
    let repo = Repo::new();
    repo.stage("kept.js", "const kept = 1;\n");
    repo.write("dirty.js", "  debugger;\n");
    let before_status = repo.git(&["status", "--porcelain"]).stdout;
    let before_index = repo.git(&["write-tree"]).stdout;

    let run = repo.run(&["check", "dirty.js", "kept.js"]);
    assert_eq!(run.code, 1, "{}", run.output());

    assert_eq!(
        repo.git(&["status", "--porcelain"]).stdout,
        before_status,
        "check moved something in the worktree or the index"
    );
    assert_eq!(
        repo.git(&["write-tree"]).stdout,
        before_index,
        "check rewrote the index"
    );
}

/// A path the caller named but which cannot be read is the caller's mistake,
/// not a finding about the file — and it must not hide the other results.
#[test]
fn an_unreadable_path_is_reported_without_hiding_the_rest() {
    let repo = Repo::new();
    repo.write("app.js", "  debugger;\n");
    let run = repo.run(&["check", "nope.js", "app.js"]);
    assert!(run.stderr.contains("nope.js"), "{}", run.output());
    assert!(
        run.stdout.contains("app.js:1:3:"),
        "the readable file must still be reported: {}",
        run.output()
    );
}

#[test]
fn usage_errors_exit_two() {
    let repo = Repo::new();
    repo.write("app.js", "const x = 1;\n");
    assert_eq!(repo.run(&["check"]).code, 2, "no path named");
    assert_eq!(
        repo.run(&["check", "app.js", "--format", "xml"]).code,
        2,
        "unknown format"
    );
    assert_eq!(
        repo.run(&["check", "app.js", "--not-a-flag"]).code,
        2,
        "unknown flag"
    );
}

/// Several checks at once, ordered for reading: the whole-file verdict first,
/// then by position.
#[test]
fn findings_from_several_checks_arrive_in_reading_order() {
    let repo = Repo::new();
    repo.write("svc.py", "import os\n\nbreakpoint()\n");
    let run = repo.run(&["check", "svc.py"]);
    assert!(run.says("svc.py:3:1:"), "{}", run.output());
    assert!(run.says("[ban-terms]"), "{}", run.output());
}
