//! A busy host defers a Heavy gate to CI (ADR-0013, `hooks.busy-policy`).
//!
//! Every fixture here runs UNPINNED: `.cargo/config.toml` sets
//! `AMONT_HOST_SLOT=held` for the whole test process, which makes every
//! check Nested, and a Nested check never queues, so it never defers. Each
//! run removes it and points `AMONT_SLOT_DIR` at a directory of its own with
//! one slot, which a `perl` child holds when the host is to look busy.

use crate::common;
use common::Repo;

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

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
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A private slot directory with mode 0700, as amont requires.
fn slot_dir(tag: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let d = std::env::temp_dir().join(format!("amont-busy-{}-{tag}-{n}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
    d
}

/// Run a hook with the host knobs this file controls, feeding `stdin`.
fn hook(r: &Repo, name: &str, slots: &Path, stdin: &str, env: &[(&str, &str)]) -> (i32, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_amont"));
    cmd.arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg(name)
        .current_dir(&r.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    Repo::strip_git_env_impl(&mut cmd);
    cmd.env_remove("AMONT_HOST_SLOT")
        .env_remove("AMONT_ON_BUSY")
        .env("AMONT_IDLE_LOAD_SCALE", "1")
        .env("AMONT_SLOT_DIR", slots)
        .env("AMONT_HOST_SLOTS", "1");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn amont");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("wait amont");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn trust(r: &Repo) {
    let out = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("trust")
        .current_dir(&r.dir)
        .stdin(Stdio::null())
        .output()
        .expect("amont trust");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn head(r: &Repo) -> String {
    String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string()
}

/// How many times a gate's command ran, from the log it appends to.
fn runs(r: &Repo, log: &str) -> usize {
    std::fs::read_to_string(r.dir.join(log))
        .map(|s| s.len())
        .unwrap_or(0)
}

/// A gate declared at both stages, Heavy, logging each run; `defer` on with
/// a one-second wait.
fn paired_repo() -> (Repo, String) {
    let r = Repo::new();
    r.stage("gate.sh", "printf x >> gate.log\n");
    r.stage(
        "amont.conf",
        "set onBusy defer\nset slotWait 1\n\
         pre-commit  gate  *.txt  block  heavy sh gate.sh\n\
         pre-push    gate  *.txt  block  heavy sh gate.sh\n",
    );
    r.commit("chore: base");
    trust(&r);
    let base = head(&r);
    (r, base)
}

/// The busy host at commit: the gate defers instead of running, a `block`
/// gate does not block, and the push twin defers too rather than run the
/// suite with the remote's connection open. `AMONT_ON_BUSY=wait` brings the
/// twin back once the host is free.
#[test]
fn a_busy_commit_defers_and_its_push_twin_defers_too() {
    let (r, base) = paired_repo();
    let slots = slot_dir("pair");
    let holder = Holder::hold(&slots.join("slot-0"));

    r.stage("a.txt", "one\n");
    let (code, out) = hook(&r, "pre-commit", &slots, "", &[]);
    assert_eq!(code, 0, "a deferred block gate must not block: {out}");
    assert!(out.contains("deferred to CI"), "{out}");
    assert!(out.contains("AMONT_ON_BUSY=wait amont run"), "{out}");
    assert_eq!(
        runs(&r, "gate.log"),
        0,
        "a deferred gate must not run: {out}"
    );
    r.commit("feat: a");
    let tip = head(&r);
    let line = format!("refs/heads/feat/x {tip} refs/heads/feat/x {base}\n");

    let (code, out) = hook(&r, "pre-push", &slots, &line, &[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("deferred to CI at commit"), "{out}");
    assert!(
        !out.contains("gated at commit instead"),
        "never Gated: {out}"
    );
    assert_eq!(runs(&r, "gate.log"), 0, "the twin must not run: {out}");

    drop(holder);
    let (code, out) = hook(&r, "pre-push", &slots, &line, &[("AMONT_ON_BUSY", "wait")]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(runs(&r, "gate.log"), 1, "wait runs the twin: {out}");
    let _ = std::fs::remove_dir_all(&slots);
}

/// A push of one commit stamped at commit and one deferred: the twin defers,
/// and is never `Gated`, which would put it in the attestation as passed.
#[test]
fn a_push_mixing_a_stamped_and_a_deferred_commit_defers() {
    let (r, base) = paired_repo();
    let slots = slot_dir("mixed");
    r.stage("a.txt", "one\n");
    let (code, out) = hook(&r, "pre-commit", &slots, "", &[]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(runs(&r, "gate.log"), 1, "a free host runs the gate: {out}");
    r.commit("feat: a");
    let (code, out) = hook(&r, "post-commit", &slots, "", &[]);
    assert_eq!(code, 0, "{out}");

    let holder = Holder::hold(&slots.join("slot-0"));
    r.stage("b.txt", "two\n");
    let (code, out) = hook(&r, "pre-commit", &slots, "", &[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("deferred to CI"), "{out}");
    r.commit("feat: b");
    let tip = head(&r);

    let line = format!("refs/heads/feat/x {tip} refs/heads/feat/x {base}\n");
    let (code, out) = hook(&r, "pre-push", &slots, &line, &[]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("deferred to CI at commit on 1 pushed commit"),
        "{out}"
    );
    assert!(
        !out.contains("gated at commit instead"),
        "never Gated: {out}"
    );
    assert_eq!(runs(&r, "gate.log"), 1, "the twin must not run: {out}");
    drop(holder);
    let _ = std::fs::remove_dir_all(&slots);
}

/// Three Heavy push gates on a busy host: the first waits `slotWait`, the
/// rest defer at once, so the push waits one `slotWait`, not three.
#[test]
fn a_busy_push_waits_one_slot_wait_not_one_per_gate() {
    let r = Repo::new();
    r.stage(
        "amont.conf",
        "set onBusy defer\nset slotWait 3\n\
         pre-push  one    *.txt  block  heavy sh -c true\n\
         pre-push  two    *.txt  block  heavy sh -c true\n\
         pre-push  three  *.txt  block  heavy sh -c true\n",
    );
    r.commit("chore: base");
    trust(&r);
    let base = head(&r);
    r.stage("a.txt", "one\n");
    r.commit("feat: a");
    let tip = head(&r);
    let slots = slot_dir("serial");
    let _holder = Holder::hold(&slots.join("slot-0"));
    let line = format!("refs/heads/feat/x {tip} refs/heads/feat/x {base}\n");
    let started = std::time::Instant::now();
    let (code, out) = hook(&r, "pre-push", &slots, &line, &[]);
    let took = started.elapsed();
    assert_eq!(code, 0, "{out}");
    assert_eq!(out.matches("deferred to CI,").count(), 3, "{out}");
    assert!(
        !out.contains("could not run"),
        "a refused spawn is not a finding: {out}"
    );
    assert!(
        took < std::time::Duration::from_secs(8),
        "one wait of 3 s, not three: took {took:?}\n{out}"
    );
    let _ = std::fs::remove_dir_all(&slots);
}

/// One slot, two Heavy checks of the same commit, nobody else on the host:
/// the second waits behind the first instead of deferring behind its own
/// commit's work.
#[test]
fn a_commit_never_defers_behind_its_own_checks() {
    let r = Repo::new();
    r.stage("slow.sh", "sleep 2; printf x >> \"$1\"\n");
    r.stage(
        "amont.conf",
        "set onBusy defer\nset slotWait 1\n\
         pre-commit  one  *.txt  block  heavy sh slow.sh one.log\n\
         pre-commit  two  *.txt  block  heavy sh slow.sh two.log\n",
    );
    r.commit("chore: base");
    trust(&r);
    let slots = slot_dir("siblings");
    r.stage("a.txt", "one\n");
    let (code, out) = hook(&r, "pre-commit", &slots, "", &[]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("deferred"), "{out}");
    assert_eq!((runs(&r, "one.log"), runs(&r, "two.log")), (1, 1), "{out}");
    let _ = std::fs::remove_dir_all(&slots);
}

/// A Heavy check with nothing to do never asks for the slot, so a busy host
/// neither delays nor defers it.
#[test]
fn a_heavy_check_with_nothing_to_do_never_waits() {
    let (r, _) = paired_repo();
    let slots = slot_dir("idle");
    let _holder = Holder::hold(&slots.join("slot-0"));
    r.stage("notes.md", "words\n");
    let started = std::time::Instant::now();
    let (code, out) = hook(&r, "pre-commit", &slots, "", &[]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("deferred"), "{out}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&slots);
}
