//! `amont land` against a real bare remote: the faults the journey in
//! `flows/land.rs` does not meet. Another writer between the read and the
//! push, a remote that lies or stops answering after an accepted push, the
//! single retry after a hook that fast-forwards HEAD, a slow gate, a locked
//! tracking ref. Each needs a hook, a second clone or a `git` shim of its
//! own, so each keeps its own fixture. The fixture and its readers are shared
//! with the flow.
//!
//! Every fixture is a clone whose `origin` is a bare repository kept inside
//! its own `.git`, so nothing here touches the network or the machine's
//! hooks. Hooks a test wants are written by hand as `.git/hooks/pre-push`.
//!
//! A hook or a `git` shim is a shell script, so the tests here are unix-only
//! (like every test built on `common::fixture_exec`). The flow runs
//! everywhere, Windows included: `land` ships there too.

use crate::common;

use common::Repo;
use std::path::PathBuf;
use std::process::{Command, Stdio};

pub(crate) struct Fx {
    pub(crate) r: Repo,
    pub(crate) bare: PathBuf,
}

pub(crate) struct Ran {
    pub(crate) code: i32,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

pub(crate) fn plain_git(dir: &std::path::Path, args: &[&str]) -> String {
    let mut cmd = Command::new("git");
    // No hooks: these are the OTHER writers and the setup, and the machine's
    // own template hooks (commit-msg style, pre-push gates) are not under test.
    cmd.args(["-c", "core.hooksPath=/dev/null"])
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null());
    Repo::strip_git_env_impl(&mut cmd);
    let out = cmd.output().expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A clone on `branch` with one commit ahead of a pushed `main`, and a bare
/// `origin`.
pub(crate) fn fixture(branch: &str) -> Fx {
    let r = Repo::new();
    r.stage("a.txt", "base\n");
    r.commit("base");
    let bare = r.path(".git/bare");
    std::fs::create_dir_all(&bare).expect("mkdir bare");
    plain_git(
        &bare,
        &["init", "--bare", "-q", "--initial-branch=main", "."],
    );
    r.git(&["remote", "add", "origin", bare.to_str().expect("utf8")]);
    plain_git(&r.dir, &["push", "-q", "origin", "main"]);
    // A machine that sets `branch.autoSetupMerge` would make this branch
    // track local `main`; the fixture starts from "no upstream".
    r.git(&["config", "branch.autoSetupMerge", "false"]);
    r.git(&["switch", "-q", "-c", branch]);
    r.stage("b.txt", "work\n");
    r.commit("work");
    Fx { r, bare }
}

pub(crate) fn tip(fx: &Fx, branch: &str) -> String {
    plain_git(
        &fx.bare,
        &[
            "rev-parse",
            "--verify",
            "-q",
            &format!("refs/heads/{branch}"),
        ],
    )
}

pub(crate) fn head(fx: &Fx) -> String {
    plain_git(&fx.r.dir, &["rev-parse", "HEAD"])
}

fn land_with(fx: &Fx, args: &[&str], path_prefix: Option<&std::path::Path>) -> Ran {
    let mut cmd = crate::common::amont_cmd();
    cmd.arg("land")
        .args(args)
        .current_dir(&fx.r.dir)
        .stdin(Stdio::null());
    Repo::strip_git_env_impl(&mut cmd);
    if let Some(p) = path_prefix {
        let rest = std::env::var_os("PATH").unwrap_or_default();
        let joined = std::env::join_paths(
            std::iter::once(p.to_path_buf()).chain(std::env::split_paths(&rest)),
        )
        .expect("PATH");
        cmd.env("PATH", joined);
    }
    let out = cmd.output().expect("run amont land");
    Ran {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

pub(crate) fn land(fx: &Fx, args: &[&str]) -> Ran {
    land_with(fx, args, None)
}

pub(crate) fn status_of(ran: &Ran) -> String {
    let marker = "\"status\":\"";
    let i = ran.stdout.find(marker).unwrap_or_else(|| {
        panic!(
            "no status in stdout {:?}\nstderr: {}",
            ran.stdout, ran.stderr
        )
    }) + marker.len();
    ran.stdout[i..].split('"').next().unwrap_or("").to_string()
}

pub(crate) fn field(ran: &Ran, key: &str) -> String {
    let marker = format!("\"{key}\":\"");
    let i = ran.stdout.find(&marker).expect("field") + marker.len();
    // JSON escapes a backslash, and a Windows path is full of them.
    ran.stdout[i..]
        .split('"')
        .next()
        .unwrap_or("")
        .replace("\\\\", "\\")
}

#[cfg(unix)]
pub(crate) fn hook(fx: &Fx, body: &str) {
    let dir = fx.r.path(".git/hooks");
    std::fs::create_dir_all(&dir).expect("hooks dir");
    common::fixture_exec(&dir.join("pre-push"), &format!("#!/bin/sh\n{body}\n"));
}

#[cfg(unix)]
/// A `git` that answers every `ls-remote` with `answer` and lets the rest
/// through. The shim scans its arguments because the probe puts `-c` options
/// in front of the verb.
fn ls_remote_shim(fx: &Fx, answer: &str) -> PathBuf {
    shim(fx, answer, false)
}

#[cfg(unix)]
/// Like [`ls_remote_shim`], but the FIRST `ls-remote` is the real one: the
/// remote is read truthfully before the push and lies after it.
fn ls_remote_shim_after_first(fx: &Fx, answer: &str) -> PathBuf {
    shim(fx, answer, true)
}

#[cfg(unix)]
fn shim(fx: &Fx, answer: &str, real_first: bool) -> PathBuf {
    let real = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .expect("sh")
            .stdout,
    )
    .expect("utf8");
    let real = real.trim();
    let dir = fx.r.path(".git/gitshim");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let state = dir.join("seen-first");
    let first = if real_first {
        format!(
            "    if [ ! -e '{0}' ]; then : > '{0}'; exec {real} \"$@\"; fi\n",
            state.display()
        )
    } else {
        String::new()
    };
    // `FAIL` stands for a remote that stops answering.
    let reply = if answer == "FAIL" {
        "exit 128".to_string()
    } else {
        format!("printf '%s\\trefs/heads/%s\\n' '{answer}' feat; exit 0")
    };
    common::fixture_exec(
        &dir.join("git"),
        &format!(
            "#!/bin/sh\nfor a in \"$@\"; do\n  if [ \"$a\" = ls-remote ]; then\n{first}    {reply}\n  fi\ndone\nexec {real} \"$@\"\n"
        ),
    );
    dir
}

#[test]
#[cfg(unix)]
fn a_lease_is_lost_when_someone_else_pushes_between_the_read_and_the_push() {
    let fx = fixture("feat");
    assert_eq!(land(&fx, &[]).code, 0);
    fx.r.git(&["commit", "-q", "--no-verify", "--amend", "-m", "mine"]);
    // The hook runs after land read the tip and before the transfer: another
    // writer moves the ref right there.
    let other = fx.r.path(".git/other");
    plain_git(
        fx.r.dir.as_path(),
        &[
            "clone",
            "-q",
            fx.bare.to_str().expect("utf8"),
            other.to_str().expect("utf8"),
        ],
    );
    plain_git(&other, &["config", "user.email", "o@example.com"]);
    plain_git(&other, &["config", "user.name", "o"]);
    plain_git(&other, &["switch", "-q", "feat"]);
    plain_git(&other, &["commit", "-q", "--allow-empty", "-m", "theirs"]);
    hook(
        &fx,
        &format!(
            "env -u GIT_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE git -C '{}' push -q origin feat",
            other.display()
        ),
    );
    let before = tip(&fx, "feat");
    let ran = land(&fx, &["--force", "--json"]);
    assert_eq!(ran.code, 1, "{}", ran.stderr);
    assert_eq!(status_of(&ran), "refused");
    assert_ne!(tip(&fx, "feat"), head(&fx), "the stale lease must lose");
    assert_ne!(
        tip(&fx, "feat"),
        before,
        "their push is what the remote holds"
    );
}

#[test]
#[cfg(unix)]
fn an_unchanged_old_tip_after_an_accepted_push_is_exit_4() {
    let fx = fixture("feat");
    assert_eq!(land(&fx, &[]).code, 0);
    let old = tip(&fx, "feat");
    fx.r.stage("c.txt", "more\n");
    fx.r.commit("more");
    let shim = ls_remote_shim(&fx, &old);
    let started = std::time::Instant::now();
    let ran = land_with(&fx, &["--json"], Some(&shim));
    assert_eq!(ran.code, 4, "{}", ran.stderr);
    assert_eq!(status_of(&ran), "not-observed");
    assert_eq!(field(&ran, "remote_after"), old, "the observed sha is kept");
    assert_eq!(field(&ran, "pushed"), head(&fx));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "re-reads are bounded"
    );
    assert!(
        !ran.stderr.contains("amont land\n"),
        "no unconditional retry advice:\n{}",
        ran.stderr
    );
}

#[test]
#[cfg(unix)]
fn a_concurrent_third_sha_is_reported_as_observed_not_as_a_lost_update() {
    let fx = fixture("feat");
    assert_eq!(land(&fx, &[]).code, 0);
    fx.r.stage("c.txt", "more\n");
    fx.r.commit("more");
    let third = "3".repeat(40);
    let shim = ls_remote_shim_after_first(&fx, &third);
    let ran = land_with(&fx, &["--json"], Some(&shim));
    assert_eq!(ran.code, 4, "{}", ran.stderr);
    assert_eq!(status_of(&ran), "not-observed");
    assert_eq!(field(&ran, "remote_after"), third);
    assert_eq!(field(&ran, "pushed"), head(&fx));
    assert!(
        ran.stderr.contains("accepted the push but"),
        "the wording claims only what was seen:\n{}",
        ran.stderr
    );
}

#[test]
#[cfg(unix)]
fn a_hook_that_fast_forwards_head_gets_exactly_one_retry() {
    let fx = fixture("feat");
    assert_eq!(land(&fx, &[]).code, 0);
    // The remote moves ahead; this clone learns of it but has not merged it.
    let other = fx.r.path(".git/other");
    plain_git(
        fx.r.dir.as_path(),
        &[
            "clone",
            "-q",
            fx.bare.to_str().expect("utf8"),
            other.to_str().expect("utf8"),
        ],
    );
    plain_git(&other, &["config", "user.email", "o@example.com"]);
    plain_git(&other, &["config", "user.name", "o"]);
    plain_git(&other, &["switch", "-q", "feat"]);
    plain_git(&other, &["commit", "-q", "--allow-empty", "-m", "ahead"]);
    plain_git(&other, &["push", "-q", "origin", "feat"]);
    plain_git(&fx.r.dir, &["fetch", "-q", "origin"]);
    // The branch is strictly behind: the hook fast-forwards HEAD, says
    // "push again", and fails. Stand in for it with the real words so the
    // fixture does not depend on the gate's own configuration.
    let log = fx.r.path(".git/hook-runs");
    hook(
        &fx,
        &format!(
            "echo run >> '{}'\nif [ \"$(git rev-parse HEAD)\" != \"$(git rev-parse origin/feat)\" ]; then\n  git merge -q --ff-only origin/feat\n  echo 'Rebased onto origin/feat. This push refs predate the rebase - push again.'\n  exit 1\nfi",
            log.display()
        ),
    );
    let ran = land(&fx, &["--json"]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(status_of(&ran), "landed");
    assert!(ran.stdout.contains("\"retried\":true"), "{}", ran.stdout);
    assert!(ran.stderr.contains("retrying once"), "{}", ran.stderr);
    assert_eq!(tip(&fx, "feat"), head(&fx));
    let runs = std::fs::read_to_string(&log).expect("log").lines().count();
    assert_eq!(runs, 2, "one push, one retry");
}

#[test]
#[cfg(unix)]
fn a_diverged_branch_is_refused_without_a_retry() {
    let fx = fixture("feat");
    assert_eq!(land(&fx, &[]).code, 0);
    let other = fx.r.path(".git/other");
    plain_git(
        fx.r.dir.as_path(),
        &[
            "clone",
            "-q",
            fx.bare.to_str().expect("utf8"),
            other.to_str().expect("utf8"),
        ],
    );
    plain_git(&other, &["config", "user.email", "o@example.com"]);
    plain_git(&other, &["config", "user.name", "o"]);
    plain_git(&other, &["switch", "-q", "feat"]);
    plain_git(&other, &["commit", "-q", "--allow-empty", "-m", "theirs"]);
    plain_git(&other, &["push", "-q", "origin", "feat"]);
    fx.r.stage("d.txt", "mine\n");
    fx.r.commit("mine");
    let log = fx.r.path(".git/hook-runs");
    hook(&fx, &format!("echo run >> '{}'", log.display()));
    let ran = land(&fx, &["--json"]);
    assert_eq!(ran.code, 1, "{}", ran.stderr);
    assert_eq!(status_of(&ran), "refused");
    assert!(ran.stdout.contains("\"retried\":false"), "{}", ran.stdout);
    assert_eq!(
        std::fs::read_to_string(&log).expect("log").lines().count(),
        1
    );
}

#[test]
#[cfg(unix)]
fn a_hook_that_moves_head_for_another_reason_is_not_retried() {
    let fx = fixture("feat");
    let log = fx.r.path(".git/hook-runs");
    hook(
        &fx,
        &format!(
            "echo run >> '{}'\ngit commit -q --allow-empty --no-verify -m 'hook made this'\necho 'refusing for another reason' >&2\nexit 1",
            log.display()
        ),
    );
    let ran = land(&fx, &["--json"]);
    assert_eq!(ran.code, 1, "{}", ran.stderr);
    assert!(ran.stdout.contains("\"retried\":false"), "{}", ran.stdout);
    assert_eq!(
        std::fs::read_to_string(&log).expect("log").lines().count(),
        1
    );
}

#[test]
#[cfg(unix)]
fn a_second_failure_after_the_retry_is_final() {
    let fx = fixture("feat");
    assert_eq!(land(&fx, &[]).code, 0);
    let other = fx.r.path(".git/other");
    plain_git(
        fx.r.dir.as_path(),
        &[
            "clone",
            "-q",
            fx.bare.to_str().expect("utf8"),
            other.to_str().expect("utf8"),
        ],
    );
    plain_git(&other, &["config", "user.email", "o@example.com"]);
    plain_git(&other, &["config", "user.name", "o"]);
    plain_git(&other, &["switch", "-q", "feat"]);
    plain_git(&other, &["commit", "-q", "--allow-empty", "-m", "ahead"]);
    plain_git(&other, &["push", "-q", "origin", "feat"]);
    plain_git(&fx.r.dir, &["fetch", "-q", "origin"]);
    let log = fx.r.path(".git/hook-runs");
    // Always claims to have rebased and always fails, moving HEAD each time.
    hook(
        &fx,
        &format!(
            "echo run >> '{}'\ngit merge -q --ff-only origin/feat 2>/dev/null\ngit commit -q --allow-empty --no-verify -m again\necho 'Rebased. push again.'\nexit 1",
            log.display()
        ),
    );
    let ran = land(&fx, &["--json"]);
    assert_eq!(ran.code, 1, "{}", ran.stderr);
    assert_eq!(
        std::fs::read_to_string(&log).expect("log").lines().count(),
        2,
        "one retry and no third try"
    );
}

#[test]
#[cfg(unix)]
fn an_accepted_push_that_cannot_be_read_back_is_unverified() {
    let fx = fixture("feat");
    let shim = ls_remote_shim_after_first(&fx, "FAIL");
    let ran = land_with(&fx, &["--json"], Some(&shim));
    assert_eq!(ran.code, 3, "{}", ran.stderr);
    assert_eq!(status_of(&ran), "unverified");
    assert_eq!(tip(&fx, "feat"), head(&fx), "the push itself did land");
    assert!(
        ran.stderr.contains("could not be read back"),
        "{}",
        ran.stderr
    );
    // land never moves the tracking ref for a landing it could not verify.
    // git itself may: from 2.56, a push to a URL that matches a configured
    // remote updates that remote's tracking ref (RelNotes 2.56.0), so on a
    // newer git the ref exists, and then it can only be the pushed sha.
    // Below 2.56 nothing but land could have written it, so it must be
    // absent: the full proof. From 2.56 it may exist, and only as the pushed
    // sha.
    let tracking =
        fx.r.git(&["rev-parse", "--verify", "-q", "refs/remotes/origin/feat"]);
    if git_at_least(2, 56) {
        if tracking.status.success() {
            assert_eq!(
                String::from_utf8_lossy(&tracking.stdout).trim(),
                head(&fx),
                "only git's own update may have moved it, to what was pushed"
            );
        }
    } else {
        assert!(
            !tracking.status.success(),
            "an unverified landing moves no tracking ref"
        );
    }
}

/// Is the `git` these tests run at least `major.minor`?
#[cfg(unix)]
fn git_at_least(major: u32, minor: u32) -> bool {
    let out = Command::new("git")
        .arg("--version")
        .output()
        .expect("git --version");
    let text = String::from_utf8_lossy(&out.stdout);
    let mut parts = text
        .split_whitespace()
        .nth(2)
        .unwrap_or("")
        .split('.')
        .map(|p| p.parse::<u32>().unwrap_or(0));
    let (a, b) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    (a, b) >= (major, minor)
}

#[test]
#[cfg(unix)]
fn the_push_has_no_land_deadline_so_a_slow_gate_still_lands() {
    let fx = fixture("feat");
    // The probe budget follows `amont.timeout`; a gate that outlasts twice
    // that budget must not be killed by land.
    fx.r.git(&["config", "amont.timeout", "2"]);
    hook(&fx, "sleep 5");
    let started = std::time::Instant::now();
    let ran = land(&fx, &["--json"]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(started.elapsed() >= std::time::Duration::from_secs(5));
    assert_eq!(tip(&fx, "feat"), head(&fx));
}

/// Every receipt file of the fixture, parsed as text.
pub(crate) fn receipts(fx: &Fx) -> Vec<String> {
    let dir = fx.r.path(".git/amont/land");
    let Ok(read) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    read.flatten()
        .map(|e| std::fs::read_to_string(e.path()).expect("receipt"))
        .collect()
}

pub(crate) fn receipt_field(receipt: &str, key: &str) -> String {
    let marker = format!("\"{key}\":\"");
    let i = receipt.find(&marker).expect("receipt field") + marker.len();
    receipt[i..].split('"').next().unwrap_or("").to_string()
}

#[test]
#[cfg(unix)]
fn a_not_observed_land_writes_a_not_observed_receipt() {
    let fx = fixture("feat");
    assert_eq!(land(&fx, &[]).code, 0);
    let old = tip(&fx, "feat");
    fx.r.stage("c.txt", "more\n");
    fx.r.commit("more");
    let shim = ls_remote_shim(&fx, &old);
    let ran = land_with(&fx, &["--json"], Some(&shim));
    assert_eq!(ran.code, 4, "{}", ran.stderr);
    let all = receipts(&fx);
    assert_eq!(all.len(), 2, "{all:?}");
    let last = all
        .iter()
        .find(|r| receipt_field(r, "status") == "not-observed")
        .expect("a not-observed receipt");
    assert_eq!(receipt_field(last, "pushed"), head(&fx));
}

#[test]
#[cfg(unix)]
fn a_retry_with_a_locked_tracking_ref_still_lands_and_the_receipt_names_the_retried_sha() {
    let fx = fixture("feat");
    assert_eq!(land(&fx, &[]).code, 0);
    let other = fx.r.path(".git/other");
    plain_git(
        fx.r.dir.as_path(),
        &[
            "clone",
            "-q",
            fx.bare.to_str().expect("utf8"),
            other.to_str().expect("utf8"),
        ],
    );
    plain_git(&other, &["config", "user.email", "o@example.com"]);
    plain_git(&other, &["config", "user.name", "o"]);
    plain_git(&other, &["switch", "-q", "feat"]);
    plain_git(&other, &["commit", "-q", "--allow-empty", "-m", "ahead"]);
    plain_git(&other, &["push", "-q", "origin", "feat"]);
    plain_git(&fx.r.dir, &["fetch", "-q", "origin"]);
    let first_head = head(&fx);
    hook(
        &fx,
        "if [ \"$(git rev-parse HEAD)\" != \"$(git rev-parse origin/feat)\" ]; then\n  git merge -q --ff-only origin/feat\n  echo 'Rebased onto origin/feat. This push refs predate the rebase - push again.'\n  exit 1\nfi",
    );
    // update-ref cannot take the tracking ref: a warning, not an exit code.
    std::fs::write(fx.r.path(".git/refs/remotes/origin/feat.lock"), "").expect("lock");
    let ran = land(&fx, &["--json"]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(status_of(&ran), "landed");
    assert!(ran.stdout.contains("\"retried\":true"), "{}", ran.stdout);
    assert_ne!(head(&fx), first_head, "the hook moved HEAD");
    assert!(
        ran.stderr.contains("could not move"),
        "the lock is a warning:\n{}",
        ran.stderr
    );
    let all = receipts(&fx);
    let retried = all
        .iter()
        .find(|r| r.contains("\"retried\":true"))
        .expect("the retried land's receipt");
    assert_eq!(receipt_field(retried, "pushed"), head(&fx));
    assert_eq!(receipt_field(retried, "status"), "landed");
    assert_eq!(tip(&fx, "feat"), head(&fx));
}
