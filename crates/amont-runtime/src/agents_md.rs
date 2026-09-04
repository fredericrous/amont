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
    let clocks = |secs: u64| match secs {
        0 => "no limit".to_string(),
        s => crate::hooks::common::human_secs(s),
    };
    let idle = clocks(crate::hooks::common::idle_timeout());
    let ceiling = clocks(crate::hooks::common::check_timeout());
    let prefixes = crate::vocabulary::BRANCH_PREFIXES
        .iter()
        .map(|p| p.name)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{START}\n\
\n\
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
Each check reports its _effective_ severity (`block`/`warn`, including any\n\
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
build), and pre-push can run the test suite. Run `amont run pre-push`\n\
BEFORE `git push`: it runs the same push gate with no connection open and\n\
stamps the tree, so the push itself skips the suite and holds the remote\n\
for seconds instead of minutes (git connects before pre-push runs, and a\n\
remote may drop the idle session while a suite runs). Give both commands the\n\
longest timeout your tooling allows, never its default: here a check is\n\
killed only after {idle} of silence or {ceiling} in total\n\
(`amont.idleTimeout` / `amont.timeout`), and a test suite may legitimately\n\
run for most of that. If your tooling caps a foreground command below it,\n\
run the command in the background and read its result when it exits —\n\
while it runs, a line a minute on stderr says which check is alive and\n\
when it last printed. A push killed mid-suite pushed nothing; a commit\n\
killed mid-check committed nothing, and your unstaged work stays parked\n\
until the next run says how to recover it. Neither is the checks failing —\n\
it is the timeout. Run both bare and check the effect (`git log --oneline\n\
-1`, `git ls-remote origin <branch>`): trimming their output with `| tail`\n\
reports the pipe's exit status, so a killed or rejected run reads as\n\
success.\n\
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
\n\
{END}\n"
    )
}

/// The `CLAUDE.md` pointer, START and END inclusive.
///
/// Claude Code loads `CLAUDE.md`; the tool-neutral convention is
/// `AGENTS.md`. Writing the whole block into both would be two copies to
/// drift apart, so the guidance keeps ONE home and this is a generated
/// signpost to it — inside the same markers, so `--check` reports a stale
/// pointer exactly as it reports a stale block. A hand-written "see
/// AGENTS.md" line is the one part that could rot silently; this cannot.
pub fn generate_pointer() -> String {
    format!(
        "{START}\n\
\n\
## Git hooks (amont)\n\
\n\
This repository enforces pre-commit / pre-push checks that can REJECT a\n\
commit or a push. What runs, the branch-name rule, and why `git commit`\n\
and `git push` both need a long timeout — or a background run — are in\n\
[AGENTS.md](AGENTS.md) — read it before committing. Both files are\n\
generated: run `amont agents-md` after changing either.\n\
\n\
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
    desired_with(existing, &generate_block())
}

/// The same splice, for the pointer.
pub fn desired_pointer_content(existing: &str) -> Result<String, MarkerState> {
    desired_with(existing, &generate_pointer())
}

/// The splice itself, with the text to splice as a parameter — block and
/// pointer differ only in what goes between the markers, and two copies of
/// this logic would be two places for the "append vs replace" rules to
/// disagree.
fn desired_with(existing: &str, block: &str) -> Result<String, MarkerState> {
    match marker_range(existing)? {
        Some(range) => Ok(format!(
            "{}{}{}",
            &existing[..range.start],
            block,
            &existing[range.end..]
        )),
        None if existing.is_empty() => Ok(block.to_string()),
        None => {
            let sep = if existing.ends_with("\n\n") {
                ""
            } else if existing.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            };
            Ok(format!("{existing}{sep}{block}"))
        }
    }
}

pub fn write(path: &Path) -> Result<(), String> {
    write_with(path, &generate_block())
}

/// Write the CLAUDE.md signpost.
pub fn write_pointer(path: &Path) -> Result<(), String> {
    write_with(path, &generate_pointer())
}

fn write_with(path: &Path, block: &str) -> Result<(), String> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let content = desired_with(&existing, block).map_err(|_| {
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
    check_with(path, &generate_block())
}

/// Is the CLAUDE.md signpost the one this binary generates?
pub fn check_pointer(path: &Path) -> Result<CheckResult, String> {
    check_with(path, &generate_pointer())
}

fn check_with(path: &Path, block: &str) -> Result<CheckResult, String> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    match marker_range(&existing) {
        Err(_) => Err(format!(
            "{}: has an unpaired amont marker — fix or remove it by hand",
            path.display()
        )),
        Ok(None) => Ok(CheckResult::NotPresent),
        Ok(Some(range)) => {
            if &existing[range] == block {
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

    /// Both generated files must satisfy Prettier's markdown defaults,
    /// because amont ALSO ships `pre-commit-prettier`: when the two
    /// disagree, every JS repository is left choosing between two amont
    /// checks, and `prettier --write` on a generated file is what
    /// `agents-md --check` then reports as drift. Encoding the three rules
    /// that bit us — blank line after an HTML comment, `_emphasis_` not
    /// `*emphasis*`, blank line before a trailing HTML comment — keeps the
    /// unit suite honest without making it shell out to node.
    #[test]
    fn the_generated_markdown_satisfies_prettier() {
        for (what, text) in [("block", generate_block()), ("pointer", generate_pointer())] {
            assert!(
                text.starts_with(&format!("{START}\n\n")),
                "{what}: prettier wants a blank line after the opening comment"
            );
            assert!(
                text.ends_with(&format!("\n\n{END}\n")),
                "{what}: prettier wants a blank line before the closing comment"
            );
            // A `*` OPENING emphasis is what prettier rewrites. A trailing
            // one inside a code span (`amont.severity.*`) is untouched, so
            // the rule is "no `*` immediately followed by a letter" rather
            // than "no `*`".
            let opener = text
                .as_bytes()
                .windows(2)
                .any(|w| w[0] == b'*' && w[1].is_ascii_alphanumeric());
            assert!(
                !opener,
                "{what}: prettier rewrites *emphasis* into _emphasis_, and a \
                 formatted file then reads as drift — emit underscores"
            );
        }
    }

    /// The signpost is generated and self-checking — the whole reason it is
    /// a marked block rather than a hand-written "see AGENTS.md" line, which
    /// would be the one part of the pair able to rot in silence.
    #[test]
    fn the_pointer_is_generated_and_therefore_checkable() {
        let p = generate_pointer();
        assert!(p.starts_with(START) && p.ends_with(&format!("{END}\n")));
        assert!(p.contains("AGENTS.md"), "it has to name where to look");
        assert_ne!(p, generate_block(), "a signpost, not a second copy");
        assert!(
            !p.contains("amont list --json"),
            "the registry command lives in ONE place; duplicating it here is \
             the drift this design avoids"
        );
        // Round-trips through the same splice as the block: appended to a
        // repo's existing CLAUDE.md, then replaced in place next time.
        let existing = "# Notes\n\nsomething the repo wrote\n";
        let once = desired_pointer_content(existing).unwrap();
        assert!(once.starts_with(existing), "never touches what was there");
        assert!(once.ends_with(&p));
        assert_eq!(
            desired_pointer_content(&once).unwrap(),
            once,
            "re-running is a no-op, so `--check` can be trusted"
        );
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
