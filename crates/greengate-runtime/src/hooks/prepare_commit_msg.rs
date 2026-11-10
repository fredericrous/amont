//! prepare-commit-msg — append the issue id found in the branch name.
//!
//! JIRA first (`ABC-1234`), else a bare Kanbanize id (`1234`). Only for a
//! commit the user is authoring: `-m`, `-t`, a merge, a squash and `--amend`
//! all pass a source in $2 and are left alone.

use crate::check::Verdict;
use crate::git;
use std::fs::OpenOptions;
use std::io::Write;

/// `[A-Z]{3,32}-[1-9][0-9]{1,31}`, by hand.
///
/// A '-' is not uppercase, so the letter run always ends exactly at it: for a
/// start position, take the run and require its length in 3..=32. A 40-letter
/// prefix therefore fails at offset 0 and matches at offset 8, which is what
/// the regex's leftmost-match does too.
pub fn find_jira(s: &str) -> Option<String> {
    let b = s.as_bytes();
    for start in 0..b.len() {
        let mut e = start;
        while e < b.len() && b[e].is_ascii_uppercase() {
            e += 1;
        }
        let letters = e - start;
        if !(3..=32).contains(&letters) || e >= b.len() || b[e] != b'-' {
            continue;
        }
        let d0 = e + 1;
        // [1-9] — a leading zero is not a JIRA number
        if d0 >= b.len() || !(b'1'..=b'9').contains(&b[d0]) {
            continue;
        }
        // [0-9]{1,31}, greedy: at least one more digit, so ABC-1 does NOT match
        let mut d = d0 + 1;
        while d < b.len() && b[d].is_ascii_digit() && d - d0 < 32 {
            d += 1;
        }
        if d - d0 < 2 {
            continue;
        }
        return Some(s[start..d].to_string());
    }
    None
}

/// `[0-9]{3,}`, greedy, leftmost.
pub fn find_digits(s: &str, min: usize) -> Option<String> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i - start >= min {
            return Some(s[start..i].to_string());
        }
    }
    None
}

/// Append a line, best-effort.
///
/// Returns nothing, because there is nothing to decide: a commit is never
/// blocked over the message file, the id being a convenience rather than a
/// gate. That was previously expressed as an `i32` that could only ever be
/// zero — a decision type for a function that makes no decision.
fn append(path: &str, line: &str) {
    // create(true): the shell's `>>` makes the file when absent, and git does
    // not always pre-create the message file.
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "\n{line}");
    }
}

pub fn run(args: &[std::ffi::OsString]) -> Verdict {
    let Some(msg_file) = args.first().and_then(|a| a.to_str()) else {
        return Verdict::Proceed;
    };
    // $2 is the commit source, and the shell version switched on it with a
    // `case` that named five values and let EVERYTHING ELSE fall through to
    // the append. So the test is right to pass "magic" and expect an id: an
    // unrecognised source means "treat it as an authored commit". Skipping on
    // "not empty" instead of "one of these five" silently disabled the hook
    // for every source git may add in future.
    let source = args.get(1).and_then(|a| a.to_str()).unwrap_or("");
    if matches!(
        source,
        "message" | "template" | "merge" | "squash" | "commit"
    ) {
        return Verdict::Proceed;
    }

    let branch = git::stdout(&["branch", "--show-current"]).unwrap_or_default();

    if let Some(id) = find_jira(&branch) {
        append(msg_file, &format!("Issue: {id}"));
        return Verdict::Proceed;
    }
    // Only when there IS an id. The shell version tested `$?` after a pipeline
    // ending in `head -n 1`, which is head's status and therefore always 0 —
    // so a branch with no digits appended a dangling "Issue: #id " with an
    // empty value. Untested there, and fixed here.
    if let Some(id) = find_digits(&branch, 3) {
        append(msg_file, &format!("Issue: #id {id}"));
        return Verdict::Proceed;
    }
    Verdict::Proceed
}

#[cfg(test)]
mod tests {
    use super::{find_digits, find_jira};

    #[test]
    fn finds_a_jira_id() {
        assert_eq!(
            find_jira("feat/JIRA-1234-description").as_deref(),
            Some("JIRA-1234")
        );
        assert_eq!(find_jira("ABC-12").as_deref(), Some("ABC-12"));
    }

    #[test]
    fn rejects_shapes_the_regex_rejects() {
        assert_eq!(find_jira("AB-1234"), None); // fewer than 3 letters
        assert_eq!(find_jira("ABC-1"), None); // needs 2+ digits
        assert_eq!(find_jira("ABC-0123"), None); // leading zero
        assert_eq!(find_jira("abc-1234"), None); // lowercase
        assert_eq!(find_jira("feat/nothing-here"), None);
    }

    /// 40 uppercase letters then `-12`: no match at offset 0 (run too long),
    /// but the leftmost VALID start is offset 8. The regex behaves the same.
    #[test]
    fn long_letter_runs_match_at_a_later_offset() {
        let s = format!("{}-12", "A".repeat(40));
        assert_eq!(
            find_jira(&s).as_deref(),
            Some(&*format!("{}-12", "A".repeat(32)))
        );
    }

    #[test]
    fn finds_bare_ids_of_at_least_three_digits() {
        assert_eq!(
            find_digits("fix/1234-something", 3).as_deref(),
            Some("1234")
        );
        assert_eq!(find_digits("fix/12-something", 3), None);
        assert_eq!(find_digits("release/007", 3).as_deref(), Some("007"));
        assert_eq!(find_digits("no-digits-here", 3), None);
    }
}
