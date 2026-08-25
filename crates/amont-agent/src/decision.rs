//! The decision, and the only thing in this crate that writes to stdout.
//!
//! Claude Code parses stdout as JSON when it begins with `{`. Anything else on
//! that stream — a stray `println!`, a shell profile that echoes on startup —
//! silently breaks the parse, and a broken parse is reported nowhere the author
//! will look. So there is one emitter, and a test greps this crate's sources to
//! keep it that way.
//!
//! ## JSON or exit codes, never both
//!
//! Exit 2 also blocks, taking its message from stderr. Using both channels
//! gives one decision two sources of truth, and they disagree the first time
//! somebody edits one of them: exit 2 blocks *regardless* of what the JSON
//! says, so a JSON `allow` beside an exit 2 is a silent refusal. This crate
//! always exits 0 for a decision. A non-zero exit from this binary means the
//! guard itself broke, not that a rule fired.
//!
//! ## `allow` is never emitted
//!
//! `permissionDecision: "allow"` short-circuits the user's own permission
//! prompt. A guard that returns `allow` for everything it has no objection to
//! has switched off the permission system it was installed next to. Silence is
//! the correct way to have no objection.

use std::process::ExitCode;

use amont_runtime::{json, ui};

/// Claude Code truncates hook output at 10,000 characters and writes the
/// remainder to a file. Staying under it keeps the reason in the model's
/// context where it can act on it.
const LIMIT: usize = 10_000;

pub enum Decision {
    /// Nothing to say. Zero bytes on stdout — not `{}`, not a newline.
    Silent,
    /// Text into the model's context, no refusal.
    Advise(String),
    /// Refuse the tool call, with the reason the model will read.
    Deny(String),
    /// Text into the model's context at session start. The same field as
    /// `Advise`, on a different event — and there is nothing to refuse at a
    /// session opening, so this is the only shape that event can take.
    Context(String),
}

impl Decision {
    /// The only writer to stdout in this crate.
    pub fn emit(&self) -> ExitCode {
        match self {
            Decision::Silent => {}
            Decision::Advise(text) => {
                println!(
                    "{}",
                    json::object(&[format!(
                        "\"hookSpecificOutput\":{}",
                        json::object(&[
                            json::string_field("hookEventName", "PreToolUse"),
                            json::string_field("additionalContext", &clamp(text)),
                        ])
                    )])
                );
            }
            Decision::Deny(text) => {
                println!(
                    "{}",
                    json::object(&[format!(
                        "\"hookSpecificOutput\":{}",
                        json::object(&[
                            json::string_field("hookEventName", "PreToolUse"),
                            json::string_field("permissionDecision", "deny"),
                            json::string_field("permissionDecisionReason", &clamp(text)),
                        ])
                    )])
                );
            }
            Decision::Context(text) => {
                println!(
                    "{}",
                    json::object(&[format!(
                        "\"hookSpecificOutput\":{}",
                        json::object(&[
                            json::string_field("hookEventName", "SessionStart"),
                            json::string_field("additionalContext", &clamp(text)),
                        ])
                    )])
                );
            }
        }
        // Always zero. See the module note.
        ExitCode::SUCCESS
    }
}

/// Sanitize, then cap, then let the emitter escape.
///
/// The order is load-bearing. `ui::sanitize` EXPANDS control bytes into
/// `\x1b`-style text, so a cap applied before it can be blown afterwards by a
/// command full of escapes. And the cap has to fall on a character boundary,
/// because slicing UTF-8 by byte panics — on a command containing an emoji,
/// which is not hypothetical in a commit message.
///
/// Line by line: `sanitize` escapes every control byte, and a newline is
/// one — so two findings joined with a blank line reached the model as one
/// paragraph with a literal `\x0a\x0a` in the middle. The newline is the
/// one control character this text is allowed to carry; everything else
/// still gets escaped.
fn clamp(text: &str) -> String {
    let safe = text
        .split('\n')
        .map(ui::sanitize)
        .collect::<Vec<_>>()
        .join("\n");
    if safe.chars().count() <= LIMIT {
        return safe;
    }
    let kept: String = safe.chars().take(LIMIT - 1).collect();
    format!("{kept}…")
}

/// One rule's finding, phrased for the model.
///
/// A statement of fact, deliberately not an instruction. Injected hook text
/// that reads as a system order is the shape prompt-injection defences are
/// built to distrust, and text the model has been trained to be suspicious of
/// is text it may surface to the user instead of acting on.
pub fn phrase(rule_id: &str, reason: &str, remedy: &str) -> String {
    format!("amont-agent/{rule_id}: {reason} {remedy}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_reason_is_capped_on_a_character_boundary() {
        let long = "é".repeat(LIMIT * 2);
        let out = clamp(&long);
        assert!(out.chars().count() <= LIMIT);
        assert!(out.ends_with('…'));
    }

    /// The cap counts what is EMITTED. Sanitizing turns one control byte into
    /// four characters, so capping first would let an escape-laden command out
    /// well over the limit.
    #[test]
    fn control_bytes_are_expanded_before_the_cap_is_applied() {
        let nasty = "\u{1b}".repeat(LIMIT);
        let out = clamp(&nasty);
        assert!(out.chars().count() <= LIMIT);
        assert!(!out.contains('\u{1b}'), "no raw escape reaches the output");
    }

    /// Two findings are two paragraphs, not one with `\x0a\x0a` in it.
    #[test]
    fn newlines_between_findings_survive_the_emitter() {
        let out = clamp("first\n\nsecond\u{1b}[0m");
        assert_eq!(out, "first\n\nsecond\\x1b[0m");
    }

    #[test]
    fn silence_writes_nothing_at_all() {
        // Proven end to end in tests/hook.rs against the real binary; here we
        // only pin that Silent carries no text to write.
        assert!(matches!(Decision::Silent, Decision::Silent));
    }
}
