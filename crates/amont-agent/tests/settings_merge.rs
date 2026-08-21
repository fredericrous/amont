//! Editing `settings.json` without collateral damage.
//!
//! This is a file the user maintains by hand. The whole risk of `install` is
//! that it destroys or reshuffles something nobody asked it to touch, and the
//! consequence lands on a config the author cannot easily reconstruct.

use std::path::{Path, PathBuf};
use std::process::Command;

struct Home(PathBuf);

impl Home {
    fn new(name: &str) -> Home {
        let dir = std::env::temp_dir().join(format!(
            "amont-agent-settings-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Home(dir)
    }
    fn settings(&self) -> PathBuf {
        self.0.join("settings.json")
    }
    fn write(&self, body: &str) {
        std::fs::write(self.settings(), body).expect("write settings");
    }
    fn read(&self) -> String {
        std::fs::read_to_string(self.settings()).unwrap_or_default()
    }
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_amont-agent"))
            .args(args)
            .env("CLAUDE_CONFIG_DIR", &self.0)
            .output()
            .expect("the binary runs");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn hooks_of(raw: &str) -> serde_json::Value {
    serde_json::from_str::<serde_json::Value>(raw)
        .expect("valid JSON")
        .get("hooks")
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

/// The one that matters most. Somebody else's PreToolUse hook must come out
/// exactly as it went in — installing beside it, and uninstalling from beside
/// it, are both non-events for that entry.
#[test]
fn an_unrelated_hook_survives_install_and_uninstall() {
    let h = Home::new("unrelated");
    // Written in the shape our renderer produces, so the reformat guard does
    // not stand in for the behaviour under test.
    h.write(
        &serde_json::to_string_pretty(&serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {"matcher": "Write", "hooks": [{"type": "command", "command": "/usr/bin/true"}]}
                ],
                "Stop": [
                    {"hooks": [{"type": "command", "command": "/usr/bin/false"}]}
                ]
            }
        }))
        .unwrap(),
    );
    let before = h.read();

    let (code, _, err) = h.run(&["install", "--write"]);
    assert_eq!(code, 0, "install failed: {err}");
    let after = hooks_of(&h.read());
    let pre = after.get("PreToolUse").unwrap().as_array().unwrap();
    assert_eq!(pre.len(), 2, "ours was added beside theirs");
    assert_eq!(pre[0]["matcher"], "Write");
    assert!(after.get("Stop").is_some(), "an unrelated event survived");

    let (code, _, err) = h.run(&["uninstall", "--write"]);
    assert_eq!(code, 0, "uninstall failed: {err}");
    assert_eq!(h.read(), before, "uninstall did not restore the file");
}

/// If somebody already guards Bash, join their block rather than adding a
/// second one — two `matcher: "Bash"` blocks both fire, and the duplication is
/// invisible until you wonder why the reason appears twice.
#[test]
fn we_join_an_existing_bash_block_rather_than_adding_a_second() {
    let h = Home::new("join");
    h.write(
        &serde_json::to_string_pretty(&serde_json::json!({
            "hooks": {"PreToolUse": [
                {"matcher": "Bash", "hooks": [{"type": "command", "command": "/usr/bin/true"}]}
            ]}
        }))
        .unwrap(),
    );
    h.run(&["install", "--write"]);
    let after = hooks_of(&h.read());
    let pre = after.get("PreToolUse").unwrap().as_array().unwrap();
    assert_eq!(pre.len(), 1, "still one Bash block");
    assert_eq!(pre[0]["hooks"].as_array().unwrap().len(), 2, "two handlers");
}

#[test]
fn installing_twice_writes_the_hook_once() {
    let h = Home::new("twice");
    h.run(&["install", "--write"]);
    let once = h.read();
    let (code, out, _) = h.run(&["install", "--write"]);
    assert_eq!(code, 0);
    assert!(out.contains("already current"), "{out}");
    assert_eq!(h.read(), once, "the second install rewrote the file");
}

/// A file we never wrote to must come back byte-identical.
#[test]
fn uninstalling_from_a_file_we_never_touched_changes_nothing() {
    let h = Home::new("untouched");
    let body = serde_json::to_string_pretty(&serde_json::json!({
        "tui": "fullscreen",
        "hooks": {"PreToolUse": [
            {"matcher": "Write", "hooks": [{"type": "command", "command": "/usr/bin/true"}]}
        ]}
    }))
    .unwrap();
    h.write(&body);
    let (code, out, _) = h.run(&["uninstall", "--write"]);
    assert_eq!(code, 0);
    assert!(out.contains("nothing written"), "{out}");
    assert_eq!(h.read(), body);
}

/// Never textually patch JSON that is already broken — that is how a
/// hand-maintained config gets destroyed. Say what is wrong, print the block,
/// change nothing.
#[test]
fn a_settings_file_that_does_not_parse_is_refused_not_patched() {
    let h = Home::new("broken");
    let broken = "{ \"hooks\": { \"PreToolUse\": [ ,,, }";
    h.write(broken);
    let (code, _, err) = h.run(&["install", "--write"]);
    assert_ne!(code, 0, "a broken file must not report success");
    assert!(err.contains("not valid JSON"), "{err}");
    assert!(
        err.contains("matcher"),
        "the block to paste is offered: {err}"
    );
    assert_eq!(h.read(), broken, "the broken file was modified");
}

/// We cannot reproduce every hand-written layout, and a diff full of
/// reformatting hides the one line we actually added. When we cannot keep the
/// promise, we write nothing and say so.
#[test]
fn a_hand_formatted_file_is_left_alone_unless_reformatting_is_accepted() {
    let h = Home::new("handmade");
    let hand = "{\n    \"permissions\": {\n        \"allow\": [\"Bash(ls)\"]\n    }\n}\n";
    h.write(hand);

    let (code, _, err) = h.run(&["install", "--write"]);
    assert_ne!(code, 0);
    assert!(err.contains("cannot reproduce"), "{err}");
    assert_eq!(h.read(), hand, "the file was reformatted anyway");

    let (code, _, err) = h.run(&["install", "--write", "--reformat"]);
    assert_eq!(code, 0, "{err}");
    let after = hooks_of(&h.read());
    assert!(after.get("PreToolUse").is_some());
    // The permission entry survived the normalisation.
    let doc: serde_json::Value = serde_json::from_str(&h.read()).unwrap();
    assert_eq!(doc["permissions"]["allow"][0], "Bash(ls)");
}

/// The written command must be an absolute path that exists. A `PATH`-resolved
/// command exits 127 the moment PATH differs, and 127 is Claude Code's
/// NON-blocking bucket — the guard would be silently gone.
#[test]
fn the_written_entry_names_an_executable_that_exists() {
    let h = Home::new("abs");
    h.run(&["install", "--write"]);
    let doc: serde_json::Value = serde_json::from_str(&h.read()).unwrap();
    let cmd = doc["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .expect("a command string");
    assert!(Path::new(cmd).is_absolute(), "not absolute: {cmd}");
    assert!(Path::new(cmd).exists(), "does not exist: {cmd}");
    assert_eq!(doc["hooks"]["PreToolUse"][0]["hooks"][0]["args"][0], "hook");
}

/// Without `--write` nothing is touched, and the block is printed for pasting.
#[test]
fn a_dry_run_prints_the_block_and_writes_nothing() {
    let h = Home::new("dry");
    let (code, out, _) = h.run(&["install"]);
    assert_eq!(code, 0);
    assert!(out.contains("PreToolUse"), "{out}");
    assert!(!h.settings().exists(), "a dry run created the file");
}
