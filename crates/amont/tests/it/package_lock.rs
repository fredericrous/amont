//! pre-commit-package-lock, ported from its zsh suite.

use crate::common;
use common::Repo;

#[test]
fn passes_when_both_are_staged() {
    let r = Repo::new();
    r.stage("package.json", "{}\n");
    r.stage("package-lock.json", "{}\n");
    assert!(r.hook("pre-commit-package-lock", &[]).passed());
}

#[test]
fn rejects_a_lockfile_staged_without_its_package_json() {
    let r = Repo::new();
    r.stage("package-lock.json", "{}\n");
    let run = r.hook("pre-commit-package-lock", &[]);
    assert!(!run.passed());
    assert!(run.says("without its package.json"));
}

/// The `.git/hooks/package.json` type-marker case: no lockfile beside it, so it
/// is not an npm project and must demand nothing.
#[test]
fn a_package_json_with_no_lockfile_on_disk_demands_nothing() {
    let r = Repo::new();
    r.stage("package.json", "{}\n");
    assert!(r.hook("pre-commit-package-lock", &[]).passed());
}

/// A forgotten lockfile BLOCKS, and — the half that actually matters — the run
/// COMPLETES.
///
/// This check runs on one of twenty worker threads in pre-commit's concurrent
/// fan-out, and it used to call `trust::confirm`, which blocks on `read_line`
/// from `/dev/tty`. `thread::scope` will not return until that answers, so the
/// commit looked hung while the other checks printed over the prompt.
///
/// The `recv_timeout` here is the assertion: a regression back to a blocking
/// read would hang the whole suite rather than failing this one case.
#[test]
fn a_forgotten_lockfile_blocks_without_asking() {
    let r = Repo::new();
    r.write("package-lock.json", "{}\n");
    r.git(&["add", "package-lock.json"]);
    r.commit("chore: seed the lockfile");
    r.stage("package.json", "{\"name\":\"t\"}\n");

    let child = crate::common::amont_cmd()
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-commit-package-lock")
        .current_dir(&r.dir)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn");

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    let out = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the check never returned — it is asking a question again")
        .expect("wait on the child");

    assert!(!out.status.success(), "a forgotten lockfile must block");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("amont.severity.package-lock"),
        "the message must offer the one-time replacement for answering y: {text}"
    );
    assert!(text.contains("hook.skip=package-lock"), "{text}");
    assert!(
        !text.contains("Commit anyway"),
        "it asked a question from a worker thread: {text}"
    );
}

/// The documented way to keep the signal and lose the block, now that there is
/// no prompt to answer.
#[test]
fn severity_warn_lets_a_forgotten_lockfile_through() {
    let r = Repo::new();
    r.write("package-lock.json", "{}\n");
    r.git(&["add", "package-lock.json"]);
    r.commit("chore: seed the lockfile");
    r.git(&["config", "amont.severity.package-lock", "warn"]);
    r.stage("package.json", "{\"name\":\"t\"}\n");

    let run = r.hook("pre-commit-package-lock", &[]);
    assert!(run.passed(), "warn must not block:\n{}", run.output());
    // The signal is kept: it still says what is wrong.
    assert!(
        run.says("package-lock.json is not staged"),
        "{}",
        run.output()
    );
}

/// In a monorepo one project's lockfile does not satisfy another's.
#[test]
fn scoping_is_per_directory() {
    let r = Repo::new();
    r.write("package-lock.json", "{}\n"); // root lock, on disk only
    r.stage("apps/web/package.json", "{}\n");
    // no lock beside apps/web → nothing demanded
    assert!(r.hook("pre-commit-package-lock", &[]).passed());
}
