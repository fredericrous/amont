//! pre-push-branch-pattern, ported from tests/pre-push-branch-pattern.test.zsh.
//!
//! Every case now drives the check through a REF LIST on stdin, the way git
//! feeds a real pre-push, because that is what the check reads. It used to ask
//! `rev-parse --abbrev-ref HEAD` — the branch that happens to be checked out —
//! so `git push origin local:refs/heads/other` validated the wrong name and a
//! detached HEAD was judged as the literal string `"HEAD"`.

use crate::common;
use common::{template_hook, Repo};
use std::io::Write;
use std::process::{Command, Stdio};

/// The 40-zero oid git writes for "this ref does not exist yet".
const ZERO: &str = "0000000000000000000000000000000000000000";

/// The hook allows any name when the remote has no branches yet, so a real
/// (bare) origin with one branch is needed or every case short-circuits to a
/// pass. The zsh suite learned this the hard way; the setup is preserved.
fn repo_with_origin() -> Repo {
    let r = Repo::new();
    r.git(&["commit", "-q", "--allow-empty", "--no-verify", "-m", "init"]);
    let origin = r.path(".fake-origin.git");
    r.git(&["init", "-q", "--bare", origin.to_str().unwrap()]);
    r.git(&["remote", "add", "origin", origin.to_str().unwrap()]);
    r.git(&["push", "-q", "--no-verify", "origin", "HEAD:main"]);
    r
}

fn head(r: &Repo) -> String {
    String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string()
}

/// Feed the check one pre-push line and return whether it passed.
fn push_refs(r: &Repo, lines: &str) -> (bool, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-push-branch-pattern")
        .arg("origin")
        .current_dir(&r.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(lines.as_bytes())
        .expect("write");
    let out = child.wait_with_output().expect("wait");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// A brand-new branch: the remote oid is all zeroes.
fn new_branch(r: &Repo, name: &str) -> (bool, String) {
    push_refs(
        r,
        &format!("refs/heads/{name} {} refs/heads/{name} {ZERO}\n", head(r)),
    )
}

#[test]
fn passes_when_the_branch_is_already_on_the_server() {
    let r = repo_with_origin();
    // A non-zero remote oid IS git telling us the branch exists there. This
    // replaces the old `show-branch remotes/origin/<name>` probe, which needed
    // a remote-tracking ref a fresh clone may not have.
    let tip = head(&r);
    let (ok, out) = push_refs(
        &r,
        &format!("refs/heads/off-pattern {tip} refs/heads/off-pattern {tip}\n"),
    );
    assert!(ok, "{out}");
}

/// The name rules, judged for a new branch through the real hook. The
/// prefixes after `feat` are the vocabulary fix (#30): they were rejected
/// while being valid commit types. Dots are allowed under `chore` only.
#[test]
fn each_new_branch_name_is_judged_by_the_rules() {
    let r = repo_with_origin();
    for b in [
        "feat/0-test",
        "docs/x",
        "refactor/y",
        "perf/z",
        "build/w",
        "style/v",
        "revert/u",
        "add/t",
        "remove/s",
        "chore/duro-1.50.50",
    ] {
        let (ok, out) = new_branch(&r, b);
        assert!(ok, "{b}: {out}");
    }
    for b in ["off-pattern", "feat/duro-1.50.50", "duro-1.50.50"] {
        let (ok, out) = new_branch(&r, b);
        assert!(!ok, "{b} was accepted: {out}");
        assert!(out.contains(b), "{b} was not named: {out}");
    }
}

/// The name being CREATED is what is judged, not the one checked out. Under
/// the old `--abbrev-ref HEAD` reading, this case validated `main` — which
/// does not conform either, so it failed for entirely the wrong reason and
/// would have passed a conforming local name pushed onto a garbage remote one.
#[test]
fn a_renamed_push_is_judged_by_its_target_name() {
    let r = repo_with_origin();
    let tip = head(&r);
    let (ok, out) = push_refs(
        &r,
        &format!("refs/heads/feat/fine {tip} refs/heads/off-pattern {ZERO}\n"),
    );
    assert!(
        !ok,
        "it judged the local name, not the one being created:\n{out}"
    );
    assert!(out.contains("off-pattern"), "{out}");
}

/// A multi-ref push names every offender rather than stopping at the first.
#[test]
fn every_offending_ref_is_named() {
    let r = repo_with_origin();
    let tip = head(&r);
    let (ok, out) = push_refs(
        &r,
        &format!(
            "refs/heads/a {tip} refs/heads/bad-one {ZERO}\n\
             refs/heads/b {tip} refs/heads/feat/fine {ZERO}\n\
             refs/heads/c {tip} refs/heads/bad-two {ZERO}\n"
        ),
    );
    assert!(!ok, "{out}");
    assert!(out.contains("bad-one") && out.contains("bad-two"), "{out}");
}

/// A delete carries no name to validate, however unconventional the branch.
#[test]
fn a_delete_is_not_a_name_to_validate() {
    let r = repo_with_origin();
    let (ok, out) = push_refs(
        &r,
        &format!("(delete) {ZERO} refs/heads/off-pattern {ZERO}\n"),
    );
    assert!(ok, "{out}");
}

/// A tag is not a branch.
#[test]
fn a_tag_is_not_a_branch() {
    let r = repo_with_origin();
    let tip = head(&r);
    let (ok, out) = push_refs(
        &r,
        &format!("refs/tags/v1.0.0 {tip} refs/tags/v1.0.0 {ZERO}\n"),
    );
    assert!(ok, "{out}");
}

/// A REAL push from a DETACHED HEAD, through the shipped shim and git itself.
///
/// This is the case that was unpushable. `rev-parse --abbrev-ref HEAD` returns
/// the literal string `"HEAD"` on a detached head, which `conforms` rejects.
/// The old `show-branch remotes/origin/HEAD` short-circuit hid that in a normal
/// clone — but this fixture is `git init --bare` plus `git remote add`, which
/// creates no `refs/remotes/origin/HEAD` at all (as does
/// `git remote set-head --delete`), so `git push origin HEAD:refs/heads/feat/x`
/// was BLOCKED for a perfectly conventional branch name.
#[cfg(unix)]
#[test]
fn a_detached_head_push_is_judged_by_its_target() {
    use std::os::unix::fs::PermissionsExt;

    let r = repo_with_origin();
    assert!(
        !r.path(".git/refs/remotes/origin/HEAD").exists(),
        "fixture: this case needs a repo with no remote HEAD ref"
    );

    let shim = r.path(".git/hooks/pre-push");
    std::fs::create_dir_all(shim.parent().expect("parent")).expect("mkdir");
    std::fs::copy(template_hook("pre-push"), &shim).expect("install the shim");
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    r.git(&["checkout", "-q", "--detach"]);

    let push = |refspec: &str| -> (bool, String) {
        let mut cmd = Command::new("git");
        cmd.args(["push", "origin", refspec])
            .current_dir(&r.dir)
            .stdin(Stdio::null());
        Repo::strip_git_env_impl(&mut cmd);
        // How the shim is pointed at the binary under test; `make test` uses
        // the same escape hatch.
        cmd.env("GIT_HOOKS_BIN", env!("CARGO_BIN_EXE_amont"));
        let out = cmd.output().expect("git push");
        (
            out.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    };

    let (ok, out) = push("HEAD:refs/heads/feat/x");
    assert!(
        ok,
        "a conforming target was blocked from a detached HEAD:\n{out}"
    );

    let (ok, out) = push("HEAD:refs/heads/off-pattern");
    assert!(
        !ok,
        "an off-pattern target was allowed from a detached HEAD:\n{out}"
    );
}
