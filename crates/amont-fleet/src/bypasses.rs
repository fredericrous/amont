//! Reading each repository's bypass ledger — the count of unverified
//! commits.
//!
//! Read-only, and never parsed here: `amont_runtime::bypass::parse` is the
//! one reader, for the same reason `skips` resolves reach through
//! `skip_suppresses` — the dashboard cannot form its own opinion about a
//! format the hooks own. What this module adds is only the `Serialize`
//! wrapper the dependency-free crate cannot derive (the `severities::Level`
//! precedent) and the door that takes an already-resolved common dir, which
//! the scan has in hand anyway for worktree sharing.

use std::path::Path;

use serde::Serialize;

/// One repository's tally of commits that carry no record their commit-time
/// gate ran. `total` is a floor once the runtime has compacted the ledger.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Bypasses {
    pub total: usize,
    /// Epoch of the newest event — recency is what separates "a gate people
    /// are routing around today" from ancient history.
    pub last: Option<u64>,
    pub by_script: Vec<ScriptCount>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScriptCount {
    pub script: String,
    pub count: usize,
    pub last: u64,
}

/// The ledger under `common_dir`, or zero — an absent file is a repo that
/// never dodged anything, not an error.
pub fn read(common_dir: &Path) -> Bypasses {
    let l = amont_runtime::bypass::read_at(common_dir);
    Bypasses {
        total: l.total,
        last: l.last,
        by_script: l
            .by_script
            .into_iter()
            .map(|s| ScriptCount {
                script: s.script,
                count: s.count,
                last: s.last,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("fleet-bypass-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A repo with no ledger reads as zero, not as a failure.
    #[test]
    fn a_repo_with_no_ledger_reads_empty() {
        let d = dir("none");
        assert_eq!(read(&d), Bypasses::default());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The reader is the runtime's parser wearing a Serialize coat — the
    /// numbers must be the runtime's numbers, field for field.
    #[test]
    fn the_reader_agrees_with_the_runtimes_parser() {
        let d = dir("agree");
        let text =
            "amont-bypass-v1\n100 abcdef0 typecheck\n200 abcdef0 typecheck\n150 abcdef0 test\n";
        std::fs::write(d.join("amont-bypasses"), text).unwrap();
        let ours = read(&d);
        let theirs = amont_runtime::bypass::parse(text);
        assert_eq!(ours.total, theirs.total);
        assert_eq!(ours.last, theirs.last);
        assert_eq!(ours.by_script.len(), theirs.by_script.len());
        for (a, b) in ours.by_script.iter().zip(&theirs.by_script) {
            assert_eq!(
                (a.script.as_str(), a.count, a.last),
                (b.script.as_str(), b.count, b.last)
            );
        }
        assert_eq!(ours.total, 3);
        assert_eq!(ours.by_script[0].script, "typecheck");
        let _ = std::fs::remove_dir_all(&d);
    }
}
