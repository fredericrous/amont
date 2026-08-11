//! commit-msg — validate the summary line and reformat the message.
//!
//! Validates: a subject is present and within the subject limit; it carries a
//! conventional type prefix; a description follows, within the description
//! budget. Formats: place the type's gitmoji where the repository asked for it,
//! hard-wrap the body, and group the trailing footers with one blank line
//! before them.
//!
//! Every limit and the emoji placement are [`commit_style`] settings — four
//! `git config` keys with shipped defaults. What this hook enforces is
//! configurable; **that** it enforces is not.
//!
//! Two invariants hold across all four gitmoji placements:
//!
//! * **The limits measure what you wrote.** Decoration this hook added is
//!   removed before anything is counted, so the emoji can never eat the budget
//!   — and re-checking an already-decorated subject counts the same characters
//!   the author was told about the first time.
//! * **Re-running is a no-op.** `--amend`, a rebase reword and a `--no-verify`
//!   retry all hand this hook a subject it already wrote. See [`undecorate`].
//!
//! Ported from ~190 lines of JS. The one structural simplification is how the
//! optional leading emoji is recognised — see `split_leading_emoji`.

use crate::check::Verdict;
use crate::commit_style::{self, Style};
use crate::ui::{error_sign, highlight, valid_sign};

use crate::vocabulary::{self, COMMIT_TYPES};

pub struct Subject {
    pub prefix: String,
    pub scope: String,
    pub breaking: String,
    pub description: String,
}

/// Drop full-line comments, as git itself does.
pub fn strip_comments(msg: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for line in msg.split('\n') {
        if !line.starts_with('#') {
            out.push(line);
        }
    }
    out.join("\n")
}

/// Skip a leading emoji cluster.
///
/// The JS carried a ~2KB hand-maintained list of emoji codepoints, and had
/// already been patched once because it matched only the BASE codepoint —
/// leaving a stray variation selector (U+FE0F) between the emoji and the type,
/// so a perfectly good `⬆️ chore: …` was rejected as having no prefix.
///
/// Inverting the test removes that whole class of bug: a conventional type is
/// ASCII lowercase letters, so skip anything that is NOT ASCII, plus spaces.
/// Strictly more permissive than the codepoint list, and permissive in the
/// harmless direction — the type itself is still required below, so this only
/// decides how much leading decoration gets stripped before re-adding ours.
pub fn split_leading_emoji(subject: &str) -> &str {
    subject.trim_start_matches(|c: char| !c.is_ascii() || c == ' ' || c == '\t')
}

/// `^\s*(emoji)?\s*(type)(\(scope\))?(!)?:\s*(.*)$` over the FIRST line only.
///
/// First line only is load-bearing: the JS once used /ms flags, so `^` matched
/// any line start and a body quoting a conventional commit (a revert citing the
/// commit it undid) was picked up as the subject and rewritten into it.
pub fn parse_subject(subject_line: &str) -> Option<Subject> {
    let rest = split_leading_emoji(subject_line);
    let (prefix, rest) = COMMIT_TYPES
        .iter()
        .map(|t| t.name)
        .find(|t| rest.starts_with(t))
        .map(|t| (t.to_string(), &rest[t.len()..]))?;
    let (scope, breaking, description) = parse_tail(rest)?;
    Some(Subject {
        prefix,
        scope,
        breaking,
        description,
    })
}

/// `(\(scope\))?(!)?:\s*(.*)` — everything after the type word.
///
/// Split out because the type is not always a word: under the `replace`
/// gitmoji placement it is an emoji, and re-reading such a subject means
/// recovering the type from the emoji and then parsing exactly this tail. One
/// implementation, so a scoped, breaking subject survives an amend the same way
/// an unscoped one does.
fn parse_tail(rest: &str) -> Option<(String, String, String)> {
    let (scope, rest) = if let Some(after) = rest.strip_prefix('(') {
        let end = after.find(')')?;
        let inner = &after[..end];
        if inner.is_empty()
            || !inner
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return None;
        }
        (format!("({inner})"), &after[end + 1..])
    } else {
        (String::new(), rest)
    };

    let (breaking, rest) = match rest.strip_prefix('!') {
        Some(r) => ("!".to_string(), r),
        None => (String::new(), rest),
    };

    let description = rest.strip_prefix(':')?.trim_start_matches(' ').to_string();
    Some((scope, breaking, description))
}

/// A subject with the decoration this hook itself applied taken back off.
pub struct Undecorated<'a> {
    /// The type recovered from a leading emoji, when that emoji is one of ours.
    ///
    /// `Some` only matters for the `replace` placement, where the stored
    /// subject carries its type nowhere else. Recovering it is what stops the
    /// hook rejecting its own output on the next `--amend`.
    pub recovered_type: Option<&'static str>,
    /// The subject as the author wrote it — what every limit is measured on.
    pub text: &'a str,
}

/// Take off a leading gitmoji that this hook wrote.
///
/// Only an emoji from `COMMIT_TYPES` counts, matched whole. An emoji the author
/// chose is theirs: it stays in the text and it counts against the subject
/// limit, because the limit is about what they wrote. `split_leading_emoji`
/// remains deliberately more permissive for *parsing* — this is about
/// *measuring*, and the two questions want different answers.
pub fn undecorate(subject_line: &str) -> Undecorated<'_> {
    let trimmed = subject_line.trim_start();
    for t in COMMIT_TYPES {
        if let Some(rest) = trimmed.strip_prefix(t.emoji) {
            return Undecorated {
                recovered_type: Some(t.name),
                text: rest.trim_start(),
            };
        }
    }
    Undecorated {
        recovered_type: None,
        text: subject_line,
    }
}

/// Take off a trailing gitmoji that this hook wrote — the `suffix` placement's
/// half of [`undecorate`].
///
/// Matched against the emoji for *this* type only, so `feat: ship it 🚀` keeps
/// the rocket the author chose while `feat: ship it ✨` gives back `ship it`.
fn undecorate_tail<'a>(description: &'a str, emoji: &str) -> &'a str {
    if emoji.is_empty() {
        return description;
    }
    match description.trim_end().strip_suffix(emoji) {
        Some(rest) => rest.trim_end(),
        None => description,
    }
}

/// The subject a `replace`-decorated line describes: type from the emoji,
/// everything else parsed from what followed it.
fn recovered_subject(prefix: &'static str, text: &str) -> Subject {
    let (scope, breaking, description) = parse_tail(text).unwrap_or_else(|| {
        // No `…:` after the emoji, so there is no scope to find and the whole
        // remainder is the description — `✨  add a cart`, the common shape.
        (String::new(), String::new(), text.to_string())
    });
    Subject {
        prefix: prefix.to_string(),
        scope,
        breaking,
        description,
    }
}

/// Greedy hard wrap at `width`, breaking on spaces — the JS
/// `(?![^\n]{1,w}$)([^\n]{1,w})\s` replace. A word longer than `width` is left
/// intact rather than split.
pub fn wrap(text: &str, width: usize) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in text.split('\n') {
        if line.chars().count() <= width {
            out.push(line.to_string());
            continue;
        }
        let mut current = String::new();
        for word in line.split(' ') {
            if current.is_empty() {
                current.push_str(word);
            } else if current.chars().count() + 1 + word.chars().count() <= width {
                current.push(' ');
                current.push_str(word);
            } else {
                out.push(std::mem::take(&mut current));
                current.push_str(word);
            }
        }
        if !current.is_empty() {
            out.push(current);
        }
    }
    out.join("\n")
}

/// A trailing footer line: `Key-Word: value`, `BREAKING CHANGE: …`, `Refs: #1`,
/// or blank.
pub fn is_footer(line: &str) -> bool {
    if line.is_empty() {
        return true;
    }
    if let Some(rest) = line
        .strip_prefix("BREAKING CHANGE:")
        .or_else(|| line.strip_prefix("BREAKING-CHANGE:"))
    {
        return rest.starts_with(' ')
            && rest
                .trim_start()
                .starts_with(|c: char| c.is_alphanumeric() || c == '_');
    }
    // The bare (no-colon) form still needs a SEPARATOR after "Refs" — a
    // space or a '#' — or it also matches prose that merely starts with
    // those five letters glued to a digit, like "Refs42 was the original
    // ticket.", sweeping a body line into the footer group.
    if let Some(rest) = line.strip_prefix("Refs:").or_else(|| {
        line.strip_prefix("Refs")
            .filter(|rest| rest.starts_with(' ') || rest.starts_with('#'))
    }) {
        let r = rest.trim_start_matches(' ');
        let r = r.strip_prefix('#').unwrap_or(r);
        if r.starts_with(|c: char| c.is_ascii_digit()) {
            return true;
        }
    }
    is_hyphenated_key(line)
}

/// `^[\w][\w-]*-[\w-]*\w: \w` — a hyphenated trailer key that STARTS the line.
///
/// The anchor is the whole point. The rule used to be the JS regex
/// `/\w-\w{1,}:\s\w/` applied ANYWHERE in the line, and this repo's own commit
/// subjects match it: the formatted subject
///
/// ```text
/// 🐛  fix: pre-commit: stop hanging
/// ```
///
/// contains the fragment `pre-commit: s`, which satisfied `\w-\w+: \w`. So the
/// SUBJECT read as a footer, `group_footer` walked the whole message from the
/// bottom without ever hitting a non-footer line, and split at index 0 — the
/// emitted message became `["", subject, trailer…]`. git strips the leading
/// blank, which leaves the subject glued to the trailer block with no blank
/// line between them, and `%(trailers)` returns EMPTY for a commit that plainly
/// carries a `Co-Authored-By`. Silent, because the hook still exits 0.
///
/// Anchoring at column 0 keeps every real trailer (`Co-Authored-By: x`,
/// `Signed-off-by: y`) and rejects both `fix: pre-commit: stop hanging` and
/// prose like `see the pre-commit: docs above`, because in each of those the
/// leading key run stops at the first space and holds no hyphen.
fn is_hyphenated_key(line: &str) -> bool {
    let key: String = line
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    // A key starts and ends with a word character — `-foo: x` is a bulleted
    // list item, and `A-: x` was never a trailer either.
    if !key.starts_with(|c: char| c.is_alphanumeric() || c == '_')
        || !key.ends_with(|c: char| c.is_alphanumeric() || c == '_')
        || !key.contains('-')
    {
        return false;
    }
    let Some(rest) = line[key.len()..].strip_prefix(": ") else {
        return false;
    };
    rest.starts_with(|c: char| c.is_alphanumeric() || c == '_')
}

/// Separate the trailing run of footer lines, drop blanks inside it, and put
/// exactly one blank line before the group.
///
/// The run deliberately crosses BLANK lines, which is what merges a
/// `BREAKING CHANGE:` paragraph and a `Co-Authored-By:` paragraph into one
/// footer block. That is a feature, not an accident, so this is not a
/// "last paragraph only" rule.
///
/// The scan starts at `lines[1..]`: line 0 is the SUBJECT and can never be a
/// footer, whatever it happens to look like. Without that anchor a subject
/// misread as a footer (see `is_hyphenated_key`) let the run consume the entire
/// message, `split_at` became 0, and the subject was emitted INSIDE the footer
/// group with a blank line in front of it — destroying every trailer in the
/// commit. `lines[1..]` makes `split_at >= 1` by construction; `split('\n')`
/// never yields an empty vector, so the slice is always in range, and a
/// single-line message falls out of the general path producing exactly what it
/// produced before (`out = [subject, ""]`).
pub fn group_footer(text: &str) -> String {
    let trimmed = text.trim_end_matches('\n');
    let lines: Vec<&str> = trimmed.split('\n').collect();
    let mut footer_size = 0;
    for line in lines[1..].iter().rev() {
        if is_footer(line) {
            footer_size += 1;
        } else {
            break;
        }
    }
    let split_at = lines.len() - footer_size;
    let body = &lines[..split_at];
    let footer: Vec<&str> = lines[split_at..]
        .iter()
        .copied()
        .filter(|l| !l.is_empty())
        .collect();

    let mut out: Vec<&str> = body.to_vec();
    out.push("");
    out.extend(footer);
    format!("{}\n", out.join("\n"))
}

fn valid(msg: &str) {
    println!("  {} {msg}", valid_sign().trim());
}
fn error(msg: &str) {
    eprintln!("  {} {msg}", error_sign().trim());
}
fn orange(s: &str) -> String {
    highlight(s)
}

/// A subject git itself wrote, or one that exists only to be autosquashed
/// away.
///
/// Exact prefixes, space and quote included, so a HUMAN subject like
/// "Merges: cleanup" or "fixup the parser" is still judged — only the shapes
/// git's own porcelain emits stand aside. The trade is that a hand-written
/// "Merge the two configs" passes unjudged; commitlint draws the same line,
/// and the alternative blocks `git merge` itself.
fn git_generated(subject: &str) -> bool {
    [
        "Merge ",
        "Revert \"",
        "Reapply \"",
        "fixup! ",
        "squash! ",
        "amend! ",
    ]
    .iter()
    .any(|p| subject.starts_with(p))
}

pub fn run(args: &[std::ffi::OsString]) -> Verdict {
    let Some(filename) = args.first().and_then(|a| a.to_str()) else {
        println!("Usage:\n\n./commit-msg <filename>");
        return Verdict::Block;
    };
    let Ok(raw) = std::fs::read_to_string(filename) else {
        return Verdict::Block;
    };
    let style = Style::resolve();
    let cleaned = strip_comments(&raw);
    let mut parts = cleaned.splitn(2, '\n');
    let subject_line = parts.next().unwrap_or("");

    // Messages GIT writes are passed through, not judged. `git merge` invokes
    // this hook (githooks(5)) with "Merge branch '…'"; `git revert` writes
    // `Revert "…"` (and `Reapply "…"` for a revert of a revert); `--fixup`
    // and `--squash` write `fixup!`/`squash!`/`amend!` subjects that exist
    // only to be autosquashed away before anyone reads them. None of these
    // can carry a conventional type — blocking them blocks the porcelain
    // that produced them, and the workaround people reach for is
    // `--no-verify`, which turns off the checks that DO apply to them.
    // Said out loud, because a check that stands aside silently is the
    // invisibility this project refuses everywhere else.
    if git_generated(subject_line) {
        valid("A message git itself wrote — the convention is not applied");
        return Verdict::Proceed;
    }
    // Everything after the subject's own newline — blank separator lines
    // included. The format string below writes that blank line itself, so
    // leaving them here means writing one MORE each time.
    //
    // That is not hypothetical: it shipped. Re-running the hook over a message
    // it had already formatted — `--amend`, a rebase reword, a `--no-verify`
    // retry — grew the gap between subject and body by one line every single
    // time. Nothing caught it because the idempotence test covered
    // `group_footer` alone rather than the whole rewrite.
    let body = parts.next().unwrap_or("").trim_start_matches('\n');

    // Our own decoration comes off before anything is counted or parsed. On a
    // first commit there is none; on an amend there is, and measuring it would
    // fail a subject that was accepted five seconds earlier.
    let undecorated = undecorate(subject_line);
    let written = undecorated.text;

    if written.is_empty() || written.chars().count() > style.subject_max {
        error(&format!(
            "Commit's first line should exist and be at most {} characters.",
            orange(&style.subject_max.to_string())
        ));
        return Verdict::Block;
    }
    valid(&format!(
        "Summary size is at most {} characters",
        orange(&style.subject_max.to_string())
    ));

    let types: Vec<String> = COMMIT_TYPES.iter().map(|t| orange(t.name)).collect();
    let subject = match parse_subject(written) {
        Some(s) => s,
        // No type in the text — but if a gitmoji of ours opened the line, the
        // type IS there, carried by the emoji. That is the `replace` placement
        // being handed back its own output.
        None => match undecorated.recovered_type {
            Some(t) => recovered_subject(t, written),
            None => {
                error(&format!(
                    "Commits MUST be prefixed with a type, which consists of a noun:
    {}
    The prefix must be followed by the OPTIONAL scope, OPTIONAL !,
    and REQUIRED terminal colon and space.
    A scope MAY be provided after a type. A scope MUST consist of a noun describing
    a section of the codebase surrounded by parenthesis, e.g., fix(parser)",
                    types.join(", ")
                ));
                return Verdict::Block;
            }
        },
    };
    valid("A prefix is defined");

    // The `suffix` placement's half of the same round trip.
    let description = undecorate_tail(&subject.description, vocabulary::emoji_for(&subject.prefix));

    if description.is_empty() {
        error(&format!(
            "A description MUST immediately follow the {} and {} after the type/scope prefix.
    The description is a short summary of the code changes, e.g., fix: array parsing issue when multiple spaces were contained in string.",
            orange("colon"), orange("space")
        ));
        return Verdict::Block;
    }
    valid("A description is present in the summary");

    if description.chars().count() > style.description_max {
        error(&format!(
            "The description after the {} should be at most {} characters.",
            orange("colon"),
            orange(&style.description_max.to_string())
        ));
        return Verdict::Block;
    }
    valid(&format!(
        "Description size is at most {} characters",
        orange(&style.description_max.to_string())
    ));

    let formatted = format!(
        "{}\n\n{}\n",
        commit_style::render_subject(
            style.gitmoji,
            &subject.prefix,
            &subject.scope,
            &subject.breaking,
            description,
        ),
        wrap_body(&strip_comments(body), style.body_wrap)
    );
    if std::fs::write(filename, group_footer(&formatted)).is_err() {
        return Verdict::Block;
    }
    Verdict::Proceed
}

/// The body, wrapped — or left exactly as written when the wrap column is `0`.
///
/// `0` is what keeps a pasted stack trace, a table or a fenced code block
/// intact. Hard-wrapping those is the one thing this hook does that cannot be
/// undone by reading the message again.
fn wrap_body(body: &str, column: usize) -> String {
    if column == 0 {
        body.to_string()
    } else {
        wrap(body, column)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_conventional_shapes() {
        let s = parse_subject("feat: add a thing").unwrap();
        assert_eq!(
            (s.prefix.as_str(), s.description.as_str()),
            ("feat", "add a thing")
        );

        let s = parse_subject("fix(parser): trim").unwrap();
        assert_eq!(s.scope, "(parser)");

        let s = parse_subject("fix(my-scope): trim").unwrap();
        assert_eq!(s.scope, "(my-scope)");

        let s = parse_subject("feat!: breaking").unwrap();
        assert_eq!(s.breaking, "!");
    }

    /// The bug the JS was patched for: only the BASE codepoint was consumed, so
    /// the trailing U+FE0F sat between emoji and type and the subject failed to
    /// match. Every one of these must parse.
    #[test]
    fn accepts_emoji_prefixes_including_multi_codepoint_ones() {
        for subject in [
            "✨ feat: x",
            "⬆️ chore: x",     // variation selector
            "♻️  refactor: x", // variation selector + two spaces
            "🔧  chore: x",
            "👨‍💻 feat: x", // ZWJ sequence
        ] {
            assert!(parse_subject(subject).is_some(), "failed: {subject}");
        }
    }

    #[test]
    fn rejects_what_is_not_a_conventional_subject() {
        assert!(parse_subject("just a message").is_none());
        assert!(parse_subject("feat add a thing").is_none()); // no colon
        assert!(parse_subject("feature: x").is_none()); // unknown type
        assert!(parse_subject("fix(bad scope): x").is_none()); // space in scope
    }

    #[test]
    fn description_may_be_empty_and_is_caught_by_the_caller() {
        assert_eq!(parse_subject("feat:").unwrap().description, "");
    }

    #[test]
    fn wraps_on_spaces_without_splitting_long_words() {
        let wrapped = wrap("aaa bbb ccc ddd", 7);
        assert_eq!(wrapped, "aaa bbb\nccc ddd");
        let long = "x".repeat(20);
        assert_eq!(wrap(&long, 7), long); // never split mid-word
    }

    #[test]
    fn recognises_footers() {
        assert!(is_footer("Co-Authored-By: someone"));
        assert!(is_footer("BREAKING CHANGE: it broke"));
        assert!(is_footer("Refs: #123"));
        assert!(is_footer(""));
        assert!(!is_footer("just prose"));
        assert!(!is_footer("a sentence with - a dash"));
    }

    /// The bare (no-colon) form, `Refs #123` / `Refs 123`, is intentionally
    /// also accepted — but "Refs" glued straight to a digit with no
    /// separator is prose, not a reference, and must not be swept into the
    /// footer group.
    #[test]
    fn a_bare_refs_needs_a_separator_not_just_a_leading_digit() {
        assert!(is_footer("Refs #123"));
        assert!(is_footer("Refs 123"));
        assert!(
            !is_footer("Refs42 was the original ticket."),
            "prose starting with Refs+digit must not read as a footer"
        );
    }

    /// A trailer key is only a trailer key when it STARTS its line.
    ///
    /// The formatted subject `fix: pre-commit: stop hanging` contains
    /// `pre-commit: s`, which the old anywhere-in-the-line rule accepted — and
    /// a subject read as a footer took the whole message down with it.
    #[test]
    fn a_key_must_start_the_line_to_be_a_footer() {
        // Real trailers, at column 0.
        assert!(is_footer("Co-Authored-By: someone"));
        assert!(is_footer("Signed-off-by: someone"));
        assert!(is_footer("Reviewed-by: a"));
        // The subject shape this repo writes constantly.
        assert!(!is_footer("fix: pre-commit: stop hanging"));
        assert!(!is_footer("🐛  fix: pre-commit: stop hanging"));
        // Prose that merely mentions a hyphenated word followed by a colon.
        assert!(!is_footer("see the pre-commit: docs above"));
        assert!(!is_footer("  Co-Authored-By: indented is not a trailer"));
        // A bullet is not a key, and neither is a key with nothing after the
        // hyphen.
        assert!(!is_footer("-foo: bar"));
        assert!(!is_footer("A-: bar"));
        // Neither branch below is anchored by this rule; both still work.
        assert!(is_footer("BREAKING CHANGE: it broke"));
        assert!(is_footer("Refs: #123"));
    }

    #[test]
    fn groups_the_trailing_footer_with_one_blank_line() {
        let out = group_footer("subject\n\nbody text\n\nCo-Authored-By: x\n\n");
        assert_eq!(out, "subject\n\nbody text\n\nCo-Authored-By: x\n");
    }

    /// The shapes a real message arrives in, for the property tests below.
    ///
    /// Every subject here is one that the old anywhere-in-the-line footer rule
    /// misread as a trailer, crossed with each body/footer arrangement the
    /// formatter emits.
    const SHAPES: &[&str] = &[
        // subject only
        "fix: pre-commit: stop hanging",
        "fix: pre-commit: stop hanging\n\n\n",
        // body only
        "fix: pre-commit: stop hanging\n\nthe worker thread blocked on a tty\n",
        // trailers only
        "fix: pre-commit: stop hanging\n\nCo-Authored-By: a <a@x>\n",
        // body and trailers
        "fix: pre-commit: stop hanging\n\nthe worker thread blocked\n\nCo-Authored-By: a <a@x>\n",
        // body, trailers, trailing blanks
        "fix: pre-commit: stop hanging\n\nthe worker thread blocked\n\nCo-Authored-By: a <a@x>\n\n\n",
        // two footer PARAGRAPHS, which the run deliberately merges
        "feat: x\n\nbody\n\nBREAKING CHANGE: it broke\n\nCo-Authored-By: a <a@x>\n",
        // an ordinary subject, to prove nothing regressed for the common case
        "feat: add a thing\n\nbody\n\nCo-Authored-By: a <a@x>\n",
    ];

    /// PROPERTY: grouping the footer never drops or invents a line, and never
    /// moves the subject off line 0.
    ///
    /// Both halves failed together for the subject `fix: pre-commit: stop
    /// hanging`: the whole message was swallowed into the footer group, blank
    /// body lines inside it were filtered away, and the emitted line 0 was the
    /// inserted blank rather than the subject.
    #[test]
    fn group_footer_never_loses_a_line() {
        for shape in SHAPES {
            let out = group_footer(shape);

            let mut before: Vec<&str> = shape
                .trim_end_matches('\n')
                .split('\n')
                .filter(|l| !l.is_empty())
                .collect();
            let mut after: Vec<&str> = out
                .trim_end_matches('\n')
                .split('\n')
                .filter(|l| !l.is_empty())
                .collect();
            before.sort_unstable();
            after.sort_unstable();
            assert_eq!(before, after, "lines changed for {shape:?} -> {out:?}");

            assert_eq!(
                out.split('\n').next(),
                shape.split('\n').next(),
                "the subject left line 0 for {shape:?} -> {out:?}"
            );
        }
    }

    /// PROPERTY: grouping is idempotent. A message that has already been
    /// formatted once — an amend, a rebase reword, a `--no-verify` retry — must
    /// come back byte for byte.
    #[test]
    fn group_footer_is_idempotent() {
        for shape in SHAPES {
            let once = group_footer(shape);
            let twice = group_footer(&once);
            assert_eq!(once, twice, "not idempotent for {shape:?}");
        }
    }

    /// The exact damage the anchor prevents: a blank line must stand between
    /// the subject and the footer group, and the subject must never appear
    /// inside it. Without the blank line git reads the trailer as a
    /// continuation of the subject and `%(trailers)` comes back empty.
    #[test]
    fn a_subject_and_its_trailers_stay_separated() {
        let out = group_footer("fix: pre-commit: stop hanging\n\nCo-Authored-By: a <a@x>\n");
        assert_eq!(
            out, "fix: pre-commit: stop hanging\n\nCo-Authored-By: a <a@x>\n",
            "got: {out:?}"
        );
        assert!(
            !out.starts_with('\n'),
            "the message must not begin with a blank line: {out:?}"
        );
    }

    #[test]
    fn strips_comment_lines() {
        assert_eq!(strip_comments("keep\n# drop\nkeep2"), "keep\nkeep2");
    }

    use crate::commit_style::{render_subject, Gitmoji};

    /// The whole subject, as the hook would store it, for a message the author
    /// typed conventionally.
    fn store(placement: Gitmoji, typed: &str) -> String {
        let s = parse_subject(typed).expect("test subjects parse");
        render_subject(
            placement,
            &s.prefix,
            &s.scope,
            &s.breaking,
            undecorate_tail(&s.description, vocabulary::emoji_for(&s.prefix)),
        )
    }

    /// Re-read a stored subject the way `run` does, and store it again.
    fn restore(placement: Gitmoji, stored: &str) -> String {
        let u = undecorate(stored);
        let s = match parse_subject(u.text) {
            Some(s) => s,
            None => recovered_subject(u.recovered_type.expect("a type to recover"), u.text),
        };
        render_subject(
            placement,
            &s.prefix,
            &s.scope,
            &s.breaking,
            undecorate_tail(&s.description, vocabulary::emoji_for(&s.prefix)),
        )
    }

    /// PROPERTY: writing a subject twice writes the same subject.
    ///
    /// `--amend`, a rebase reword and a `--no-verify` retry all hand this hook
    /// a line it wrote itself. Without the undecorate step `suffix` grew an
    /// emoji per amend and `replace` REJECTED its own output — the type it
    /// demands had been replaced by the emoji it wrote.
    #[test]
    fn decorating_a_subject_is_idempotent() {
        for typed in [
            "feat: add a cart",
            "fix(parser): trim",
            "feat(api)!: drop v1",
            "docs: explain the trust model",
        ] {
            for placement in Gitmoji::ALL {
                let once = store(placement, typed);
                let twice = restore(placement, &once);
                assert_eq!(
                    once,
                    twice,
                    "{} is not idempotent for {typed:?}",
                    placement.as_str()
                );
                // And a third pass, since `suffix` grew by one emoji per run.
                assert_eq!(twice, restore(placement, &twice));
            }
        }
    }

    /// Each placement puts the emoji where it says it does, and `none` leaves
    /// the line alone.
    #[test]
    fn each_placement_puts_the_emoji_where_it_says() {
        assert_eq!(store(Gitmoji::None, "feat: add a cart"), "feat: add a cart");
        assert_eq!(
            store(Gitmoji::Prefix, "feat: add a cart"),
            "✨  feat: add a cart"
        );
        assert_eq!(
            store(Gitmoji::Suffix, "feat: add a cart"),
            "feat: add a cart ✨"
        );
        assert_eq!(
            store(Gitmoji::Replace, "feat: add a cart"),
            "✨  add a cart"
        );
    }

    /// `suffix` keeps a clean conventional subject at the START of the line,
    /// which is the reason to prefer it: commitlint and changelog generators
    /// still see the type. `replace` deliberately does not, and the docs say so.
    #[test]
    fn suffix_leaves_the_type_where_tooling_looks_for_it() {
        assert!(store(Gitmoji::Suffix, "fix: a bug").starts_with("fix:"));
        assert!(!store(Gitmoji::Replace, "fix: a bug").starts_with("fix:"));
    }

    /// A scope and a breaking marker are not types, so `replace` keeps them.
    /// They must also survive the round trip, which is why the recovery parses
    /// the tail rather than treating everything after the emoji as prose.
    #[test]
    fn replace_keeps_a_scope_and_a_breaking_marker() {
        let stored = store(Gitmoji::Replace, "feat(api)!: drop v1");
        assert_eq!(stored, "✨  (api)!: drop v1");
        let u = undecorate(&stored);
        let s = recovered_subject(u.recovered_type.unwrap(), u.text);
        assert_eq!(
            (s.prefix.as_str(), s.scope.as_str(), s.breaking.as_str()),
            ("feat", "(api)", "!")
        );
        assert_eq!(s.description, "drop v1");
    }

    /// The type is recovered from OUR emoji only. One the author chose is
    /// theirs, and a subject carrying it still needs a real type word.
    #[test]
    fn only_our_own_emoji_recovers_a_type() {
        assert_eq!(undecorate("✨  add a cart").recovered_type, Some("feat"));
        assert_eq!(undecorate("🐛  fix: x").recovered_type, Some("fix"));
        assert_eq!(undecorate("🚀 ship it").recovered_type, None);
        assert_eq!(undecorate("feat: x").recovered_type, None);
        // The text handed on is what remains once ours is off.
        assert_eq!(undecorate("✨  add a cart").text, "add a cart");
        assert_eq!(undecorate("🚀 ship it").text, "🚀 ship it");
    }

    /// The trailing half: only the emoji for THIS type is ours to remove.
    #[test]
    fn a_trailing_emoji_is_only_stripped_when_we_wrote_it() {
        assert_eq!(undecorate_tail("add a cart ✨", "✨"), "add a cart");
        assert_eq!(undecorate_tail("ship it 🚀", "✨"), "ship it 🚀");
        assert_eq!(undecorate_tail("plain", "✨"), "plain");
        assert_eq!(undecorate_tail("nothing to strip", ""), "nothing to strip");
    }

    /// PROPERTY: the limits measure what the author wrote.
    ///
    /// A subject at exactly the limit must stay acceptable after decoration —
    /// otherwise the first amend of a maximal subject is rejected for length
    /// the hook itself added.
    #[test]
    fn decoration_never_counts_against_the_limit() {
        let typed = format!("feat: {}", "x".repeat(60));
        assert_eq!(typed.chars().count(), 66);
        for placement in Gitmoji::ALL {
            let stored = store(placement, &typed);
            let remeasured = undecorate(&stored);
            let s = match parse_subject(remeasured.text) {
                Some(s) => s,
                None => recovered_subject(remeasured.recovered_type.unwrap(), remeasured.text),
            };
            let description = undecorate_tail(&s.description, vocabulary::emoji_for(&s.prefix));
            assert_eq!(
                description.chars().count(),
                60,
                "{} changed the measured description: {stored:?}",
                placement.as_str()
            );
        }
    }

    #[test]
    fn a_zero_wrap_column_leaves_the_body_alone() {
        let long = "x ".repeat(100);
        assert_eq!(wrap_body(&long, 0), long);
        assert!(wrap_body(&long, 72).contains('\n'));
    }
}
