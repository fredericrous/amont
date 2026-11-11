//! `fix --migrate`: greengate 4.0's committed names, swept across the fleet.
//!
//! Each repository that still commits a greengate 3.x name (`amont.conf`,
//! `amont.pack`, the old AGENTS.md / CLAUDE.md markers) gets
//! `greengate migrate --tracked`'s rename, staged, then ONE commit made
//! through the repository's own hooks, so the gate judges it like any other.
//! Nothing is pushed: a commit is what a person reviews and lands.
//!
//! A repository with uncommitted changes to tracked files is skipped, never
//! stashed: the commit would carry somebody's work in progress, or the
//! rename would be mixed into it.

use std::path::{Path, PathBuf};

use greengate_runtime::migrate::{old, system_git, tracked};
use serde::Serialize;

/// The sweep commit's subject.
/// Short, so a commit-msg hook that prefixes it stays under 50 columns.
pub const SUBJECT: &str = "chore: rename to greengate names";

/// The generated files that may carry the old marker pair.
const MARKED: &[&str] = &["AGENTS.md", "CLAUDE.md"];

/// Does `repo_abs` still commit a 3.x name? Read from the work tree alone:
/// three `stat`s and two small reads, no spawn.
pub fn needs(repo_abs: &Path) -> bool {
    repo_abs.join(old::MANIFEST).exists()
        || repo_abs.join(old::PACK_FILE).exists()
        || MARKED.iter().any(|name| {
            std::fs::read_to_string(repo_abs.join(name))
                .is_ok_and(|text| text.contains(old::AGENTS_START))
        })
}

/// What happened in one repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    /// The rename was committed; `leftovers` tracked lines still name an old
    /// name and need a person's reading.
    Committed { commit: String, leftovers: usize },
    /// Nothing was changed, and why.
    Skipped { why: String },
    /// The rename was staged but not committed (a hook refused it, say):
    /// it stays staged for a person to look at.
    Failed { why: String },
}

/// One repository's line in the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    pub repo: PathBuf,
    #[serde(flatten)]
    pub outcome: Outcome,
}

fn git(repo_abs: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    std::process::Command::new("git")
        .arg("-C")
        .arg(repo_abs)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))
}

/// The branch checked out at `repo_abs` when it is the default one: the
/// remote's `HEAD` when known, else `main` or `master`. A detached HEAD is
/// no branch, so it is not the default one.
fn on_default_branch(repo_abs: &Path) -> Option<String> {
    let stdout = |args: &[&str]| -> Option<String> {
        let out = git(repo_abs, args).ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let branch = stdout(&["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    let default = stdout(&[
        "symbolic-ref",
        "--quiet",
        "--short",
        "refs/remotes/origin/HEAD",
    ])
    .and_then(|d| d.strip_prefix("origin/").map(str::to_string));
    let is_default = match default {
        Some(d) => branch == d,
        None => branch == "main" || branch == "master",
    };
    is_default.then_some(branch)
}

/// The last lines of a failed command's output, enough to say why.
fn tail(out: &std::process::Output) -> String {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(5)..].join("\n")
}

/// Rename, stage and commit in `repo_abs`, the top of its work tree.
pub fn run(repo_abs: &Path) -> Outcome {
    if let Some(branch) = on_default_branch(repo_abs) {
        return Outcome::Skipped {
            why: format!(
                "on {branch}, the default branch; run the sweep in a task branch or \
                 worktree, so the commit lands through a pull request"
            ),
        };
    }
    let dirty = match git(repo_abs, &["status", "--porcelain", "--untracked-files=no"]) {
        Ok(out) if out.status.success() => !out.stdout.is_empty(),
        Ok(out) => return Outcome::Skipped { why: tail(&out) },
        Err(why) => return Outcome::Skipped { why },
    };
    if dirty {
        return Outcome::Skipped {
            why: "uncommitted changes to tracked files; commit or put them aside first".to_string(),
        };
    }
    let staged = match tracked::migrate_tracked(repo_abs, &system_git) {
        Ok(staged) => staged,
        Err(why) => {
            return Outcome::Skipped {
                why: why.to_string(),
            }
        }
    };
    if !staged.changed() {
        return Outcome::Skipped {
            why: "nothing committed here carries greengate 3.x's names".to_string(),
        };
    }
    match git(repo_abs, &["commit", "-q", "-m", SUBJECT]) {
        Ok(out) if out.status.success() => {}
        Ok(out) => {
            return Outcome::Failed {
                why: format!(
                    "the commit was refused; the rename stays staged:\n{}",
                    tail(&out)
                ),
            }
        }
        Err(why) => return Outcome::Failed { why },
    }
    match git(repo_abs, &["rev-parse", "--short", "HEAD"]) {
        Ok(out) if out.status.success() => Outcome::Committed {
            commit: String::from_utf8_lossy(&out.stdout).trim().to_string(),
            leftovers: staged.leftover_count,
        },
        Ok(out) => Outcome::Failed { why: tail(&out) },
        Err(why) => Outcome::Failed { why },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "greengate-fleet-migrate-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir); // a fresh dir; absent is fine
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn needs_reads_the_work_tree_alone() {
        let dir = scratch("needs");
        assert!(!needs(&dir));
        std::fs::write(dir.join("AGENTS.md"), "# no block\n").unwrap();
        assert!(!needs(&dir));
        std::fs::write(
            dir.join("AGENTS.md"),
            format!("{}\nx\n{}\n", old::AGENTS_START, old::AGENTS_END),
        )
        .unwrap();
        assert!(needs(&dir));
        std::fs::remove_file(dir.join("AGENTS.md")).unwrap();
        std::fs::write(dir.join(old::MANIFEST), "").unwrap();
        assert!(needs(&dir));
        let _ = std::fs::remove_dir_all(&dir); // best-effort cleanup of a scratch dir
    }
}
