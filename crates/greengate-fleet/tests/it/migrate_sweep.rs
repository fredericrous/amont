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
    git(&dir, &["init", "-q", "-b", "task", "."]);
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
    let done = repo(
        &root,
        "done",
        &[("greengate.conf", "pre-commit  a  *  warn  true\n")],
    );

    // The dry run lists the three due, and changes nothing.
    let (code, out) = fleet(&root, &["fix", "--migrate"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("4 repositories"), "{out}");
    assert!(!out.contains("done"), "{out}");
    assert!(one.join(OLD_MANIFEST).is_file());

    let (code, out) = fleet(&root, &["fix", "--apply", "--migrate"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("one: committed"), "{out}");
    assert!(out.contains("two: committed"), "{out}");
    assert!(out.contains("dirty: skipped: uncommitted changes"), "{out}");
    assert!(
        out.contains("on-main: skipped: on main, the default branch"),
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

    // A second sweep finds only the two it skipped.
    let (code, out) = fleet(&root, &["fix", "--migrate"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("2 repositories commit"), "{out}");
    let _ = std::fs::remove_dir_all(&root); // best-effort cleanup of a scratch dir
}

#[test]
fn migrate_goes_with_fix_only() {
    let root = std::env::temp_dir();
    let (code, out) = fleet(&root, &["install", "--migrate"]);
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("--migrate goes with fix only"), "{out}");
}
