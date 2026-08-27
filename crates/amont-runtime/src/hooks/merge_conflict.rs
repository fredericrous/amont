//! pre-commit-merge-conflict — refuse staged files still carrying conflict
//! markers.

use super::common::{fail, hl, ok};
use crate::check::{Outcome, Severity};
use crate::finding::Finding;

/// The markers, BUILT rather than written.
///
/// A file that greps for conflict markers cannot contain them literally, or it
/// flags itself — which is exactly what happened the first time this was
/// ported, caught by the hook running over this very commit. The shell version
/// solved it by excluding its own path and its test's; constructing the strings
/// removes the need for any exclusion list, so a future rename cannot silently
/// reintroduce the problem.
fn markers() -> [String; 3] {
    ["<", "=", ">"].map(|c| c.repeat(7))
}

/// `git grep --cached` scans the WHOLE INDEX, not just this commit's changes,
/// so any tracked file containing all three markers matches on every commit —
/// including this hook's own TEST, which must contain them to test with.
///
/// The shell version excluded both its source and its test by path. Building
/// the markers (see `markers`) removed the need to exclude the SOURCE, but not
/// the test, and dropping that exclusion made this repo uncommittable — the
/// same failure that once hit ban-terms, reintroduced by a fix for a different
/// self-reference. Excluded by hook name so a rename cannot silently strand it.
fn is_own_test(file: &str, hook_name: &str) -> bool {
    file.starts_with(&format!("tests/{hook_name}.test."))
}

/// The check's own short name, and the `check` field of every finding it makes.
pub const NAME: &str = "merge-conflict";

/// Conflict markers in one file's content, with the line each sits on. PURE —
/// no git, no index — so `amont check` can run it over a path or a buffer.
///
/// A file qualifies only when ALL THREE markers are present, which is what
/// distinguishes an unresolved conflict from a file that happens to contain a
/// row of equals signs. The reported position is the first marker line, since
/// that is where a reader wants the cursor.
pub fn scan(file: &str, content: &str, hook_name: &str) -> Vec<Finding> {
    if is_own_test(file, hook_name) {
        return Vec::new();
    }
    let m = markers();
    if !m.iter().all(|mk| content.contains(mk.as_str())) {
        return Vec::new();
    }
    let first = content
        .lines()
        .enumerate()
        .find(|(_, l)| m.iter().any(|mk| l.starts_with(mk.as_str())))
        .map(|(i, _)| i + 1);
    let mut finding = Finding::new(
        NAME,
        file,
        Severity::Block,
        "unresolved merge conflict markers",
    );
    if let Some(line) = first {
        finding = finding.at_line(line);
    }
    vec![finding]
}

pub fn run(hook_name: &str, _args: &[std::ffi::OsString]) -> Outcome {
    // Scoped to what this commit STAGES, not the whole index.
    //
    // `git grep --cached` scanned every tracked file, so a marker anywhere in
    // the repo blocked every commit until someone fixed it — including files
    // this change never touched. That is the argument ban-terms already makes
    // for its own two-stage design, applied here.
    //
    // NOTE: this does NOT remove the need for the self-exclusion in `scan`,
    // which I previously claimed it would. Editing the hook's own test still
    // stages a file that must contain markers.
    let findings: Vec<Finding> = super::common::staged_files(&[])
        .into_iter()
        .filter_map(|file| {
            // The STAGED content, not the worktree's: what is being committed
            // is what matters, and they differ during a partial `git add -p`.
            let content = crate::git::stdout(&["show", &format!(":{file}")])?;
            Some(scan(&file, &content, hook_name))
        })
        .flatten()
        .collect();

    if !findings.is_empty() {
        for f in &findings {
            fail(&format!("Merge conflict detected in {}", hl(&f.location())));
        }
        return Outcome::Failed;
    }
    ok("No merge conflict detected");
    Outcome::Passed
}

#[cfg(test)]
mod tests {
    use super::markers;

    #[test]
    fn the_markers_are_the_real_seven_character_ones() {
        let m = markers();
        assert_eq!(m[0].len(), 7);
        assert_eq!(m[0], "<".repeat(7));
        assert_eq!(m[1], "=".repeat(7));
        assert_eq!(m[2], ">".repeat(7));
    }

    #[test]
    fn the_hooks_own_test_is_excluded() {
        let n = "pre-commit-merge-conflict";
        assert!(super::is_own_test(
            "tests/pre-commit-merge-conflict.test.zsh",
            n
        ));
        assert!(!super::is_own_test(
            "tests/pre-commit-ban-terms.test.zsh",
            n
        ));
        assert!(!super::is_own_test("src/main.rs", n));
    }

    /// The point of building them: this source file must not contain a literal
    /// marker, or the hook flags its own implementation.
    #[test]
    fn this_file_contains_no_literal_marker() {
        let src = include_str!("merge_conflict.rs");
        for m in markers() {
            assert!(!src.contains(&m), "literal marker present in this file");
        }
    }
}
