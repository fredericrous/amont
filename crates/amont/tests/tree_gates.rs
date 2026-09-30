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
    // node_modules/, which the allow-list admits by default. The gate would
    // sleep 300 s; a commit well under that was not made to wait for it. The
    // bound is loose on purpose: a loaded machine slows every hook, and only
    // a commit that waited for the gate can reach 300 s.
    let r = repo_with("slow", "pyright", "sh slow.sh");
    r.stage(
        "slow.sh",
        "#!/bin/sh\nif [ -e node_modules/slow ]; then sleep 300 & echo $! > node_modules/slow.pid; wait; fi\nexit 0\n",
    );
    r.git(&["config", "amont.treeLintSlack", "1"]);
    r.stage(".gitignore", "node_modules/\n");
    warm(&r);
    r.write("node_modules/slow", "");
    let started = Instant::now();
    let (ok, out) = commit(&r, "feat: a");
    assert!(ok, "{out}");
    assert!(
        started.elapsed() < Duration::from_secs(200),
        "the commit waited for the tree gate: {:?}\n{out}",
        started.elapsed()
    );
    assert!(
        out.contains("slow — still running when the commit was ready"),
        "{out}"
    );
    assert!(!stamped(&r, "HEAD").contains("tree:slow"), "{out}");
    // Cancelled means gone: THIS gate's child does not outlive the commit.
    let pid = std::fs::read_to_string(r.dir.join("node_modules/slow.pid"))
        .expect("the gate recorded its child")
        .trim()
        .to_string();
    let alive = Command::new("kill")
        .args(["-0", &pid])
        .stderr(Stdio::null())
        .status()
        .expect("kill -0")
        .success();
    assert!(!alive, "the gate's child {pid} outlived the commit");
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

/// A bare remote, a signing key, and `amont.attest` on. Returns the remote.
fn attesting(r: &Repo) -> std::path::PathBuf {
    let remote = r.dir.join("..").join(format!(
        "{}-remote.git",
        r.dir.file_name().unwrap().to_string_lossy()
    ));
    let _ = std::fs::remove_dir_all(&remote);
    let ok = Command::new("git")
        .args(["init", "-q", "--bare", "--template="])
        .arg(&remote)
        .status()
        .expect("git init")
        .success();
    assert!(ok);
    let key = r.dir.join(".git").join("attest-key");
    let ok = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "t@example.org", "-f"])
        .arg(&key)
        .status()
        .expect("ssh-keygen")
        .success();
    assert!(ok);
    r.git(&["remote", "add", "origin", remote.to_str().unwrap()]);
    r.git(&["config", "amont.attest", "true"]);
    r.git(&["config", "amont.attestKey", key.to_str().unwrap()]);
    remote
}

/// Push HEAD to a feature branch; what the hooks said.
fn push(r: &Repo) -> String {
    let out = r.git(&["push", "-q", "origin", "HEAD:refs/heads/feat/tree"]);
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn remote_note(remote: &std::path::Path, commit: &str) -> String {
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(remote)
        .args(["notes", "--ref", "amont-attest", "show", commit])
        .output()
        .expect("git notes");
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn a_proven_tree_is_attested_on_push() {
    let r = repo_with("ok", "ruff", "true");
    let remote = attesting(&r);
    warm(&r);
    let (ok, out) = commit(&r, "feat: a");
    assert!(ok, "{out}");
    let pushed = push(&r);
    let head = String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    let note = remote_note(&remote, &head);
    let gates = note.lines().find(|l| l.starts_with("gates ")).unwrap_or("");
    assert!(
        gates.split_whitespace().any(|g| g == "tree-ok"),
        "note: {note}\npush said: {pushed}"
    );
}

#[test]
fn an_unproven_tree_is_not_attested_and_says_so() {
    let r = repo_with("ok", "ruff", "true");
    let remote = attesting(&r);
    // --no-verify: no hook ran, so no tree gate proved anything.
    let out = r.git(&["commit", "-q", "--no-verify", "-m", "feat: a"]);
    assert!(out.status.success());
    let pushed = push(&r);
    assert!(
        pushed.contains("lint not attested (cold or changed tree): ok — CI will lint"),
        "{pushed}"
    );
    let head = String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    assert!(!remote_note(&remote, &head).contains("tree-ok"), "{pushed}");
}

/// `amont rehearse --wait`, as the rehearsal tests run it.
fn rehearse_wait(r: &Repo) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_amont"));
    cmd.args(["rehearse", "--wait"])
        .current_dir(&r.dir)
        .stdin(Stdio::null());
    Repo::strip_git_env_impl(&mut cmd);
    cmd.env("GIT_CONFIG_GLOBAL", r.dir.join("fake-gitconfig"));
    let out = cmd.output().expect("amont rehearse");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// A feature branch whose tip carries NO commit-time proof (`--no-verify`),
/// with an upstream the rehearsal can push against.
fn unproven_branch(name: &str, tool: &str, command: &str) -> Repo {
    let r = repo_with(name, tool, command);
    let base = String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    r.git(&["checkout", "-q", "--no-track", "-b", "feat/x"]);
    r.git(&["update-ref", "refs/remotes/origin/main", &base]);
    let out = r.git(&["commit", "-q", "--no-verify", "-m", "feat: a"]);
    assert!(out.status.success());
    r
}

#[test]
fn a_rehearsal_proves_a_lint_only_push_on_its_tree() {
    let r = unproven_branch("ok", "ruff", "true");
    assert!(!stamped(&r, "HEAD^{tree}").contains("tree:ok"));
    let out = rehearse_wait(&r);
    assert!(out.contains("tree lint proven: ok"), "{out}");
    assert!(stamped(&r, "HEAD^{tree}").contains("tree:ok"), "{out}");
}

#[test]
fn a_prepare_that_writes_an_allowed_output_still_proves() {
    let r = unproven_branch("ok", "ruff", "true");
    r.stage(".gitignore", ".venv/\n");
    let out = r.git(&["commit", "-q", "--no-verify", "-m", "chore: ignore .venv"]);
    assert!(out.status.success());
    r.git(&[
        "config",
        "amont.snapshotPrepare",
        "mkdir -p .venv && touch .venv/marker",
    ]);
    let out = rehearse_wait(&r);
    assert!(stamped(&r, "HEAD^{tree}").contains("tree:ok"), "{out}");
}

#[test]
fn a_prepare_that_writes_an_ignored_module_cannot_forge_the_tree() {
    let r = unproven_branch("ok", "ruff", "true");
    r.stage(".gitignore", "gen/\n");
    let out = r.git(&["commit", "-q", "--no-verify", "-m", "chore: ignore gen"]);
    assert!(out.status.success());
    r.git(&[
        "config",
        "amont.snapshotPrepare",
        "mkdir -p gen && touch gen/m.py",
    ]);
    let out = rehearse_wait(&r);
    assert!(
        out.contains("tree lint not proven in the snapshot: an ignored file outside"),
        "{out}"
    );
    assert!(!stamped(&r, "HEAD^{tree}").contains("tree:ok"), "{out}");
}

#[test]
fn a_prepare_that_edits_tracked_content_cannot_forge_the_tree() {
    let r = unproven_branch("ok", "ruff", "true");
    r.git(&["config", "amont.snapshotPrepare", "echo changed >> a.txt"]);
    let out = rehearse_wait(&r);
    assert!(
        out.contains("snapshotPrepare changed tracked content"),
        "{out}"
    );
    assert!(!stamped(&r, "HEAD^{tree}").contains("tree:ok"), "{out}");
}

#[test]
fn a_pinned_tool_at_another_version_withholds_without_blocking() {
    let r = repo_with("ok", "ruff", "true");
    // A pin nothing installed here satisfies.
    let conf = std::fs::read_to_string(r.dir.join("amont.conf")).unwrap();
    r.stage("amont.conf", &format!("{conf}tool ruff 99.99.99\n"));
    let out = r.git(&["commit", "-q", "--no-verify", "-m", "chore: pin"]);
    assert!(out.status.success());
    trust_and_install(&r);
    warm(&r);
    r.stage("b.txt", "b\n");
    let (ok, out) = commit(&r, "feat: b");
    assert!(ok, "skew never blocks a commit: {out}");
    assert!(
        out.contains("tree lint not proven: ok — ruff is pinned to 99.99.99"),
        "{out}"
    );
    assert!(!stamped(&r, "HEAD").contains("tree:ok"), "{out}");
}

/// The `run` evidence lines for `tree-<gate>` on `rev`'s gate note.
fn evidence(r: &Repo, rev: &str, gate: &str) -> Vec<String> {
    stamped(r, rev)
        .lines()
        .filter(|l| l.starts_with("run ") && l.split_whitespace().nth(2) == Some(gate))
        .map(|l| l.split_whitespace().nth(3).unwrap_or("").to_string())
        .collect()
}

#[test]
fn every_tree_outcome_is_recorded_as_evidence_on_the_tree() {
    let r = repo_with("ok", "ruff", "true");
    // cold
    let (ok, out) = commit(&r, "feat: a");
    assert!(ok, "{out}");
    assert_eq!(
        evidence(&r, "HEAD^{tree}", "tree-ok"),
        vec!["cold"],
        "{out}"
    );
    // proven
    warm(&r);
    r.stage("b.txt", "b\n");
    let (ok, out) = commit(&r, "feat: b");
    assert!(ok, "{out}");
    assert_eq!(
        evidence(&r, "HEAD^{tree}", "tree-ok"),
        vec!["pass"],
        "{out}"
    );
    // withheld
    r.write("stray.py", "x = 1\n");
    r.stage("c.txt", "c\n");
    let (ok, out) = commit(&r, "feat: c");
    assert!(ok, "{out}");
    assert_eq!(
        evidence(&r, "HEAD^{tree}", "tree-ok"),
        vec!["withheld"],
        "{out}"
    );
}

/// Fresh start: the gate's time is known (it outlasts the slack), the
/// covering check's is not. Unknown cover means run — and learn — never skip.
#[test]
fn an_unmeasured_covering_check_counts_as_cover() {
    let r = Repo::new();
    r.stage(
        "amont.conf",
        "pre-commit  suite  *.txt  block  sleep 3\ntree slowish ruff * attest sleep 2\n",
    );
    r.stage(
        ".forgejo/workflows/ci.yaml",
        &workflow(&[("slowish", "sleep 2")]),
    );
    r.commit("chore: gates");
    trust_and_install(&r);
    r.git(&["config", "amont.treeLint", "true"]);
    r.git(&["config", "amont.treeLintSlack", "1"]);
    warm(&r);
    // Teach the gate its time on a docs commit (no cover): it runs, and is
    // cancelled at the slack.
    r.stage("notes.md", "n\n");
    let (ok, out) = commit(&r, "docs: n");
    assert!(ok, "{out}");
    // Now a covered commit, the suite never measured before.
    r.stage("a.txt", "a\n");
    let (ok, out) = commit(&r, "feat: a");
    assert!(ok, "{out}");
    assert!(stamped(&r, "HEAD").contains("tree:slowish"), "{out}");
}

#[test]
fn a_gate_runs_behind_a_long_check_and_is_skipped_when_nothing_covers_it() {
    // A declared commit check that takes 3 s on *.txt — the cover — and a
    // tree gate that takes ~2 s, with a 1 s slack.
    let r = Repo::new();
    r.stage(
        "amont.conf",
        "pre-commit  suite  *.txt  block  sleep 3\ntree slowish ruff * attest sleep 2\n",
    );
    r.stage(
        ".forgejo/workflows/ci.yaml",
        &workflow(&[("slowish", "sleep 2")]),
    );
    r.commit("chore: gates");
    trust_and_install(&r);
    r.git(&["config", "amont.treeLint", "true"]);
    r.git(&["config", "amont.treeLintSlack", "1"]);
    warm(&r);

    // Covered: the suite runs, the gate hides behind it and is proven.
    r.stage("a.txt", "a\n");
    let (ok, out) = commit(&r, "feat: a");
    assert!(ok, "{out}");
    assert!(stamped(&r, "HEAD").contains("tree:slowish"), "{out}");
    r.stage("b.txt", "b\n");
    let (ok, out) = commit(&r, "feat: b");
    assert!(ok, "{out}");
    assert!(
        stamped(&r, "HEAD").contains("tree:slowish"),
        "covered again: {out}"
    );

    // Uncovered: a docs-only commit leaves 1 s; the gate needs ~2 s.
    r.stage("notes.md", "n\n");
    let started = Instant::now();
    let (ok, out) = commit(&r, "docs: n");
    assert!(ok, "{out}");
    assert!(out.contains("tree lint skipped: slowish (~2."), "{out}");
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "a skipped gate cost time: {:?}",
        started.elapsed()
    );
    assert_eq!(
        evidence(&r, "HEAD^{tree}", "tree-slowish"),
        vec!["slow"],
        "{out}"
    );

    // Regression: the docs commit's instant "pass" of the unscoped suite must
    // not overwrite its real duration — the next covered commit is proven.
    r.stage("c.txt", "c\n");
    let (ok, out) = commit(&r, "feat: c");
    assert!(ok, "{out}");
    assert!(
        stamped(&r, "HEAD").contains("tree:slowish"),
        "still covered: {out}"
    );
}

/// Regression: the version probe ran in the hook's main thread, unbounded,
/// before the commit's checks even started — and pyright's wrapper may reach
/// the network. A probe that hangs must cost the commit nothing.
#[test]
fn a_hanging_version_probe_never_delays_the_commit() {
    let r = repo_with("pr", "pyright", "true");
    r.git(&["config", "amont.treeLintSlack", "1"]);
    let bin = r.dir.join(".git").join("fake-bin");
    std::fs::create_dir_all(&bin).unwrap();
    let fake = bin.join("pyright");
    std::fs::write(&fake, "#!/bin/sh\nsleep 301\necho pyright 1.1.400\n").unwrap();
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let started = Instant::now();
    let mut cmd = Command::new("git");
    cmd.args(["commit", "-q", "-m", "feat: a"])
        .current_dir(&r.dir)
        .stdin(Stdio::null())
        .env("PATH", &path);
    Repo::strip_git_env_impl(&mut cmd);
    let out = cmd.output().expect("git commit");
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{said}");
    assert!(
        started.elapsed() < Duration::from_secs(200),
        "the commit waited for a version probe: {:?}\n{said}",
        started.elapsed()
    );
    assert!(
        said.contains("its tool's version could not be read in time"),
        "{said}"
    );
    assert!(!stamped(&r, "HEAD").contains("tree:pr"), "{said}");
}

/// A reword keeps the tree, so the commit-time proof on the tree note still
/// attests the push.
#[test]
fn a_reword_keeps_the_tree_proof() {
    let r = repo_with("ok", "ruff", "true");
    let remote = attesting(&r);
    warm(&r);
    let (ok, out) = commit(&r, "feat: a");
    assert!(ok, "{out}");
    let out = r.git(&[
        "commit",
        "-q",
        "--amend",
        "--no-verify",
        "-m",
        "feat: reworded",
    ]);
    assert!(out.status.success());
    assert!(
        !stamped(&r, "HEAD").contains("tree:ok"),
        "the new commit has no note of its own"
    );
    push(&r);
    let head = String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    let note = remote_note(&remote, &head);
    let gates = note.lines().find(|l| l.starts_with("gates ")).unwrap_or("");
    assert!(gates.split_whitespace().any(|g| g == "tree-ok"), "{note}");
}

#[test]
fn a_prepare_that_writes_an_untracked_module_cannot_forge_the_tree() {
    let r = unproven_branch("ok", "ruff", "true");
    r.git(&["config", "amont.snapshotPrepare", "touch generated_mod.py"]);
    let out = rehearse_wait(&r);
    assert!(
        out.contains("tree lint not proven in the snapshot: an untracked file is present"),
        "{out}"
    );
    assert!(!stamped(&r, "HEAD^{tree}").contains("tree:ok"), "{out}");
}

/// The gates lint the working tree the hold made the index; if it moves
/// while they run, the proof is about another tree.
#[test]
fn a_tree_that_moves_during_the_run_is_withheld() {
    let r = repo_with("mover", "ruff", "sh mover.sh");
    r.stage(
        "mover.sh",
        "#!/bin/sh\n[ -e node_modules/move ] && echo moved >> a.txt\nexit 0\n",
    );
    r.stage(".gitignore", "node_modules/\n");
    warm(&r);
    r.write("node_modules/move", "");
    r.stage("b.txt", "b\n");
    let (ok, out) = commit(&r, "feat: b");
    assert!(ok, "{out}");
    assert!(out.contains("mover — the tree moved while it ran"), "{out}");
    assert!(!stamped(&r, "HEAD").contains("tree:mover"), "{out}");
}
