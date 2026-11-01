//! `amont run --stamp` and `amont stamps` (ADR-0015): a runner other than the
//! commit hook earns the stamp the hooks honour, and a tree's stamps can be
//! asked for.
//!
//! The gate is a pair declared at both stages, as in `gate_pairs.rs`, logging
//! each execution, so a count says how many times the suite really ran.

use crate::common;
use common::Repo;

use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;

fn head(r: &Repo) -> String {
    String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string()
}

fn tree_of(r: &Repo, rev: &str) -> String {
    String::from_utf8_lossy(&r.git(&["rev-parse", &format!("{rev}^{{tree}}")]).stdout)
        .trim()
        .to_string()
}

fn tree(r: &Repo) -> String {
    tree_of(r, "HEAD")
}

fn runs(r: &Repo) -> usize {
    std::fs::read_to_string(r.dir.join("gate.log"))
        .map(|s| s.len())
        .unwrap_or(0)
}

fn note(r: &Repo, key: &str) -> String {
    String::from_utf8_lossy(&r.git(&["notes", "--ref", "amont-gate", "show", key]).stdout)
        .into_owned()
}

fn trust_and_install(r: &Repo) {
    for verb in ["trust", "init"] {
        let out = common::amont_cmd()
            .arg(verb)
            .current_dir(&r.dir)
            .stdin(Stdio::null())
            .output()
            .expect("amont");
        assert!(out.status.success(), "amont {verb} failed");
    }
}

/// Captured output without colour codes, so a word that `highlight` wraps in
/// them still reads as the word.
fn plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for n in chars.by_ref() {
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// `amont <args>` in the repository: the pinned host plus `env`, minus the
/// variables in `unset`.
fn amont(r: &Repo, args: &[&str], env: &[(&str, &str)], unset: &[&str]) -> (i32, String) {
    let mut cmd = common::amont_cmd();
    cmd.args(args).current_dir(&r.dir).stdin(Stdio::null());
    for k in unset {
        cmd.env_remove(k);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run amont");
    (
        out.status.code().unwrap_or(-1),
        plain(&format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )),
    )
}

/// The pre-push dispatcher over one feature ref line.
fn push_out(r: &Repo, from: &str, to: &str) -> (i32, String) {
    let line = format!("refs/heads/feat/x {to} refs/heads/feat/x {from}\n");
    let mut child = common::amont_cmd()
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
        plain(&format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )),
    )
}

/// A pair `check` on `*.txt` at both stages, `gate.js` logging each run and
/// exiting with the content of `exit-code` (0 when absent). Returns the base.
fn paired_repo(extra_conf: &str) -> (Repo, String) {
    let r = Repo::new();
    r.stage(
        "gate.js",
        "const fs = require('fs');\n\
         fs.appendFileSync('gate.log', 'x');\n\
         if (fs.existsSync('exit-code')) process.exit(Number(fs.readFileSync('exit-code', 'utf8')));\n",
    );
    r.stage(".gitignore", "gate.log\nexit-code\n");
    r.commit("chore: base");
    let base = head(&r);
    r.stage(
        "amont.conf",
        &format!(
            "{extra_conf}pre-commit  check  *.txt  block  heavy node gate.js\n\
             pre-push    check  *.txt  block  heavy node gate.js\n"
        ),
    );
    r.commit("chore: the pair");
    trust_and_install(&r);
    r.stage("a.txt", "hello\n");
    r.commit("feat: a"); // --no-verify: nothing has judged this tree
    (r, base)
}

/// The tree `run --stamp` stamps is not judged again: the commit-time token
/// lands on the tree note, the push twin is covered (the suite runs once),
/// a push of the range says "gated at commit instead", and a re-commit of
/// the same tree through the hook reuses it.
#[test]
fn a_stamped_tree_is_not_judged_again_at_push_or_commit() {
    if common::missing("node") {
        return;
    }
    let (r, base) = paired_repo("");
    assert_eq!(runs(&r), 0);
    let (code, out) = amont(&r, &["run", "--stamp"], &[], &[]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(runs(&r), 1, "the suite ran once, not once per stage: {out}");
    assert!(out.contains("stamped pre-commit-check"), "{out}");
    assert!(
        out.contains("pre-push-check covered by its commit-time twin pre-commit-check"),
        "{out}"
    );
    assert!(
        note(&r, &tree(&r)).contains("check@"),
        "tree note: {}",
        note(&r, &tree(&r))
    );

    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("gated at commit instead"), "{out}");
    assert_eq!(runs(&r), 1, "the push did not run the suite again: {out}");

    // The same tree, committed again through the hook: reused, not rerun.
    let reset = r.git(&["reset", "-q", "--soft", "HEAD~1"]);
    assert!(reset.status.success());
    let commit = r.git(&["commit", "-q", "-m", "feat: a, again"]);
    assert!(
        commit.status.success(),
        "{}",
        String::from_utf8_lossy(&commit.stderr)
    );
    assert_eq!(runs(&r), 1, "the pre-commit hook reused the tree's stamp");
}

/// Tracked changes are refused before anything runs: the gates would judge
/// content no tree holds. Untracked files (a gate's log) do not count.
#[test]
fn a_dirty_tree_is_refused() {
    if common::missing("node") {
        return;
    }
    let (r, _) = paired_repo("");
    r.write("a.txt", "changed\n");
    let (code, out) = amont(&r, &["run", "--stamp"], &[], &[]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("needs a clean worktree"), "{out}");
    assert_eq!(runs(&r), 0, "nothing ran");
    assert!(!note(&r, &tree(&r)).contains("check@"));
}

/// A detached HEAD is allowed — relais verifies in a detached worktree — and
/// still stamps; only tracked changes are refused.
#[test]
fn a_detached_head_is_stamped() {
    if common::missing("node") {
        return;
    }
    let (r, _) = paired_repo("");
    let detach = r.git(&["checkout", "-q", "--detach", "HEAD"]);
    assert!(detach.status.success());
    let (code, out) = amont(&r, &["run", "--stamp", "pre-commit-check"], &[], &[]);
    assert_eq!(code, 0, "{out}");
    assert!(note(&r, &tree(&r)).contains("check@"));
}

/// A failing gate writes no token and exits 1.
#[test]
fn a_failing_gate_is_not_stamped() {
    if common::missing("node") {
        return;
    }
    let (r, _) = paired_repo("");
    std::fs::write(r.dir.join("exit-code"), "3").unwrap();
    let (code, out) = amont(&r, &["run", "--stamp", "pre-commit-check"], &[], &[]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("failed — not stamped"), "{out}");
    assert!(!note(&r, &tree(&r)).contains("check@"));
}

/// `--stamp` judges the whole tree and prints its own report, runs the gates
/// itself and not through hooks: a flag that would change that is refused,
/// naming it, instead of being accepted and ignored.
#[test]
fn flags_that_stamp_cannot_honour_are_refused() {
    if common::missing("node") {
        return;
    }
    let (r, _) = paired_repo("");
    for flag in [
        vec!["--all-files"],
        vec!["--quiet"],
        vec!["--raw"],
        vec!["--hooks-dir", ".git/hooks"],
    ] {
        let mut args = vec!["run", "--stamp"];
        args.extend(&flag);
        let (code, out) = amont(&r, &args, &[], &[]);
        assert_eq!(code, 2, "{flag:?}: {out}");
        assert!(out.contains(flag[0]), "{flag:?} is named: {out}");
    }
    assert_eq!(runs(&r), 0, "nothing ran");
}

/// `stamps --json` reports absent, then stamped, for the gate as declared.
#[test]
fn stamps_reports_absent_then_stamped() {
    if common::missing("node") {
        return;
    }
    let (r, _) = paired_repo("");
    let (code, out) = amont(&r, &["stamps", "--json"], &[], &[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("\"pre-commit-check\":\"absent\""), "{out}");
    assert!(out.contains(&format!("\"tree\":\"{}\"", tree(&r))), "{out}");
    assert!(out.contains("\"platform\":\""), "{out}");

    let (code, _) = amont(&r, &["run", "--stamp"], &[], &[]);
    assert_eq!(code, 0);
    let (code, out) = amont(&r, &["stamps", "--json", "check"], &[], &[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("\"pre-commit-check\":\"stamped\""), "{out}");
    assert!(
        out.contains("\"pre-push-check\":\"stamped\""),
        "the push gate is stamped through its twin: {out}"
    );
    let (code, out) = amont(&r, &["stamps", "--tree", "HEAD~1"], &[], &[]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("absent"),
        "another tree carries nothing: {out}"
    );
    let (code, out) = amont(&r, &["stamps", "no-such-gate"], &[], &[]);
    assert_eq!(code, 2, "{out}");
}

/// A signed attestation, end to end. `run --stamp` earns the stamp, the push
/// skips the suite and signs what it finds stamped, and with the local stamps
/// gone `stamps` still reports the gate as attested — for the tree the
/// attestation was made on, whether or not it is the one checked out.
///
/// Unix only: the shims it pushes through are shell scripts, and the signer
/// is `ssh-keygen`.
#[cfg(unix)]
#[test]
fn an_attested_tree_reads_as_attested_even_when_it_is_not_checked_out() {
    if common::missing("node") || common::missing("ssh-keygen") {
        return;
    }
    let (r, _) = paired_repo("");
    let remote = r.dir.join("remote.git");
    let init = r.git(&["init", "-q", "--bare", &remote.display().to_string()]);
    assert!(init.status.success());
    r.git(&["remote", "add", "origin", &remote.display().to_string()]);

    let key = r.path(".git/attest-key");
    let made = std::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "t@example.org", "-f"])
        .arg(&key)
        .status()
        .expect("ssh-keygen")
        .success();
    assert!(made, "ssh-keygen");
    let public = std::fs::read_to_string(r.path(".git/attest-key.pub")).unwrap();
    r.write(
        ".github/allowed_signers",
        &format!(
            "t@example.org namespaces=\"amont-attest,amont-provenance\" {}\n",
            public.trim()
        ),
    );
    r.git(&["config", "amont.attest", "true"]);
    r.git(&["config", "amont.attestKey", &key.display().to_string()]);

    let stamped_tree = tree(&r);
    let (code, out) = amont(&r, &["run", "--stamp"], &[], &[]);
    assert_eq!(code, 0, "{out}");
    let pushed = r.git(&["push", "-q", "-u", "origin", "HEAD:refs/heads/feat/x"]);
    assert!(
        pushed.status.success(),
        "{}{}",
        String::from_utf8_lossy(&pushed.stdout),
        String::from_utf8_lossy(&pushed.stderr)
    );
    assert_eq!(runs(&r), 1, "the push did not run the suite again");

    // The remote's signed note names the gate the push found stamped.
    let listed = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(&remote)
        .args(["notes", "--ref", "amont-attest", "list"])
        .output()
        .expect("git notes list");
    let keys: Vec<String> = String::from_utf8_lossy(&listed.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().nth(1).map(str::to_string))
        .collect();
    let gates: String = keys
        .iter()
        .map(|k| {
            let shown = std::process::Command::new("git")
                .arg("--git-dir")
                .arg(&remote)
                .args(["notes", "--ref", "amont-attest", "show", k])
                .output()
                .expect("git notes show");
            String::from_utf8_lossy(&shown.stdout).into_owned()
        })
        .collect();
    assert!(
        gates
            .lines()
            .any(|l| l.starts_with("gates ") && l.split_whitespace().any(|g| g == "pre-push-check")),
        "the remote's note names the gate: {gates}"
    );

    // The machine's own stamps gone: only the signed note is left.
    for notes in ["refs/notes/amont-gate", "refs/notes/amont-gate-inputs"] {
        r.git(&["update-ref", "-d", notes]);
    }
    let (code, out) = amont(&r, &["stamps", "--json"], &[], &[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("\"pre-push-check\":\"attested\""), "{out}");
    assert!(
        out.contains("\"pre-commit-check\":\"attested\""),
        "a commit-time gate reads its push twin's attestation: {out}"
    );

    // Another tree is checked out now: the old tree still reads attested,
    // and the new one reads absent.
    r.stage("b.txt", "two\n");
    r.commit("feat: b");
    assert_ne!(tree(&r), stamped_tree);
    let (code, out) = amont(&r, &["stamps", "--json"], &[], &[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("\"pre-push-check\":\"absent\""), "{out}");
    let (code, out) = amont(&r, &["stamps", "--json", "--tree", "HEAD~1"], &[], &[]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("\"pre-push-check\":\"attested\""),
        "the attested tree is not HEAD: {out}"
    );
    let (code, out) = amont(&r, &["stamps", "--json", "--tree", &stamped_tree], &[], &[]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("\"pre-push-check\":\"attested\""),
        "asked for by its tree id: {out}"
    );
    let worktrees = r.git(&["worktree", "list", "--porcelain"]);
    assert_eq!(
        String::from_utf8_lossy(&worktrees.stdout)
            .lines()
            .filter(|l| l.starts_with("worktree "))
            .count(),
        1,
        "the throwaway checkout was removed"
    );
}

/// Host slots are `flock` files and the busy host is a `perl` child holding
/// one: Windows has neither.
#[cfg(unix)]
mod busy_host {
    use super::*;
    use std::io::BufRead;
    use std::path::Path;
    use std::process::{Child, Command};

    /// One slot file held by another process until dropped: the busy host.
    struct Holder(Child);

    impl Holder {
        fn hold(slot: &Path) -> Holder {
            let mut child = Command::new("perl")
                .args([
                    "-MFcntl=:flock",
                    "-e",
                    "$|=1; open(F, '>', $ARGV[0]) or die; flock(F, LOCK_EX) or die; \
                     print \"locked\\n\"; sleep 120",
                ])
                .arg(slot)
                .stdout(Stdio::piped())
                .spawn()
                .expect("spawn perl");
            let mut line = String::new();
            std::io::BufReader::new(child.stdout.as_mut().unwrap())
                .read_line(&mut line)
                .expect("read perl");
            assert_eq!(line.trim(), "locked", "the holder did not take the slot");
            Holder(child)
        }
    }

    impl Drop for Holder {
        fn drop(&mut self) {
            let _ = self.0.kill(); // best-effort: it may have exited already
            let _ = self.0.wait(); // best-effort: reap, nothing to report
        }
    }

    fn slot_dir(tag: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("amont-runstamp-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }

    /// A gate the busy policy defers writes no token and exits 4, naming it.
    /// A deferral the commit hook recorded is what `stamps` reports as such.
    #[test]
    fn a_deferred_gate_leaves_no_token() {
        if common::missing("perl") || common::missing("node") {
            return;
        }
        let (r, _) = paired_repo("set onBusy defer\nset slotWait 1\n");
        let slots = slot_dir("defer");
        let _holder = Holder::hold(&slots.join("slot-0"));
        let busy = [
            ("AMONT_SLOT_DIR", slots.to_str().unwrap()),
            ("AMONT_HOST_SLOTS", "1"),
            ("AMONT_IDLE_LOAD_SCALE", "1"),
        ];
        let (code, out) = amont(
            &r,
            &["run", "--stamp", "pre-commit-check"],
            &busy,
            &["AMONT_HOST_SLOT"],
        );
        assert_eq!(code, 4, "{out}");
        assert!(
            out.contains("pre-commit-check deferred, no host slot freed"),
            "{out}"
        );
        assert_eq!(runs(&r), 0, "a deferred gate did not run");
        assert!(!note(&r, &tree(&r)).contains("check@"));

        // The commit hook's own deferral is what `stamps` calls deferred.
        r.stage("b.txt", "two\n");
        let mut hook = common::amont_cmd();
        hook.arg("--hooks-dir")
            .arg(r.path(".git/hooks"))
            .arg("pre-commit")
            .current_dir(&r.dir)
            .stdin(Stdio::null())
            .env_remove("AMONT_HOST_SLOT");
        for (k, v) in busy {
            hook.env(k, v);
        }
        let out = hook.output().expect("pre-commit");
        assert!(out.status.success());
        r.commit("feat: b");
        let (code, out) = amont(&r, &["stamps", "--json", "pre-commit-check"], &[], &[]);
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("\"pre-commit-check\":\"deferred\""), "{out}");
        let _ = std::fs::remove_dir_all(&slots); // best-effort: a scratch dir
    }
}
