//! A declared pre-commit gate judges the index `git commit` is sealing.
//!
//! `git commit -a` and `git commit <paths>` build a TEMPORARY index and name
//! it to the hooks only in `GIT_INDEX_FILE`. amont strips every `GIT_*`
//! variable before a declared command runs (a suite's fixture repositories
//! must not write into the real one), and with it went the only way to find
//! that index: a gate reading `git diff --cached` judged the old index,
//! passed on nothing, and its pass was stamped onto the committed tree.
//!
//! - Every pre-commit command is told the index as `AMONT_INDEX_FILE`.
//! - A command declared `index` is handed it as `GIT_INDEX_FILE` too.
//! - Any other command still sees no `GIT_*` variable.
//!
//! Unix only: the gates are `sh` scripts, and the fixtures are trusted and
//! installed through `tree_gates`, which is Unix only too.
#![cfg(unix)]

use crate::common::{missing, Repo};
use std::process::Stdio;

/// A repository with `a.txt` committed as `ok`, a `sub/` directory, the gate
/// `gate.sh` and the declaration `line`, trusted and installed.
fn repo(gate: &str, line: &str) -> Repo {
    let fixture = Repo::new();
    fixture.stage("a.txt", "ok\n");
    fixture.stage("sub/keep.txt", "keep\n");
    fixture.stage("gate.sh", gate);
    fixture.stage("amont.conf", line);
    fixture.commit("chore: base");
    crate::tree_gates::trust_and_install(&fixture);
    fixture
}

/// `git commit -a` run from `sub/`, with the hooks. Whether it was accepted,
/// and what it printed.
fn commit_all_from_sub(fixture: &Repo, msg: &str) -> (bool, String) {
    let mut cmd = std::process::Command::new("git");
    cmd.args(["commit", "-q", "-a", "-m", msg])
        .current_dir(fixture.dir.join("sub"))
        .stdin(Stdio::null());
    Repo::strip_git_env_impl(&mut cmd);
    let out = cmd.output().expect("git commit");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// Refuses when the STAGED `a.txt` says BAD, reading the index through git.
const READS_THE_INDEX: &str = "git show :a.txt | grep -q BAD && exit 1\nexit 0\n";

#[test]
fn a_gate_declared_index_judges_what_commit_dash_a_seals() {
    if missing("sh") {
        return;
    }
    let fixture = repo(
        READS_THE_INDEX,
        "pre-commit  idxgate7  *.txt  block  index sh gate.sh\n",
    );
    fixture.write("a.txt", "BAD\n");
    let (accepted, out) = commit_all_from_sub(&fixture, "feat: bad");
    assert!(
        !accepted,
        "the gate read the old index and passed on nothing:\n{out}"
    );
    fixture.write("a.txt", "fine\n");
    let (accepted, out) = commit_all_from_sub(&fixture, "feat: fine");
    assert!(accepted, "{out}");
}

#[test]
fn every_pre_commit_gate_is_told_the_index_being_committed() {
    if missing("sh") {
        return;
    }
    // No `index` marker: the gate finds the index through AMONT_INDEX_FILE,
    // and fails loudly when it is not set.
    let gate = "GIT_INDEX_FILE=\"${AMONT_INDEX_FILE:?not set}\" git show :a.txt \
                | grep -q BAD && exit 1\nexit 0\n";
    let fixture = repo(gate, "pre-commit  idxgate7  *.txt  block  sh gate.sh\n");
    fixture.write("a.txt", "fine\n");
    let (accepted, out) = commit_all_from_sub(&fixture, "feat: fine");
    assert!(accepted, "AMONT_INDEX_FILE names a readable index:\n{out}");
    fixture.write("a.txt", "BAD\n");
    let (accepted, out) = commit_all_from_sub(&fixture, "feat: bad");
    assert!(!accepted, "and it is the one commit -a seals:\n{out}");
}

#[test]
fn a_gate_not_declared_index_still_sees_no_git_variable() {
    if missing("sh") {
        return;
    }
    let gate = "env | grep -q '^GIT_' && { env | grep '^GIT_'; exit 1; }\n\
                [ \"$AMONT_INDEX_GIT_DIR\" = \"$(git rev-parse --absolute-git-dir)\" ] \
                || { echo \"AMONT_INDEX_GIT_DIR is not this repository: $AMONT_INDEX_GIT_DIR\"; exit 1; }\n\
                case \"$AMONT_INDEX_FILE\" in /*) exit 0 ;; esac\n\
                echo \"AMONT_INDEX_FILE is not absolute: $AMONT_INDEX_FILE\"; exit 1\n";
    let fixture = repo(gate, "pre-commit  clean  *.txt  block  sh gate.sh\n");
    fixture.write("a.txt", "fine\n");
    let (accepted, out) = commit_all_from_sub(&fixture, "feat: fine");
    assert!(accepted, "{out}");
}

#[test]
fn a_linked_worktree_commit_of_named_paths_is_judged_too() {
    if missing("sh") {
        return;
    }
    let fixture = repo(
        READS_THE_INDEX,
        "pre-commit  idxgate7  *.txt  block  index sh gate.sh\n",
    );
    let wt = fixture
        .dir
        .parent()
        .expect("parent")
        .join(format!("staged-index-wt-{}", std::process::id()));
    if let Err(e) = std::fs::remove_dir_all(&wt) {
        assert_eq!(
            e.kind(),
            std::io::ErrorKind::NotFound,
            "a stale {wt:?}: {e}"
        );
    }
    // Removed however the test ends, a panic included: it lives outside the
    // fixture's own directory.
    struct RemoveOnDrop(std::path::PathBuf);
    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            if let Err(e) = std::fs::remove_dir_all(&self.0) {
                eprintln!("could not remove {:?}: {e}", self.0);
            }
        }
    }
    let _cleanup = RemoveOnDrop(wt.clone());
    let added = fixture.git(&[
        "worktree",
        "add",
        "-q",
        "-b",
        "feat/wt",
        wt.to_str().expect("utf8"),
    ]);
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    // `git commit <paths>` (`--only`) seals a temporary index too, from a
    // subdirectory of a linked worktree.
    let commit_only = |content: &str, msg: &str| {
        std::fs::write(wt.join("a.txt"), content).expect("write");
        let mut cmd = std::process::Command::new("git");
        cmd.args(["commit", "-q", "-m", msg, "--", "../a.txt"])
            .current_dir(wt.join("sub"))
            .stdin(Stdio::null());
        Repo::strip_git_env_impl(&mut cmd);
        let out = cmd.output().expect("git commit");
        (
            out.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    };
    let (bad_accepted, bad_out) = commit_only("BAD\n", "feat: bad");
    let (good_accepted, good_out) = commit_only("fine\n", "feat: fine");
    assert!(!bad_accepted, "the gate read the old index:\n{bad_out}");
    assert!(
        bad_out.contains("idxgate7"),
        "refused by the gate:\n{bad_out}"
    );
    assert!(good_accepted, "{good_out}");
}

#[test]
fn a_relative_git_index_file_is_handed_on_absolute() {
    if missing("sh") {
        return;
    }
    // Also refuses unless the index it is told is absolute: a consumer that
    // changes directory must still find it.
    let gate = format!(
        "case \"$AMONT_INDEX_FILE\" in /*) ;; *) echo relative; exit 1 ;; esac\n{READS_THE_INDEX}"
    );
    let fixture = repo(
        &gate,
        "pre-commit  idxgate7  *.txt  block  index sh gate.sh\n",
    );
    // An index of the caller's own, named relative to the repository root.
    let alt = |args: &[&str]| {
        let mut cmd = std::process::Command::new("git");
        cmd.args(args)
            .current_dir(&fixture.dir)
            .stdin(Stdio::null());
        Repo::strip_git_env_impl(&mut cmd);
        cmd.env("GIT_INDEX_FILE", ".git/alt-index");
        cmd.output().expect("git")
    };
    let stage_in_alt = |content: &str| {
        assert!(alt(&["read-tree", "HEAD"]).status.success(), "read-tree");
        fixture.write("a.txt", content);
        assert!(alt(&["add", "a.txt"]).status.success(), "add");
        fixture.write("a.txt", "ok\n"); // the working tree and the real index say ok
    };
    let said = |out: &std::process::Output| {
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    };
    stage_in_alt("BAD\n");
    let bad = alt(&["commit", "-q", "-m", "feat: bad"]);
    assert!(
        !bad.status.success(),
        "the gate read the real index, not the one named relative:\n{}",
        said(&bad)
    );
    assert!(
        said(&bad).contains("idxgate7"),
        "refused by the gate:\n{}",
        said(&bad)
    );
    stage_in_alt("fine\n");
    let good = alt(&["commit", "-q", "-m", "feat: fine"]);
    assert!(good.status.success(), "{}", said(&good));
}

#[test]
fn a_pre_push_gate_never_inherits_an_index() {
    if missing("sh") {
        return;
    }
    let fixture = Repo::new();
    let log = fixture.path(".git/seen.log");
    fixture.stage(
        "gate.sh",
        &format!(
            "echo \"${{AMONT_INDEX_FILE:-unset}} ${{AMONT_INDEX_GIT_DIR:-unset}}\" > '{}'\n",
            log.to_str().expect("utf8").replace('\'', "'\\''")
        ),
    );
    fixture.stage("amont.conf", "pre-push  probe  *  warn  sh gate.sh\n");
    fixture.commit("chore: base");
    crate::tree_gates::trust_and_install(&fixture);
    fixture.git(&["config", "amont.pushGates", "inline"]);
    let head = String::from_utf8_lossy(&fixture.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    let zeros = "0".repeat(head.len());
    let mut cmd = crate::common::amont_cmd();
    cmd.arg("--hooks-dir")
        .arg(fixture.path(".git/hooks"))
        .arg("pre-push")
        .current_dir(&fixture.dir)
        .env("AMONT_INDEX_FILE", "/stale/index")
        .env("AMONT_INDEX_GIT_DIR", "/stale/.git")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().expect("spawn");
    {
        use std::io::Write;
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(format!("refs/heads/feat/x {head} refs/heads/feat/x {zeros}\n").as_bytes())
            .expect("write");
    }
    assert!(
        child.wait().expect("wait").success(),
        "the push hook passed"
    );
    let seen = std::fs::read_to_string(&log).expect("the probe ran");
    assert_eq!(seen.trim(), "unset unset");
}
