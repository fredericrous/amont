//! `amont run <check>` under quiet (fleet plan 2026-10-11, phases 1–2): the
//! tool's output goes to a log, a pass is one line, anything else is the
//! check's own messages, the failure blocks in full and the log's path.
//!
//! The tests run the binary with piped stdio, which is how an agent runs it,
//! so `amont.quiet auto` is on unless a test says otherwise.
#![cfg(unix)]

mod common;
use common::Repo;
use std::process::Command;

/// A committed script, run directly by the manifest (no shell between).
fn script(r: &Repo, name: &str, body: &str) {
    r.stage(name, &format!("#!/bin/sh\n{body}"));
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(r.path(name), std::fs::Permissions::from_mode(0o755)).unwrap();
    r.git(&["add", name]);
    // Run it once: macOS holds the first exec of a fresh script for seconds.
    let _ = Command::new(r.path(name)).output();
}

fn declare(r: &Repo, severity: &str, script_name: &str) {
    r.stage(
        "amont.conf",
        &format!("pre-commit  probe  *  {severity}  ./{script_name}\n"),
    );
    let out = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("trust")
        .current_dir(&r.dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "could not trust the manifest");
}

fn run(r: &Repo, args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_amont"));
    cmd.arg("run")
        .args(args)
        .current_dir(&r.dir)
        .env_remove("CI");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn logged(stdout: &str, prefix: &str) -> String {
    let line = stdout
        .lines()
        .find_map(|l| l.split_once(prefix).map(|(_, p)| p.trim().to_string()))
        .unwrap_or_else(|| panic!("no {prefix:?} in\n{stdout}"));
    std::fs::read_to_string(&line).unwrap_or_else(|e| panic!("{line}: {e}"))
}

const CHATTY_PASS: &str = "i=0\nwhile [ $i -lt 200 ]; do echo \"   Compiling crate-$i v0.1.0\"; i=$((i+1)); done\necho 'test result: ok. 3 passed; 0 failed'\n";
const PANIC: &str = "i=0\nwhile [ $i -lt 20 ]; do echo \"   Compiling widget-$i v0.3.0\"; i=$((i+1)); done\necho \"thread 'render::diff' panicked at src/render.rs:88:9:\"\necho 'assertion `left == right` failed'\necho '  left: \"beta\"'\necho ' right: \"BETA\"'\necho\necho 'test result: FAILED. 1 passed; 1 failed'\nexit 101\n";

fn repo_with(severity: &str, body: &str) -> Repo {
    let r = Repo::new();
    // The harness pins `never` so the rest of the suite sees today's output;
    // these tests are about the real default, `auto`, which a piped run (an
    // agent) resolves to quiet.
    r.git(&["config", "amont.quiet", "auto"]);
    script(&r, "probe.sh", body);
    declare(&r, severity, "probe.sh");
    r.stage("a.txt", "a\n");
    r
}

#[test]
fn a_passing_check_is_one_line_and_its_log_has_everything() {
    let r = repo_with("block", CHATTY_PASS);
    let (code, stdout, _) = run(&r, &["probe"], &[]);
    assert_eq!(code, 0, "{stdout}");
    assert_eq!(stdout.lines().count(), 1, "{stdout}");
    assert!(stdout.starts_with("ok probe "), "{stdout}");
    let log = logged(&stdout, "log=");
    assert!(log.contains("Compiling crate-199"), "{log}");
    assert!(log.contains("test result: ok"), "{log}");
}

#[test]
fn a_failing_check_shows_its_failure_its_runner_exit_and_the_log() {
    let r = repo_with("block", PANIC);
    let (code, stdout, _) = run(&r, &["probe"], &[]);
    assert_eq!(
        code, 1,
        "amont's own exit code, not the runner's:\n{stdout}"
    );
    let log = logged(&stdout, "full log: ");
    for must in [
        "panicked at src/render.rs:88:9",
        "left: \"beta\"",
        "right: \"BETA\"",
        "test result: FAILED. 1 passed; 1 failed",
        "probe failed, runner exit 101",
        "full log: ",
    ] {
        assert!(
            stdout.contains(must),
            "missing {must:?} in\n{stdout}\n--- log:\n{log}"
        );
    }
    assert!(
        log.contains("Compiling widget"),
        "the log keeps everything:\n{log}"
    );
}

#[test]
fn raw_keeps_the_tools_own_output() {
    let r = repo_with("block", CHATTY_PASS);
    let (code, stdout, _) = run(&r, &["probe", "--raw"], &[]);
    assert_eq!(code, 0);
    assert!(stdout.contains("Compiling crate-199"), "{stdout}");
    assert!(!stdout.contains("ok probe "), "{stdout}");
}

#[test]
fn quiet_never_keeps_the_tools_own_output() {
    let r = repo_with("block", CHATTY_PASS);
    r.git(&["config", "amont.quiet", "never"]);
    let (_, stdout, _) = run(&r, &["probe"], &[]);
    assert!(stdout.contains("Compiling crate-199"), "{stdout}");
}

#[test]
fn in_ci_a_failure_prints_its_whole_log() {
    let body =
        "i=0\nwhile [ $i -lt 2000 ]; do echo \"line $i of a long log\"; i=$((i+1)); done\nexit 1\n";
    let r = repo_with("block", body);
    let (code, stdout, _) = run(&r, &["probe"], &[("CI", "true")]);
    assert_eq!(code, 1);
    assert!(stdout.contains("line 0 of a long log"), "{stdout}");
    assert!(stdout.contains("line 1999 of a long log"));
}

#[test]
fn a_warning_check_shows_its_failure_and_does_not_block() {
    let r = repo_with("warn", PANIC);
    let (code, stdout, _) = run(&r, &["probe"], &[]);
    assert_eq!(code, 0, "{stdout}");
    assert!(
        stdout.contains("panicked at src/render.rs:88:9"),
        "{stdout}"
    );
    assert!(stdout.contains("full log: "), "{stdout}");
    assert!(!stdout.starts_with("ok "), "{stdout}");
}

#[test]
fn quiet_captures_even_with_progress_off() {
    let r = repo_with("block", CHATTY_PASS);
    r.git(&["config", "amont.progress", "false"]);
    let (code, stdout, _) = run(&r, &["probe", "--quiet"], &[]);
    assert_eq!(code, 0);
    assert_eq!(stdout.lines().count(), 1, "{stdout}");
    assert!(stdout.starts_with("ok probe "), "{stdout}");
}

#[test]
fn a_failing_check_that_prints_nothing_says_so() {
    let r = repo_with("block", "exit 4\n");
    let (code, stdout, _) = run(&r, &["probe"], &[]);
    assert_eq!(code, 1);
    assert!(stdout.contains("(no output)"), "{stdout}");
    assert!(stdout.contains("probe failed, runner exit 4"), "{stdout}");
}

#[test]
fn in_a_stage_a_pass_is_silent_and_a_failure_shows_its_excerpt() {
    let r = repo_with("block", CHATTY_PASS);
    let (code, stdout, _) = run(&r, &[], &[]);
    assert_eq!(code, 0, "{stdout}");
    assert!(!stdout.contains("Compiling crate"), "{stdout}");
    assert!(
        !stdout.contains("ok probe"),
        "the stage tally speaks for a pass:\n{stdout}"
    );

    let r = repo_with("block", PANIC);
    let (code, stdout, _) = run(&r, &[], &[]);
    assert_eq!(code, 1, "{stdout}");
    assert!(
        stdout.contains("panicked at src/render.rs:88:9"),
        "{stdout}"
    );
    assert!(stdout.contains("full log: "), "{stdout}");
    assert!(!stdout.contains("Compiling widget-0 "), "{stdout}");
}
