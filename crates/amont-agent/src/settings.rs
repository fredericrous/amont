//! Editing Claude Code's `settings.json` without breaking it.
//!
//! This is a file the user writes by hand. Everything here is arranged around
//! that one fact: we add one handler, we remove exactly what we added, and we
//! touch nothing else — not the key order, not the indentation, not somebody
//! else's hook.
//!
//! ## Refuse rather than guess
//!
//! If the file does not parse, we do **not** attempt a textual patch. A regex
//! edit of JSON that is already malformed is how a hand-maintained config gets
//! destroyed. We say what is wrong, print the block to paste, and change
//! nothing.
//!
//! ## Why not `hookfile::guard_write`
//!
//! That guard is a statement about `.git/hooks`: it refuses a **tracked** file
//! outright, and refuses when git cannot answer at all. Both are right there —
//! `.git/hooks` is machine-local scratch, so a tracked file in it means a
//! symlink has escaped. Neither is right here. A project `.claude/settings.json`
//! is legitimately tracked and we are being asked to edit it, and a user-level
//! `~/.claude/settings.json` sits outside any repository, where "git cannot
//! answer" is the normal case rather than a warning.
//!
//! So the symlink and regular-file checks are re-stated for this context, and
//! only the atomic swap — `hookfile::stage` then `hookfile::commit_all` — is
//! reused. That swap is what makes a symlinked `settings.json` get REPLACED
//! rather than written through to its target.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

/// How we recognise our own handler on the way back out. Matched on the
/// command's file name, so moving the binary does not orphan the entry.
pub const BIN: &str = "amont-agent";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    User,
    Project,
    ProjectLocal,
}

impl Scope {
    pub fn path(self, project: &Path) -> Option<PathBuf> {
        match self {
            Scope::User => {
                let base = match std::env::var_os("CLAUDE_CONFIG_DIR") {
                    Some(d) => PathBuf::from(d),
                    None => PathBuf::from(std::env::var_os("HOME")?).join(".claude"),
                };
                Some(base.join("settings.json"))
            }
            Scope::Project => Some(project.join(".claude").join("settings.json")),
            Scope::ProjectLocal => Some(project.join(".claude").join("settings.local.json")),
        }
    }
}

pub enum Change {
    Add,
    Update,
    AlreadyCurrent,
    Remove,
    NothingToRemove,
    /// The file uses formatting this program cannot reproduce, so writing it
    /// back would reformat parts we were not asked to touch. See
    /// [`would_reformat`].
    WouldReformat,
}

impl Change {
    pub fn describe(&self, path: &Path) -> String {
        let p = path.display();
        match self {
            Change::Add => format!("added the PreToolUse hook to {p}"),
            Change::Update => format!("updated the PreToolUse hook in {p}"),
            Change::AlreadyCurrent => format!("{p} is already current — nothing written"),
            Change::Remove => format!("removed the PreToolUse hook from {p}"),
            Change::NothingToRemove => format!("no amont-agent hook in {p} — nothing written"),
            Change::WouldReformat => format!(
                "{p} uses formatting this program cannot reproduce — writing it back would \
                 reformat parts of the file nobody asked to change.\n\
                 Paste the block below in by hand, or re-run with --reformat to accept a \
                 normalised file."
            ),
        }
    }
}

/// Would writing this file back change anything we were not asked to change?
///
/// The test is a round trip: parse the file and render it with no edits at all.
/// If that already differs from what is on disk, then our renderer and the
/// author's formatting disagree — `serde_json`'s pretty printer always expands
/// arrays, so a hand-written `"allow": ["one"]` comes back over three lines —
/// and every edit we make would arrive buried in that noise.
///
/// The promise at the top of this module is that we touch nothing else. When we
/// cannot keep it, the right move is to say so and write nothing, not to keep it
/// approximately.
fn would_reformat(raw: &str, doc: &Value, indent: &str, nl: bool) -> bool {
    if raw.trim().is_empty() {
        return false;
    }
    // Re-render the document as it was READ, before any of our edits.
    match serde_json::from_str::<Value>(raw) {
        Ok(original) => {
            let _ = doc;
            render(&original, indent, nl) != raw
        }
        Err(_) => false,
    }
}

pub struct Plan {
    pub path: PathBuf,
    pub after: String,
    pub change: Change,
}

pub enum MergeError {
    Unparseable { path: PathBuf, why: String },
    WrongShape { path: PathBuf, key: &'static str },
    NotAFile(PathBuf),
}

impl MergeError {
    pub fn explain(&self) -> String {
        match self {
            MergeError::Unparseable { path, why } => format!(
                "{} is not valid JSON ({why}).\n\
                 Refusing to edit it — fix the file, or paste the block below in by hand.",
                path.display()
            ),
            MergeError::WrongShape { path, key } => format!(
                "{} has a `{key}` that is not the shape Claude Code expects; refusing to edit it",
                path.display()
            ),
            MergeError::NotAFile(p) => format!(
                "{} is not a regular file; refusing to write through it",
                p.display()
            ),
        }
    }
}

/// The handler we write. Three deliberate choices, each of which has a failure
/// mode attached:
///
/// * **An absolute path.** A command resolved from `PATH` exits 127 the moment
///   `PATH` differs — and 127 lands in Claude Code's *non-blocking* bucket, so
///   the guard is silently gone with nothing to notice it.
/// * **A positional verb first.** `crates/amont/src/main.rs` learned this the
///   hard way: only position 0 should ever decide what runs.
/// * **No `if` field.** `if: "Bash(git *)"` looks like a cheap prefilter, but
///   it fails open when it cannot parse a command and would silently drop every
///   non-git rule. This hook is not slow enough to need it.
fn handler(bin: &Path) -> Value {
    json!({
        "type": "command",
        "command": bin.display().to_string(),
        "args": ["hook"],
        "timeout": 10
    })
}

fn is_ours(h: &Value) -> bool {
    h.get("command")
        .and_then(|c| c.as_str())
        .map(|c| {
            Path::new(c)
                .file_name()
                .is_some_and(|n| n.to_string_lossy().trim_end_matches(".exe") == BIN)
        })
        .unwrap_or(false)
}

/// Two spaces unless the file says otherwise, and whether it ended in a
/// newline. Preserving both is the difference between a one-line diff and a
/// diff that looks like we rewrote the file.
fn shape(raw: &str) -> (String, bool) {
    let indent = raw
        .lines()
        .find_map(|l| {
            let ws: String = l.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
            if !ws.is_empty() && l.trim_start().starts_with('"') {
                Some(ws)
            } else {
                None
            }
        })
        .unwrap_or_else(|| "  ".to_string());
    (indent, raw.ends_with('\n'))
}

fn render(doc: &Value, indent: &str, trailing_newline: bool) -> String {
    let mut buf = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(indent.as_bytes());
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, fmt);
    use serde::Serialize;
    doc.serialize(&mut ser).expect("a Value always serialises");
    let mut out = String::from_utf8(buf).expect("serde_json emits UTF-8");
    if trailing_newline {
        out.push('\n');
    }
    out
}

fn read(path: &Path) -> Result<(Value, String), MergeError> {
    if path.exists() {
        let meta =
            std::fs::symlink_metadata(path).map_err(|_| MergeError::NotAFile(path.into()))?;
        // A symlink is allowed; `commit_all` renames over it, which replaces
        // the link rather than writing through to whatever it points at.
        if !meta.is_file() && !meta.file_type().is_symlink() {
            return Err(MergeError::NotAFile(path.into()));
        }
        let raw = std::fs::read_to_string(path).map_err(|_| MergeError::NotAFile(path.into()))?;
        if raw.trim().is_empty() {
            return Ok((Value::Object(Map::new()), raw));
        }
        let doc = serde_json::from_str(&raw).map_err(|e| MergeError::Unparseable {
            path: path.into(),
            why: e.to_string(),
        })?;
        Ok((doc, raw))
    } else {
        Ok((Value::Object(Map::new()), String::new()))
    }
}

pub fn plan_install(path: &Path, bin: &Path, reformat: bool) -> Result<Plan, MergeError> {
    let (mut doc, raw) = read(path)?;
    let (indent, nl) = shape(&raw);
    if !reformat && would_reformat(&raw, &doc, &indent, nl) {
        return Ok(Plan {
            path: path.into(),
            after: raw,
            change: Change::WouldReformat,
        });
    }
    let before = if raw.is_empty() {
        String::new()
    } else {
        raw.clone()
    };

    let root = doc.as_object_mut().ok_or(MergeError::WrongShape {
        path: path.into(),
        key: "(root)",
    })?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let hooks = hooks.as_object_mut().ok_or(MergeError::WrongShape {
        path: path.into(),
        key: "hooks",
    })?;
    let pre = hooks
        .entry("PreToolUse")
        .or_insert_with(|| Value::Array(Vec::new()));
    let pre = pre.as_array_mut().ok_or(MergeError::WrongShape {
        path: path.into(),
        key: "PreToolUse",
    })?;

    let want = handler(bin);
    let mut replaced = false;
    for block in pre.iter_mut() {
        if block.get("matcher").and_then(|m| m.as_str()) != Some("Bash") {
            continue;
        }
        if let Some(list) = block.get_mut("hooks").and_then(|h| h.as_array_mut()) {
            for h in list.iter_mut() {
                if is_ours(h) {
                    *h = want.clone();
                    replaced = true;
                    break;
                }
            }
            if !replaced {
                // Somebody else already guards Bash. Join their block rather
                // than adding a competing one.
                list.push(want.clone());
                replaced = true;
            }
        }
        if replaced {
            break;
        }
    }
    let change = if replaced {
        Change::Update
    } else {
        pre.push(json!({ "matcher": "Bash", "hooks": [want] }));
        Change::Add
    };

    let after = render(&doc, &indent, nl || before.is_empty());
    let change = if after == before {
        Change::AlreadyCurrent
    } else {
        change
    };
    Ok(Plan {
        path: path.into(),
        after,
        change,
    })
}

pub fn plan_uninstall(path: &Path, reformat: bool) -> Result<Plan, MergeError> {
    let (mut doc, raw) = read(path)?;
    let (indent, nl) = shape(&raw);
    if !reformat && would_reformat(&raw, &doc, &indent, nl) {
        return Ok(Plan {
            path: path.into(),
            after: raw,
            change: Change::WouldReformat,
        });
    }
    if raw.is_empty() {
        return Ok(Plan {
            path: path.into(),
            after: raw,
            change: Change::NothingToRemove,
        });
    }

    let mut removed = false;
    if let Some(pre) = doc
        .get_mut("hooks")
        .and_then(|h| h.get_mut("PreToolUse"))
        .and_then(|p| p.as_array_mut())
    {
        for block in pre.iter_mut() {
            if let Some(list) = block.get_mut("hooks").and_then(|h| h.as_array_mut()) {
                let before = list.len();
                list.retain(|h| !is_ours(h));
                removed |= list.len() != before;
            }
        }
        // Tidy up only what became empty because of us.
        pre.retain(|b| {
            b.get("hooks")
                .and_then(|h| h.as_array())
                .map(|l| !l.is_empty())
                .unwrap_or(true)
        });
        let empty = pre.is_empty();
        if empty {
            if let Some(hooks) = doc.get_mut("hooks").and_then(|h| h.as_object_mut()) {
                hooks.remove("PreToolUse");
                if hooks.is_empty() {
                    doc.as_object_mut().map(|r| r.remove("hooks"));
                }
            }
        }
    }

    if !removed {
        return Ok(Plan {
            path: path.into(),
            after: raw,
            change: Change::NothingToRemove,
        });
    }
    Ok(Plan {
        path: path.into(),
        after: render(&doc, &indent, nl),
        change: Change::Remove,
    })
}

/// Write the plan, atomically. Staged to a temp file next to the destination,
/// then renamed — so a symlinked `settings.json` is replaced rather than
/// written through, and a crash mid-write cannot leave a half-file.
pub fn apply(plan: &Plan) -> std::io::Result<()> {
    if matches!(
        plan.change,
        Change::AlreadyCurrent | Change::NothingToRemove | Change::WouldReformat
    ) {
        return Ok(());
    }
    if let Some(parent) = plan.path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let staged = amont_runtime::hookfile::stage(&plan.path, &plan.after, false)?;
    amont_runtime::hookfile::commit_all(vec![staged])
        .map(|_| ())
        .map_err(|e| std::io::Error::other(format!("{e:?}")))
}

/// The block to paste when we will not write it ourselves.
pub fn snippet(bin: &Path) -> String {
    render(
        &json!({ "hooks": { "PreToolUse": [ { "matcher": "Bash", "hooks": [handler(bin)] } ] } }),
        "  ",
        true,
    )
}
