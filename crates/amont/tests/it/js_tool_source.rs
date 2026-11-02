//! A JS check says which tool it ran (ADR-0019: a repository pins its tools,
//! never the machine's PATH). Prettier and eslint run the repository's
//! `node_modules/.bin` copy silently. An undeclared copy taken from PATH or
//! npx's cache still runs, with two warning lines that name it and the
//! command that pins it. A copy package.json declares but nobody installed is
//! not replaced by PATH's: the check does not run, and says to install.
//!
//! Fake tools on a PATH holding nothing else (plus git) record every run, so
//! "it never ran" does not depend on what the machine has installed.
#![cfg(unix)]

use crate::common;
use common::Repo;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

/// A fake tool at `path`: `--version` runs `version` (shell), anything else
/// appends its argv to `<log>` and exits 0.
fn fake(path: &Path, version: &str, log: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
    std::fs::write(
        path,
        format!(
            "#!/bin/sh\ncase \"$1\" in --version) {version};; esac\necho \"$@\" >> {}\nexit 0\n",
            log.display()
        ),
    )
    .expect("write");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

fn shims(r: &Repo) -> PathBuf {
    let dir = r.path(".git/toolshims");
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn log(r: &Repo) -> PathBuf {
    r.path(".git/toolshims/argv.txt")
}

fn ran(r: &Repo) -> String {
    std::fs::read_to_string(log(r)).unwrap_or_default()
}

/// Run one check from `cwd` with PATH = the shim dir alone plus a git symlink.
fn run_check_at(r: &Repo, cwd: &Path, check: &str) -> (bool, String, Duration) {
    let dir = shims(r);
    let git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|d| d.join("git"))
        .find(|p| p.is_file())
        .expect("git on PATH");
    let _ = std::os::unix::fs::symlink(git, dir.join("git"));
    let started = Instant::now();
    let out = common::amont_cmd()
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg(check)
        .current_dir(cwd)
        .env("PATH", dir.display().to_string())
        .stdin(Stdio::null())
        .output()
        .expect("run");
    let text = plain(&format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ));
    (out.status.success(), text, started.elapsed())
}

/// `s` without its colour codes (`ESC [ … m`), so a highlighted command reads
/// as the text a person would copy.
fn plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn run_check(r: &Repo, check: &str) -> (bool, String, Duration) {
    run_check_at(r, &r.dir, check)
}

/// A repo that opts into prettier, with one staged markdown file.
fn prettier_repo(package_json: &str) -> Repo {
    let r = Repo::new();
    r.stage(".prettierrc", "{}\n");
    r.stage("package.json", package_json);
    r.stage("a.md", "# a\n");
    r
}

const UNDECLARED: &str = "{\"name\":\"x\"}\n";
const DECLARED: &str = "{\"name\":\"x\",\"devDependencies\":{\"prettier\":\"3.9.10\"}}\n";

#[test]
fn an_undeclared_prettier_on_path_runs_and_is_named() {
    let r = prettier_repo(UNDECLARED);
    let bin = shims(&r).join("prettier");
    fake(&bin, "echo 3.9.10; exit 0", &log(&r));
    let (ok, out, _) = run_check(&r, "pre-commit-prettier");
    assert!(ok, "{out}");
    assert!(ran(&r).contains("--check"), "prettier ran: {out}");
    assert!(
        out.contains(&format!(
            "prettier 3.9.10 from {} is not pinned in package.json",
            bin.display()
        )),
        "{out}"
    );
    assert!(
        out.contains("npm install --save-dev --save-exact prettier@3.9.10"),
        "{out}"
    );
}

#[test]
fn the_pin_command_follows_the_lockfile() {
    let r = prettier_repo(UNDECLARED);
    r.stage("pnpm-lock.yaml", "lockfileVersion: '9.0'\n");
    fake(&shims(&r).join("prettier"), "echo 3.9.10; exit 0", &log(&r));
    let (_, out, _) = run_check(&r, "pre-commit-prettier");
    assert!(
        out.contains("pnpm add -D --save-exact prettier@3.9.10"),
        "{out}"
    );
}

#[test]
fn an_undeclared_prettier_from_npx_is_named_and_npx_starts_once_for_its_version() {
    let r = prettier_repo(UNDECLARED);
    let dir = shims(&r);
    let probes = dir.join("probes.txt");
    // npx answers the probe with a warning on stderr first, as npm does.
    std::fs::write(
        dir.join("npx"),
        format!(
            "#!/bin/sh\nif [ \"$1 $2 $3\" = \"--no-install prettier --version\" ]; then\n  echo x >> {}\n  echo 'npm warn exec something' >&2\n  echo 3.9.10\n  exit 0\nfi\necho \"$@\" >> {}\nexit 0\n",
            probes.display(),
            log(&r).display()
        ),
    )
    .expect("write");
    std::fs::set_permissions(dir.join("npx"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
    let (ok, out, _) = run_check(&r, "pre-commit-prettier");
    assert!(ok, "{out}");
    assert!(
        ran(&r).contains("--check"),
        "prettier ran through npx: {out}"
    );
    assert!(
        out.contains("prettier 3.9.10 from npx's cache (npx --no-install prettier)"),
        "{out}"
    );
    assert_eq!(
        std::fs::read_to_string(&probes)
            .unwrap_or_default()
            .lines()
            .count(),
        1,
        "one npx probe, its output reused for the version"
    );
}

#[test]
fn a_declared_prettier_that_is_not_installed_never_runs_from_path() {
    let r = prettier_repo(DECLARED);
    fake(&shims(&r).join("prettier"), "echo 2.4.0; exit 0", &log(&r));
    let (ok, out, _) = run_check(&r, "pre-commit-prettier");
    assert!(ok, "not installed does not block the commit: {out}");
    assert_eq!(ran(&r), "", "PATH's prettier must not run: {out}");
    assert!(
        out.contains("prettier is in package.json but not installed: run npm install"),
        "{out}"
    );
}

#[test]
fn a_pinned_prettier_says_nothing_new_declared_or_not() {
    for manifest in [UNDECLARED, DECLARED] {
        let r = prettier_repo(manifest);
        fake(
            &r.path("node_modules/.bin/prettier"),
            "echo 3.9.10; exit 0",
            &log(&r),
        );
        let (ok, out, _) = run_check(&r, "pre-commit-prettier");
        assert!(ok, "{out}");
        assert!(ran(&r).contains("--check"), "{out}");
        assert!(!out.contains("not pinned"), "{out}");
        assert!(!out.contains("not installed"), "{out}");
    }
}

#[test]
fn a_version_probe_that_reads_stdin_does_not_hang() {
    let r = prettier_repo(UNDECLARED);
    fake(
        &shims(&r).join("prettier"),
        "read x; echo 3.9.10; exit 0",
        &log(&r),
    );
    let (ok, out, took) = run_check(&r, "pre-commit-prettier");
    assert!(ok, "{out}");
    assert!(out.contains("prettier 3.9.10 from"), "{out}");
    assert!(took < Duration::from_secs(30), "took {took:?}");
}

fn group_alive(pgid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", "--", &format!("-{}", pgid.trim())])
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// The probe's five seconds (never scaled), the kill grace, and room for a
/// loaded machine.
fn probe_bound() -> Duration {
    Duration::from_secs(5) + Duration::from_millis(500) + Duration::from_secs(4)
}

#[test]
fn a_version_probe_that_stalls_is_killed_with_its_child_and_the_check_runs() {
    let r = prettier_repo(UNDECLARED);
    let pgid = r.path(".git/toolshims/pgid.txt");
    fake(
        &shims(&r).join("prettier"),
        &format!("echo $$ > {}; /bin/sleep 600 & wait", pgid.display()),
        &log(&r),
    );
    let (ok, out, took) = run_check(&r, "pre-commit-prettier");
    assert!(ok, "{out}");
    assert!(took < probe_bound(), "took {took:?}");
    assert!(
        out.contains("prettier (unknown version: --version did not answer in 5 s)"),
        "{out}"
    );
    assert!(ran(&r).contains("--check"), "the check still runs: {out}");
    let pgid = std::fs::read_to_string(&pgid).expect("the probe wrote its group");
    std::thread::sleep(Duration::from_millis(100));
    assert!(!group_alive(&pgid), "the probe's group {pgid} survived");
}

#[test]
fn a_version_probe_that_leaves_a_child_holding_its_output_is_killed() {
    let r = prettier_repo(UNDECLARED);
    let pgid = r.path(".git/toolshims/pgid.txt");
    fake(
        &shims(&r).join("prettier"),
        &format!(
            "echo $$ > {}; /bin/sleep 600 & echo 3.9.10; exit 0",
            pgid.display()
        ),
        &log(&r),
    );
    let (ok, out, took) = run_check(&r, "pre-commit-prettier");
    assert!(ok, "{out}");
    assert!(took < probe_bound(), "took {took:?}");
    let pgid = std::fs::read_to_string(&pgid).expect("the probe wrote its group");
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        !group_alive(&pgid),
        "the child left holding the pipe survived"
    );
}

/// Only this checkout's package.json decides; the main worktree may lend a
/// binary, never its manifest.
#[test]
fn a_linked_worktree_is_judged_by_its_own_package_json() {
    let r = prettier_repo(DECLARED);
    r.commit("declares prettier");
    let wt = r.worktree("linked");
    // The linked checkout drops the declaration; the main one keeps it.
    std::fs::write(wt.join("package.json"), UNDECLARED).expect("write");
    std::fs::write(wt.join("b.md"), "# b\n").expect("write");
    let add = std::process::Command::new("git")
        .args(["add", "--", "package.json", "b.md"])
        .current_dir(&wt)
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_WORK_TREE")
        .status()
        .expect("git add");
    assert!(add.success());
    fake(&shims(&r).join("prettier"), "echo 3.9.10; exit 0", &log(&r));
    let (ok, out, _) = run_check_at(&r, &wt, "pre-commit-prettier");
    assert!(ok, "{out}");
    assert!(
        ran(&r).contains("--check"),
        "undeclared here, so it runs: {out}"
    );
    assert!(out.contains("not pinned in package.json"), "{out}");

    // And the other way: the main checkout declares it, so it does not run
    // there, whatever the linked one says.
    std::fs::remove_file(log(&r)).ok();
    r.stage("c.md", "# c\n");
    let (ok, out, _) = run_check(&r, "pre-commit-prettier");
    assert!(ok, "{out}");
    assert_eq!(ran(&r), "", "{out}");
    assert!(out.contains("not installed"), "{out}");
}

fn eslint_repo(package_json: &str) -> Repo {
    let r = Repo::new();
    r.stage(".eslintrc.json", "{}\n");
    r.stage("package.json", package_json);
    r.stage("a.js", "let x = 1\n");
    r
}

#[test]
fn an_undeclared_eslint_is_named_without_its_v() {
    let r = eslint_repo(UNDECLARED);
    fake(&shims(&r).join("eslint"), "echo v9.14.0; exit 0", &log(&r));
    let (ok, out, _) = run_check(&r, "pre-commit-lint-js");
    assert!(ok, "{out}");
    assert!(ran(&r).contains("--max-warnings"), "{out}");
    assert!(out.contains("eslint 9.14.0 from"), "{out}");
    assert!(out.contains("eslint@9.14.0"), "{out}");
}

#[test]
fn a_declared_eslint_that_is_not_installed_never_runs_from_path() {
    let r = eslint_repo("{\"devDependencies\":{\"eslint\":\"9.14.0\"}}\n");
    fake(&shims(&r).join("eslint"), "echo v8.0.0; exit 0", &log(&r));
    let (ok, out, _) = run_check(&r, "pre-commit-lint-js");
    assert!(ok, "{out}");
    assert_eq!(ran(&r), "", "{out}");
    assert!(
        out.contains("eslint is in package.json but not installed: run npm install"),
        "{out}"
    );
}
