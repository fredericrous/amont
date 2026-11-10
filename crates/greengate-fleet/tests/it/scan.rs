//! Scanner behaviour, driven through the real binary.
//!
//! The cases that matter are the ones where the old shell sweep was silent: a
//! root that is wrong, a depth that is too shallow, a directory that cannot be
//! read. Each has to be distinguishable from "the fleet is clean", which is the
//! single failure this tool exists to prevent.

use crate::common::{bin, shim_for, DISPATCHERS};
use std::path::{Path, PathBuf};
use std::process::Command;

struct Tree(PathBuf);

impl Tree {
    fn new(name: &str) -> Self {
        let d = std::env::temp_dir().join(format!("fleet-scan-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("mkdir");
        Tree(d)
    }
    /// A repo whose hooks dispatch to the binary, i.e. one of ours.
    fn managed_repo(&self, rel: &str) -> &Self {
        let hooks = self.0.join(rel).join(".git/hooks");
        std::fs::create_dir_all(&hooks).expect("mkdir");
        std::fs::write(
            hooks.join("pre-commit"),
            "#!/bin/sh\n# git-templates hook shim.\nexec \"$BIN\" --hooks-dir \"$(dirname \"$0\")\" pre-commit \"$@\"\n",
        )
        .expect("write");
        self
    }
    /// A REAL repository (`managed_repo` only fakes the
    /// directory shape) whose `core.hooksPath` points somewhere other than
    /// `.git/hooks`, with a managed shim living at that redirected location
    /// and nothing at the default one.
    fn custom_hooks_path_repo(&self, rel: &str, hooks_rel: &str) -> &Self {
        let repo = self.0.join(rel);
        std::fs::create_dir_all(&repo).expect("mkdir");
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .expect("git");
        };
        git(&["init", "-q", "--template=", "."]);
        git(&["config", "core.hooksPath", hooks_rel]);
        let hooks = repo.join(hooks_rel);
        std::fs::create_dir_all(&hooks).expect("mkdir");
        std::fs::write(
            hooks.join("pre-commit"),
            "#!/bin/sh\n# git-templates hook shim.\nexec \"$BIN\" --hooks-dir \"$(dirname \"$0\")\" pre-commit \"$@\"\n",
        )
        .expect("write");
        self
    }
    /// `core.hooksPath` set via an INCLUDED file rather than directly in
    /// `.git/config` — the case a local-text-only shortcut would miss, since
    /// the literal string `hooksPath` never appears in `.git/config` itself.
    fn hooks_path_via_include_repo(&self, rel: &str, hooks_rel: &str) -> &Self {
        let repo = self.0.join(rel);
        std::fs::create_dir_all(&repo).expect("mkdir");
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .expect("git");
        };
        git(&["init", "-q", "--template=", "."]);
        std::fs::write(
            repo.join("shared.gitconfig"),
            format!("[core]\n\thooksPath = {hooks_rel}\n"),
        )
        .expect("write shared config");
        git(&["config", "--add", "include.path", "../shared.gitconfig"]);
        let hooks = repo.join(hooks_rel);
        std::fs::create_dir_all(&hooks).expect("mkdir");
        std::fs::write(
            hooks.join("pre-commit"),
            "#!/bin/sh\n# git-templates hook shim.\nexec \"$BIN\" --hooks-dir \"$(dirname \"$0\")\" pre-commit \"$@\"\n",
        )
        .expect("write");
        self
    }
    /// A real repository with one commit, ready to be a superproject, a
    /// submodule source or a worktree host.
    fn real_repo(&self, rel: &str) -> PathBuf {
        let repo = self.0.join(rel);
        std::fs::create_dir_all(&repo).expect("mkdir");
        git(&repo, &["init", "-q", "--template=", "."]);
        git(&repo, &["config", "user.email", "t@t"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("seed.txt"), "x\n").expect("write");
        git(&repo, &["add", "seed.txt"]);
        git(&repo, &["commit", "-q", "--no-verify", "-m", "chore: seed"]);
        repo
    }
    fn dir(&self, rel: &str) -> &Self {
        std::fs::create_dir_all(self.0.join(rel)).expect("mkdir");
        self
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        // Cloning a submodule from a local path needs this since git 2.38's
        // CVE-2022-39253 fix; without it `submodule add` fails and the fixture
        // silently builds a repository with no submodule in it.
        .args(["-c", "protocol.file.allow=always"])
        // And an EMPTY template, because `submodule add` clones and a clone
        // honours `init.templateDir`. Whoever runs this suite very likely has
        // that pointing at THIS project's hook templates — so the submodule
        // arrived pre-installed and the fixture asserted "unmanaged" against a
        // repository that was, in fact, managed. A fixture must not depend on
        // the machine's git config, least of all on this project's own.
        .args(["-c", "init.templateDir="])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

struct Run {
    code: i32,
    stdout: String,
}

fn run(args: &[&str]) -> Run {
    let out = Command::new(bin()).args(args).output().expect("run");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
    }
}

fn json(args: &[&str]) -> serde_json::Value {
    let mut a = args.to_vec();
    a.push("--json");
    serde_json::from_str(&run(&a).stdout).expect("valid json")
}

/// THE case. A wrong root must be loud in three independent ways: a non-zero
/// exit, the words "SCAN FAILURE", and counters that show nothing was examined.
/// Any one of them alone could be missed.
#[test]
fn a_wrong_root_is_a_failure_not_an_empty_fleet() {
    let t = Tree::new("empty");
    t.dir("nothing/here");
    let r = run(&["--root", t.path().to_str().unwrap()]);
    assert_eq!(r.code, 1, "an empty scan must not exit 0");
    assert!(
        r.stdout.contains("SCAN FAILURE"),
        "must say so in words: {}",
        r.stdout
    );
    assert!(
        r.stdout.contains("--root") && r.stdout.contains("--depth"),
        "must name the two things worth checking: {}",
        r.stdout
    );
    let v = json(&["--root", t.path().to_str().unwrap()]);
    assert_eq!(v["git_dirs_found"], 0);
    assert!(v["dirs_visited"].as_u64().unwrap() > 0, "it did look");
}

/// A depth too shallow to reach the repos is the same failure wearing a
/// different hat, and it is the one that produced `0 copies / 0 distinct`.
#[test]
fn a_too_shallow_depth_finds_nothing_and_says_so() {
    let t = Tree::new("deep");
    t.managed_repo("one/two/three/four");
    let deep = json(&["--root", t.path().to_str().unwrap(), "--depth", "6"]);
    assert_eq!(deep["git_dirs_found"], 1, "reachable at depth 6");

    let r = run(&["--root", t.path().to_str().unwrap(), "--depth", "1"]);
    assert_eq!(r.code, 1);
    assert!(r.stdout.contains("SCAN FAILURE"), "{}", r.stdout);
    assert!(
        r.stdout.contains("currently: 1"),
        "must show the depth it used: {}",
        r.stdout
    );
}

#[test]
fn a_root_that_does_not_exist_is_rejected_before_scanning() {
    let r = run(&["--root", "/definitely/not/here"]);
    assert_eq!(r.code, 2, "usage error, distinct from an empty scan");
}

/// A shim installed by `make install` must classify as OK, not drifted — the
/// failure that would make the tool cry wolf on all 96 repos.
#[test]
fn a_baked_shim_is_ok_and_a_hand_edited_one_is_not() {
    let t = Tree::new("baked");
    let hooks = t.path().join("r/.git/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    for n in DISPATCHERS {
        std::fs::write(hooks.join(n), shim_for("/opt/amont")).unwrap();
    }
    let v = json(&["--root", t.path().to_str().unwrap(), "--depth", "3"]);
    let r = &v["repos"][0];
    assert!(
        r["shims"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["state"] == "ok"),
        "baked shims must not read as drift: {:?}",
        r["shims"]
    );
    assert_eq!(
        r["baked"]["bake"], "stale",
        "and /opt is not where we install"
    );

    std::fs::write(hooks.join("pre-push"), "#!/bin/sh\necho tampered\n").unwrap();
    let v = json(&["--root", t.path().to_str().unwrap(), "--depth", "3"]);
    let states: Vec<&str> = v["repos"][0]["shims"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["state"].as_str().unwrap())
        .collect();
    assert!(
        states.contains(&"drifted"),
        "real drift must show: {states:?}"
    );
}

/// The husky case, at scan level. `.husky/_` is inside the working tree, so
/// every containment test the scanner had said "in" — and it reported eleven
/// repositories as managed-but-drifted when what was true is that amont was not
/// running in any of them.
///
/// A hook manager that REGENERATES its directory is the distinguishing fact, and
/// `redirect_is_hostile` reads it off the destination. So this must report
/// `redirected`, `hostile`, and NOT managed.
#[test]
fn a_husky_hooks_path_is_reported_as_hostile_not_managed() {
    let t = Tree::new("hookspath-husky");
    t.custom_hooks_path_repo("redirected", ".husky/_");
    let v = json(&["--root", t.path().to_str().unwrap()]);
    let repo = &v["repos"][0];
    assert_eq!(repo["hooks_dir"]["where"], "redirected", "{repo}");
    assert_eq!(repo["hooks_dir"]["hostile"], true, "{repo}");
    assert_eq!(
        repo["managed"], false,
        "a repository whose dispatch husky owns is not managed by us: {repo}"
    );
}

/// `core.hooksPath` reaching a repo through an `include`, not a literal key
/// in its own `.git/config` — a local-file-only shortcut would see nothing
/// to act on here and fall back to `.git/hooks`, wrongly.
#[test]
fn core_hooks_path_via_an_include_is_still_honoured() {
    let t = Tree::new("hookspath-include");
    t.hooks_path_via_include_repo("redirected", "tooling/hooks");
    let v = json(&["--root", t.path().to_str().unwrap()]);
    let repo = &v["repos"][0];
    assert_eq!(repo["managed"], true, "{repo}");
    assert!(!t.path().join("redirected/.git/hooks").exists());
}

/// THE invisibility bug. A submodule's `.git` is a FILE holding `gitdir: …`,
/// and the walk tested `path.is_dir()` before it looked at the name — so every
/// submodule on the machine was not scanned, not installed into, and NOT
/// COUNTED AS UNCOVERED. The dashboard reported a clean fleet while commits
/// inside a submodule ran no checks at all, which is the same failure the
/// scanner's module doc opens with, one file type along.
///
/// And it must be installable, at the resolved location: a submodule's hooks
/// live in the SUPERPROJECT's `.git/modules/<name>/hooks`, so an install that
/// wrote to `<super>/sub/.git/hooks` would create a directory git never reads
/// and report success.
#[test]
fn a_submodule_is_found_counted_and_installed_at_its_real_hooks_dir() {
    let t = Tree::new("submodule");
    let sup = t.real_repo("sup");
    let src = t.real_repo("src");
    git(
        &sup,
        &["submodule", "add", "-q", src.to_str().unwrap(), "sub"],
    );
    git(&sup, &["commit", "-q", "--no-verify", "-m", "chore: sub"]);
    assert!(
        sup.join("sub/.git").is_file(),
        "fixture: a submodule's .git must be a FILE, not a directory"
    );

    let v = json(&["--root", t.path().to_str().unwrap()]);
    let paths: Vec<&str> = v["repos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();
    let sub_rel = PathBuf::from("sup").join("sub");
    assert!(
        paths.contains(&sub_rel.to_string_lossy().as_ref()),
        "the submodule was invisible to the scan: {paths:?}"
    );
    assert_eq!(
        v["git_dirs_found"], 3,
        "sup, src and the submodule — a `.git` file counts: {v}"
    );
    assert_eq!(
        v["unmanaged_seen"], 3,
        "and an invisible repo cannot be counted as uncovered"
    );

    // Installing must land where git will actually read it.
    let binary = t.path().join("fake-amont");
    std::fs::write(&binary, "#!/bin/sh\nexit 0\n").unwrap();
    let out = Command::new(bin())
        .args(["install", "--root"])
        .arg(t.path())
        .arg("--binary")
        .arg(&binary)
        .output()
        .expect("install");
    assert!(out.status.success(), "{out:?}");
    let real = sup.join(".git/modules/sub/hooks/pre-commit");
    assert!(
        real.is_file(),
        "the submodule's shim must land in .git/modules/sub/hooks, not beside its .git file:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !sup.join("sub/.git/hooks").exists(),
        "and nothing may be created next to the .git FILE"
    );
}

/// A linked worktree reaches the SAME hooks directory as its main repo. Once
/// `.git` files are recognised, both show up in `repos` — so exactly one of them
/// must own the hooks, or `fix --apply` writes four files twice and reports
/// eight, which is the shape of the `192 removals across 96 repos` number this
/// crate's module doc opens with.
#[test]
fn a_linked_worktree_shares_its_hooks_rather_than_doubling_them() {
    let t = Tree::new("worktree");
    let main = t.real_repo("main");
    git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            t.path().join("wt").to_str().unwrap(),
            "-b",
            "side",
        ],
    );

    let v = json(&["--root", t.path().to_str().unwrap()]);
    let repos = v["repos"].as_array().unwrap();
    assert_eq!(repos.len(), 2, "both are repositories: {v}");
    let shared: Vec<&serde_json::Value> = repos
        .iter()
        .filter(|r| !r["shares_hooks_with"].is_null())
        .collect();
    assert_eq!(
        shared.len(),
        1,
        "exactly one of the pair must defer to the other: {v}"
    );
    assert_eq!(
        shared[0]["shares_hooks_with"], "main",
        "and it must name the repository that owns the hooks: {}",
        shared[0]
    );

    // The plan for the deferring one is empty — not refused, because nothing is
    // wrong with it.
    let binary = t.path().join("fake-amont");
    std::fs::write(&binary, "#!/bin/sh\nexit 0\n").unwrap();
    let plans = json(&[
        "fix",
        "--root",
        t.path().to_str().unwrap(),
        "--binary",
        binary.to_str().unwrap(),
    ]);
    let deferring = plans
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["repo"] == shared[0]["path"])
        .expect("planned");
    assert!(
        deferring["write"].as_array().unwrap().is_empty()
            && deferring["remove"].as_array().unwrap().is_empty()
            && deferring["refuse"].as_array().unwrap().is_empty(),
        "a repo covered by its sibling plans nothing at all: {deferring}"
    );
}

/// `core.hooksPath` may be an ABSOLUTE path anywhere on the disk, and
/// `Intent::Activate` used to `create_dir_all` whatever came back and write four
/// 0o755 files into it. So a scanned repository could name any directory it
/// liked and a fleet-wide install would create and populate it.
///
/// The assertion that matters is the last one: after a full `fix --apply`, the
/// directory the repository named STILL DOES NOT EXIST.
#[test]
fn a_hooks_path_outside_the_repo_is_reported_and_never_created() {
    let t = Tree::new("hookspath-outside");
    let repo = t.real_repo("redirected");
    let elsewhere = t.path().join(format!("elsewhere-{}", std::process::id()));
    git(
        &repo,
        &["config", "core.hooksPath", elsewhere.to_str().unwrap()],
    );

    let v = json(&["--root", t.path().to_str().unwrap()]);
    let r = &v["repos"][0];
    assert_eq!(r["hooks_dir"]["where"], "outside", "{r}");
    assert_eq!(v["hooks_outside_seen"], 1, "and it must be counted: {v}");
    assert_eq!(
        r["managed"], false,
        "a repo we will not look inside is never managed: {r}"
    );

    let binary = t.path().join("fake-amont");
    std::fs::write(&binary, "#!/bin/sh\nexit 0\n").unwrap();
    let out = Command::new(bin())
        .args(["fix", "--apply", "--root"])
        .arg(t.path())
        .arg("--binary")
        .arg(&binary)
        .output()
        .expect("apply");
    assert!(
        !elsewhere.exists(),
        "fix --apply created a directory a scanned repository named:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );

    // Install is the mode that used to create it, so it is checked separately.
    let out = Command::new(bin())
        .args(["install", "--root"])
        .arg(t.path())
        .arg("--binary")
        .arg(&binary)
        .output()
        .expect("install");
    assert!(
        !elsewhere.exists(),
        "install created a directory a scanned repository named:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// A dispatcher that is a SYMLINK read as a perfectly healthy shim, because
/// `read_to_string` follows links and reported the target's bytes. Nothing in
/// the dashboard or the plan could see that a write there lands somewhere else —
/// which is the verified incident: four tracked files rewritten, reported as
/// "4 written".
#[cfg(unix)]
#[test]
fn a_symlinked_dispatcher_is_reported_as_a_symlink_not_as_ok() {
    let t = Tree::new("symlinked-dispatcher");
    let hooks = t.path().join("r/.git/hooks");
    let shared = t.path().join("r/devhooks");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::write(shared.join("pre-commit"), shim_for("/opt/amont")).unwrap();
    std::os::unix::fs::symlink(shared.join("pre-commit"), hooks.join("pre-commit")).unwrap();

    let v = json(&["--root", t.path().to_str().unwrap()]);
    let r = &v["repos"][0];
    let states: Vec<&str> = r["shims"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["state"].as_str().unwrap())
        .collect();
    assert!(
        states.contains(&"symlink"),
        "a linked dispatcher must not read as a healthy shim: {states:?}"
    );
    // The deliberate consequence, stated: a repo whose dispatchers are links no
    // longer counts as managed, so `fix` reports it rather than writing through
    // the link.
    assert_eq!(r["managed"], false, "{r}");
}

/// A compiled hook — somebody's Go binary at `.git/hooks/pre-commit`, a
/// perfectly ordinary thing to have — is not valid UTF-8. `read_to_string`
/// returned `Err`, and every predicate in this crate answered that with
/// `unwrap_or(false)`: not ours for `is_managed`, and `Missing` for the shim
/// state, which is the value that makes `fix` decide to WRITE one.
#[test]
fn a_non_utf8_hook_is_never_ours_and_never_missing() {
    let t = Tree::new("binary-hook");
    let hooks = t.path().join("r/.git/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::write(
        hooks.join("pre-commit"),
        [0x7f, b'E', b'L', b'F', 0x02, 0x01, 0xff, 0xfe],
    )
    .unwrap();
    std::fs::write(
        hooks.join("pre-commit-compiled"),
        [0x7f, b'E', b'L', b'F', 0x02, 0x01, 0xff, 0xfe],
    )
    .unwrap();

    let v = json(&["--root", t.path().to_str().unwrap()]);
    let r = &v["repos"][0];
    assert_eq!(r["managed"], false, "a binary hook is not one of ours: {r}");
    // Index 3: shims sit in DISPATCHERS order, and `pre-commit` follows
    // `commit-msg`, `post-commit` and `post-rewrite`.
    assert_eq!(
        r["shims"][3]["state"], "unreadable",
        "and it is not MISSING, which is what makes fix write: {r}"
    );
    assert_eq!(
        r["stale_ours"].as_array().map(Vec::len),
        Some(0),
        "a binary must never reach the removal list: {r}"
    );
}

/// A fleet with no ledgers anywhere aggregates to zero.
#[test]
fn a_clean_fleet_has_zero_bypasses_in_the_json() {
    let t = Tree::new("bypasses-clean");
    t.managed_repo("a");
    let v = json(&["--root", t.path().to_str().unwrap()]);
    assert_eq!(v["repos"][0]["bypasses"]["total"], 0);
    assert_eq!(v["bypassed_commits"], 0);
}
