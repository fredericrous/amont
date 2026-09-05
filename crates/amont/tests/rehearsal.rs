//! Background rehearsal: the push gate runs on a snapshot of HEAD, off the
//! commit path and off the wire, and the push that follows finds the stamp.
//!
//! Same fixture shape as `push_stamps.rs` — a declared pre-push gate that
//! appends to a log — except the log lives at an ABSOLUTE path baked into the
//! script: the gate runs inside a throwaway worktree, and a relative log
//! would be written there and vanish with it.

mod common;
use common::{missing, Repo};

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Drive the pre-push DISPATCHER over one ref line, capturing output.
fn push_out(r: &Repo, from: &str, to: &str) -> (i32, String) {
    let line = format!("refs/heads/feat/x {to} refs/heads/feat/x {from}\n");
    let mut child = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-push")
        .current_dir(&r.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(line.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("wait");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// `amont rehearse …` from the repo, the way a person or the agent types it.
fn rehearse(r: &Repo, flags: &[&str]) -> (i32, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_amont"));
    cmd.arg("rehearse")
        .args(flags)
        .current_dir(&r.dir)
        .stdin(Stdio::null());
    Repo::strip_git_env_impl(&mut cmd);
    cmd.env("GIT_CONFIG_GLOBAL", r.dir.join("fake-gitconfig"));
    let out = cmd.output().expect("run amont rehearse");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn head(r: &Repo) -> String {
    String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string()
}

fn trust_and_install(r: &Repo) {
    for verb in ["trust", "init"] {
        let out = Command::new(env!("CARGO_BIN_EXE_amont"))
            .arg(verb)
            .current_dir(&r.dir)
            .stdin(Stdio::null())
            .output()
            .expect("amont");
        assert!(
            out.status.success(),
            "amont {verb}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn runs(r: &Repo) -> usize {
    std::fs::read_to_string(r.dir.join("gate.log"))
        .map(|s| s.len())
        .unwrap_or(0)
}

fn note(r: &Repo, rev: &str) -> String {
    String::from_utf8_lossy(&r.git(&["notes", "--ref", "amont-gate", "show", rev]).stdout)
        .to_string()
}

fn state(r: &Repo) -> String {
    std::fs::read_to_string(r.dir.join(".git/amont-rehearsal")).unwrap_or_default()
}

fn log(r: &Repo) -> String {
    std::fs::read_to_string(r.dir.join(".git/amont-rehearsal.log")).unwrap_or_default()
}

/// Wait for the state file to satisfy `pred`, or fail loudly.
fn wait_state(r: &Repo, what: &str, pred: impl Fn(&str) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if pred(&state(r)) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "waited 30s for {what}\nstate:\n{}\nlog:\n{}",
            state(r),
            log(r)
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Registered worktrees — the primary plus any snapshot left behind.
fn worktrees(r: &Repo) -> usize {
    String::from_utf8_lossy(&r.git(&["worktree", "list", "--porcelain"]).stdout)
        .lines()
        .filter(|l| l.starts_with("worktree "))
        .count()
}

/// A gate script that appends one byte to the repo's `gate.log` — at its
/// absolute path — after `body` has run.
fn gate_js(r: &Repo, body: &str) -> String {
    let log = r.dir.join("gate.log").display().to_string();
    format!("const fs=require('fs');\n{body}\nfs.appendFileSync({log:?},'x');\n")
}

/// A repo on `feat/x` with ONE declared pre-push gate (no commit-time twin),
/// `origin/main` at the base, and one committed `.txt` in the gate's scope.
/// The gate FAILS when `a.txt` — in its own cwd — says `broken`, which is
/// how the tests tell a snapshot run from a working-tree run.
fn gated_repo(body: &str) -> (Repo, String) {
    let r = Repo::new();
    let script = gate_js(
        &r,
        &format!(
            "if (fs.readFileSync('a.txt','utf8').includes('broken')) process.exit(1);\n{body}"
        ),
    );
    r.stage("gate.js", &script);
    r.stage("a.txt", "seed\n");
    r.commit("chore: base");
    let base = head(&r);
    r.stage(
        "amont.conf",
        "pre-push    suite  *.txt  block  node gate.js\n",
    );
    r.commit("chore: the gate");
    trust_and_install(&r);
    // `--no-track`: a global `branch.autoSetupMerge=always` would otherwise
    // make the branch track the local `main`.
    r.git(&["checkout", "-q", "--no-track", "-b", "feat/x"]);
    r.git(&["update-ref", "refs/remotes/origin/main", &base]);
    r.stage("a.txt", "hello\n");
    r.commit("feat: something to push");
    (r, base)
}

const SKIP: &str = "passed on this exact tree earlier — not repeating it here";

/// The point: the rehearsal tests the COMMIT, not the working tree, stamps
/// it, and the push that follows skips the suite.
#[test]
fn a_rehearsal_tests_a_snapshot_of_head_and_the_push_skips_the_suite() {
    if missing("node") {
        return;
    }
    let (r, base) = gated_repo("");
    // The working tree says something the gate would refuse. A run on the
    // tree fails; a run on the snapshot never sees it.
    r.write("a.txt", "broken\n");

    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(runs(&r), 1, "the gate ran once: {out}");
    assert!(out.contains("rehearsal of"), "{out}");
    assert!(
        note(&r, "HEAD").contains("pre-push-suite"),
        "HEAD is stamped: {out}"
    );
    assert_eq!(
        std::fs::read_to_string(r.dir.join("a.txt")).unwrap(),
        "broken\n",
        "the working tree was not touched"
    );
    assert_eq!(worktrees(&r), 1, "the snapshot was removed");
    assert!(state(&r).contains("phase=passed"), "{}", state(&r));

    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(out.contains(SKIP), "{out}");
    assert_eq!(runs(&r), 1, "the push repeated nothing");
}

/// `amont.rehearseOnCommit`: the commit itself starts the worker, git does
/// not wait for it, and the gate ends up run exactly once.
#[test]
fn a_commit_starts_the_rehearsal_when_asked() {
    if missing("node") {
        return;
    }
    let (r, base) = gated_repo("");
    // Without the opt-in a commit starts nothing.
    r.stage("a.txt", "quiet\n");
    let out = r.git(&["commit", "-q", "-m", "feat: nothing happens"]);
    assert!(out.status.success());
    let (_, status) = rehearse(&r, &["--status"]);
    assert!(status.contains("no rehearsal recorded"), "{status}");

    r.git(&["config", "amont.rehearseOnCommit", "true"]);
    r.stage("a.txt", "rehearsed\n");
    let out = r.git(&["commit", "-q", "-m", "feat: rehearsed"]);
    assert!(out.status.success());
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        said.contains("rehearsing the push gate in the background"),
        "{said}"
    );
    wait_state(&r, "the background rehearsal to pass", |s| {
        s.contains("phase=passed")
    });
    assert_eq!(runs(&r), 1, "log:\n{}", log(&r));
    assert!(note(&r, "HEAD").contains("pre-push-suite"));

    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(out.contains(SKIP), "{out}");
    assert_eq!(runs(&r), 1);
    assert_eq!(worktrees(&r), 1);
}

/// Latest wins: a rehearsal of a tree nobody will push is cancelled, suite
/// and snapshot included, the moment a newer commit exists.
#[test]
fn a_newer_commit_cancels_the_running_rehearsal() {
    if missing("node") {
        return;
    }
    // The gate takes three seconds, so a cancelled run never reaches its log line.
    let (r, _base) = gated_repo("require('child_process').execSync('sleep 3');");
    let older = head(&r);
    let (code, out) = rehearse(&r, &[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("in the background"), "{out}");
    wait_state(&r, "the first worker to start", |s| {
        s.contains(&format!("commit={older}")) && s.contains("phase=running")
    });
    let first_snapshot = state(&r)
        .lines()
        .find_map(|l| l.strip_prefix("snapshot="))
        .map(PathBuf::from)
        .expect("snapshot path recorded");
    assert!(
        first_snapshot.exists(),
        "the snapshot is there while it runs"
    );

    r.stage("a.txt", "newer\n");
    r.commit("feat: newer");
    let newer = head(&r);
    let (code, out) = rehearse(&r, &[]);
    assert_eq!(code, 0, "{out}");
    wait_state(&r, "the second worker to take over", |s| {
        s.contains(&format!("commit={newer}"))
    });
    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 0, "{out}\nlog:\n{}", log(&r));
    assert_eq!(runs(&r), 1, "only the newer tree's gate reached the log");
    assert!(
        !first_snapshot.exists(),
        "the cancelled worker's snapshot was removed"
    );
    assert_eq!(worktrees(&r), 1);
    assert!(note(&r, &newer).contains("pre-push-suite"));
    assert!(
        !note(&r, &older).contains("pre-push-suite"),
        "the cancelled tree earned nothing"
    );
    assert!(
        log(&r).contains("cancelling the rehearsal of"),
        "{}",
        log(&r)
    );
}

/// A push that arrives mid-rehearsal waits for the verdict instead of
/// starting the suite over, then finds the stamp.
#[test]
fn a_push_waits_for_a_running_rehearsal_rather_than_repeating_it() {
    if missing("node") {
        return;
    }
    let (r, base) = gated_repo("require('child_process').execSync('sleep 3');");
    let (code, out) = rehearse(&r, &[]);
    assert_eq!(code, 0, "{out}");
    wait_state(&r, "the worker to start", |s| s.contains("phase=running"));

    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("waiting for it rather than starting the suite over"),
        "{out}"
    );
    assert!(out.contains("rehearsal passed"), "{out}");
    assert!(out.contains(SKIP), "{out}");
    assert_eq!(runs(&r), 1, "the push repeated nothing");
}

/// A rehearsal that failed is reported and NOT honoured: the push runs the
/// gate again, in the terminal the developer is looking at.
#[test]
fn a_failed_rehearsal_is_reported_and_the_push_runs_the_gate() {
    if missing("node") {
        return;
    }
    let (r, base) = gated_repo("");
    // Fail AFTER logging, so the run count still says what ran.
    let script = gate_js(&r, "process.exitCode = 1;");
    r.stage("gate.js", &script);
    r.commit("feat: a gate that fails");

    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 1, "{out}");
    assert_eq!(runs(&r), 1);
    assert!(!note(&r, "HEAD").contains("pre-push-suite"), "no stamp");
    let (_, status) = rehearse(&r, &["--status"]);
    assert!(status.contains("FAILED"), "{status}");

    let (code, out) = push_out(&r, &base, &head(&r));
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("rehearsal of this tree failed"), "{out}");
    assert!(out.contains("running the gate here"), "{out}");
    assert_eq!(runs(&r), 2, "the push ran it again");
}

/// `amont.snapshotPrepare` makes the checkout a workspace — in the snapshot,
/// never in the developer's tree — and its failure is the snapshot's.
#[test]
fn snapshot_prepare_runs_in_the_snapshot_only() {
    if missing("node") {
        return;
    }
    let (r, _base) = gated_repo("fs.readFileSync('prepared.txt');");
    // Without preparation the gate cannot even start.
    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 1, "{out}");
    assert_eq!(runs(&r), 0);

    r.git(&[
        "config",
        "amont.snapshotPrepare",
        "echo ready > prepared.txt",
    ]);
    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("preparing the snapshot"), "{out}");
    assert_eq!(runs(&r), 1);
    assert!(
        !r.dir.join("prepared.txt").exists(),
        "preparation happened in the snapshot, not the working tree"
    );
    assert!(note(&r, "HEAD").contains("pre-push-suite"));

    // A preparation that fails tests nothing and stamps nothing.
    r.stage("a.txt", "again\n");
    r.commit("feat: again");
    r.git(&["config", "amont.snapshotPrepare", "exit 3"]);
    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 2, "{out}");
    assert!(
        out.contains("could not check out HEAD into a snapshot"),
        "{out}"
    );
    assert_eq!(runs(&r), 1);
    assert!(!note(&r, "HEAD").contains("pre-push-suite"));
    assert_eq!(worktrees(&r), 1);
}

/// Nothing to rehearse is said, not done: a push that touches no gate's
/// scope starts no suite and creates no snapshot.
#[test]
fn a_rehearsal_with_nothing_to_run_says_so() {
    if missing("node") {
        return;
    }
    let (r, _base) = gated_repo("");
    // Rewind the branch to the base plus a change outside the gate's scope.
    r.git(&["reset", "-q", "--hard", "refs/remotes/origin/main"]);
    r.stage(
        "amont.conf",
        "pre-push    suite  *.txt  block  node gate.js\n",
    );
    r.stage("notes.md", "prose\n");
    r.commit("docs: prose only");
    trust_and_install(&r);

    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("nothing to rehearse"), "{out}");
    assert_eq!(runs(&r), 0);
    assert_eq!(worktrees(&r), 1);
}

/// `--stop` ends the worker and takes the snapshot with it.
#[test]
fn stop_cancels_the_worker_and_removes_its_snapshot() {
    if missing("node") {
        return;
    }
    let (r, _base) = gated_repo("require('child_process').execSync('sleep 5');");
    let (code, out) = rehearse(&r, &[]);
    assert_eq!(code, 0, "{out}");
    wait_state(&r, "the worker to start", |s| s.contains("phase=running"));
    let (code, out) = rehearse(&r, &["--stop"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("stopped the rehearsal"), "{out}");
    assert_eq!(worktrees(&r), 1, "the snapshot went with the worker");
    let (_, status) = rehearse(&r, &["--status"]);
    assert!(status.contains("no rehearsal recorded"), "{status}");
    std::thread::sleep(Duration::from_secs(6));
    assert_eq!(runs(&r), 0, "the killed suite never reached its log line");
}
