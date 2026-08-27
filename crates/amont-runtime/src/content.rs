//! The content checks, asked about a FILE rather than about the index.
//!
//! Every check reachable from here is pure: give it a path and some bytes and
//! it answers, with no git, no index, no stash, and no opinion about what is
//! staged. That is the whole difference from [`crate::dispatch`], which
//! rehearses a commit and is entitled to all of those things.
//!
//! The separation is deliberate and load-bearing. `amont run` answers "is this
//! commit ready", which is a question about the index; an editor asks "what is
//! wrong with this buffer", which is a question about content that may not be
//! staged and may never have been saved. Conflating them would drag index
//! fidelity — the stash, the restore path, the staged-only hold — into what
//! ought to be a read-only lookup.
//!
//! Not every check can come here, and that is fine. `branch-pattern`,
//! `pull-rebase` and `package-lock` are not about the contents of a file at
//! all, and `clippy`/`ruff`/`eslint` already have editor integrations of their
//! own. What is left is the set amont uniquely owns — which is also the set
//! `docs/ci.md` says CI deliberately does not reproduce.

use crate::finding::Finding;
use crate::hooks::{ban_terms, large_files, merge_conflict, secrets};

/// Every finding for one file, ordered for reading.
///
/// `bytes` rather than `&str` because two of the four decisions are made on
/// bytes: a file's SIZE is a byte count, and whether a file is text at all is
/// git's NUL heuristic. Converting first would answer both questions wrongly
/// for exactly the files where they matter.
pub fn scan(file: &str, bytes: &[u8]) -> Vec<Finding> {
    let mut out = Vec::new();

    // Size first, and unconditionally: it is the one thing still worth saying
    // about a file too big or too binary for everything below.
    if let Some(f) = large_files::scan(file, bytes.len() as u64) {
        out.push(f);
    }
    if !secrets::is_scannable(bytes) {
        return sorted(out);
    }

    let text = String::from_utf8_lossy(bytes);
    // Each check's own short name is passed as the self-exclusion key. In a
    // hook that key is the hook's name; here there is no stage, so the check
    // names itself. The only visible difference is in amont's own repository,
    // where a fixture named after a hook rather than after a check is no
    // longer skipped — which is the correct answer for a verb that was asked
    // about that file by name.
    out.extend(ban_terms::scan(file, &text, ban_terms::NAME));
    out.extend(merge_conflict::scan(file, &text, merge_conflict::NAME));
    out.extend(secrets::findings(file, &text));
    sorted(out)
}

/// By position, then by check, so output is stable across runs.
///
/// Stability is not cosmetic here: a diagnostic list that reorders between
/// invocations makes an editor's gutter flicker and makes a diff of two runs
/// unreadable. Findings without a line sort first — they are about the whole
/// file, so they belong above anything inside it.
fn sorted(mut findings: Vec<Finding>) -> Vec<Finding> {
    findings.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.line.cmp(&b.line))
            .then(a.column.cmp(&b.column))
            .then(a.check.cmp(b.check))
    });
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::Severity;

    #[test]
    fn a_clean_file_has_nothing_to_say() {
        assert!(scan("app.js", b"const x = 1;\n").is_empty());
    }

    #[test]
    fn a_banned_term_is_placed() {
        let f = scan("app.js", b"const x = 1;\n\n  debugger;\n");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].check, "ban-terms");
        assert_eq!(f[0].line, Some(3));
        assert_eq!(f[0].column, Some(3));
    }

    /// The extension gate still applies: `debugger;` in a `.txt` is prose.
    #[test]
    fn scope_is_respected_outside_the_index_too() {
        assert!(scan("notes.txt", b"debugger;\n").is_empty());
    }

    /// A binary blob gets a size verdict and no text scanning — reading it as
    /// text would produce nonsense positions in nonexistent lines.
    #[test]
    fn a_binary_file_is_sized_but_not_read() {
        let mut bytes = vec![0u8; 200];
        bytes.extend_from_slice(b"debugger;");
        let f = scan("blob.js", &bytes);
        assert!(f.iter().all(|f| f.check != "ban-terms"), "{f:?}");
    }

    /// Whole-file findings sort above positioned ones.
    #[test]
    fn findings_are_ordered_for_reading() {
        let whole = Finding::new("large-files", "a.js", Severity::Warn, "x");
        let late = Finding::new("ban-terms", "a.js", Severity::Block, "y").at_line(9);
        let early = Finding::new("secrets", "a.js", Severity::Block, "z").at_line(2);
        let out = sorted(vec![late, whole, early]);
        assert_eq!(
            out.iter().map(|f| f.line).collect::<Vec<_>>(),
            vec![None, Some(2), Some(9)]
        );
    }
}
