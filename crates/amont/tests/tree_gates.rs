#![cfg(unix)]
//! Tree gates at commit time (ADR-0024): they run beside the commit's checks,
//! never decide it, and stamp `tree:<name>` on the commit AND its tree only
//! when they proved exactly the tree being committed.

mod common;
use common::Repo;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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

fn commit(r: &Repo, msg: &str) -> (bool, String) {
    let out = r.git(&["commit", "-q", "-m", msg]);
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// The gate-stamp tokens on `rev` (a commit or a tree).
fn stamped(r: &Repo, rev: &str) -> String {
    let out = r.git(&["notes", "--ref", "amont-gate", "show", rev]);
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// `amont warm`, in the foreground: fills every cold gate's cache.
fn warm(r: &Repo) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("warm")
        .current_dir(&r.dir)
        .stdin(Stdio::null())
        .output()
        .expect("amont warm");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// The namespace directories of `gate` under `$GIT_DIR/amont-cache`.
fn namespaces(r: &Repo, gate: &str) -> Vec<String> {
    let dir = r.dir.join(".git").join("amont-cache").join(gate);
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .map(|es| {
            es.filter_map(Result::ok)
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// A workflow with one step per `(name, command)`, each skipped on its gate —
/// what `pre-commit-tree-parity` requires before any tree gate may attest.
fn workflow(gates: &[(&str, &str)]) -> String {
    let mut w = String::from(
        "name: ci\njobs:\n  lint:\n    steps:\n      - id: attest\n        uses: fredericrous/attest@v1\n",
    );
    for (name, command) in gates {
        w.push_str(&format!(
            "      - if: ${{{{ !contains(fromJSON(steps.attest.outputs.gates || '[]'), 'tree-{name}') }}}}\n        run: '{command}'\n"
        ));
    }
    w
}

/// A repository declaring the tree gate `name` running `command` (tool
/// `tool`), with the matching CI step, trusted, hooks on, tree lint on, and
/// `a.txt` staged.
fn repo_with(name: &str, tool: &str, command: &str) -> Repo {
    let r = Repo::new();
    r.stage(
        "amont.conf",
        &format!("tree {name} {tool} * attest {command}\n"),
    );
    r.stage(".forgejo/workflows/ci.yaml", &workflow(&[(name, command)]));
    r.commit("chore: tree gates");
    trust_and_install(&r);
    r.git(&["config", "amont.treeLint", "true"]);
    r.stage("a.txt", "hello\n");
    r
}

#[test]
fn a_passing_tree_gate_stamps_the_commit_and_its_tree() {
    let r = repo_with("ok", "ruff", "true");
    warm(&r);
    let (ok, out) = commit(&r, "feat: a");
    assert!(ok, "{out}");
    assert!(stamped(&r, "HEAD").contains("tree:ok"), "{out}");
    assert!(stamped(&r, "HEAD^{tree}").contains("tree:ok"), "{out}");
}

#[test]
fn a_failing_tree_gate_never_blocks_and_never_stamps() {
    let r = repo_with("bad", "ruff", "./bad.sh");
    r.stage("bad.sh", "#!/bin/sh\necho '3 problems'\nexit 1\n");
    r.git(&["update-index", "--chmod=+x", "bad.sh"]);
    std::fs::set_permissions(
        r.dir.join("bad.sh"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let warmed = warm(&r);
    let (ok, out) = commit(&r, "feat: a");
    assert!(
        ok,
        "the commit is judged by its checks, not by a tree gate: {out}"
    );
    assert!(
        out.contains("tree lint not proven: bad — 3 problems — CI will lint"),
        "warm said: {warmed}\ncommit said: {out}"
    );
    assert!(!stamped(&r, "HEAD").contains("tree:bad"), "{out}");
}

#[test]
fn an_untracked_file_withholds_the_proof() {
    let r = repo_with("ok", "ruff", "true");
    r.write("stray.py", "import os\n");
    let (ok, out) = commit(&r, "feat: a");
    assert!(ok, "{out}");
    assert!(
        out.contains("an untracked file is present (stray.py)"),
        "{out}"
    );
    assert!(!stamped(&r, "HEAD").contains("tree:ok"), "{out}");
}

#[test]
fn partial_staging_withholds_the_proof() {
    let r = repo_with("ok", "ruff", "true");
    r.write("a.txt", "hello, but not what is staged\n");
    let (ok, out) = commit(&r, "feat: a");
    assert!(ok, "{out}");
    assert!(out.contains("unstaged edits"), "{out}");
    assert!(!stamped(&r, "HEAD").contains("tree:ok"), "{out}");
}

#[test]
fn an_ignored_module_outside_the_allow_list_withholds_the_proof() {
    let r = repo_with("ok", "ruff", "true");
    r.stage(".gitignore", "gen/\n");
    r.write("gen/types.py", "X = 1\n");
    let (ok, out) = commit(&r, "feat: a");
    assert!(ok, "{out}");
    assert!(out.contains("an ignored file outside"), "{out}");
    assert!(!stamped(&r, "HEAD").contains("tree:ok"), "{out}");

    // Declared as a reproducible output, it no longer stands in the way.
    r.git(&["config", "amont.snapshotPrepareOutputs", "gen/"]);
    warm(&r);
    r.stage("b.txt", "b\n");
    let (ok, out) = commit(&r, "feat: b");
    assert!(ok, "{out}");
    assert!(stamped(&r, "HEAD").contains("tree:ok"), "{out}");
}

#[test]
fn a_slow_tree_gate_is_cancelled_at_the_slack_and_never_stamps() {
    // Fast while warming, slow at commit: the switch is an ignored file under
    // node_modules/, which the allow-list admits by default.
    let r = repo_with("slow", "pyright", "sh slow.sh");
    r.stage(
        "slow.sh",
        "#!/bin/sh\n[ -e node_modules/slow ] && sleep 30\nexit 0\n",
    );
    r.git(&["config", "amont.treeLintSlack", "1"]);
    r.stage(".gitignore", "node_modules/\n");
    warm(&r);
    r.write("node_modules/slow", "");
    let started = Instant::now();
    let (ok, out) = commit(&r, "feat: a");
    assert!(ok, "{out}");
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "the commit waited for the tree gate: {:?}",
        started.elapsed()
    );
    assert!(
        out.contains("slow — still running when the commit was ready"),
        "{out}"
    );
    assert!(!stamped(&r, "HEAD").contains("tree:slow"), "{out}");
}

#[test]
fn a_cold_commit_warms_in_the_background_and_the_next_one_is_proven() {
    let r = repo_with("ok", "ruff", "true");
    let (ok, out) = commit(&r, "feat: a");
    assert!(ok, "{out}");
    assert!(
        out.contains("tree lint cold: ok — warming in background"),
        "{out}"
    );
    assert!(!stamped(&r, "HEAD").contains("tree:ok"), "{out}");
    let until = Instant::now() + Duration::from_secs(20);
    let marker = || {
        namespaces(&r, "ok").iter().any(|ns| {
            r.dir
                .join(".git/amont-cache/ok")
                .join(ns)
                .join(".complete")
                .is_file()
        })
    };
    while !marker() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(marker(), "the background warm-up never completed");
    r.stage("b.txt", "b\n");
    let (ok, out) = commit(&r, "feat: b");
    assert!(ok, "{out}");
    assert!(stamped(&r, "HEAD").contains("tree:ok"), "{out}");
}

#[test]
fn a_lockfile_change_moves_the_namespace_and_drops_the_old_cache() {
    let r = repo_with("ok", "ruff", "true");
    warm(&r);
    let before = namespaces(&r, "ok");
    assert_eq!(before.len(), 1, "{before:?}");
    // A plugin upgrade: same config, a different lockfile.
    r.stage("uv.lock", "version = 1\n");
    let (ok, out) = commit(&r, "chore: upgrade");
    assert!(ok, "{out}");
    assert!(
        out.contains("tree lint cold: ok"),
        "a new namespace starts cold: {out}"
    );
    assert!(!stamped(&r, "HEAD").contains("tree:ok"), "{out}");
    warm(&r);
    let after = namespaces(&r, "ok");
    assert_eq!(
        after.len(),
        1,
        "the old namespace was not deleted: {after:?}"
    );
    assert_ne!(after, before);
}

#[test]
fn tree_lint_off_runs_nothing() {
    let r = repo_with("ok", "ruff", "true");
    r.git(&["config", "amont.treeLint", "false"]);
    let (ok, out) = commit(&r, "feat: a");
    assert!(ok, "{out}");
    assert!(!out.contains("tree lint"), "{out}");
    assert!(!stamped(&r, "HEAD").contains("tree:ok"), "{out}");
}
