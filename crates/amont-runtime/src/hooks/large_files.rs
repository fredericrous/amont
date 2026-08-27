//! The file that makes every future clone slower — caught while it is
//! still one `git rm --cached` away.
//!
//! Git history never forgets a megabyte: an accidentally committed
//! dataset, bundle, or core dump is paid for by every clone, forever,
//! even after it is deleted — deletion adds a commit, it does not remove
//! the bytes. And the hosting side has hard opinions: GitHub warns at
//! 50 MB and refuses files over 100 MB outright, so the block threshold
//! here is not policy invented by a hook, it is the push failing later
//! with a worse error.
//!
//! Two thresholds, both in megabytes and both configurable:
//!
//! - `amont.largeFileWarn` (default 10) — named, never blocking: a large
//!   asset can be deliberate, and the warning is the moment to decide;
//! - `amont.largeFileBlock` (default 100, GitHub's refusal line) — blocks,
//!   with the remedy named (git-lfs, or keep it out of history).
//!
//! Under the staged-only hold the working tree IS the commit's content,
//! so file sizes on disk are the sizes being committed.

use crate::check::{Outcome, Severity};
use crate::finding::Finding;

use super::common;

const MB: u64 = 1024 * 1024;

/// The check's own short name, and the `check` field of every finding it makes.
pub const NAME: &str = "large-files";

/// The two thresholds, in bytes, as configured here.
pub fn thresholds() -> (u64, u64) {
    let warn = crate::config::integer_or("amont.largeFileWarn", 10, 1..=1_000_000) as u64;
    let block = crate::config::integer_or("amont.largeFileBlock", 100, 1..=1_000_000) as u64;
    (warn * MB, block * MB)
}

/// A finding for one file's SIZE, or none if it is within both thresholds.
///
/// Deliberately without a position: the problem is the file, not a place in
/// it. `Finding::line` being `None` is how that is said, and every renderer
/// degrades to naming the file — which is all this check could ever say.
pub fn scan(file: &str, len: u64) -> Option<Finding> {
    let (warn_bytes, block_bytes) = thresholds();
    let mb = len / MB;
    if len >= block_bytes {
        Some(Finding::new(
            NAME,
            crate::ui::sanitize(file),
            Severity::Block,
            format!(
                "{mb} MB — over the {} MB limit (GitHub refuses these at push). \
                 Use git-lfs, or keep it out of history: deletion later does \
                 not remove the bytes",
                block_bytes / MB
            ),
        ))
    } else if len >= warn_bytes {
        Some(Finding::new(
            NAME,
            crate::ui::sanitize(file),
            Severity::Warn,
            format!(
                "{mb} MB — every future clone pays for this forever \
                 (git config amont.largeFileWarn to tune)"
            ),
        ))
    } else {
        None
    }
}

pub fn staged() -> Outcome {
    let root = common::repo_root();
    let mut blocked = false;
    let mut warned = false;
    for f in common::staged_files(&[]) {
        let path = std::path::Path::new(&root).join(&f);
        let Ok(meta) = std::fs::metadata(&path) else {
            continue; // deleted or unreadable: nothing this size to commit
        };
        if !meta.is_file() {
            continue;
        }
        let Some(finding) = scan(&f, meta.len()) else {
            continue;
        };
        let line = format!("large-files: {} is {}", finding.file, finding.message);
        match finding.severity {
            Severity::Block => {
                blocked = true;
                common::fail(&line);
            }
            Severity::Warn => {
                warned = true;
                common::warn(&line);
            }
        }
    }
    if blocked {
        return Outcome::Failed;
    }
    if warned {
        return Outcome::Warned;
    }
    common::ok("No oversized files staged");
    Outcome::Passed
}
