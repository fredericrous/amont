//! `amont agents-md` — writing/updating the marker-delimited block in
//! `AGENTS.md`, and `--check` for CI drift detection.
//!
//! The core marker-detection logic (`desired_file_content`) is unit-tested in
//! `amont-runtime`; these drive the actual binary and a real file on disk,
//! the way `pull_rebase.rs`'s suite drives `pre-push-pull-rebase` rather than
//! re-testing `divergence` through the CLI.
//!
//! Writing a new file and `--check` before and after it are steps of the
//! install flow (`flows::onboarding`); the edge cases stay here.

use crate::common;
use common::Repo;

fn run_agents_md(r: &Repo, extra: &[&str]) -> (i32, String, String) {
    let out = crate::common::amont_cmd()
        .arg("agents-md")
        .args(extra)
        .current_dir(&r.dir)
        .output()
        .expect("run");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn preserves_unrelated_existing_content_when_appending() {
    let r = Repo::new();
    r.commit("init");
    r.write("AGENTS.md", "# My Project\n\nSome existing notes.\n");

    let (code, _, _) = run_agents_md(&r, &[]);
    assert_eq!(code, 0);
    let content = std::fs::read_to_string(r.path("AGENTS.md")).expect("read");
    assert!(content.starts_with("# My Project\n\nSome existing notes.\n"));
    assert!(content.contains("<!-- amont:start -->"));
}

#[test]
fn replaces_only_the_marked_block() {
    let r = Repo::new();
    r.commit("init");
    r.write(
        "AGENTS.md",
        "before\n\n<!-- amont:start -->\nSTALE CONTENT\n<!-- amont:end -->\n\nafter\n",
    );

    let (code, _, _) = run_agents_md(&r, &[]);
    assert_eq!(code, 0);
    let content = std::fs::read_to_string(r.path("AGENTS.md")).expect("read");
    assert!(content.starts_with("before\n\n"));
    assert!(content.ends_with("\n\nafter\n"));
    assert!(!content.contains("STALE CONTENT"));
}

#[test]
fn check_detects_drift() {
    let r = Repo::new();
    r.commit("init");
    r.write(
        "AGENTS.md",
        "<!-- amont:start -->\nSTALE\n<!-- amont:end -->\n",
    );
    let (code, _, stderr) = run_agents_md(&r, &["--check"]);
    assert_ne!(code, 0);
    assert!(stderr.contains("drifted"), "{stderr}");
}

#[test]
fn respects_a_custom_path() {
    let r = Repo::new();
    r.commit("init");
    let (code, stdout, _) = run_agents_md(&r, &["--path", "docs/AGENTS.md"]);
    assert_eq!(code, 0, "{stdout}");
    assert!(
        std::fs::read_to_string(r.path("docs/AGENTS.md")).is_ok(),
        "the custom path must be honoured, not the default AGENTS.md"
    );
    assert!(!r.path("AGENTS.md").exists());
}

/// A `--path` with nothing after it (a typo, or a truncated command) must be
/// a usage error, not a silent fall-through to the default `AGENTS.md` — that
/// would mutate the wrong tracked file, mirroring `--stage`'s own handling of
/// a missing value.
#[test]
fn a_path_flag_with_no_value_is_a_usage_error() {
    let r = Repo::new();
    r.commit("init");
    let (code, _, stderr) = run_agents_md(&r, &["--path"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("--path requires a value"), "{stderr}");
    assert!(
        !r.path("AGENTS.md").exists(),
        "must not fall back to writing the default file"
    );
}

/// Outside a repository there is no root to write into — and `repo_root()`
/// answered `"."` anyway, so this wrote `./AGENTS.md` into whatever directory
/// the user was standing in and printed `wrote ./AGENTS.md` as if that were an
/// answer. Typed from `~`, that is a file in somebody's home directory
/// describing checks that no repository there runs.
///
/// `GIT_CEILING_DIRECTORIES` stops git walking up out of the temp dir, so this
/// tests "no repository" rather than "no repository unless the CI runner keeps
/// its temp dir inside one".
#[test]
fn outside_a_repository_it_writes_nothing_and_says_so() {
    let dir = std::env::temp_dir().join(format!("gh-agents-norepo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");

    let out = crate::common::amont_cmd()
        .arg("agents-md")
        .current_dir(&dir)
        .env("GIT_CEILING_DIRECTORIES", &dir)
        .output()
        .expect("run");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert_eq!(
        out.status.code(),
        Some(2),
        "writing into a non-repository reported success:\n{text}"
    );
    assert!(
        text.contains("not inside a git repository"),
        "and did not say why:\n{text}"
    );
    assert!(
        !dir.join("AGENTS.md").exists(),
        "AGENTS.md WAS WRITTEN into a directory that is not a repository"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
