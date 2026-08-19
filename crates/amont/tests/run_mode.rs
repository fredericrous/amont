//! `amont run` — the checks, on demand.
//!
//! Two questions, and the tests pin that they are different. Staged is "would
//! my commit pass"; `--all-files` is "does my working tree pass", which on a
//! dirty tree reports on content that is not committed and may never be.

mod common;
use common::{missing, Repo};
use std::process::Command;

fn run(r: &Repo, args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("run")
        .args(args)
        .current_dir(&r.dir)
        .output()
        .expect("amont run");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// The default is a rehearsal of the commit: the same set a commit would check.
#[test]
fn run_checks_the_staged_set() {
    let r = Repo::new();
    r.stage("good.json", "{ \"a\": 1 }\n");
    r.commit("chore: seed");
    // Committed but NOT staged now — invisible to the staged question.
    r.write("bad.json", "{ BROKEN\n");
    r.stage("good.json", "{ \"a\": 2 }\n");

    let (code, out) = run(&r, &[]);
    assert_eq!(code, 0, "the staged set is valid:\n{out}");
    assert!(
        !out.contains("bad.json"),
        "it looked outside the index:\n{out}"
    );
}

/// `--all-files` asks the other question, and finds what staging hid.
#[test]
fn all_files_sees_what_is_not_staged() {
    let r = Repo::new();
    r.stage("good.json", "{ \"a\": 1 }\n");
    r.commit("chore: seed");
    r.stage("bad.json", "{ BROKEN\n");
    r.commit("chore: add bad");

    let (code, out) = run(&r, &["--all-files"]);
    assert_ne!(code, 0, "should have found the broken file:\n{out}");
    assert!(out.contains("bad.json"), "{out}");
}

/// The two modes must actually differ, or one of them is decoration.
#[test]
fn the_two_modes_answer_different_questions() {
    let r = Repo::new();
    r.stage("good.json", "{ \"a\": 1 }\n");
    r.commit("chore: seed");
    r.stage("bad.json", "{ BROKEN\n");
    r.commit("chore: add bad");
    // Nothing staged at all now.
    let (staged, _) = run(&r, &[]);
    let (all, _) = run(&r, &["--all-files"]);
    assert_eq!(staged, 0, "nothing staged means nothing to judge");
    assert_ne!(all, 0, "but the tree still holds a broken file");
}

/// Staged mode is a REHEARSAL of the commit, so it must take the same
/// index-fidelity hold the commit takes.
///
/// It did not. Reproduced with the fixture in
/// docs/index-fidelity-and-run-modes.md §1: valid JSON staged, garbage in the
/// working tree. `git commit` passed (the hold puts the staged content at the
/// path) and `amont run` failed — the two modes disagreed about the same
/// repository, which is the one thing this mode exists not to do.
#[test]
fn run_judges_the_staged_content_not_the_tree() {
    if missing("node") {
        return;
    }
    let r = Repo::new();
    r.stage("x.json", "{ \"a\": 1 }\n");
    r.commit("chore: seed");
    r.stage("x.json", "{ \"a\": 2 }\n");
    r.write("x.json", "{ THIS IS NOT JSON\n");

    let (code, out) = run(&r, &[]);
    assert_eq!(code, 0, "it judged the working tree, not the index:\n{out}");
    assert_eq!(
        std::fs::read_to_string(r.path("x.json")).expect("read"),
        "{ THIS IS NOT JSON\n",
        "the unstaged work was not put back"
    );
}

/// The false-negative direction, which is the worse one: the tree is fine and
/// the staged content is not.
#[test]
fn run_still_catches_a_broken_staged_change() {
    if missing("node") {
        return;
    }
    let r = Repo::new();
    r.stage("x.json", "{ \"a\": 1 }\n");
    r.commit("chore: seed");
    r.stage("x.json", "{ THIS IS NOT JSON\n");
    r.write("x.json", "{ \"a\": 2 }\n");

    let (code, out) = run(&r, &[]);
    assert_ne!(code, 0, "a broken staged change passed:\n{out}");
    assert_eq!(
        std::fs::read_to_string(r.path("x.json")).expect("read"),
        "{ \"a\": 2 }\n",
        "the unstaged work was not put back"
    );
}

/// `--all-files` stays stash-free — decision 1 of
/// docs/index-fidelity-and-run-modes.md. There is no staged/unstaged
/// distinction to protect when the input set is `git ls-files`.
///
/// Proven by POLLING for the store while a deliberately slow declared check is
/// running, the trick `index_fidelity.rs` already uses: `enter()` parks before
/// any check starts, so if a hold were taken the directory would exist for the
/// whole five seconds.
#[cfg(unix)]
#[test]
fn all_files_takes_no_hold() {
    let r = Repo::new();
    r.stage("x.json", "{ \"a\": 1 }\n");
    r.stage("amont.conf", "pre-commit  slow  *  block  sleep  5\n");
    r.commit("chore: seed");
    // Something unstaged, so a hold would have work to do.
    r.write("x.json", "{ \"a\": 999 }\n");
    let trusted = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("trust")
        .current_dir(&r.dir)
        .output()
        .expect("amont trust");
    assert!(trusted.status.success(), "could not trust the manifest");

    let mut child = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("run")
        .arg("--all-files")
        .current_dir(&r.dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn amont run --all-files");

    let store = r.path(".git/amont-held");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        assert!(
            !store.exists(),
            "--all-files took a hold; it must be stash-free"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// A pre-push check invoked by name must never touch the working tree: nothing
/// about a push is a staging operation.
#[cfg(unix)]
#[test]
fn a_named_pre_push_check_takes_no_hold() {
    let r = Repo::new();
    r.stage("x.json", "{ \"a\": 1 }\n");
    r.commit("chore: seed");
    let origin = r.path(".git/test-origin.git");
    r.git(&["init", "-q", "--bare", origin.to_str().unwrap()]);
    r.git(&["remote", "add", "origin", origin.to_str().unwrap()]);
    r.git(&["push", "-q", "--no-verify", "-u", "origin", "main"]);
    // Unstaged work a hold would have parked.
    r.write("x.json", "{ \"a\": 999 }\n");

    let (_, out) = run(&r, &["pre-push-branch-pattern"]);
    assert!(
        !r.path(".git/amont-held").exists(),
        "a pre-push check parked the tree:\n{out}"
    );
    assert_eq!(
        std::fs::read_to_string(r.path("x.json")).expect("read"),
        "{ \"a\": 999 }\n",
        "a pre-push check modified the working tree:\n{out}"
    );
}

/// `--all-files` with `amont.fix` on must stage NOTHING.
///
/// The override makes `staged_files()` return every tracked file, and every
/// fixer's `restage(&files)` would then `git add` everything in the tree that
/// differs from the index — turning a read-only "does my tree pass" query into
/// `git add .`. That is the hazard §2 of
/// docs/index-fidelity-and-run-modes.md names.
#[test]
fn all_files_never_stages_anything() {
    let r = Repo::new();
    r.stage("a.js", "const a = BAD\n");
    r.stage("b.js", "const b = BAD\n");
    // A declared fixer, so the mode has something that WOULD re-stage.
    r.write(
        "rewrite.sh",
        "#!/bin/sh\nfor f in *.js; do [ -f \"$f\" ] || continue; sed -i.bak 's/BAD/GOOD/' \"$f\"; rm -f \"$f.bak\"; done\nexit 0\n",
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(r.path("rewrite.sh"), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
    }
    r.git(&["add", "rewrite.sh"]);
    r.stage(
        "amont.conf",
        "pre-commit  fmt  *.js  block  fix ./rewrite.sh\n",
    );
    r.commit("chore: seed");
    let trusted = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("trust")
        .current_dir(&r.dir)
        .output()
        .expect("amont trust");
    assert!(trusted.status.success(), "could not trust the manifest");
    r.git(&["config", "amont.fix", "true"]);

    // A dirty tree, nothing staged.
    r.write("a.js", "const a = BAD\n");
    r.write("b.js", "const b = STILL BAD\n");

    let (_, out) = run(&r, &["--all-files"]);
    let staged = r.git(&["diff", "--cached", "--name-only"]);
    let staged = String::from_utf8_lossy(&staged.stdout);
    assert!(
        staged.trim().is_empty(),
        "--all-files staged files: {staged}\n{out}"
    );
    assert!(
        out.contains("fixing is off"),
        "it must say why amont.fix did nothing:\n{out}"
    );
}

/// One check by name, which is how you adopt a single check.
#[test]
fn a_named_check_runs_alone() {
    let r = Repo::new();
    r.stage("a.txt", "hello\n");
    let (code, out) = run(&r, &["pre-commit-merge-conflict"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("merge"), "the named check did not run:\n{out}");
    assert!(
        !out.contains("package.json"),
        "it ran the whole stage instead:\n{out}"
    );
}

/// A typo must not look like a clean run.
#[test]
fn an_unknown_check_is_an_error_not_a_pass() {
    let r = Repo::new();
    let (code, out) = run(&r, &["pre-commit-not-a-thing"]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("amont list"), "should point somewhere:\n{out}");
}

/// A pre-push check run standalone has no real push to read refs from. With
/// no upstream configured either, there is nothing to synthesize one from —
/// that must be a clear error, not a silent pass with an empty ref list
/// (which is what an unset `PushRefs` looked like to `branch_protect`, since
/// `amont/tests/common::Repo::git` always closes the child's stdin).
#[test]
fn a_named_pre_push_check_with_no_upstream_errors_instead_of_passing_empty() {
    let r = Repo::new();
    r.stage("a.txt", "x\n");
    r.commit("init");
    let (code, out) = run(&r, &["pre-push-branch-protect"]);
    assert_eq!(code, 2, "should refuse, not silently pass:\n{out}");
    assert!(out.contains("upstream"), "{out}");
}

/// With an upstream configured, the standalone run synthesizes the same ref
/// a real push would carry — proven here by a check whose verdict depends on
/// which branch that ref names.
#[test]
fn a_named_pre_push_check_uses_the_upstream_as_its_push_ref() {
    let r = Repo::new();
    r.stage("a.txt", "x\n");
    r.commit("init");
    let origin = r.path(".git/test-origin.git");
    r.git(&["init", "-q", "--bare", origin.to_str().unwrap()]);
    r.git(&["remote", "add", "origin", origin.to_str().unwrap()]);
    r.git(&["push", "-q", "--no-verify", "-u", "origin", "main"]);

    // `main` is protected, and this push targets `main` — must be refused.
    let (code, out) = run(&r, &["pre-push-branch-protect"]);
    assert_ne!(
        code, 0,
        "a synthesized push to main must be blocked:\n{out}"
    );
    assert!(out.contains("main"), "{out}");
}

/// A short name runs the check it names, exactly as `hook.skip` would have
/// resolved it — the rest of the tool taught the short vocabulary, and `run`
/// demanding the full id was a second one.
#[test]
fn a_short_name_resolves_like_a_skip() {
    let r = Repo::new();
    r.stage("f.ts", "debugger;\n");
    let (code, out) = run(&r, &["ban-terms"]);
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("Unwanted terms"), "{out}");
}

/// A short name reaching two checks is an answer, not a guess: both full ids
/// are listed and nothing runs.
#[test]
fn an_ambiguous_short_name_lists_its_candidates() {
    let r = Repo::new();
    r.stage("a.txt", "x\n");
    let (code, out) = run(&r, &["branch-pattern"]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("more than one check"), "{out}");
    assert!(out.contains("pre-commit-branch-pattern"), "{out}");
    assert!(out.contains("pre-push-branch-pattern"), "{out}");
}

/// A nonsense name is still unknown, with the same pointer it always had.
#[test]
fn an_unknown_name_is_still_unknown() {
    let r = Repo::new();
    let (code, out) = run(&r, &["no-such-check"]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("unknown check"), "{out}");
}
