//! One problem, in one place, in one file.
//!
//! [`Outcome`](crate::check::Outcome) answers what a CHECK concluded, in five
//! variants and no payload. That is the right shape for git — a hook has to
//! resolve to proceed-or-block — but it is the reason a report could only ever
//! name the file:
//!
//! ```text
//! ✗ Unwanted terms found
//!   The following files contains 'debugger' in them:
//!   - app.js
//! ```
//!
//! The line was known. `ban_terms`'s comment-blanking preserves length and line
//! count precisely so offsets stay valid, and `secrets::scan` has always
//! returned line numbers — but with nowhere to put them, the position was
//! computed and dropped at the boundary. This type is that place.
//!
//! Positions are a REPORTING concern and never a decision input. A check
//! decides pass or fail exactly as it did before; a finding says where. Where a
//! position cannot be pinned down — a whole-file judgement like `large-files`,
//! or a match the locator cannot resolve to one offset — [`Finding::line`] is
//! `None` and the report degrades to naming the file, which is what it did for
//! everything until now.

use crate::check::Severity;

/// The one spelling of a severity for consumers. `error`/`warning` rather than
/// `block`/`warn` because this crosses a boundary into other people's tools,
/// where those two words already mean this.
fn severity_word(severity: Severity) -> &'static str {
    match severity {
        Severity::Block => "error",
        Severity::Warn => "warning",
    }
}

/// A single problem, addressable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The check's short name — `ban-terms`, not `pre-commit-ban-terms`. The
    /// stage is not a property of the finding: the same content problem is the
    /// same problem whichever hook noticed it, and an editor has no stages.
    pub check: &'static str,
    /// Repository-relative, as git spells it.
    pub file: String,
    /// 1-based, as every editor and terminal counts. `None` when the finding is
    /// about the file as a whole.
    pub line: Option<usize>,
    /// 1-based, counted in CHARACTERS rather than bytes — see [`line_col`].
    pub column: Option<usize>,
    pub severity: Severity,
    /// One line, no trailing period, safe to print. Anything derived from file
    /// content must be sanitised by its producer before it gets here: this type
    /// will happily print whatever it is given, and `secrets` in particular
    /// must never put the matched text in it.
    pub message: String,
}

impl Finding {
    pub fn new(
        check: &'static str,
        file: impl Into<String>,
        severity: Severity,
        message: impl Into<String>,
    ) -> Self {
        Finding {
            check,
            file: file.into(),
            line: None,
            column: None,
            severity,
            message: message.into(),
        }
    }

    /// Place this finding at a byte offset within `content`.
    pub fn at_offset(mut self, content: &str, offset: usize) -> Self {
        let (line, column) = line_col(content, offset);
        self.line = Some(line);
        self.column = Some(column);
        self
    }

    /// Place it on a line whose column is not known or not meaningful.
    pub fn at_line(mut self, line: usize) -> Self {
        self.line = Some(line);
        self
    }

    /// `file:line:col` — just the address.
    ///
    /// Used inside a hook's own report, which already says which check spoke
    /// and how loudly; repeating either there would be noise. `render` is the
    /// form for anything outside amont.
    pub fn location(&self) -> String {
        let mut out = self.file.clone();
        if let Some(line) = self.line {
            out.push_str(&format!(":{line}"));
            if let Some(column) = self.column {
                out.push_str(&format!(":{column}"));
            }
        }
        out
    }

    /// `file:line:col: severity: message [check]`
    ///
    /// The shape every editor's error parser already understands, and which
    /// every modern terminal turns into a clickable link. That is the whole
    /// reason for choosing it over something prettier: it needs no adapter.
    pub fn render(&self) -> String {
        let mut out = self.file.clone();
        if let Some(line) = self.line {
            out.push_str(&format!(":{line}"));
            if let Some(column) = self.column {
                out.push_str(&format!(":{column}"));
            }
        }
        format!(
            "{out}: {}: {} [{}]",
            severity_word(self.severity),
            self.message,
            self.check
        )
    }
}

/// A byte offset into 1-based (line, column).
///
/// The column counts CHARACTERS, not bytes. Editors place a cursor by
/// character, so a byte column puts the caret in the wrong place on any line
/// containing a non-ASCII character — an accented identifier, a comment in a
/// language that is not English, an emoji in a string. Byte offsets are what
/// the matchers produce and characters are what the reader wants, so the
/// conversion belongs here rather than at each call site.
///
/// An offset past the end clamps to the last position rather than panicking:
/// this is a reporting path, and a wrong column is a far better outcome than
/// taking down a commit hook.
pub fn line_col(content: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(content.len());
    let before = &content[..offset];
    let line = before.matches('\n').count() + 1;
    let line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let column = content[line_start..offset].chars().count() + 1;
    (line, column)
}

/// Findings as JSON, for anything that would rather not parse a line.
///
/// Hand-rolled to keep the commit path dependency-free, and versioned like
/// `amont-list-v1` for the same reason: a consumer that does not recognise the
/// format string should say so rather than guess at the shape.
pub fn to_json(findings: &[Finding]) -> String {
    let items: Vec<String> = findings
        .iter()
        .map(|f| {
            format!(
                "{{{},{},{},{},{},{}}}",
                crate::json::string_field("check", f.check),
                crate::json::string_field("file", &f.file),
                crate::json::opt_int_field("line", f.line.map(|l| l as i64)),
                crate::json::opt_int_field("column", f.column.map(|c| c as i64)),
                crate::json::string_field("severity", severity_word(f.severity)),
                crate::json::string_field("message", &f.message),
            )
        })
        .collect();
    format!(
        "{{{},\"findings\":[{}]}}",
        crate::json::string_field("format", "amont-check-v1"),
        items.join(",")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_become_one_based_positions() {
        let src = "abc\ndef\nghi";
        assert_eq!(line_col(src, 0), (1, 1));
        assert_eq!(line_col(src, 2), (1, 3));
        assert_eq!(line_col(src, 4), (2, 1)); // first char of line 2
        assert_eq!(line_col(src, 8), (3, 1));
    }

    /// The reason the column counts characters. With a byte column the caret
    /// lands mid-codepoint and the editor highlights the wrong token.
    #[test]
    fn the_column_counts_characters_not_bytes() {
        let src = "let café = dbg!(1);";
        let at = src.find("dbg!").unwrap();
        assert_eq!(at, 12); // twelve BYTES in, because é is two
        assert_eq!(line_col(src, at), (1, 12)); // but eleven characters
    }

    /// A reporting path must not be able to panic a commit hook.
    #[test]
    fn an_offset_past_the_end_clamps() {
        let src = "abc";
        assert_eq!(line_col(src, 99), (1, 4));
        assert_eq!(line_col("", 5), (1, 1));
    }

    #[test]
    fn render_degrades_when_there_is_no_position() {
        let whole = Finding::new("large-files", "big.bin", Severity::Block, "12 MB");
        assert_eq!(whole.render(), "big.bin: error: 12 MB [large-files]");
        let placed = Finding::new("ban-terms", "app.js", Severity::Block, "'debugger'")
            .at_offset("a\nb\ndebugger;", 4);
        assert_eq!(placed.render(), "app.js:3:1: error: 'debugger' [ban-terms]");
        let lined = Finding::new("secrets", "k.pem", Severity::Warn, "a private key").at_line(7);
        assert_eq!(lined.render(), "k.pem:7: warning: a private key [secrets]");
    }

    #[test]
    fn json_is_versioned_and_escapes_its_input() {
        assert_eq!(
            to_json(&[]),
            "{\"format\":\"amont-check-v1\",\"findings\":[]}"
        );
        let f = Finding::new("ban-terms", "a\"b.js", Severity::Block, "x").at_line(2);
        let json = to_json(&[f]);
        assert!(json.contains(r#""file":"a\"b.js""#), "{json}");
        assert!(json.contains(r#""line":2,"column":null"#), "{json}");
    }
}
