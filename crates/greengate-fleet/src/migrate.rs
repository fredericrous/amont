//! `fix --migrate`: greengate 4.0's committed names, swept across the fleet.
//!
//! Each repository that still commits a greengate 3.x name (`amont.conf`,
//! `amont.pack`, the old AGENTS.md / CLAUDE.md markers) gets
//! `greengate migrate --tracked`'s rename, staged, then ONE commit made
//! through the repository's own hooks, so the gate judges it like any other.
//! Nothing is pushed: a commit is what a person reviews and lands.
//!
//! The commit goes only on a branch named [`BRANCH`], checked out by whoever
//! runs the sweep (one task worktree per repository): never on a default
//! branch, and never inside somebody's unrelated pull request. A repository
//! with uncommitted changes to tracked files is skipped, never stashed. A
//! sweep that fails half-way is undone, so a repository is either renamed
//! and committed, or left exactly as it was.

use std::path::{Path, PathBuf};

use greengate_runtime::migrate::{system_git, tracked, MigrateError};
use serde::Serialize;

/// The sweep commit's subject.
/// Short, so a commit-msg hook that prefixes it stays under 50 columns.
pub const SUBJECT: &str = "chore: rename to greengate names";

/// The only branch the sweep commits on.
pub const BRANCH: &str = "chore/greengate-names";

/// Does `repo_abs` still commit a 3.x name the rename would change? A
/// repository that cannot be read is listed, so the sweep reports why.
pub fn needs(repo_abs: &Path) -> bool {
    tracked::needs(repo_abs, &system_git).unwrap_or(true)
}

/// What happened in one repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    /// The rename was committed; `leftovers` tracked lines still name an old
    /// name and need a person's reading.
    Committed { commit: String, leftovers: usize },
    /// Refused before anything changed, and why.
    Skipped { why: String },
    /// Something failed after the sweep started; whatever it had changed was
    /// undone, or `why` says what was not.
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

/// A git command's stdout when it succeeded, else why it did not.
fn git_stdout(repo_abs: &Path, args: &[&str]) -> Result<String, String> {
    let out = git(repo_abs, args)?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!("git {}: {}", args.join(" "), tail(&out)))
    }
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

/// Put the work tree and index back at `HEAD`. Safe here because the sweep
/// starts only on a tree with no uncommitted tracked change, so everything
/// it resets is the sweep's own; untracked files are not touched.
fn undo(repo_abs: &Path, why: String) -> Outcome {
    match git_stdout(repo_abs, &["reset", "-q", "--hard", "HEAD"]) {
        Ok(_) => Outcome::Failed {
            why: format!("{why}\nnothing was kept; fix that, then run the sweep again"),
        },
        Err(reset) => Outcome::Failed {
            why: format!(
                "{why}\nand the undo failed too ({reset}); the rename may be staged: \
                 `git status` there"
            ),
        },
    }
}

/// Rename, stage and commit in `repo_abs`, the top of its work tree.
pub fn run(repo_abs: &Path) -> Outcome {
    let branch = match git_stdout(repo_abs, &["symbolic-ref", "--quiet", "--short", "HEAD"]) {
        Ok(branch) => branch,
        Err(_) => "a detached HEAD".to_string(),
    };
    if branch != BRANCH {
        return Outcome::Skipped {
            why: format!(
                "on {branch}; the sweep commits only on {BRANCH}: \
                 `git switch -c {BRANCH} origin/HEAD` there, or a worktree on it"
            ),
        };
    }
    match git_stdout(repo_abs, &["status", "--porcelain", "--untracked-files=no"]) {
        Ok(status) if status.is_empty() => {}
        Ok(_) => {
            return Outcome::Skipped {
                why: "uncommitted changes to tracked files; commit or put them aside first"
                    .to_string(),
            }
        }
        Err(why) => return Outcome::Failed { why },
    }
    let staged = match tracked::migrate_tracked(repo_abs, &system_git) {
        Ok(staged) => staged,
        // Checked before anything moves: nothing to undo.
        Err(refused @ MigrateError::BothCommitted { .. }) => {
            return Outcome::Skipped {
                why: refused.to_string(),
            }
        }
        Err(why) => return undo(repo_abs, why.to_string()),
    };
    if !staged.changed() {
        return Outcome::Skipped {
            why: "nothing committed here carries greengate 3.x's names".to_string(),
        };
    }
    if let Err(why) = git_stdout(repo_abs, &["commit", "-q", "-m", SUBJECT]) {
        return undo(
            repo_abs,
            format!("the commit was refused (its hooks say why):\n{why}"),
        );
    }
    match git_stdout(repo_abs, &["rev-parse", "--short", "HEAD"]) {
        Ok(commit) => Outcome::Committed {
            commit,
            leftovers: staged.leftover_count,
        },
        Err(why) => Outcome::Failed { why },
    }
}
