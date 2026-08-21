//! Can `doctor` tell a working guard from a dead one?
//!
//! The failure this command exists to catch is silent by construction: a hook
//! whose command cannot be found exits 127, and 127 is a NON-blocking status,
//! so Claude Code carries on and nobody is told. A dead guard and a quiet week
//! produce identical evidence. Every test here sets up one of those states and
//! insists `doctor` distinguishes it.

use std::path::PathBuf;
use std::process::Command;

struct Home(PathBuf);

impl Home {
    fn new(name: &str) -> Home {
        let dir =
            std::env::temp_dir().join(format!("amont-agent-doctor-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Home(dir)
    }
    fn settings(&self) -> PathBuf {
        self.0.join("settings.json")
    }
    /// A transcripts tree, so the liveness check has sessions to reason about.
    fn with_a_session(&self) -> &Home {
        let d = self.0.join("projects").join("-tmp-x");
        std::fs::create_dir_all(&d).expect("projects dir");
        std::fs::write(d.join("s.jsonl"), "{}\n").expect("a transcript");
        self
    }
    fn run(&self, args: &[&str]) -> (i32, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_amont-agent"))
            .args(args)
            .env("CLAUDE_CONFIG_DIR", &self.0)
            .env_remove("AMONT_AGENT_OFF")
            .output()
            .expect("the binary runs");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned()
                + &String::from_utf8_lossy(&out.stderr),
        )
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn an_uninstalled_guard_is_reported_and_exits_nonzero() {
    let h = Home::new("absent");
    let (code, out) = h.run(&["doctor"]);
    assert_ne!(code, 0, "a guard that is not installed is not healthy");
    assert!(out.contains("not installed"), "{out}");
}

/// The 127 case, and the reason this command exists. A command that does not
/// resolve produces a hook that silently does nothing.
#[test]
fn a_missing_binary_is_reported_as_broken() {
    let h = Home::new("missing-bin");
    // Built from the scratch dir rather than written as `/nonexistent/...`,
    // because that literal is NOT absolute on Windows — `doctor` then correctly
    // reported "not an absolute path" and this test, which had assumed POSIX
    // path semantics, failed on the Windows runner while the code was right.
    let missing = h.0.join("nowhere").join("amont-agent");
    assert!(missing.is_absolute(), "the fixture must be absolute");
    std::fs::write(
        h.settings(),
        serde_json::to_string_pretty(&serde_json::json!({
            "hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [
                {"type": "command", "command": missing, "args": ["hook"]}
            ]}]}
        }))
        .unwrap(),
    )
    .unwrap();
    let (code, out) = h.run(&["doctor"]);
    assert_ne!(code, 0);
    assert!(out.contains("does not exist"), "{out}");
    assert!(
        out.contains("127"),
        "the reason it is invisible is named: {out}"
    );
}

/// A `PATH`-resolved command is the same failure waiting to happen.
#[test]
fn a_relative_command_is_reported_as_fragile() {
    let h = Home::new("relative");
    std::fs::write(
        h.settings(),
        serde_json::to_string_pretty(&serde_json::json!({
            "hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [
                {"type": "command", "command": "amont-agent", "args": ["hook"]}
            ]}]}
        }))
        .unwrap(),
    )
    .unwrap();
    let (code, out) = h.run(&["doctor"]);
    assert_ne!(code, 0);
    assert!(out.contains("absolute"), "{out}");
}

/// A fresh install must be healthy, and must say what it is acting on rather
/// than merely that it exists.
#[test]
fn a_fresh_install_is_healthy() {
    let h = Home::new("fresh");
    h.with_a_session();
    let (code, out) = h.run(&["install", "--write"]);
    assert_eq!(code, 0, "{out}");
    let (code, out) = h.run(&["doctor"]);
    assert_eq!(code, 0, "a fresh install should be healthy:\n{out}");
    assert!(out.contains("pipe-to-tail"), "names what is armed: {out}");
}

/// Installing writes BOTH entries. Without the SessionStart one no heartbeat is
/// ever written, and liveness silently becomes unanswerable.
#[test]
fn installing_covers_the_session_start_event_too() {
    let h = Home::new("both");
    h.run(&["install", "--write"]);
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(h.settings()).unwrap()).unwrap();
    assert!(doc["hooks"]["PreToolUse"].is_array(), "{doc}");
    assert!(doc["hooks"]["SessionStart"].is_array(), "{doc}");
    let (_, out) = h.run(&["doctor"]);
    assert!(!out.contains("1 of 2 events"), "{out}");
}

/// The bug this test was written for: with no SessionStart entry there is no
/// heartbeat, and the first version reported "the guard has never run" — on a
/// machine where it had refused a command minutes earlier. Absence of a
/// heartbeat that could never have been written is not evidence of death.
#[test]
fn a_missing_heartbeat_is_not_evidence_the_guard_is_dead() {
    let h = Home::new("no-beat");
    h.with_a_session();
    std::fs::write(
        h.settings(),
        serde_json::to_string_pretty(&serde_json::json!({
            "hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [
                {"type": "command", "command": env!("CARGO_BIN_EXE_amont-agent"), "args": ["hook"]}
            ]}]}
        }))
        .unwrap(),
    )
    .unwrap();
    let (code, out) = h.run(&["doctor"]);
    assert!(
        out.contains("cannot be judged") || out.contains("no heartbeat"),
        "{out}"
    );
    assert!(
        !out.contains("never run"),
        "accused a guard that was never given a way to prove itself:\n{out}"
    );
    assert_eq!(
        code, 0,
        "a missing SessionStart entry is advisory, not fatal"
    );
}

/// Hooks from different settings files MERGE rather than override, so two
/// installs mean every command is judged twice and every reason printed twice.
#[test]
fn two_installs_are_reported_as_a_double_fire() {
    let h = Home::new("double");
    h.with_a_session();
    h.run(&["install", "--write"]);
    // A project-scoped copy alongside the user one.
    let proj = h.0.join("proj").join(".claude");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::copy(h.settings(), proj.join("settings.json")).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_amont-agent"))
        .arg("doctor")
        .current_dir(h.0.join("proj"))
        .env("CLAUDE_CONFIG_DIR", &h.0)
        .output()
        .expect("runs");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("settings files"), "{text}");
    assert!(text.contains("twice"), "{text}");
}

/// The kill switch must be visible. A guard that is installed, runnable and
/// completely inert is the most misleading state of all.
#[test]
fn the_kill_switch_is_reported() {
    let h = Home::new("off");
    h.with_a_session();
    h.run(&["install", "--write"]);
    let out = Command::new(env!("CARGO_BIN_EXE_amont-agent"))
        .arg("doctor")
        .env("CLAUDE_CONFIG_DIR", &h.0)
        .env("AMONT_AGENT_OFF", "1")
        .output()
        .expect("runs");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("AMONT_AGENT_OFF"), "{text}");
    assert!(text.contains("observe"), "{text}");
}
