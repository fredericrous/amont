//! A busy host defers a Heavy gate to CI (ADR-0013, `hooks.busy-policy`).
//!
//! Every fixture here runs UNPINNED: `.cargo/config.toml` sets
//! `AMONT_HOST_SLOT=held` for the whole test process, which makes every
//! check Nested, and a Nested check never queues, so it never defers. Each
//! run removes it and points `AMONT_SLOT_DIR` at a directory of its own with
//! one slot, which a `perl` child holds when the host is to look busy.

use crate::common;
use common::{missing, Repo};

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
        let _ = self.0.kill(); // best-effort: it may have exited already
        let _ = self.0.wait(); // best-effort: reap, nothing to report
    }
}

/// A private slot directory with mode 0700, as amont requires.
fn slot_dir(tag: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("amont-busy-{}-{tag}-{seq}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

/// Run a hook with `args` and the host knobs this file controls, feeding
/// `stdin`.
fn hook(
    repo: &Repo,
    name: &str,
    args: &[&str],
    slots: &Path,
    stdin: &str,
    env: &[(&str, &str)],
) -> (i32, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_amont"));
    cmd.arg("--hooks-dir")
        .arg(repo.path(".git/hooks"))
        .arg(name)
        .args(args)
        .current_dir(&repo.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    Repo::strip_git_env_impl(&mut cmd);
    cmd.env_remove("AMONT_HOST_SLOT")
        .env_remove("AMONT_ON_BUSY")
        .env("AMONT_IDLE_LOAD_SCALE", "1")
        .env("AMONT_SLOT_DIR", slots)
        .env("AMONT_HOST_SLOTS", "1");
    for (key, value) in env {
        cmd.env(key, value);
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

fn trust(repo: &Repo) {
    let out = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("trust")
        .current_dir(&repo.dir)
        .stdin(Stdio::null())
        .output()
        .expect("amont trust");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn head(repo: &Repo) -> String {
    rev(repo, "HEAD")
}

fn rev(repo: &Repo, what: &str) -> String {
    String::from_utf8_lossy(&repo.git(&["rev-parse", what]).stdout)
        .trim()
        .to_string()
}

/// How many times a gate's command ran, from the log it appends to.
fn runs(repo: &Repo, log: &str) -> usize {
    std::fs::read_to_string(repo.dir.join(log))
        .map(|text| text.len())
        .unwrap_or(0)
}

/// The local gate note on `key` (a commit or a tree), empty when none.
fn gate_note(repo: &Repo, key: &str) -> String {
    String::from_utf8_lossy(
        &repo
            .git(&["notes", "--ref", "amont-gate", "show", key])
            .stdout,
    )
    .into_owned()
}

/// One push ref line, `from..to` on a feature branch.
fn push_line(branch: &str, from: &str, to: &str) -> String {
    format!("refs/heads/{branch} {to} refs/heads/{branch} {from}\n")
}

/// A gate declared at both stages, Heavy, logging each run, plus a Light
/// push gate (so a push always has something to attest); `defer` on with a
/// one-second wait.
fn paired_repo() -> (Repo, String) {
    let repo = Repo::new();
    repo.stage("gate.sh", "printf x >> gate.log\n");
    repo.stage(
        "amont.conf",
        "set onBusy defer\nset slotWait 1\n\
         pre-commit  gate   *.txt  block  heavy sh gate.sh\n\
         pre-push    gate   *.txt  block  heavy sh gate.sh\n\
         pre-push    light  *.txt  block  true\n",
    );
    repo.commit("chore: base");
    trust(&repo);
    let base = head(&repo);
    (repo, base)
}

/// A pre-commit run on a busy host: the gate defers. Asserts the deferral
/// was said and nothing ran.
fn commit_deferred(repo: &Repo, slots: &Path, file: &str, msg: &str) {
    repo.stage(file, &format!("{file}\n"));
    let before = runs(repo, "gate.log");
    let (code, out) = hook(repo, "pre-commit", &[], slots, "", &[]);
    assert_eq!(code, 0, "a deferred block gate must not block: {out}");
    assert!(out.contains("deferred to CI"), "{out}");
    assert_eq!(runs(repo, "gate.log"), before, "deferred, not run: {out}");
    repo.commit(msg);
}

/// The busy host at commit: the gate defers instead of running, a `block`
/// gate does not block, nothing is stamped, and the push twin defers too
/// rather than run the suite with the remote's connection open.
/// `AMONT_ON_BUSY=wait` brings the twin back once the host is free.
#[test]
fn a_busy_commit_defers_and_its_push_twin_defers_too() {
    let (repo, base) = paired_repo();
    let slots = slot_dir("pair");
    let holder = Holder::hold(&slots.join("slot-0"));

    repo.stage("a.txt", "one\n");
    let (code, out) = hook(&repo, "pre-commit", &[], &slots, "", &[]);
    assert_eq!(code, 0, "a deferred block gate must not block: {out}");
    assert!(out.contains("deferred to CI"), "{out}");
    assert!(out.contains("AMONT_ON_BUSY=wait amont run"), "{out}");
    assert_eq!(
        runs(&repo, "gate.log"),
        0,
        "a deferred gate must not run: {out}"
    );
    let marker = std::fs::read_to_string(repo.path(".git/amont-gate")).unwrap_or_default();
    assert!(
        !marker.lines().any(|token| token.starts_with("gate@")),
        "a deferred gate earns no stamp: {marker:?}"
    );
    repo.commit("feat: a");
    let tip = head(&repo);
    let line = push_line("feat/x", &base, &tip);

    let (code, out) = hook(&repo, "pre-push", &[], &slots, &line, &[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("deferred to CI at commit"), "{out}");
    assert!(
        !out.contains("gated at commit instead"),
        "never Gated: {out}"
    );
    assert_eq!(runs(&repo, "gate.log"), 0, "the twin must not run: {out}");
    let note = gate_note(&repo, &rev(&repo, "HEAD^{tree}"));
    let tokens = note.lines().next().unwrap_or_default();
    assert!(
        !tokens.contains("pre-push-gate"),
        "a deferred twin earns no push stamp: {note}"
    );

    drop(holder);
    let (code, out) = hook(
        &repo,
        "pre-push",
        &[],
        &slots,
        &line,
        &[("AMONT_ON_BUSY", "wait")],
    );
    assert_eq!(code, 0, "{out}");
    assert_eq!(runs(&repo, "gate.log"), 1, "wait runs the twin: {out}");
    let _ = std::fs::remove_dir_all(&slots); // best-effort: a scratch dir
}

/// Quiet mode rolls passes up into a count; a deferral is still said, or a
/// gate skipped without a word would look exactly like a pass.
#[test]
fn quiet_mode_still_says_a_check_was_deferred() {
    let (repo, _) = paired_repo();
    repo.git(&["config", "amont.quiet", "always"]);
    let slots = slot_dir("quiet");
    let _holder = Holder::hold(&slots.join("slot-0"));
    repo.stage("a.txt", "one\n");
    let (code, out) = hook(&repo, "pre-commit", &[], &slots, "", &[("NO_COLOR", "1")]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("check(s) passed"),
        "quiet mode is on (passes rolled up into a count): {out}"
    );
    assert!(
        out.contains("check(s) deferred to CI, no host slot freed: pre-commit-gate"),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&slots); // best-effort: a scratch dir
}

/// A deferred code commit followed by a commit the gate's scope never sees
/// (documentation): the twin still defers on the one commit that counts.
#[test]
fn a_documentation_commit_after_a_deferred_one_keeps_the_deferral() {
    let (repo, base) = paired_repo();
    let slots = slot_dir("docs");
    let holder = Holder::hold(&slots.join("slot-0"));
    commit_deferred(&repo, &slots, "a.txt", "feat: a");
    repo.stage("notes.md", "words\n");
    repo.commit("docs: notes");
    drop(holder);
    let line = push_line("feat/x", &base, &head(&repo));
    let (code, out) = hook(&repo, "pre-push", &[], &slots, &line, &[]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("deferred to CI at commit on 1 pushed commit"),
        "{out}"
    );
    assert_eq!(runs(&repo, "gate.log"), 0, "{out}");
    let _ = std::fs::remove_dir_all(&slots); // best-effort: a scratch dir
}

/// Two refs in one push: one deferred at commit, one made without the hooks.
/// The unchecked commit carries no record at all, so the twin runs, as it
/// always did: a deferral on one branch never excuses another.
#[test]
fn a_deferred_branch_does_not_excuse_an_unchecked_one() {
    let (repo, base) = paired_repo();
    let slots = slot_dir("refs");
    let holder = Holder::hold(&slots.join("slot-0"));
    repo.git(&["checkout", "-q", "--no-track", "-b", "feat/x"]);
    commit_deferred(&repo, &slots, "a.txt", "feat: a");
    let deferred_tip = head(&repo);
    drop(holder);
    repo.git(&["checkout", "-q", "--no-track", "-b", "feat/y", &base]);
    repo.stage("b.txt", "unchecked\n");
    repo.commit("feat: b");
    let unchecked_tip = head(&repo);
    let lines = format!(
        "{}{}",
        push_line("feat/x", &base, &deferred_tip),
        push_line("feat/y", &base, &unchecked_tip)
    );
    let (code, out) = hook(&repo, "pre-push", &[], &slots, &lines, &[]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("deferred to CI at commit"), "{out}");
    assert!(out.contains("running it here"), "{out}");
    // Once per pushed ref, as a declared push gate always runs (manifest.rs,
    // `Stage::PrePush`): the twin ran, and was not deferred, on both.
    assert_eq!(runs(&repo, "gate.log"), 2, "the twin runs: {out}");
    let _ = std::fs::remove_dir_all(&slots); // best-effort: a scratch dir
}

/// Signing on, a bare `origin`: what the remote's attestation names.
fn attested(repo: &Repo, slots: &Path, line: &str) -> Option<String> {
    if missing("ssh-keygen") {
        return None;
    }
    let key = repo.path(".git/attest-key");
    if !key.exists() {
        let made = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "t@example.org", "-f"])
            .arg(&key)
            .status()
            .expect("ssh-keygen")
            .success();
        assert!(made, "ssh-keygen");
        let remote = repo.path(".git/origin.git");
        repo.git(&["init", "-q", "--bare", &remote.display().to_string()]);
        repo.git(&["remote", "add", "origin", &remote.display().to_string()]);
        repo.git(&["config", "amont.attest", "true"]);
        repo.git(&["config", "amont.attestKey", &key.display().to_string()]);
    }
    let remote = repo.path(".git/origin.git");
    let url = remote.display().to_string();
    let (code, out) = hook(repo, "pre-push", &["origin", &url], slots, line, &[]);
    assert_eq!(code, 0, "{out}");
    let tip = line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_string();
    let note = Command::new("git")
        .args(["notes", "--ref", "amont-attest", "show", &tip])
        .current_dir(&remote)
        .output()
        .expect("git notes on the remote");
    let body = String::from_utf8_lossy(&note.stdout).into_owned();
    let gates = body
        .lines()
        .find(|l| l.starts_with("gates "))
        .unwrap_or_default()
        .to_string();
    assert!(
        gates
            .split_whitespace()
            .any(|gate| gate == "pre-push-light"),
        "the push attested its passing gate: {body}\n{out}"
    );
    Some(gates)
}

/// A push of one commit stamped at commit and one deferred, signing on: the
/// twin defers, is never `Gated`, and the signed `gates` line leaves it out,
/// so CI runs it.
#[test]
fn a_push_mixing_a_stamped_and_a_deferred_commit_defers_and_is_not_attested() {
    let (repo, base) = paired_repo();
    let slots = slot_dir("mixed");
    repo.stage("a.txt", "one\n");
    let (code, out) = hook(&repo, "pre-commit", &[], &slots, "", &[]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        runs(&repo, "gate.log"),
        1,
        "a free host runs the gate: {out}"
    );
    repo.commit("feat: a");
    let (code, out) = hook(&repo, "post-commit", &[], &slots, "", &[]);
    assert_eq!(code, 0, "{out}");

    let holder = Holder::hold(&slots.join("slot-0"));
    commit_deferred(&repo, &slots, "b.txt", "feat: b");
    let line = push_line("feat/x", &base, &head(&repo));
    // The push half first, signing off: it holds wherever ssh-keygen is
    // missing and `attested` returns early.
    let (code, out) = hook(&repo, "pre-push", &[], &slots, &line, &[]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("deferred to CI at commit on 1 pushed commit"),
        "{out}"
    );
    assert!(
        !out.contains("gated at commit instead"),
        "never Gated: {out}"
    );
    let Some(gates) = attested(&repo, &slots, &line) else {
        return;
    };
    assert!(
        !gates.split_whitespace().any(|gate| gate == "pre-push-gate"),
        "a deferred twin is never attested: {gates}"
    );
    assert_eq!(runs(&repo, "gate.log"), 1, "the twin did not run");
    drop(holder);
    let _ = std::fs::remove_dir_all(&slots); // best-effort: a scratch dir
}

/// A forged `deferred` line on a commit nothing checked: it moves the twin to
/// CI and can never make it pass — the signed `gates` line leaves it out.
#[test]
fn a_forged_deferral_only_moves_the_gate_to_ci() {
    let (repo, base) = paired_repo();
    let slots = slot_dir("forged");
    repo.stage("a.txt", "one\n");
    repo.commit("feat: a");
    let tree = rev(&repo, "HEAD^{tree}");
    repo.git(&[
        "notes",
        "--ref",
        "amont-gate",
        "add",
        "-f",
        "-m",
        "amont-gate-v1 \ndeferred gate",
        &tree,
    ]);
    let line = push_line("feat/x", &base, &head(&repo));
    let (code, out) = hook(&repo, "pre-push", &[], &slots, &line, &[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("deferred to CI at commit"), "{out}");
    let Some(gates) = attested(&repo, &slots, &line) else {
        return;
    };
    assert!(
        !gates.split_whitespace().any(|gate| gate == "pre-push-gate"),
        "a forged deferral never attests the twin: {gates}"
    );
    assert_eq!(runs(&repo, "gate.log"), 0, "moved to CI, not run here");
    let _ = std::fs::remove_dir_all(&slots); // best-effort: a scratch dir
}

/// Three Heavy push gates on a busy host: the first waits `slotWait`, the
/// rest defer at once, so the push waits one `slotWait`, not three. A
/// deferred check says nothing about its refused spawn.
#[test]
fn a_busy_push_waits_one_slot_wait_not_one_per_gate() {
    let repo = Repo::new();
    repo.stage(
        "amont.conf",
        "set onBusy defer\nset slotWait 3\n\
         pre-push  one    *.txt  block  heavy sh -c true\n\
         pre-push  two    *.txt  block  heavy sh -c true\n\
         pre-push  three  *.txt  block  heavy sh -c true\n",
    );
    repo.commit("chore: base");
    trust(&repo);
    let base = head(&repo);
    repo.stage("a.txt", "one\n");
    repo.commit("feat: a");
    let slots = slot_dir("serial");
    let _holder = Holder::hold(&slots.join("slot-0"));
    let line = push_line("feat/x", &base, &head(&repo));
    let started = std::time::Instant::now();
    let (code, out) = hook(&repo, "pre-push", &[], &slots, &line, &[]);
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
    let _ = std::fs::remove_dir_all(&slots); // best-effort: a scratch dir
}

/// One slot, two Heavy checks of the same commit, nobody else on the host:
/// the second waits behind the first instead of deferring behind its own
/// commit's work.
#[test]
fn a_commit_never_defers_behind_its_own_checks() {
    let repo = Repo::new();
    repo.stage("slow.sh", "sleep 2; printf x >> \"$1\"\n");
    repo.stage(
        "amont.conf",
        "set onBusy defer\nset slotWait 1\n\
         pre-commit  one  *.txt  block  heavy sh slow.sh one.log\n\
         pre-commit  two  *.txt  block  heavy sh slow.sh two.log\n",
    );
    repo.commit("chore: base");
    trust(&repo);
    let slots = slot_dir("siblings");
    repo.stage("a.txt", "one\n");
    let (code, out) = hook(&repo, "pre-commit", &[], &slots, "", &[]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("deferred"), "{out}");
    assert_eq!(
        (runs(&repo, "one.log"), runs(&repo, "two.log")),
        (1, 1),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&slots); // best-effort: a scratch dir
}

/// A Heavy check whose scope the commit does not touch never runs, so it
/// never asks for the slot: a busy host neither delays nor defers it. (The
/// in-scope case — a built-in that finds nothing to do before its first
/// spawn — is ADR-0009's lazy slot, unchanged here.)
#[test]
fn a_heavy_check_out_of_scope_never_waits() {
    let (repo, _) = paired_repo();
    let slots = slot_dir("idle");
    let _holder = Holder::hold(&slots.join("slot-0"));
    repo.stage("notes.md", "words\n");
    let started = std::time::Instant::now();
    let (code, out) = hook(&repo, "pre-commit", &[], &slots, "", &[]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("deferred"), "{out}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&slots); // best-effort: a scratch dir
}
