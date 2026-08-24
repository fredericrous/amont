//! The hook payload, as it arrives on stdin.
//!
//! Claude Code writes one JSON object to this process's stdin before it runs a
//! tool, and reads a decision back from stdout. The shape is defined by
//! somebody else and grows between releases, which decides how it is read here:
//! [`serde_json::Value`] and hand-written accessors, never a
//! `#[derive(Deserialize)]` struct.
//!
//! The difference matters at exactly one moment. A derived struct turns a
//! renamed or retyped field into a hard parse error, and the temptation is then
//! to treat that error as *something* — to block, or to warn. Reading field by
//! field turns the same event into an absent value, which this crate already
//! knows how to answer: no opinion. A guard that starts refusing commands
//! because a payload gained a field is worse than no guard.
//!
//! ## Everything unrecognised is silence
//!
//! Not our event, not our tool, no command, unparseable JSON, a `cwd` that no
//! longer exists — every one of those returns [`Event::NotOurs`], and the
//! caller exits 0 having written nothing.

use std::path::PathBuf;

/// The largest payload worth reading. Commands in the wild reach ~13 KB; a
/// megabyte is a generated blob, and a blob is not a shell command.
const MAX_PAYLOAD: usize = 1024 * 1024;

pub struct Bash {
    pub command: String,
    pub cwd: PathBuf,
    pub session: String,
    /// Journalled, never consulted. A `PreToolUse` hook fires before any
    /// permission check, in every mode including `bypassPermissions`, and a
    /// rule that quietly stopped applying in one mode would be a rule nobody
    /// could reason about. Recording it is how you would notice if that ever
    /// stopped being true.
    pub permission_mode: String,
}

pub struct Session {
    pub cwd: PathBuf,
    pub session: String,
}

pub enum Event {
    /// A Bash tool call we can have an opinion about.
    PreBash(Box<Bash>),
    /// A session opening. The guard leaves proof that it ran, and — this being
    /// the one moment a fetch is worth its cost — says where the checkout
    /// stands against the remote.
    SessionStart(Session),
    NotOurs,
}

pub fn parse(raw: &str) -> Event {
    if raw.len() > MAX_PAYLOAD {
        return Event::NotOurs;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Event::NotOurs;
    };
    let str_at = |key: &str| -> String {
        v.get(key)
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let cwd = {
        let c = str_at("cwd");
        if c.is_empty() {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        } else {
            PathBuf::from(c)
        }
    };

    match v.get("hook_event_name").and_then(|x| x.as_str()) {
        Some("SessionStart") => Event::SessionStart(Session {
            cwd,
            session: str_at("session_id"),
        }),
        Some("PreToolUse") => {
            // Exact, not a prefix. An MCP server may expose a tool whose name
            // merely starts with `Bash`, and that tool is not this one.
            if v.get("tool_name").and_then(|x| x.as_str()) != Some("Bash") {
                return Event::NotOurs;
            }
            let command = v
                .get("tool_input")
                .and_then(|i| i.get("command"))
                .and_then(|c| c.as_str())
                .unwrap_or_default()
                .to_string();
            if command.trim().is_empty() {
                return Event::NotOurs;
            }
            Event::PreBash(Box::new(Bash {
                command,
                cwd,
                session: str_at("session_id"),
                permission_mode: str_at("permission_mode"),
            }))
        }
        _ => Event::NotOurs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pre(command: &str) -> String {
        format!(
            r#"{{"hook_event_name":"PreToolUse","tool_name":"Bash","cwd":"/tmp",
                 "session_id":"s","tool_use_id":"t","permission_mode":"default",
                 "tool_input":{{"command":{}}}}}"#,
            serde_json::Value::String(command.to_string())
        )
    }

    #[test]
    fn a_bash_call_is_ours() {
        match parse(&pre("git push | tail -1")) {
            Event::PreBash(b) => {
                assert_eq!(b.command, "git push | tail -1");
                assert_eq!(b.cwd, PathBuf::from("/tmp"));
                assert_eq!(b.session, "s");
            }
            _ => panic!("expected a Bash call"),
        }
    }

    /// The list of things that must produce silence rather than an error. Each
    /// of these WILL happen — a payload gains a field, a new tool appears, a
    /// session is starting, a write is truncated mid-flight.
    #[test]
    fn anything_we_do_not_recognise_is_not_an_opinion() {
        for raw in [
            "",
            "{",
            "null",
            "[]",
            r#"{"hook_event_name":"PostToolUse","tool_name":"Bash"}"#,
            r#"{"hook_event_name":"PreToolUse","tool_name":"Read"}"#,
            r#"{"hook_event_name":"PreToolUse","tool_name":"BashOutput"}"#,
            r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{}}"#,
            r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"  "}}"#,
        ] {
            assert!(
                matches!(parse(raw), Event::NotOurs),
                "expected silence for {raw:?}"
            );
        }
    }

    /// A field arriving with the wrong type is a schema change, not a reason to
    /// start refusing commands.
    #[test]
    fn a_retyped_field_degrades_to_silence() {
        let raw = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash",
                      "tool_input":{"command":{"was":"a string"}}}"#;
        assert!(matches!(parse(raw), Event::NotOurs));
    }

    /// Unknown fields are the normal case, not an error: the payload grows
    /// between Claude Code releases and this crate must not notice.
    #[test]
    fn unknown_fields_are_ignored() {
        let raw = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","cwd":"/tmp",
                      "tool_input":{"command":"git status","timeout":5,"future":true},
                      "brand_new_field":{"nested":[1,2,3]}}"#;
        assert!(matches!(parse(raw), Event::PreBash(_)));
    }

    #[test]
    fn a_payload_too_large_to_be_a_command_is_ignored() {
        let huge = format!(
            r#"{{"hook_event_name":"PreToolUse","pad":"{}"}}"#,
            "x".repeat(MAX_PAYLOAD)
        );
        assert!(matches!(parse(&huge), Event::NotOurs));
    }
}
