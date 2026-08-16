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

use crate::check::Outcome;

use super::common;

const MB: u64 = 1024 * 1024;

pub fn staged() -> Outcome {
    let warn_mb = crate::config::integer_or("amont.largeFileWarn", 10, 1..=1_000_000) as u64;
    let block_mb = crate::config::integer_or("amont.largeFileBlock", 100, 1..=1_000_000) as u64;
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
        let mb = meta.len() / MB;
        if meta.len() >= block_mb * MB {
            blocked = true;
            common::fail(&format!(
                "large-files: {} is {mb} MB — over the {block_mb} MB limit \
                 (GitHub refuses these at push). Use git-lfs, or keep it out \
                 of history: deletion later does not remove the bytes",
                crate::ui::sanitize(&f),
            ));
        } else if meta.len() >= warn_mb * MB {
            warned = true;
            common::warn(&format!(
                "large-files: {} is {mb} MB — every future clone pays for \
                 this forever (git config amont.largeFileWarn to tune)",
                crate::ui::sanitize(&f),
            ));
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
