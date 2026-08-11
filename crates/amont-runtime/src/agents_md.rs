//! `amont agents-md` — a self-verifying pointer for coding agents.
//!
//! Deliberately NOT a prose contract and NOT a list of check names: both
//! drift from what the registry actually enforces the moment a check is
//! added, removed, or its severity changes — see `amont-fleet`'s
//! `severities` module doc for the canonical shape of that failure. This
//! generates a short block that instead tells an agent to ask
//! `amont list --json`, which cannot go stale because it IS the registry.
//!
//! Never touches anything outside its own markers. An `AGENTS.md` a
//! repository already wrote is the repository's, not this tool's, to
//! rewrite — matching `amont.conf`'s "declared, never assumed" posture.

use std::ops::Range;
use std::path::Path;

pub const START: &str = "<!-- amont:start -->";
pub const END: &str = "<!-- amont:end -->";

/// The block, START and END inclusive, always ending in exactly one newline.
///
/// The branch paragraph is rendered from `vocabulary::BRANCH_PREFIXES` — the
/// table `pre-push-branch-pattern` enforces — precisely so this file cannot
/// become a second, driftable copy of the rule. What the block says and what
/// the hook rejects are the same constant.
pub fn generate_block() -> String {
    let prefixes = crate::vocabulary::BRANCH_PREFIXES
        .iter()
        .map(|p| p.name)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{START}\n\
## Git hooks (amont)\n\
\n\
This repository enforces pre-commit / pre-push checks. Ask the registry\n\
rather than guessing before assuming a change is safe:\n\
\n\
```sh\n\
amont list --json\n\
amont list --json --stage pre-push --pushed  # exactly what pushing next gates\n\
```\n\
\n\
Each check reports its *effective* severity (`block`/`warn`, including any\n\
`amont.severity.*` override) and whether it fires here. The same output\n\
carries `commit_style`: the subject and description limits `commit-msg`\n\
enforces, and where the type's gitmoji is placed. It also carries\n\
`branch_style`: name a branch `<prefix>/<name>` BEFORE creating it —\n\
prefixes are {prefixes} — because `pre-push` refuses a\n\
new branch that breaks the pattern, at the end of the work instead of the\n\
start.\n\
\n\
`git commit` and `git push` both run their checks first, and neither is\n\
instant: pre-commit can invoke formatters, linters or clippy (a workspace\n\
build), and pre-push can run the test suite. Give both commands a timeout\n\
of at least 10 minutes instead of your tooling's default. A push killed\n\
mid-suite pushed nothing; a commit killed mid-check committed nothing, and\n\
your unstaged work stays parked until the next run says how to recover it.\n\
Neither is the checks failing — it is the timeout.\n\
\n\
Never bypass with `--no-verify`. To change enforcement, downgrade it\n\
intentionally instead:\n\
\n\
```sh\n\
git config amont.severity.<check-id> warn\n\
```\n\
\n\
`commit-msg` takes neither `hook.skip` nor a severity override. Write the\n\
message it asks for, or change what it asks for — `amont setup`, or\n\
`amont.commit.*` directly.\n\
{END}\n"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerState {
    /// Exactly one marker present, or `END` appears before `START` — a state
    /// this tool refuses to guess its way out of.
    Malformed,
}

/// The byte range to replace (markers inclusive, one trailing newline
/// consumed), or `None` when neither marker is present at all.
///
/// `start`/`end` are searched INDEPENDENTLY — not `end` searched only within
/// the text after a found `start` — so a file holding `END` with no `START`
/// at all (searching from `start`'s position finds nothing, because there is
/// no `start`) still surfaces as `(None, Some(_))` rather than silently
/// reading as "no markers, nothing to guard against."
fn marker_range(text: &str) -> Result<Option<Range<usize>>, MarkerState> {
    let start = text.find(START);
    let end = text.find(END);

    match (start, end) {
        (None, None) => Ok(None),
        (Some(s), Some(e)) if e >= s + START.len() => {
            let mut range_end = e + END.len();
            if text[range_end..].starts_with('\n') {
                range_end += 1; // swallow exactly one trailing newline
            }
            Ok(Some(s..range_end))
        }
        // Exactly one present, or END appears before START.
        _ => Err(MarkerState::Malformed),
    }
}

/// What writing would produce, given what is on disk today.
///
/// - no file / empty file → the block alone.
/// - file, no markers → block appended, separated by exactly one blank line.
/// - file, markers found → everything outside the markers is byte-for-byte
///   untouched; the marked span is replaced.
/// - markers malformed (exactly one present, or out of order) → refused.
pub fn desired_file_content(existing: &str) -> Result<String, MarkerState> {
    match marker_range(existing)? {
        Some(range) => Ok(format!(
            "{}{}{}",
            &existing[..range.start],
            generate_block(),
            &existing[range.end..]
        )),
        None if existing.is_empty() => Ok(generate_block()),
        None => {
            let sep = if existing.ends_with("\n\n") {
                ""
            } else if existing.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            };
            Ok(format!("{existing}{sep}{}", generate_block()))
        }
    }
}

pub fn write(path: &Path) -> Result<(), String> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let content = desired_file_content(&existing).map_err(|_| {
        format!(
            "{}: has an unpaired amont marker — fix or remove it by hand, \
             then re-run `amont agents-md`",
            path.display()
        )
    })?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(path, content).map_err(|e| format!("{}: {e}", path.display()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckResult {
    /// No file, or a file with no markers at all — opt-in tooling, so this
    /// is not drift.
    NotPresent,
    MatchesGenerated,
    Drifted,
}

pub fn check(path: &Path) -> Result<CheckResult, String> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    match marker_range(&existing) {
        Err(_) => Err(format!(
            "{}: has an unpaired amont marker — fix or remove it by hand",
            path.display()
        )),
        Ok(None) => Ok(CheckResult::NotPresent),
        Ok(Some(range)) => {
            if existing[range] == generate_block() {
                Ok(CheckResult::MatchesGenerated)
            } else {
                Ok(CheckResult::Drifted)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_file_produces_just_the_block() {
        assert_eq!(desired_file_content("").unwrap(), generate_block());
    }

    #[test]
    fn no_markers_appends_with_one_blank_line() {
        let got = desired_file_content("# My Project\n\nSome docs.\n").unwrap();
        assert!(got.starts_with("# My Project\n\nSome docs.\n\n"));
        assert!(got.ends_with(&generate_block()));
    }

    #[test]
    fn no_markers_and_no_trailing_newline_still_gets_a_blank_line() {
        let got = desired_file_content("# My Project").unwrap();
        assert!(got.starts_with("# My Project\n\n"));
    }

    #[test]
    fn existing_markers_are_replaced_and_everything_else_survives() {
        let before = format!("before\n\n{START}\nstale\n{END}\n\nafter\n");
        let got = desired_file_content(&before).unwrap();
        assert!(got.starts_with("before\n\n"));
        assert!(got.ends_with("\n\nafter\n"));
        assert!(got.contains(&generate_block()));
        assert!(!got.contains("stale"));
    }

    #[test]
    fn applying_twice_is_idempotent() {
        let once = desired_file_content("preamble\n").unwrap();
        let twice = desired_file_content(&once).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn an_unpaired_marker_is_refused_not_guessed_at() {
        assert_eq!(
            desired_file_content(&format!("{START}\nno end here\n")),
            Err(MarkerState::Malformed)
        );
        assert_eq!(
            desired_file_content(&format!("no start\n{END}\n")),
            Err(MarkerState::Malformed)
        );
    }

    /// The bug the fix above was for: searching for `END` only within the
    /// text AFTER a found `START` meant an `END`-with-no-`START` file
    /// searched from a `start` that did not exist — finding nothing, and
    /// reading as "no markers at all" rather than "unpaired."
    #[test]
    fn end_appearing_before_start_is_malformed_not_reordered() {
        assert_eq!(
            desired_file_content(&format!("{END}\n...\n{START}\n...\n")),
            Err(MarkerState::Malformed)
        );
    }

    #[test]
    fn check_reports_not_present_for_a_missing_file() {
        let tmp = std::env::temp_dir().join("amont-agents-md-test-nonexistent-xyz");
        let _ = std::fs::remove_file(&tmp);
        assert_eq!(check(&tmp).unwrap(), CheckResult::NotPresent);
    }

    #[test]
    fn check_reports_matches_generated_after_a_write() {
        let tmp =
            std::env::temp_dir().join(format!("amont-agents-md-test-{}-match", std::process::id()));
        std::fs::write(&tmp, generate_block()).unwrap();
        assert_eq!(check(&tmp).unwrap(), CheckResult::MatchesGenerated);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn check_reports_drifted_for_a_stale_block() {
        let tmp =
            std::env::temp_dir().join(format!("amont-agents-md-test-{}-drift", std::process::id()));
        std::fs::write(&tmp, format!("{START}\nstale\n{END}\n")).unwrap();
        assert_eq!(check(&tmp).unwrap(), CheckResult::Drifted);
        let _ = std::fs::remove_file(&tmp);
    }
}
