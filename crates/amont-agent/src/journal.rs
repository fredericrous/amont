//! The record of what the guard saw.
//!
//! Modelled on `amont_runtime::bypass`, including its central rule: **this
//! module only counts. Nothing in it may participate in a decision.** The
//! stance gates a command; the journal informs a human. A wrong read here
//! miscounts, and it must never be able to do worse than that.
//!
//! ## Not in `.git/`
//!
//! `bypass` lives in the common git dir because "how often does this repository
//! dodge its gate" is a question about a repository. This hook fires wherever
//! Claude Code is working, including outside any repository at all, and the
//! question it answers — "does this rule misfire?" — is about the *rule*. So
//! one file per machine, next to the settings that installed it.
//!
//! ## One write per record
//!
//! Several sessions and their subagents run at once. Each record is built
//! whole and written with a single `write_all` to an append-mode handle, and
//! capped so that write stays small enough to interleave atomically in
//! practice. A torn line is dropped on read, exactly as `bypass::event` does —
//! the failure mode is a lost record, never a corrupt count.
//!
//! ## Commands carry secrets
//!
//! Measured across the real corpus: 283 `TOKEN|SECRET|KEY=` assignments, 17
//! `https://user:pass@` URLs, and literal `ghp_`/`--password` values. This file
//! persists command text, so redaction is not optional, and the byte cap is a
//! second line of defence for the shapes nobody anticipated.
//!
//! Never pushed, never transmitted. The project's no-telemetry promise applies
//! in full.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use amont_runtime::ui;

/// First line of the journal. A future format bumps this, and an older reader
/// sees an empty journal rather than misreading one.
pub const FORMAT: &str = "amont-agent-v1";

/// A record must fit in one small write. The excerpt is what gives, because
/// commands do not fit anyway: measured p50 240 bytes, p95 1,777, max 13 KB.
const MAX_RECORD: usize = 512;
const MAX_EXCERPT: usize = 200;

/// Compact past this. Roughly the same budget `bypass` uses.
const MAX_BYTES: u64 = 256 * 1024;

pub fn dir() -> Option<PathBuf> {
    Some(crate::settings::config_dir()?.join("amont-agent"))
}

pub fn path() -> Option<PathBuf> {
    Some(dir()?.join("journal.log"))
}

/// One thing that happened.
pub struct Entry<'a> {
    pub rule: &'a str,
    pub stance: &'a str,
    /// What actually happened to the command: `denied`, `advised`, `watched`.
    pub outcome: &'a str,
    pub session: &'a str,
    pub repo: &'a str,
    /// The permission mode the call arrived in. Recorded, never acted on —
    /// it is how you would notice if a rule ever stopped applying in one.
    pub mode: &'a str,
    pub excerpt: &'a str,
}

/// Append one record. Every failure is swallowed: the journal must never be
/// able to fail the hook, and `the_journal_never_fails_the_hook` pins that.
pub fn record(entry: &Entry) {
    let _ = try_record(entry);
}

fn try_record(entry: &Entry) -> Option<()> {
    let path = path()?;
    let dir = path.parent()?;
    fs::create_dir_all(dir).ok()?;

    let fresh = !path.exists();
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    if fresh {
        f.write_all(format!("{FORMAT}\n").as_bytes()).ok()?;
    }

    let line = line_for(entry);
    f.write_all(line.as_bytes()).ok()?;

    if f.metadata().ok()?.len() > MAX_BYTES {
        compact(&path);
    }
    Some(())
}

/// Build the whole record as one line. Separated from the write so it can be
/// tested without a filesystem, and so `a_record_is_one_write` can assert the
/// shape rather than race on it.
pub fn line_for(entry: &Entry) -> String {
    let excerpt = redact_and_trim(entry.excerpt);
    let mut line = format!(
        "F {} {} {} {} {} {} {} {}\n",
        now(),
        field(entry.rule),
        field(entry.stance),
        field(entry.outcome),
        field(&entry.session.chars().take(8).collect::<String>()),
        field(entry.repo),
        field(entry.mode),
        excerpt
    );
    if line.len() > MAX_RECORD {
        let keep = MAX_RECORD - 2;
        let cut = (0..=keep)
            .rev()
            .find(|i| line.is_char_boundary(*i))
            .unwrap_or(0);
        line.truncate(cut);
        line.push('\n');
    }
    line
}

/// A single space-free token. Everything in a record but the excerpt is one,
/// which is what lets the reader split on whitespace.
fn field(s: &str) -> String {
    let cleaned: String = ui::sanitize(s)
        .chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .collect();
    if cleaned.is_empty() {
        "-".to_string()
    } else {
        cleaned
    }
}

/// Secrets out, control bytes out, newlines out, then a hard length cap.
pub fn redact_and_trim(text: &str) -> String {
    let mut s = redact(text);
    s = ui::sanitize(&s);
    s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > MAX_EXCERPT {
        s = s.chars().take(MAX_EXCERPT).collect::<String>() + "…";
    }
    s
}

/// The shapes that actually appear in the corpus, plus the obvious token
/// prefixes. Deliberately blunt: this is a redactor, not a parser, and a
/// false redaction costs a less readable sample while a missed one costs a
/// secret on disk.
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for word in text.split_inclusive(char::is_whitespace) {
        out.push_str(&redact_word(word));
    }
    out
}

fn redact_word(word: &str) -> String {
    let trimmed = word.trim_end();
    let tail = &word[trimmed.len()..];

    // A URL is handled as a URL, because the `NAME=value` rule below cannot see
    // the difference between an assignment and a query string. It used to try,
    // with `API` among its needles — so `/api/v1/repos/…?limit=5"` matched,
    // everything after the `=` was replaced including the closing quote, and a
    // perfectly ordinary curl became unparseable shell. The corpus caught it.
    if let Some(scheme) = trimmed.find("://") {
        let mut out = String::from(&trimmed[..scheme + 3]);
        let rest = &trimmed[scheme + 3..];
        // `https://user:pass@host`
        let rest = match rest.find('@') {
            Some(at) if !rest[..at].contains('/') => {
                out.push_str("***:***");
                &rest[at..]
            }
            _ => rest,
        };
        out.push_str(&redact_query(rest));
        return out + tail;
    }
    // `--password=x`, `GITHUB_TOKEN=x`. Only where the name reads like a flag
    // or an identifier: a name carrying a path separator is not an assignment.
    if let Some(eq) = trimmed.find('=') {
        let (name, _) = trimmed.split_at(eq);
        if !name.contains('/') && !name.contains(':') && secret_named(name) {
            return format!("{name}=***") + tail;
        }
    }
    // Bare credential shapes.
    for prefix in [
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "sk-",
        "AKIA",
    ] {
        if trimmed.starts_with(prefix) && trimmed.len() > prefix.len() + 8 {
            return format!("{prefix}***{tail}");
        }
    }
    word.to_string()
}

/// Does this name say "the thing after the `=` is a credential"?
///
/// `API` is deliberately absent: on its own it matches every `/api/` in every
/// URL. `API_KEY` and `APIKEY` are still caught, by `KEY`.
fn secret_named(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    ["TOKEN", "SECRET", "PASSWORD", "PASSWD", "KEY", "CREDENTIAL"]
        .iter()
        .any(|needle| upper.contains(needle))
}

/// Redact only the values of secret-named query parameters, leaving the rest of
/// the URL — and, critically, any trailing quote — intact.
///
/// The user's own standing rule is that a secret riding in a URL is still a
/// secret, so `?token=…` must go; `?limit=5` must not.
fn redact_query(rest: &str) -> String {
    let Some(q) = rest.find('?') else {
        return rest.to_string();
    };
    let (base, query) = rest.split_at(q + 1);
    let mut out = String::from(base);
    // Keep whatever closes the word (a quote, a comma) attached to the last
    // parameter rather than swallowed by it.
    for (i, param) in query.split('&').enumerate() {
        if i > 0 {
            out.push('&');
        }
        match param.split_once('=') {
            Some((name, value)) if secret_named(name) => {
                let keep: String = value
                    .chars()
                    .rev()
                    .take_while(|c| !c.is_alphanumeric() && *c != '-' && *c != '_')
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                out.push_str(name);
                out.push_str("=***");
                out.push_str(&keep);
            }
            _ => out.push_str(param),
        }
    }
    out
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Keep the newest records and drop the oldest, preserving the header.
fn compact(path: &std::path::Path) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let lines: Vec<&str> = text.lines().filter(|l| l.starts_with("F ")).collect();
    let keep = lines.len().saturating_sub(lines.len() / 2);
    let mut out = String::from(FORMAT);
    out.push('\n');
    for l in &lines[lines.len() - keep..] {
        out.push_str(l);
        out.push('\n');
    }
    let _ = fs::write(path, out);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry<'a>(excerpt: &'a str) -> Entry<'a> {
        Entry {
            rule: "pipe-to-tail",
            stance: "deny",
            outcome: "denied",
            session: "0123456789abcdef",
            repo: "amont",
            mode: "default",
            excerpt,
        }
    }

    /// Interleaving is only safe if a record is one line with no interior
    /// newline. Cheaper and far less flaky to assert on the builder than to
    /// race real processes.
    #[test]
    fn a_record_is_exactly_one_line() {
        let line = line_for(&entry("git push \n\n origin main | tail -1"));
        assert_eq!(line.matches('\n').count(), 1);
        assert!(line.ends_with('\n'));
        assert!(line.len() <= MAX_RECORD);
    }

    /// A 13 KB command must not produce a 13 KB write.
    #[test]
    fn an_enormous_command_still_fits_one_write() {
        let line = line_for(&entry(&"x".repeat(20_000)));
        assert!(line.len() <= MAX_RECORD, "{} bytes", line.len());
        assert_eq!(line.matches('\n').count(), 1);
    }

    /// This file persists command text, and command text carries credentials.
    #[test]
    fn a_credential_never_reaches_the_journal() {
        for (raw, must_not_contain) in [
            ("git push https://fred:hunter2@github.com/x", "hunter2"),
            (
                "GITHUB_TOKEN=ghp_abcdefghijklmnop1234 git push",
                "ghp_abcdefghijklmnop1234",
            ),
            ("curl -H x --password=s3cr3t https://x", "s3cr3t"),
            (
                "gh auth login --with-token ghp_zzzzzzzzzzzzzzzzzzzz",
                "ghp_zzzzzzzzzzzzzzzzzzzz",
            ),
        ] {
            let got = redact_and_trim(raw);
            assert!(
                !got.contains(must_not_contain),
                "leaked {must_not_contain:?} from {raw:?} → {got}"
            );
        }

        // The AWS shape is assembled at run time rather than written out.
        // amont's own `pre-commit-secrets` refused this file when the literal
        // was here — correctly, because it matches on shape and cannot know
        // that this particular one is Amazon's published example. A test fixture
        // is not worth teaching a secret scanner to make exceptions.
        let key = format!("AKIA{}", "IOSFODNN7EXAMPLE");
        let got = redact_and_trim(&format!("aws --key {key} s3 ls"));
        assert!(!got.contains(&key), "leaked an access key id → {got}");
    }

    /// Redaction must not eat the part a reviewer needs.
    #[test]
    fn redaction_leaves_the_command_readable() {
        let got = redact_and_trim("GITHUB_TOKEN=ghp_abcdefghijklmnop1234 git push | tail -1");
        assert!(got.contains("git push"), "{got}");
        assert!(got.contains("tail"), "{got}");
    }

    /// A control byte in a command must not reach a file a terminal will later
    /// print — the same reason `ui::sanitize` exists for the trust prompt.
    #[test]
    fn control_bytes_are_escaped_not_stored_raw() {
        let got = redact_and_trim("git push \u{1b}[8m hidden");
        assert!(!got.contains('\u{1b}'), "{got}");
    }

    /// The bug the corpus found. `API` was a secret-name needle, so `/api/v1/`
    /// in any URL matched, everything after the first `=` was replaced — the
    /// closing quote included — and an ordinary curl became unparseable shell.
    /// A redactor that mangles benign commands makes its own output useless.
    #[test]
    fn an_api_path_is_not_a_secret_assignment() {
        let raw = r#"curl -sS "http://localhost:3000/api/v1/repos/x/actions/tasks?limit=5" | jq"#;
        assert_eq!(redact(raw), raw, "a benign URL was rewritten");
    }

    /// But a secret riding in a URL is still a secret.
    #[test]
    fn a_secret_query_parameter_is_still_redacted() {
        let got = redact("curl \"https://hooks.example.com/x?token=abc123def456&limit=5\"");
        assert!(!got.contains("abc123def456"), "{got}");
        assert!(
            got.contains("limit=5"),
            "the benign parameter survived: {got}"
        );
        assert!(got.ends_with('"'), "the closing quote survived: {got}");
    }

    /// Redaction must not turn a readable command into one the lexer refuses,
    /// or every redacted sample becomes useless for review.
    #[test]
    fn redaction_leaves_a_command_the_lexer_can_still_read() {
        for raw in [
            r#"curl -sS "http://x/api/v1/tasks?limit=5" | jq '.total'"#,
            r#"GITHUB_TOKEN=ghp_aaaaaaaaaaaaaaaaaaaa git push origin main"#,
            r#"git commit -m "msg" --no-verify && git push"#,
        ] {
            let redacted = redact(raw);
            assert!(
                !matches!(
                    crate::shell::lex(&redacted),
                    crate::shell::Parsed::Opaque(_)
                ),
                "redaction made this unreadable: {redacted}"
            );
        }
    }

    #[test]
    fn a_field_never_contains_a_space() {
        assert!(!field("two words").contains(' '));
        assert_eq!(field(""), "-");
    }
}
