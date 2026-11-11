//! `fix --migrate` across a root of real repositories: the dry run lists the
//! ones still committing greengate 3.x's names, `--apply` makes exactly one
//! commit in each, and a repository with uncommitted work is skipped
//! untouched (plan "greengate 4.0 renames its data names", Phase 3).

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::common::bin;

const OLD_MANIFEST: &str = "amont.conf";
const OLD_START: &str = "<!-- amont:start -->";
const OLD_END: &str = "<!-- amont:end -->";

fn git(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A repository with `files` committed once.
fn repo(root: &Path, name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "chore/greengate-names", "."]);
    git(&dir, &["config", "user.name", "t"]);
    git(&dir, &["config", "user.email", "t@example.invalid"]);
    git(&dir, &["config", "commit.gpgsign", "false"]);
    for (path, body) in files {
        std::fs::write(dir.join(path), body).unwrap();
    }
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "chore: base"]);
    dir
}

fn fleet(root: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(bin())
        .args(args)
        .arg("--root")
        .arg(root)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("fleet");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn the_sweep_commits_once_per_repository_and_skips_dirty_ones() {
    let root = std::env::temp_dir().join(format!("greengate-fleet-sweep-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root); // a fresh dir; absent is fine
    std::fs::create_dir_all(&root).unwrap();
    let block = format!("{OLD_START}\nblock\n{OLD_END}\n");
    let one = repo(
        &root,
        "one",
        &[(OLD_MANIFEST, "pre-commit  a  *  warn  true\n")],
    );
    let two = repo(&root, "two", &[("AGENTS.md", &block)]);
    let dirty = repo(
        &root,
        "dirty",
        &[(OLD_MANIFEST, "pre-commit  a  *  warn  true\n")],
    );
    std::fs::write(dirty.join(OLD_MANIFEST), "pre-commit  b  *  warn  true\n").unwrap();
    let on_main = repo(
        &root,
        "on-main",
        &[(OLD_MANIFEST, "pre-commit  a  *  warn  true\n")],
    );
    git(&on_main, &["branch", "-m", "main"]);
    let feature = repo(
        &root,
        "feature",
        &[(OLD_MANIFEST, "pre-commit  a  *  warn  true\n")],
    );
    git(&feature, &["branch", "-m", "feat/unrelated"]);
    // A local, untracked manifest is not the repository's to rename.
    let local = repo(&root, "local", &[("README.md", "r\n")]);
    std::fs::write(local.join(OLD_MANIFEST), "pre-commit  a  *  warn  true\n").unwrap();
    let local_block = format!("{OLD_START}\nmine\n{OLD_END}\n");
    std::fs::write(local.join("CLAUDE.md"), &local_block).unwrap();
    let done = repo(
        &root,
        "done",
        &[("greengate.conf", "pre-commit  a  *  warn  true\n")],
    );

    // The dry run lists the five due, and changes nothing.
    let (code, out) = fleet(&root, &["fix", "--migrate"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("5 repositories"), "{out}");
    assert!(!out.contains("done") && !out.contains("local"), "{out}");
    assert!(one.join(OLD_MANIFEST).is_file());

    let (code, out) = fleet(&root, &["fix", "--apply", "--migrate"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("one: committed"), "{out}");
    assert!(out.contains("two: committed"), "{out}");
    assert!(out.contains("dirty: skipped: uncommitted changes"), "{out}");
    assert!(
        out.contains("on-main: skipped: on main, not chore/greengate-names"),
        "{out}"
    );
    assert!(
        out.contains("feature: skipped: on feat/unrelated, not"),
        "{out}"
    );

    for dir in [&one, &two] {
        let log = git(dir, &["log", "--format=%s"]);
        assert_eq!(
            log,
            "chore: rename to greengate names\nchore: base\n",
            "{}",
            dir.display()
        );
        assert!(git(dir, &["status", "--porcelain"]).is_empty());
    }
    assert!(one.join("greengate.conf").is_file() && !one.join(OLD_MANIFEST).exists());
    assert!(std::fs::read_to_string(two.join("AGENTS.md"))
        .unwrap()
        .contains("<!-- greengate:start -->"));
    assert_eq!(git(&dirty, &["log", "--format=%s"]), "chore: base\n");
    assert!(dirty.join(OLD_MANIFEST).is_file());
    assert_eq!(git(&done, &["log", "--format=%s"]), "chore: base\n");
    assert_eq!(git(&on_main, &["log", "--format=%s"]), "chore: base\n");
    assert!(on_main.join(OLD_MANIFEST).is_file());
    assert_eq!(git(&feature, &["log", "--format=%s"]), "chore: base\n");
    assert!(local.join(OLD_MANIFEST).is_file() && !local.join("greengate.conf").exists());
    assert_eq!(
        std::fs::read_to_string(local.join("CLAUDE.md")).unwrap(),
        local_block,
        "an untracked file is never rewritten"
    );

    // A second sweep finds only the three it skipped.
    let (code, out) = fleet(&root, &["fix", "--migrate"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("3 repositories commit"), "{out}");
    let _ = std::fs::remove_dir_all(&root); // best-effort cleanup of a scratch dir
}

/// A commit its own hooks refuse is undone: the repository is left exactly
/// as it was, the run says why and exits 1.
#[cfg(unix)]
#[test]
fn a_refused_commit_is_undone_and_fails_the_sweep() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("greengate-fleet-refused-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root); // a fresh dir; absent is fine
    std::fs::create_dir_all(&root).unwrap();
    let refused = repo(
        &root,
        "refused",
        &[(OLD_MANIFEST, "pre-commit  a  *  warn  true\n")],
    );
    let hook = refused.join(".git/hooks/pre-commit");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, "#!/bin/sh\necho 'no renames today' >&2\nexit 1\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    git(&refused, &["config", "core.hooksPath", ".git/hooks"]);

    let (code, out) = fleet(&root, &["fix", "--apply", "--migrate"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("refused: FAILED"), "{out}");
    assert!(out.contains("no renames today"), "the hook's reason: {out}");
    assert!(out.contains("nothing was kept"), "{out}");
    assert_eq!(git(&refused, &["log", "--format=%s"]), "chore: base\n");
    assert!(git(&refused, &["status", "--porcelain", "--untracked-files=no"]).is_empty());
    assert!(refused.join(OLD_MANIFEST).is_file() && !refused.join("greengate.conf").exists());
    let _ = std::fs::remove_dir_all(&root); // best-effort cleanup of a scratch dir
}

/// A failure half-way through the rename (the manifest already moved, then
/// `.github/attest-inputs` cannot be read: a directory there) is undone the
/// same way: exit 1, the tree as it was. No permission bits involved, so it
/// fails the same way as root.
#[test]
fn a_failure_after_the_rename_started_is_undone() {
    let root = std::env::temp_dir().join(format!("greengate-fleet-halfway-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root); // a fresh dir; absent is fine
                                            // The rename reads `.github/attest-inputs` after it has moved the
                                            // manifest; a directory there makes that read fail, half-way.
    std::fs::create_dir_all(root.join("halfway/.github/attest-inputs")).unwrap();
    let halfway = repo(
        &root,
        "halfway",
        &[
            (OLD_MANIFEST, "pre-commit  a  *  warn  true\n"),
            (".github/attest-inputs/x", "x\n"),
        ],
    );

    let (code, out) = fleet(&root, &["fix", "--apply", "--migrate"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("halfway: FAILED"), "{out}");
    assert!(
        out.contains("could not read") && out.contains("attest-inputs"),
        "it failed at the read after the manifest moved: {out}"
    );
    assert!(out.contains("nothing was kept"), "{out}");
    assert_eq!(git(&halfway, &["log", "--format=%s"]), "chore: base\n");
    assert!(git(&halfway, &["status", "--porcelain", "--untracked-files=no"]).is_empty());
    assert!(halfway.join(OLD_MANIFEST).is_file() && !halfway.join("greengate.conf").exists());
    let _ = std::fs::remove_dir_all(&root); // best-effort cleanup of a scratch dir
}

/// Flags of the hook repair would be dropped silently: refused instead.
#[test]
fn migrate_refuses_the_flags_it_would_ignore() {
    let (code, out) = fleet(&std::env::temp_dir(), &["fix", "--migrate", "--agents-md"]);
    assert_eq!(code, 2, "a usage error: {out}");
    assert!(out.contains("drop --agents-md"), "{out}");
}

#[test]
fn migrate_goes_with_fix_only() {
    let root = std::env::temp_dir();
    let (code, out) = fleet(&root, &["install", "--migrate"]);
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("--migrate goes with fix only"), "{out}");
}
