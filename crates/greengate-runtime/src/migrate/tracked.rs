//! `greengate migrate --tracked`: the names a repository COMMITS, renamed in
//! its work tree and staged, for the repository to commit (the fleet sweep
//! lands one such commit per repository).
//!
//! Never part of the automatic first run: a tracked rename is a change to
//! the repository, and greengate 3.x, greenpiste and relais before 0.54
//! read only `amont.conf`, so it happens when someone asks for it.
//!
//! What it does, each step skipped when there is nothing to do:
//!
//! - `amont.conf` → `greengate.conf` and `amont.pack` → `greengate.pack`,
//!   with `git mv` when tracked. The content is unchanged: trust is the
//!   content's blob id, so a trusted manifest stays trusted, and the
//!   `amont:pack:*` markers inside it are still read until the census.
//! - the AGENTS.md / CLAUDE.md marker pair, old spelling to new; what lies
//!   between the markers is left for `greengate agents-md` to regenerate.
//! - `.github/attest-inputs`: a path token naming an old file.
//!
//! Then it lists, without touching them, the tracked lines that still name
//! an old file or an `AMONT_` variable (CI, dotfiles, docs): those need a
//! person's reading, not a substitution.

use std::path::{Path, PathBuf};

use super::{old, run_git, GitFn, MigrateError};
use crate::names as n;

/// The committed files renamed, old name first.
const FILES: &[(&str, &str)] = &[(old::MANIFEST, n::MANIFEST), (old::PACK_FILE, n::PACK_FILE)];

/// The generated files whose marker pair is rewritten.
const MARKED: &[&str] = &["AGENTS.md", "CLAUDE.md"];

/// The attest inputs file, which lists the manifest by name.
const ATTEST_INPUTS: &str = ".github/attest-inputs";

/// At most this many leftover lines are listed; the rest are counted.
const LEFTOVER_LINES: usize = 20;

/// What `migrate --tracked` changed and staged, and what it left.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Staged {
    /// Files renamed, old name first.
    pub renamed: Vec<(&'static str, &'static str)>,
    /// Files whose marker pair was rewritten.
    pub markers: Vec<&'static str>,
    /// Whether `.github/attest-inputs` was rewritten.
    pub attest_inputs: bool,
    /// Tracked lines still naming an old name, `path:line:text`, at most
    /// [`LEFTOVER_LINES`] of them.
    pub leftovers: Vec<String>,
    /// How many leftover lines there were in all.
    pub leftover_count: usize,
}

impl Staged {
    /// Whether anything was changed.
    pub fn changed(&self) -> bool {
        !self.renamed.is_empty() || !self.markers.is_empty() || self.attest_inputs
    }
}

/// Rename the tracked names in the work tree at `root` (its top level) and
/// stage them. Refuses, changing nothing, when both spellings of a file
/// exist.
pub fn migrate_tracked(root: &Path, git: &GitFn<'_>) -> Result<Staged, MigrateError> {
    let mut staged = Staged::default();
    for &(old_name, new_name) in FILES {
        let (source, target) = (root.join(old_name), root.join(new_name));
        if source.exists() && target.exists() {
            return Err(MigrateError::BothExist { source, target });
        }
    }
    for &(old_name, new_name) in FILES {
        let source = root.join(old_name);
        if !source.exists() {
            continue;
        }
        if is_tracked(root, old_name, git)? {
            in_tree(root, git, &["mv", "--", old_name, new_name])?;
        } else {
            let target = root.join(new_name);
            std::fs::rename(&source, &target).map_err(super::io("rename", &source))?;
        }
        staged.renamed.push((old_name, new_name));
    }
    for &name in MARKED {
        if rewrite(root, name, git, swap_markers)? {
            staged.markers.push(name);
        }
    }
    staged.attest_inputs = rewrite(root, ATTEST_INPUTS, git, swap_inputs)?;
    let (leftovers, count) = leftovers(root, git)?;
    staged.leftovers = leftovers;
    staged.leftover_count = count;
    Ok(staged)
}

/// `git -C <root> <args>`.
fn in_tree(root: &Path, git: &GitFn<'_>, args: &[&str]) -> Result<Option<String>, MigrateError> {
    let root_arg = root.to_string_lossy();
    let mut all = vec!["-C", root_arg.as_ref()];
    all.extend_from_slice(args);
    run_git(git, None, &all)
}

fn is_tracked(root: &Path, name: &str, git: &GitFn<'_>) -> Result<bool, MigrateError> {
    Ok(in_tree(root, git, &["ls-files", "--", name])?.is_some_and(|out| !out.trim().is_empty()))
}

/// Rewrite `name` under `root` with `edit`, when the file exists and `edit`
/// changes it; a tracked file is staged. Whether it was rewritten.
fn rewrite(
    root: &Path,
    name: &str,
    git: &GitFn<'_>,
    edit: fn(&str) -> Option<String>,
) -> Result<bool, MigrateError> {
    let path: PathBuf = root.join(name);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(false);
    };
    let Some(edited) = edit(&text) else {
        return Ok(false);
    };
    std::fs::write(&path, edited).map_err(super::io("write", &path))?;
    if is_tracked(root, name, git)? {
        in_tree(root, git, &["add", "--", name])?;
    }
    Ok(true)
}

/// The old marker pair, respelled, when the file carries it and not the new
/// one; `None` when there is nothing to change.
fn swap_markers(text: &str) -> Option<String> {
    let has_old = text.contains(old::AGENTS_START) || text.contains(old::AGENTS_END);
    if !has_old || text.contains(n::AGENTS_START) || text.contains(n::AGENTS_END) {
        return None;
    }
    Some(
        text.replacen(old::AGENTS_START, n::AGENTS_START, 1)
            .replacen(old::AGENTS_END, n::AGENTS_END, 1),
    )
}

/// Each whitespace-separated token naming an old file, respelled, on the
/// lines that are not comments; `None` when no token named one. Spacing and
/// comments are kept byte for byte.
fn swap_inputs(text: &str) -> Option<String> {
    let respelled: String = text.split_inclusive('\n').map(respell_line).collect();
    (respelled != text).then_some(respelled)
}

/// One line of [`swap_inputs`]: a comment as it is, else each token that
/// names an old file respelled, with the whitespace after it kept.
fn respell_line(line: &str) -> String {
    if line.trim_start().starts_with('#') {
        return line.to_string();
    }
    line.split_inclusive(char::is_whitespace)
        .map(|piece| {
            let token = piece.trim_end_matches(char::is_whitespace);
            let gap = &piece[token.len()..];
            format!("{}{gap}", respelled(token).unwrap_or(token))
        })
        .collect()
}

/// `token`'s new spelling, when it names an old file.
fn respelled(token: &str) -> Option<&'static str> {
    FILES
        .iter()
        .find(|(old_name, _)| *old_name == token)
        .map(|(_, new_name)| *new_name)
}

/// The tracked lines that still name an old file or an `AMONT_` variable,
/// at most [`LEFTOVER_LINES`], and how many there were.
fn leftovers(root: &Path, git: &GitFn<'_>) -> Result<(Vec<String>, usize), MigrateError> {
    let out = in_tree(
        root,
        git,
        &[
            "grep",
            "-n",
            "-I",
            "-F",
            "-e",
            old::MANIFEST,
            "-e",
            old::PACK_FILE,
            "-e",
            old::ENV_PREFIX,
            "--",
            ".",
        ],
    )?;
    // A line naming only greenpiste's own variables is greenpiste's.
    let lines: Vec<String> = out
        .unwrap_or_default()
        .lines()
        .filter(|line| {
            line.contains(old::MANIFEST)
                || line.contains(old::PACK_FILE)
                || line
                    .replace(old::FOREIGN_ENV_PREFIX, "")
                    .contains(old::ENV_PREFIX)
        })
        .map(|line| line.chars().take(200).collect())
        .collect();
    let count = lines.len();
    Ok((lines.into_iter().take(LEFTOVER_LINES).collect(), count))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_old_marker_pair_is_respelled_once() {
        let before = format!("a\n{}\nbody\n{}\nb\n", old::AGENTS_START, old::AGENTS_END);
        let after = swap_markers(&before).unwrap();
        assert_eq!(
            after,
            format!("a\n{}\nbody\n{}\nb\n", n::AGENTS_START, n::AGENTS_END)
        );
        assert_eq!(swap_markers(&after), None, "a second run changes nothing");
        assert_eq!(swap_markers("no markers\n"), None);
    }

    /// A file carrying both pairs is a hand edit: not guessed at.
    #[test]
    fn a_file_with_both_pairs_is_left_alone() {
        let both = format!(
            "{}\n{}\n{}\n{}\n",
            old::AGENTS_START,
            old::AGENTS_END,
            n::AGENTS_START,
            n::AGENTS_END
        );
        assert_eq!(swap_markers(&both), None);
    }

    #[test]
    fn attest_inputs_tokens_are_respelled_and_comments_kept() {
        let before = format!(
            "# lists {m} on purpose\ngate a  {m} b\tc {p}\nother x{m} y\n",
            m = old::MANIFEST,
            p = old::PACK_FILE
        );
        let after = swap_inputs(&before).unwrap();
        assert_eq!(
            after,
            format!(
                "# lists {m} on purpose\ngate a  {nm} b\tc {np}\nother x{m} y\n",
                m = old::MANIFEST,
                nm = n::MANIFEST,
                np = n::PACK_FILE
            )
        );
        assert_eq!(swap_inputs(&after), None);
    }
}
