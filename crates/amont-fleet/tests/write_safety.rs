//! What `amont-fleet` is willing to write over, driven through the real
//! binary.
//!
//! Every case here is one where the tool used to write and now refuses, and all
//! of them share a shape: the guard was asked about the path it was HANDED
//! rather than about the file that would actually be changed.
//!
//! - `.git/hooks/pre-commit` is a symlink into the working tree. The link is
//!   untracked, sits in `.git`, and has nothing alarming about it — so the
//!   tracked guard said yes and `std::fs::write` followed the link and rewrote
//!   a tracked file. Verified: `fix --apply` reported `4 written` and left four
//!   tracked files modified.
//! - git cannot answer whether a path is tracked (`fatal: detected dubious
//!   ownership`, i.e. every repository owned by another uid, i.e. every repo
//!   inside a container bind mount) and the predicate turned that into "not
//!   tracked".
//!
//! The two `plan`-then-apply RACE cases — a repo that stopped being managed,
//! and a dispatcher that became foreign — are unit tests in `src/apply.rs`
//! instead, and deliberately: `fix --apply` computes and applies in one breath,
//! so nothing outside the process can open that window. A CLI-driven version
//! would only prove that the planner and `apply` agree, which is exactly the
//! assumption that let `apply` skip four of its five re-checks for two
//! releases.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_amont-fleet")
}

fn template() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../templates/hooks/pre-commit"
    ))
    .expect("template")
}

fn shim_for(binary: &str) -> String {
    template().replace("__AMONT_BIN__", binary)
}

const DISPATCHERS: [&str; 5] = [
    "commit-msg",
    "post-commit",
    "pre-commit",
    "pre-push",
    "prepare-commit-msg",
];

struct Tree(PathBuf);

impl Tree {
    fn new(name: &str) -> Self {
        let d =
            std::env::temp_dir().join(format!("fleet-writesafety-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("mkdir");
        Tree(d)
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

/// A real repository whose four dispatchers are SYMLINKS into its own working
/// tree, pointing at four TRACKED files under `shared/`.
///
/// This is not a contrived shape. `~/.config/git/git-templates` is a symlink to
/// a checkout in this very project's recommended setup, and a team that wants
/// one reviewable copy of its hooks keeps them in the tree and links
/// `.git/hooks` at them. Both overwrite incidents in this repository's history
/// came through a link like this one.
#[cfg(unix)]
fn repo_with_symlinked_dispatchers(t: &Tree, rel: &str, binary: &str, link: &[&str]) -> PathBuf {
    let repo = t.path().join(rel);
    std::fs::create_dir_all(repo.join("shared")).expect("mkdir");
    git(&repo, &["init", "-q", "--template=", "."]);
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "t"]);
    for n in DISPATCHERS {
        std::fs::write(repo.join("shared").join(n), shim_for(binary)).expect("write");
    }
    git(&repo, &["add", "shared"]);
    git(&repo, &["commit", "-q", "--no-verify", "-m", "chore: seed"]);

    let hooks = repo.join(".git/hooks");
    std::fs::create_dir_all(&hooks).expect("mkdir");
    for n in DISPATCHERS {
        if link.contains(&n) {
            std::os::unix::fs::symlink(repo.join("shared").join(n), hooks.join(n))
                .expect("symlink");
        } else {
            std::fs::write(hooks.join(n), shim_for(binary)).expect("write");
        }
    }
    repo
}

fn fake_binary(t: &Tree) -> PathBuf {
    let b = t.path().join("fake-amont");
    std::fs::write(&b, "#!/bin/sh\nexit 0\n").expect("write");
    b
}

/// THE incident. Four tracked dispatchers under `shared/`, `.git/hooks/*`
/// symlinked at them, `fix --apply` — which reported `4 written` and left all
/// four TRACKED FILES MODIFIED, because `std::fs::write` follows links and the
/// tracked guard had been asked about the link.
///
/// Two halves, and both matter:
///
/// (a) every tracked file is BYTE-IDENTICAL afterwards, which is the property,
///     not "the outcome was not `applied`";
/// (b) when one dispatcher is a link and the rest are ours — so the repository
///     is genuinely managed and the plan genuinely reaches `apply` — the
///     outcome NAMES the symlink and what it points at. A refusal that says
///     only "failed" sends somebody looking at a file for a difference they
///     cannot see.
#[cfg(unix)]
#[test]
fn apply_never_writes_through_a_symlinked_dispatcher() {
    let t = Tree::new("symlink");
    let binary = fake_binary(&t);
    let bin_str = binary.to_str().unwrap();

    // (a) The literal reproduction: all four linked. The shims are baked at a
    // DIFFERENT binary, so every one of the four is a change the tool wants to
    // make — a fixture where nothing needed writing would pass for free.
    let all = repo_with_symlinked_dispatchers(&t, "all", "/opt/other-amont", &DISPATCHERS);
    let before: Vec<String> = DISPATCHERS
        .iter()
        .map(|n| std::fs::read_to_string(all.join("shared").join(n)).expect("read"))
        .collect();

    let out = Command::new(bin())
        .args(["fix", "--apply", "--root"])
        .arg(t.path())
        .arg("--binary")
        .arg(&binary)
        .output()
        .expect("apply");
    for (i, n) in DISPATCHERS.iter().enumerate() {
        assert_eq!(
            std::fs::read_to_string(all.join("shared").join(n))
                .ok()
                .as_deref(),
            Some(before[i].as_str()),
            "apply wrote THROUGH the link and rewrote tracked {n}:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    // (b) One link among three real shims: managed, planned, and stopped by
    // `apply`'s own guard rather than by the planner.
    let one = repo_with_symlinked_dispatchers(&t, "one", "/opt/other-amont", &["commit-msg"]);
    let target = one.join("shared/commit-msg");
    let before = std::fs::read_to_string(&target).expect("read");

    let out = Command::new(bin())
        .args(["fix", "--apply", "--json", "--root"])
        .arg(t.path())
        .arg("--binary")
        .arg(&binary)
        .output()
        .expect("apply");
    let reports: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    let report = reports
        .as_array()
        .expect("array")
        .iter()
        .find(|r| r["repo"] == "one")
        .expect("planned");

    assert_eq!(
        std::fs::read_to_string(&target).ok().as_deref(),
        Some(before.as_str()),
        "a tracked file behind a link was rewritten: {report}"
    );
    assert_eq!(report["outcome"]["outcome"], "failed", "{report}");
    let error = report["outcome"]["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("symlink") && error.contains("shared/commit-msg"),
        "the refusal must name the link AND what it points at: {error:?}"
    );
    // And the link itself survives: refusing is not quietly replacing.
    assert!(
        std::fs::symlink_metadata(one.join(".git/hooks/commit-msg"))
            .expect("stat")
            .file_type()
            .is_symlink(),
        "the link was replaced by a refusal that claimed to touch nothing"
    );
    let _ = bin_str;
}

/// A `git` that will not answer must never read as "not tracked".
///
/// Provoked with a PATH-shimmed `git` that exits 128 with `fatal: detected
/// dubious ownership in repository` for `ls-files --error-unmatch` and passes
/// everything else through to the real git — the exact shape a repository owned
/// by another uid produces on every call, which is the steady state inside any
/// container bind mount.
///
/// The `$PATH` is set on the SPAWNED PROCESS, never with `std::env::set_var`.
/// `set_var` in a `#[test]` changes the environment of the whole process, and
/// rust's harness runs every test in that one process in parallel; the first
/// attempt at a test like this took eleven unrelated tests down with it.
#[cfg(unix)]
#[test]
fn apply_refuses_when_git_cannot_answer_whether_a_path_is_tracked() {
    use std::os::unix::fs::PermissionsExt;

    let real_git = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .expect("a real git to delegate to");

    let t = Tree::new("dubious");
    let binary = fake_binary(&t);

    // A managed repo with one dispatcher missing, so there is a write to stop.
    let hooks = t.path().join("r/.git/hooks");
    std::fs::create_dir_all(&hooks).expect("mkdir");
    for n in DISPATCHERS {
        std::fs::write(hooks.join(n), shim_for(binary.to_str().unwrap())).expect("write");
    }
    std::fs::remove_file(hooks.join("pre-push")).expect("rm");

    let shimdir = t.path().join("gitshim");
    std::fs::create_dir_all(&shimdir).expect("mkdir");
    std::fs::write(
        shimdir.join("git"),
        format!(
            "#!/bin/sh\n\
             for a in \"$@\"; do\n\
             \x20 if [ \"$a\" = \"--error-unmatch\" ]; then\n\
             \x20   echo 'fatal: detected dubious ownership in repository' >&2\n\
             \x20   exit 128\n\
             \x20 fi\n\
             done\n\
             exec {real_git} \"$@\"\n"
        ),
    )
    .expect("write shim");
    std::fs::set_permissions(shimdir.join("git"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");

    let path = format!(
        "{}:{}",
        shimdir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new(bin())
        .args(["fix", "--apply", "--json", "--root"])
        .arg(t.path())
        .arg("--binary")
        .arg(&binary)
        .env("PATH", &path)
        .output()
        .expect("apply");

    assert!(
        !hooks.join("pre-push").exists(),
        "a write went ahead while git could not say whether the path is tracked:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let reports: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    let report = &reports.as_array().expect("array")[0];
    // Refused, not failed: the planner reaches the same guard first and
    // suppresses the whole repository, which is fail-closed one layer earlier.
    // `apply` refuses independently — `a_write_that_became_tracked_is_refused_not_overwritten`
    // proves that half without the planner's help.
    assert_eq!(report["outcome"]["outcome"], "refused", "{report}");

    // And the refusal must be legible. `1 refused` in a summary line cannot be
    // acted on; this one is one `git config` away from being resolved and the
    // output has to say so.
    let human = Command::new(bin())
        .args(["fix", "--apply", "--root"])
        .arg(t.path())
        .arg("--binary")
        .arg(&binary)
        .env("PATH", &path)
        .output()
        .expect("apply");
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(
        text.contains("cannot tell whether") && text.contains("safe.directory"),
        "a refusal must name the reason and the fix:\n{text}"
    );
}
