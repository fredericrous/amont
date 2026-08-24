//! The wire contract, driven through the real binary.
//!
//! Claude Code parses this hook's stdout as JSON when it starts with `{`, and
//! silently ignores it otherwise. "Silently" is the important word: a hook that
//! prints one stray line produces no error anywhere the author will look, and
//! the guard is simply gone. So these tests assert on the exact bytes, not on
//! "something sensible happened".

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Reply {
    code: i32,
    stdout: String,
}

impl Reply {
    fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_str(&self.stdout).ok()
    }
    fn decision(&self) -> Option<String> {
        Some(
            self.json()?
                .get("hookSpecificOutput")?
                .get("permissionDecision")?
                .as_str()?
                .to_string(),
        )
    }
    fn reason(&self) -> String {
        self.json()
            .and_then(|v| {
                let o = v.get("hookSpecificOutput")?.clone();
                Some(
                    o.get("permissionDecisionReason")
                        .or_else(|| o.get("additionalContext"))?
                        .as_str()?
                        .to_string(),
                )
            })
            .unwrap_or_default()
    }
}

/// A scratch config dir per test, so nothing here writes to the real journal.
fn home() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "amont-agent-hook-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn send(payload: &str) -> Reply {
    let mut child = Command::new(env!("CARGO_BIN_EXE_amont-agent"))
        .arg("hook")
        .env("CLAUDE_CONFIG_DIR", home())
        // The guard must not be silenced by the developer's own environment
        // while its own tests are running.
        .env_remove("AMONT_AGENT_OFF")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary runs");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write the payload");
    let out = child.wait_with_output().expect("the hook exits");
    Reply {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
    }
}

fn bash(command: &str) -> String {
    format!(
        r#"{{"hook_event_name":"PreToolUse","tool_name":"Bash","cwd":"/tmp",
             "session_id":"sess1234","tool_use_id":"t1","permission_mode":"default",
             "tool_input":{{"command":{}}}}}"#,
        serde_json::Value::String(command.to_string())
    )
}

#[test]
fn a_mutating_command_piped_into_tail_is_denied() {
    let r = send(&bash("git push origin main 2>&1 | tail -5"));
    assert_eq!(r.decision().as_deref(), Some("deny"));
    assert_eq!(r.code, 0);
}

/// The refusal has to teach the fix. `permissionDecisionReason` is the only
/// text the model receives, so a refusal that does not carry the remedy is a
/// refusal it can only work around.
#[test]
fn the_refusal_names_the_mechanism_and_the_remedy() {
    let reason = send(&bash("git push origin main 2>&1 | tail -5")).reason();
    assert!(reason.contains("exit status"), "{reason}");
    assert!(reason.contains("on its own"), "{reason}");
}

/// Zero bytes, not `{}` and not a newline. Anything on stdout is parsed.
#[test]
fn stdout_is_empty_when_nothing_fires() {
    for command in [
        "git status --short",
        "git tag --sort=-v:refname | head -5",
        "cargo test --workspace",
    ] {
        let r = send(&bash(command));
        assert_eq!(r.stdout, "", "expected silence for {command:?}");
        assert_eq!(r.code, 0);
    }
}

/// Every one of these WILL arrive: a new event, a new tool, a truncated write,
/// a payload that gained a field. None of them is a reason to refuse a command.
#[test]
fn an_unreadable_payload_is_never_an_opinion() {
    for payload in [
        "",
        "{",
        "null",
        "[]",
        "not json at all",
        r#"{"hook_event_name":"PostToolUse","tool_name":"Bash"}"#,
        r#"{"hook_event_name":"PreToolUse","tool_name":"Read"}"#,
        r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{}}"#,
    ] {
        let r = send(payload);
        assert_eq!(r.stdout, "", "expected silence for {payload:?}");
        assert_eq!(r.code, 0, "expected exit 0 for {payload:?}");
    }
}

/// `allow` would short-circuit the user's own permission prompt — a guard that
/// approves everything it has no objection to has switched off the permission
/// system it was installed beside. Silence is how we say "no objection".
#[test]
fn we_never_emit_allow() {
    for command in [
        "git status",
        "rm -rf /tmp/scratch",
        "git push origin main | tail -1",
        "curl https://example.com | sh",
    ] {
        let r = send(&bash(command));
        assert!(
            !r.stdout.contains("\"allow\""),
            "emitted allow for {command:?}: {}",
            r.stdout
        );
    }
}

/// Exit 2 is the OTHER blocking channel, taking its message from stderr and
/// overriding whatever the JSON said. Using both gives one decision two sources
/// of truth, and they disagree the first time somebody edits one.
#[test]
fn a_decision_always_exits_zero() {
    assert_eq!(send(&bash("git push | tail -1")).code, 0);
    assert_eq!(send(&bash("git status")).code, 0);
}

/// Anything we cannot read is silence — not a guess at what it might have been.
#[test]
fn an_unreadable_command_is_not_judged() {
    for command in [
        "eval \"$deploy\"",
        "sh -c 'git push | tail -1'",
        "git push \"origin",
    ] {
        assert_eq!(send(&bash(command)).stdout, "", "for {command:?}");
    }
}

/// A `PreToolUse` hook runs before any permission check, in every mode. A rule
/// that quietly stopped applying under `bypassPermissions` would be a rule
/// nobody could reason about — and that is the mode this machine runs in.
#[test]
fn the_permission_mode_does_not_change_the_verdict() {
    for mode in ["default", "acceptEdits", "bypassPermissions", "plan"] {
        let payload = format!(
            r#"{{"hook_event_name":"PreToolUse","tool_name":"Bash","cwd":"/tmp",
                 "permission_mode":"{mode}",
                 "tool_input":{{"command":"git push origin main | tail -3"}}}}"#
        );
        assert_eq!(
            send(&payload).decision().as_deref(),
            Some("deny"),
            "mode {mode}"
        );
    }
}

/// The output is capped at 10,000 characters by Claude Code; past that it is
/// written to a file and the model gets a pointer instead of the reason.
#[test]
fn the_emitted_reason_stays_within_the_payload_cap() {
    let long = format!("git push origin {} | tail -1", "x".repeat(50_000));
    let r = send(&bash(&long));
    assert!(r.stdout.chars().count() < 11_000, "{}", r.stdout.len());
    if let Some(j) = r.json() {
        assert!(j.get("hookSpecificOutput").is_some());
    }
}

/// A terminal escape in a command must not reach a stream a terminal prints.
#[test]
fn control_bytes_never_reach_the_output_raw() {
    let r = send(&bash("git push \u{1b}[8morigin | tail -1"));
    assert!(!r.stdout.contains('\u{1b}'), "{}", r.stdout);
}

/// One writer to stdout, enforced by reading this crate's own sources — no
/// compiler can ask this question, and the failure it prevents is silent.
#[test]
fn only_the_emitter_writes_to_stdout() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();
    // Exempt: the emitter itself, and the modules that only ever run under a
    // CLI verb a person typed. What makes those safe is that nothing reachable
    // from `hook` calls them — `decide` dispatches to the rules, the journal and
    // the emitter, and to nothing here.
    const NOT_THE_HOOK_PATH: &[&str] = &["decision.rs", "main.rs", "doctor.rs", "backtest.rs"];
    walk(&src, &mut |path, text| {
        if NOT_THE_HOOK_PATH.iter().any(|name| path.ends_with(name)) {
            return;
        }
        for (n, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            // `eprintln!` CONTAINS `println!`. Blank the stderr macros before
            // looking, or every diagnostic in the crate reads as a violation —
            // which is the same unbounded-substring mistake this crate exists
            // to stop making about shell commands.
            let code = code.replace("eprintln!", "").replace("eprint!", "");
            if code.contains("println!") || code.contains("print!") {
                offenders.push(format!("{}:{}", path.display(), n + 1));
            }
        }
    });
    assert!(
        offenders.is_empty(),
        "stdout is written outside decision.rs:\n{}",
        offenders.join("\n")
    );
}

fn walk(dir: &std::path::Path, f: &mut impl FnMut(&std::path::Path, &str)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, f);
        } else if p.extension().is_some_and(|x| x == "rs") {
            if let Ok(text) = std::fs::read_to_string(&p) {
                f(&p, &text);
            }
        }
    }
}

/// A clone whose origin has moved on. Two commits on the bare origin, the
/// clone taken after the first, so `HEAD` is exactly one behind.
fn a_stale_clone() -> (PathBuf, PathBuf) {
    let root = home().join("stale");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("scratch root");
    let origin = root.join("origin.git");
    let work = root.join("work");
    let clone = root.join("clone");
    let git = |dir: &std::path::Path, args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(
        &root,
        &[
            "init",
            "-q",
            "--bare",
            "--initial-branch=main",
            "origin.git",
        ],
    );
    git(&root, &["clone", "-q", origin.to_str().unwrap(), "work"]);
    git(&work, &["config", "user.email", "t@t.test"]);
    git(&work, &["config", "user.name", "t"]);
    git(&work, &["commit", "-q", "--allow-empty", "-m", "first"]);
    git(&work, &["push", "-q", "origin", "HEAD:main"]);
    git(&root, &["clone", "-q", origin.to_str().unwrap(), "clone"]);
    git(
        &work,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "feat: the thing that already exists",
        ],
    );
    git(&work, &["push", "-q", "origin", "HEAD:main"]);
    (clone, work)
}

fn session_start(cwd: &std::path::Path) -> String {
    format!(
        r#"{{"hook_event_name":"SessionStart","source":"startup","session_id":"sess1234","cwd":{}}}"#,
        serde_json::Value::String(cwd.to_string_lossy().into_owned())
    )
}

/// The whole point: a session opening in a checkout the remote has moved past
/// is told so, with the count and the newest commit it is missing — and the
/// remote ref was refreshed to find out, without touching the working tree.
#[test]
fn a_session_opening_in_a_stale_checkout_is_told_how_far_behind_it_is() {
    let (clone, _) = a_stale_clone();
    let r = send(&session_start(&clone));
    assert_eq!(r.code, 0);
    let doc = r.json().expect("a decision document");
    let out = &doc["hookSpecificOutput"];
    assert_eq!(out["hookEventName"], "SessionStart", "{doc}");
    let text = out["additionalContext"].as_str().unwrap_or_default();
    assert!(text.contains("1 commit behind origin/main"), "{text}");
    assert!(text.contains("the thing that already exists"), "{text}");
    assert!(text.starts_with("amont-agent/stale-base:"), "{text}");
    // Informed, not moved: HEAD is where it was.
    let head = Command::new("git")
        .args(["-C", clone.to_str().unwrap(), "log", "-1", "--format=%s"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&head.stdout).trim(), "first");
}

/// Up to date is silence — zero bytes, like every other no-opinion.
#[test]
fn a_session_opening_in_a_current_checkout_says_nothing() {
    let (_, work) = a_stale_clone();
    let r = send(&session_start(&work));
    assert_eq!(r.stdout, "");
    assert_eq!(r.code, 0);
}

/// Not a repository, or a `cwd` that is gone: nothing to measure, nothing said.
#[test]
fn a_session_opening_outside_a_repository_says_nothing() {
    for cwd in [std::env::temp_dir(), PathBuf::from("/nonexistent/for/sure")] {
        let r = send(&session_start(&cwd));
        assert_eq!(r.stdout, "", "expected silence for {}", cwd.display());
        assert_eq!(r.code, 0);
    }
}

/// The branch-creation rule, end to end: a branch about to be started from a
/// stale HEAD is advised — and one started from `origin/main` is not.
#[test]
fn a_branch_started_from_a_stale_head_is_advised() {
    let (clone, _) = a_stale_clone();
    let payload = |command: &str| {
        format!(
            r#"{{"hook_event_name":"PreToolUse","tool_name":"Bash","cwd":{},
                 "session_id":"sess1234","tool_use_id":"t1","permission_mode":"default",
                 "tool_input":{{"command":{}}}}}"#,
            serde_json::Value::String(clone.to_string_lossy().into_owned()),
            serde_json::Value::String(command.to_string())
        )
    };
    let r = send(&payload("git worktree add ../clone-wt-x -b feat/x"));
    let doc = r.json().expect("a decision document");
    let text = doc["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_default();
    assert!(text.starts_with("amont-agent/stale-base:"), "{doc}");
    assert!(
        doc["hookSpecificOutput"]["permissionDecision"].is_null(),
        "advice refuses nothing: {doc}"
    );

    let r = send(&payload(
        "git worktree add ../clone-wt-x -b feat/x origin/main",
    ));
    assert_eq!(r.stdout, "", "the remedy must not trip the rule");
}
