//! Reading each repository's downgrade ledger — the count of problems that
//! were found and did not block.
//!
//! Read-only, and never parsed here: `amont_runtime::downgrade::parse` is the
//! one reader, for the same reason `bypasses` defers to its own — the
//! dashboard cannot form its own opinion about a format the hooks own. What
//! this module adds is only the `Serialize` wrapper the dependency-free crate
//! cannot derive, and the door that takes an already-resolved common dir,
//! which the scan has in hand anyway for worktree sharing.

use std::path::Path;

use serde::Serialize;

/// One repository's tally of checks that reported a problem without blocking.
/// `total` is a floor once the runtime has compacted the ledger.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Downgrades {
    pub total: usize,
    /// Of `total`, the events that would have blocked but for an override —
    /// the only ones that are evidence about a rollout.
    pub would_block: usize,
    /// Distinct commits those events happened against. A different fact from
    /// `total`: many commits tripping a check once means the team disagrees
    /// with it, one commit tripping it many times means somebody lost an
    /// afternoon to it.
    pub commits: usize,
    pub first: Option<u64>,
    /// Epoch of the newest event — recency separates "a check people are
    /// fighting today" from a trial that ended months ago.
    pub last: Option<u64>,
    pub by_check: Vec<CheckCount>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckCount {
    pub check: String,
    pub count: usize,
    pub would_block: usize,
    pub last: u64,
}

/// The ledger under `common_dir`, or zero — an absent file is a repository
/// that never warned about anything, not an error.
pub fn read(common_dir: &Path) -> Downgrades {
    let l = amont_runtime::downgrade::read_at(common_dir);
    Downgrades {
        total: l.total,
        would_block: l.would_block,
        commits: l.commits,
        first: l.first,
        last: l.last,
        by_check: l
            .by_check
            .into_iter()
            .map(|c| CheckCount {
                check: c.check,
                count: c.count,
                would_block: c.would_block,
                last: c.last,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("fleet-downgrade-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A repo with no ledger reads as zero, not as a failure.
    #[test]
    fn a_repo_with_no_ledger_reads_empty() {
        let d = dir("none");
        assert_eq!(read(&d), Downgrades::default());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The reader is the runtime's parser wearing a Serialize coat — the
    /// numbers must be the runtime's numbers, field for field.
    #[test]
    fn the_reader_agrees_with_the_runtimes_parser() {
        let d = dir("agree");
        let text = "amont-downgrade-v1\n\
                    100 abcdef0 pre-commit-ban-terms config\n\
                    200 abcdef0 pre-commit-ban-terms config\n\
                    150 bbbbbbb pre-commit-agents-md declared\n";
        std::fs::write(d.join("amont-downgrades"), text).unwrap();
        let ours = read(&d);
        let theirs = amont_runtime::downgrade::parse(text);
        assert_eq!(ours.total, theirs.total);
        assert_eq!(ours.would_block, theirs.would_block);
        assert_eq!(ours.commits, theirs.commits);
        assert_eq!(ours.first, theirs.first);
        assert_eq!(ours.last, theirs.last);
        for (a, b) in ours.by_check.iter().zip(&theirs.by_check) {
            assert_eq!(
                (a.check.as_str(), a.count, a.would_block, a.last),
                (b.check.as_str(), b.count, b.would_block, b.last)
            );
        }
        assert_eq!((ours.total, ours.would_block, ours.commits), (3, 2, 2));
        let _ = std::fs::remove_dir_all(&d);
    }
}
