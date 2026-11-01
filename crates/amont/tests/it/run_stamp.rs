//! `amont run --stamp` and `amont stamps` (ADR-0015): a runner other than the
//! commit hook earns the stamp the hooks honour, and a tree's stamps can be
//! asked for.
//!
//! The gate is a pair declared at both stages, as in `gate_pairs.rs`, logging
//! each execution, so a count says how many times the suite really ran.

use crate::common;
use common::Repo;

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

fn head(r: &Repo) -> String {
    String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string()
}

fn tree(r: &Repo) -> String {
    String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD^{tree}"]).stdout)
        .trim()
        .to_string()
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

/// `amont <args>` in the repository, with `env` on top of the pinned host.
fn amont(r: &Repo, args: &[&str], env: &[(&str, &str)], unpin_slot: bool) -> (i32, String) {
    let mut cmd = common::amont_cmd();
    cmd.args(args).current_dir(&r.dir).stdin(Stdio::null());
    if unpin_slot {
        cmd.env_remove("AMONT_HOST_SLOT");
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

/// Captured output without colour codes, so a word that `highlight` wraps in
/// them still reads as the word.
fn plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
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
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// A pair `check` on `*.txt` at both stages, `gate.sh` logging each run and
/// exiting with the content of `exit-code` (0 when absent). Returns the base.
fn paired_repo(extra_conf: &str) -> (Repo, String) {
    let r = Repo::new();
    r.stage(
        "gate.sh",
        "printf x >> gate.log\n[ -f exit-code ] && exit \"$(cat exit-code)\"\nexit 0\n",
    );
    r.stage(".gitignore", "gate.log\nexit-code\n");
    r.commit("chore: base");
    let base = head(&r);
    r.stage(
        "amont.conf",
        &format!(
            "{extra_conf}pre-commit  check  *.txt  block  heavy sh gate.sh\n\
             pre-push    check  *.txt  block  heavy sh gate.sh\n"
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
    let (r, base) = paired_repo("");
    assert_eq!(runs(&r), 0);
    let (code, out) = amont(&r, &["run", "--stamp"], &[], false);
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
    let (r, _) = paired_repo("");
    r.write("a.txt", "changed\n");
    let (code, out) = amont(&r, &["run", "--stamp"], &[], false);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("needs a clean worktree"), "{out}");
    assert_eq!(runs(&r), 0, "nothing ran");
    assert!(!note(&r, &tree(&r)).contains("check@"));
}

/// A failing gate writes no token and exits 1.
#[test]
fn a_failing_gate_is_not_stamped() {
    let (r, _) = paired_repo("");
    std::fs::write(r.dir.join("exit-code"), "3").unwrap();
    let (code, out) = amont(&r, &["run", "--stamp", "pre-commit-check"], &[], false);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("failed — not stamped"), "{out}");
    assert!(!note(&r, &tree(&r)).contains("check@"));
}

/// `stamps --json` reports absent, then stamped, for the gate as declared.
#[test]
fn stamps_reports_absent_then_stamped() {
    let (r, _) = paired_repo("");
    let (code, out) = amont(&r, &["stamps", "--json"], &[], false);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("\"pre-commit-check\":\"absent\""), "{out}");
    assert!(out.contains(&format!("\"tree\":\"{}\"", tree(&r))), "{out}");
    assert!(out.contains("\"platform\":\""), "{out}");

    let (code, _) = amont(&r, &["run", "--stamp"], &[], false);
    assert_eq!(code, 0);
    let (code, out) = amont(&r, &["stamps", "--json", "check"], &[], false);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("\"pre-commit-check\":\"stamped\""), "{out}");
    assert!(
        out.contains("\"pre-push-check\":\"stamped\""),
        "the push gate is stamped through its twin: {out}"
    );
    let (code, out) = amont(&r, &["stamps", "--tree", "HEAD~1"], &[], false);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("absent"),
        "another tree carries nothing: {out}"
    );
    let (code, out) = amont(&r, &["stamps", "no-such-gate"], &[], false);
    assert_eq!(code, 2, "{out}");
}

/// One slot file held by another process: the busy host.
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
    if common::missing("perl") {
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
    let (code, out) = amont(&r, &["run", "--stamp", "pre-commit-check"], &busy, true);
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
    let (code, out) = amont(&r, &["stamps", "--json", "pre-commit-check"], &[], false);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("\"pre-commit-check\":\"deferred\""), "{out}");
    let _ = std::fs::remove_dir_all(&slots); // best-effort: a scratch dir
}
