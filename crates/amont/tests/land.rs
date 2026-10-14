//! `amont land` against a real bare remote: the exit codes, the proof, the
//! single retry, and the shape of what is printed.
//!
//! Every fixture is a clone whose `origin` is a bare repository kept inside
//! its own `.git`, so nothing here touches the network or the machine's
//! hooks. Hooks a test wants are written by hand as `.git/hooks/pre-push`.
//!
//! A hook or a `git` shim is a shell script, so the tests that need one are
//! unix-only (like every test built on `common::fixture_exec`). The rest run
//! everywhere, Windows included: `land` ships there too.

mod common;

use common::Repo;
use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Fx {
    r: Repo,
    bare: PathBuf,
}

struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

fn plain_git(dir: &std::path::Path, args: &[&str]) -> String {
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
fn fixture(branch: &str) -> Fx {
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

fn tip(fx: &Fx, branch: &str) -> String {
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

fn head(fx: &Fx) -> String {
    plain_git(&fx.r.dir, &["rev-parse", "HEAD"])
}

fn land_with(fx: &Fx, args: &[&str], path_prefix: Option<&std::path::Path>) -> Ran {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_amont"));
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

fn land(fx: &Fx, args: &[&str]) -> Ran {
    land_with(fx, args, None)
}

fn status_of(ran: &Ran) -> String {
    let marker = "\"status\":\"";
    let i = ran.stdout.find(marker).unwrap_or_else(|| {
        panic!(
            "no status in stdout {:?}\nstderr: {}",
            ran.stdout, ran.stderr
        )
    }) + marker.len();
    ran.stdout[i..].split('"').next().unwrap_or("").to_string()
}

fn field(ran: &Ran, key: &str) -> String {
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
fn hook(fx: &Fx, body: &str) {
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
fn a_branch_lands_and_the_remote_is_proven() {
    let fx = fixture("feat");
    let ran = land(&fx, &["--json"]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(status_of(&ran), "landed");
    assert_eq!(tip(&fx, "feat"), head(&fx));
    assert_eq!(field(&ran, "remote_after"), head(&fx));
    assert_eq!(field(&ran, "pushed"), head(&fx));
    assert_eq!(field(&ran, "remote_before"), "");
    // A URL push moves no tracking ref; land does, and sets the upstream.
    assert_eq!(
        plain_git(&fx.r.dir, &["rev-parse", "refs/remotes/origin/feat"]),
        head(&fx)
    );
    assert_eq!(
        plain_git(&fx.r.dir, &["rev-parse", "--abbrev-ref", "feat@{u}"]),
        "origin/feat"
    );
}

#[test]
#[cfg(unix)]
fn stdout_is_the_json_document_alone() {
    let fx = fixture("feat");
    hook(&fx, "echo 'hook says hello'");
    let ran = land(&fx, &["--json"]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert!(ran.stdout.trim().starts_with('{') && ran.stdout.trim().ends_with('}'));
    assert_eq!(ran.stdout.trim().lines().count(), 1, "{}", ran.stdout);
    assert!(
        ran.stderr.contains("hook says hello"),
        "hook output is never suppressed:\n{}",
        ran.stderr
    );
    let last = ran.stderr.lines().last().expect("a verdict line");
    assert!(last.starts_with("landed"), "{last}");
}

#[test]
#[cfg(unix)]
fn a_hook_refusal_reaches_the_user_and_moves_nothing() {
    let fx = fixture("feat");
    hook(&fx, "echo 'gate says no' >&2; exit 1");
    let ran = land(&fx, &["--json"]);
    assert_eq!(ran.code, 1);
    assert_eq!(status_of(&ran), "refused");
    assert!(ran.stderr.contains("gate says no"), "{}", ran.stderr);
    assert!(
        !plain_git(&fx.bare, &["branch", "--list", "feat"]).contains("feat"),
        "a refused push must not create the branch"
    );
    let tracking =
        fx.r.git(&["rev-parse", "--verify", "-q", "refs/remotes/origin/feat"]);
    assert!(!tracking.status.success(), "tracking ref must stay absent");
}

#[test]
fn a_non_fast_forward_needs_force_and_force_leases_on_what_it_read() {
    let fx = fixture("feat");
    assert_eq!(land(&fx, &[]).code, 0);
    // Rewrite the branch: the push is no longer a fast-forward.
    fx.r.git(&[
        "commit",
        "-q",
        "--no-verify",
        "--amend",
        "-m",
        "work, reworded",
    ]);
    let refused = land(&fx, &["--json"]);
    assert_eq!(refused.code, 1, "{}", refused.stderr);
    assert_eq!(status_of(&refused), "refused");
    assert_ne!(tip(&fx, "feat"), head(&fx));

    let forced = land(&fx, &["--force", "--json"]);
    assert_eq!(forced.code, 0, "{}", forced.stderr);
    assert_eq!(tip(&fx, "feat"), head(&fx));
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
fn an_unreachable_remote_is_exit_3_from_the_probe_within_its_budget() {
    let fx = fixture("feat");
    fx.r.git(&["remote", "set-url", "origin", "http://127.0.0.1:1/x.git"]);
    let started = std::time::Instant::now();
    let ran = land(&fx, &["--json"]);
    assert_eq!(ran.code, 3, "{}", ran.stderr);
    assert_eq!(status_of(&ran), "unreachable");
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
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
fn the_push_url_is_the_one_that_is_verified() {
    let fx = fixture("feat");
    // The fetch URL points nowhere; only `pushurl` is real.
    fx.r.git(&["remote", "set-url", "origin", "http://127.0.0.1:1/x.git"]);
    fx.r.git(&[
        "config",
        "remote.origin.pushurl",
        fx.bare.to_str().expect("utf8"),
    ]);
    let ran = land(&fx, &["--json"]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(field(&ran, "push_url"), fx.bare.to_str().expect("utf8"));
    assert_eq!(tip(&fx, "feat"), head(&fx));
}

#[test]
fn two_push_destinations_are_refused() {
    let fx = fixture("feat");
    fx.r.git(&[
        "config",
        "--add",
        "remote.origin.pushurl",
        fx.bare.to_str().expect("utf8"),
    ]);
    fx.r.git(&[
        "config",
        "--add",
        "remote.origin.pushurl",
        "/nowhere/else.git",
    ]);
    let ran = land(&fx, &["--json"]);
    assert_eq!(ran.code, 1, "{}", ran.stderr);
    assert_eq!(status_of(&ran), "refused");
    assert!(ran.stderr.contains("push destinations"), "{}", ran.stderr);
}

#[test]
fn main_and_a_dirty_tree_are_refused_before_any_push() {
    let fx = fixture("feat");
    fx.r.write("b.txt", "edited\n");
    let dirty = land(&fx, &["--json"]);
    assert_eq!(dirty.code, 1);
    assert_eq!(status_of(&dirty), "refused");
    assert!(dirty.stderr.contains("git stash"), "{}", dirty.stderr);

    fx.r.git(&["checkout", "-q", "--", "b.txt"]);
    fx.r.git(&["switch", "-q", "main"]);
    let on_main = land(&fx, &["--json"]);
    assert_eq!(on_main.code, 1);
    assert!(
        on_main.stderr.contains("git switch -c"),
        "{}",
        on_main.stderr
    );
}

#[test]
fn a_usage_error_is_exit_2_with_a_status_on_stdout() {
    let fx = fixture("feat");
    let bogus = land(&fx, &["--bogus"]);
    assert_eq!(bogus.code, 2);
    let json = land(&fx, &["--bogus", "--json"]);
    assert_eq!(json.code, 2);
    assert_eq!(status_of(&json), "usage");
    let extra = land(&fx, &["origin", "--json"]);
    assert_eq!(extra.code, 2);
    assert_eq!(status_of(&extra), "usage");
}

#[test]
fn the_verdict_line_fits_eighty_columns_for_a_long_branch() {
    let branch = format!("feature/{}", "x".repeat(112));
    let fx = fixture(&branch);
    let ran = land(&fx, &[]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    let last = ran.stderr.lines().last().expect("a verdict line");
    assert!(last.chars().count() <= 80, "{last}");
    assert!(last.starts_with("landed"), "{last}");
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
    let tracking =
        fx.r.git(&["rev-parse", "--verify", "-q", "refs/remotes/origin/feat"]);
    assert!(
        !tracking.status.success(),
        "an unverified landing moves no tracking ref"
    );
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

#[test]
fn a_renamed_upstream_is_the_branch_that_is_pushed_leased_and_tracked() {
    let fx = fixture("feat");
    fx.r.git(&["config", "branch.feat.remote", "origin"]);
    fx.r.git(&["config", "branch.feat.merge", "refs/heads/feature-x"]);
    let ran = land(&fx, &["--json"]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(tip(&fx, "feature-x"), head(&fx));
    assert!(
        !plain_git(&fx.bare, &["branch", "--list", "feat"]).contains("feat"),
        "the local name must not be created on the remote"
    );
    assert_eq!(
        plain_git(&fx.r.dir, &["rev-parse", "refs/remotes/origin/feature-x"]),
        head(&fx)
    );
    // A forced rewrite leases on the REMOTE name.
    fx.r.git(&["commit", "-q", "--no-verify", "--amend", "-m", "reworded"]);
    let forced = land(&fx, &["--force", "--json"]);
    assert_eq!(forced.code, 0, "{}", forced.stderr);
    assert_eq!(tip(&fx, "feature-x"), head(&fx));
}

/// Every receipt file of the fixture, parsed as text.
fn receipts(fx: &Fx) -> Vec<String> {
    let dir = fx.r.path(".git/amont/land");
    let Ok(read) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    read.flatten()
        .map(|e| std::fs::read_to_string(e.path()).expect("receipt"))
        .collect()
}

fn receipt_field(receipt: &str, key: &str) -> String {
    let marker = format!("\"{key}\":\"");
    let i = receipt.find(&marker).expect("receipt field") + marker.len();
    receipt[i..].split('"').next().unwrap_or("").to_string()
}

fn remote_ref_exists(fx: &Fx, name: &str) -> bool {
    !plain_git(&fx.r.dir, &["for-each-ref", name]).is_empty()
}

#[test]
fn a_dry_run_on_a_branch_with_no_upstream_would_land_and_touches_nothing() {
    let fx = fixture("feat");
    let ran = land(&fx, &["--dry-run", "--json"]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(status_of(&ran), "would-land");
    assert_eq!(field(&ran, "dst"), "refs/heads/feat");
    assert_eq!(field(&ran, "pushed"), head(&fx));
    assert_eq!(field(&ran, "branch"), "feat");
    assert_eq!(field(&ran, "remote"), "origin");
    assert_eq!(field(&ran, "push_url"), fx.bare.to_str().expect("utf8"));
    assert!(ran.stdout.contains("\"retried\":false"), "{}", ran.stdout);
    assert!(ran.stdout.contains("\"lease\":false"), "{}", ran.stdout);
    assert!(
        field(&ran, "receipt_dir").ends_with("amont/land")
            || field(&ran, "receipt_dir").ends_with("amont\\land"),
        "{}",
        ran.stdout
    );
    let last = ran.stderr.lines().last().expect("a verdict line");
    assert!(last.starts_with("would-land"), "{last}");
    assert!(
        !plain_git(&fx.bare, &["branch", "--list", "feat"]).contains("feat"),
        "a dry run must not create the branch"
    );
    assert!(!remote_ref_exists(&fx, "refs/remotes/origin/feat"));
    assert!(receipts(&fx).is_empty(), "a dry run writes no receipt");
}

#[test]
fn a_dry_run_with_a_renamed_upstream_names_the_remote_branch() {
    let fx = fixture("feat");
    fx.r.git(&["config", "branch.feat.remote", "origin"]);
    fx.r.git(&["config", "branch.feat.merge", "refs/heads/feature-x"]);
    let ran = land(&fx, &["--dry-run", "--json"]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(status_of(&ran), "would-land");
    assert_eq!(field(&ran, "dst"), "refs/heads/feature-x");
    assert!(!remote_ref_exists(&fx, "refs/remotes/origin/feature-x"));
    assert!(receipts(&fx).is_empty());
}

#[test]
fn a_dry_run_with_force_reports_the_lease() {
    let fx = fixture("feat");
    let ran = land(&fx, &["--dry-run", "--force", "--json"]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    assert_eq!(status_of(&ran), "would-land");
    assert!(ran.stdout.contains("\"lease\":true"), "{}", ran.stdout);
}

#[test]
fn a_dry_run_is_refused_on_main_a_dirty_tree_and_two_push_urls() {
    let fx = fixture("feat");
    fx.r.write("b.txt", "edited\n");
    let dirty = land(&fx, &["--dry-run", "--json"]);
    assert_eq!(dirty.code, 1, "{}", dirty.stderr);
    assert_eq!(status_of(&dirty), "refused");
    assert!(dirty.stderr.contains("git stash"), "{}", dirty.stderr);
    fx.r.git(&["checkout", "-q", "--", "b.txt"]);

    fx.r.git(&[
        "config",
        "--add",
        "remote.origin.pushurl",
        fx.bare.to_str().expect("utf8"),
    ]);
    fx.r.git(&[
        "config",
        "--add",
        "remote.origin.pushurl",
        "/nowhere/else.git",
    ]);
    let two = land(&fx, &["--dry-run", "--json"]);
    assert_eq!(two.code, 1, "{}", two.stderr);
    assert_eq!(status_of(&two), "refused");
    assert!(two.stderr.contains("push destinations"), "{}", two.stderr);
    fx.r.git(&["config", "--unset-all", "remote.origin.pushurl"]);

    fx.r.git(&["switch", "-q", "main"]);
    let main_before = tip(&fx, "main");
    let on_main = land(&fx, &["--dry-run", "--json"]);
    assert_eq!(on_main.code, 1, "{}", on_main.stderr);
    assert_eq!(status_of(&on_main), "refused");
    assert!(
        on_main.stderr.contains("git switch -c"),
        "{}",
        on_main.stderr
    );
    let last = on_main.stderr.lines().last().expect("a verdict line");
    assert!(last.starts_with("refused"), "{last}");

    assert_eq!(tip(&fx, "main"), main_before);
    assert!(!plain_git(&fx.bare, &["branch", "--list", "feat"]).contains("feat"));
    assert!(!remote_ref_exists(&fx, "refs/remotes/origin/feat"));
    assert!(receipts(&fx).is_empty(), "a dry run writes no receipt");
}

#[test]
fn a_real_land_writes_one_receipt_naming_what_it_pushed() {
    let fx = fixture("feat");
    let ran = land(&fx, &["--json"]);
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    let all = receipts(&fx);
    assert_eq!(all.len(), 1, "{all:?}");
    let r = &all[0];
    assert_eq!(receipt_field(r, "status"), "landed");
    assert_eq!(receipt_field(r, "pushed"), tip(&fx, "feat"));
    assert_eq!(receipt_field(r, "branch"), "feat");
    assert_eq!(receipt_field(r, "dst"), "refs/heads/feat");
    assert!(r.contains("\"retried\":false"), "{r}");
    assert!(
        r.contains("\"started\":") && r.contains("\"ended\":"),
        "{r}"
    );
    // The dry run's receipt_dir is where that receipt lives.
    let dry = land(&fx, &["--dry-run", "--json"]);
    let dir = PathBuf::from(field(&dry, "receipt_dir"));
    assert_eq!(std::fs::read_dir(&dir).expect("dir").count(), 1);
}

#[test]
fn a_refused_land_writes_a_refused_receipt() {
    let fx = fixture("feat");
    fx.r.write("b.txt", "edited\n");
    let ran = land(&fx, &["--json"]);
    assert_eq!(ran.code, 1, "{}", ran.stderr);
    let all = receipts(&fx);
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(receipt_field(&all[0], "status"), "refused");
    assert_eq!(receipt_field(&all[0], "branch"), "feat");
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
