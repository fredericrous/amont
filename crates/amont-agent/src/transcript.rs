//! Reading Claude Code's session transcripts.
//!
//! Claude Code appends one JSON object per line to
//! `~/.claude/projects/<encoded-project-path>/<session-uuid>.jsonl`. Every tool
//! call it ever made is in there, which is what makes a rule's fire rate a
//! measurable quantity instead of an opinion.
//!
//! Four decisions in here exist because getting them wrong produces numbers
//! that look right:
//!
//! 1. **Bucket on the entry's own `timestamp`, never the file's mtime.** A
//!    session open for six hours has one mtime and hundreds of entries spread
//!    across it; a resumed session has an mtime weeks after its contents. Using
//!    mtime collapses a whole session into whatever day it was last touched,
//!    and the resulting weekly table is confidently wrong. Measured both ways
//!    while planning this: they disagree materially.
//!
//! 2. **Filter on the entry's own `cwd`, never the directory name.** The
//!    project directory is a lossy encoding of a path —
//!    `-Users-me-Developer-my-project` cannot be decoded, because a literal
//!    dash and a path separator both became `-`.
//!
//! 3. **Deduplicate on `tool_use.id`.** Resuming or forking a session copies
//!    earlier entries into the new transcript. Those are the same tool call
//!    observed twice, and counting them twice inflates exactly the busy
//!    sessions that dominate a bucket.
//!
//! 4. **A truncated final line is normal, not corruption.** A session being
//!    written right now ends mid-object. Skip it, count it, keep the file.
//!
//! ## What is not here
//!
//! Sessions run on the web are not on this machine, so no scan can see them.
//! Every report says so rather than quietly under-reporting.

use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::civil::{day_of, Day};

/// A single tool invocation, borrowed from the parsed line it came from.
///
/// The backtester reads only `command`, `day` and `cwd` today. The rest is what
/// `explain` and the labelled-corpus export need in order to say WHICH session
/// and which call a sample came from, which is the difference between a sample
/// a human can go and check and one they have to take on trust.
#[allow(dead_code)]
pub struct ToolCall<'a> {
    /// `tool_use.id` — the deduplication key.
    pub id: &'a str,
    pub tool: &'a str,
    /// `input.command` for Bash. Empty for a tool that has no such field.
    pub command: &'a str,
    pub day: Day,
    pub session: &'a str,
    /// The entry's own working directory, which is the only trustworthy one.
    pub cwd: &'a str,
    /// A subagent's call. These count: the hook fires for them too.
    pub sidechain: bool,
}

/// How the transcript roots were found, so a report can say where it looked.
#[derive(Debug, PartialEq, Eq)]
pub enum Discovery {
    Explicit,
    ConfigDirEnv,
    HomeDefault,
}

pub struct Roots {
    pub dirs: Vec<PathBuf>,
    /// Reported by `doctor`, which has to say where it looked before it can
    /// claim the guard is or is not alive.
    #[allow(dead_code)]
    pub how: Discovery,
}

#[derive(Debug)]
pub enum ScanError {
    NoHome,
    Missing(PathBuf),
}

impl ScanError {
    pub fn explain(&self) -> String {
        match self {
            ScanError::NoHome => {
                "cannot find your home directory: set CLAUDE_CONFIG_DIR or pass --transcripts"
                    .to_string()
            }
            ScanError::Missing(p) => format!(
                "no transcripts at {} — pass --transcripts <dir> if they live elsewhere",
                p.display()
            ),
        }
    }
}

/// Where the transcripts are. `$CLAUDE_CONFIG_DIR` wins over `$HOME/.claude`
/// because that is the override Claude Code itself honours.
pub fn roots(explicit: &[PathBuf]) -> Result<Roots, ScanError> {
    if !explicit.is_empty() {
        for d in explicit {
            if !d.is_dir() {
                return Err(ScanError::Missing(d.clone()));
            }
        }
        return Ok(Roots {
            dirs: explicit.to_vec(),
            how: Discovery::Explicit,
        });
    }
    let (base, how) = match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(d) => (PathBuf::from(d), Discovery::ConfigDirEnv),
        None => {
            let home = std::env::var_os("HOME").ok_or(ScanError::NoHome)?;
            (PathBuf::from(home).join(".claude"), Discovery::HomeDefault)
        }
    };
    let projects = base.join("projects");
    if !projects.is_dir() {
        return Err(ScanError::Missing(projects));
    }
    Ok(Roots {
        dirs: vec![projects],
        how,
    })
}

pub struct Scan<'a> {
    pub roots: &'a Roots,
    /// Only calls to this tool. Also the second-stage prefilter.
    pub tool: &'static str,
    pub since: Option<Day>,
}

/// What the walk saw. Reported so a surprising rate can be checked against a
/// surprising denominator rather than believed.
#[derive(Default, Debug)]
pub struct Stats {
    pub files: usize,
    pub bytes: u64,
    pub lines: u64,
    pub parsed: u64,
    pub calls: u64,
    pub duplicates: u64,
    pub bad_lines: u64,
    pub oversized: u64,
    pub truncated_tail: usize,
}

/// A line longer than this is not a tool call we can use. The largest command
/// in the corpus measured 13 KB; a megabyte is a base64 payload or a corrupt
/// line, and either way parsing it costs more than it can be worth.
const MAX_LINE: usize = 1024 * 1024;

/// Every `.jsonl` under the roots, sorted so a run is reproducible.
pub fn files(roots: &Roots) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in &roots.dirs {
        collect(dir, &mut out);
    }
    out.sort();
    out
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => collect(&path, out),
            Ok(t) if t.is_file() => {
                if path.extension().is_some_and(|x| x == "jsonl") {
                    out.push(path);
                }
            }
            _ => {}
        }
    }
}

/// Walk every transcript, calling `f` once per DEDUPLICATED tool call.
///
/// Single-threaded on purpose, for now. The corpus is ~800 MB and the two-stage
/// prefilter drops all but a few percent of lines before any parsing happens;
/// measure before adding threads, because a parallel walk has to reconcile the
/// dedup set anyway and a result that depends on scheduling is worse than a
/// result that takes another second.
pub fn for_each_call<F>(scan: &Scan, mut f: F) -> Result<Stats, ScanError>
where
    F: FnMut(&ToolCall),
{
    let mut stats = Stats::default();
    let mut seen: HashSet<String> = HashSet::new();
    let needle_tool = format!("\"{}\"", scan.tool);

    for path in files(scan.roots) {
        let Ok(file) = fs::File::open(&path) else {
            continue;
        };
        stats.files += 1;
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        loop {
            line.clear();
            let read = match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(n) => n,
                // Invalid UTF-8 mid-file: the rest of this transcript is not
                // readable, but the ones already counted still are.
                Err(_) => break,
            };
            stats.bytes += read as u64;
            stats.lines += 1;

            let complete = line.ends_with('\n');
            let raw = line.trim_end();
            if raw.is_empty() {
                continue;
            }
            if raw.len() > MAX_LINE {
                stats.oversized += 1;
                continue;
            }

            // Stage one: does this line mention a tool call at all? Stage two:
            // does it mention OUR tool? Both are substring tests over a
            // superset of what the parser would accept, so neither can change
            // the result — only how much text reaches the parser. Measured on
            // the real corpus: 110k lines to 43k.
            if !raw.contains("\"tool_use\"") || !raw.contains(&needle_tool) {
                continue;
            }

            let Ok(entry) = serde_json::from_str::<serde_json::Value>(raw) else {
                // An unparseable LAST line with no newline is a session being
                // written as we read it. Anywhere else it is a damaged line.
                if !complete {
                    stats.truncated_tail += 1;
                } else {
                    stats.bad_lines += 1;
                }
                continue;
            };
            stats.parsed += 1;

            let Some(day) = entry
                .get("timestamp")
                .and_then(|t| t.as_str())
                .and_then(day_of)
            else {
                continue;
            };
            if scan.since.is_some_and(|s| day < s) {
                continue;
            }

            let session = entry
                .get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let cwd = entry.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
            let sidechain = entry
                .get("isSidechain")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let Some(blocks) = entry
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
            else {
                continue;
            };

            for block in blocks {
                if block.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                    continue;
                }
                let Some(tool) = block.get("name").and_then(|n| n.as_str()) else {
                    continue;
                };
                if tool != scan.tool {
                    continue;
                }
                let Some(id) = block.get("id").and_then(|i| i.as_str()) else {
                    continue;
                };
                if !seen.insert(id.to_string()) {
                    stats.duplicates += 1;
                    continue;
                }
                let command = block
                    .get("input")
                    .and_then(|i| i.get("command"))
                    .and_then(|c| c.as_str())
                    .unwrap_or("");
                stats.calls += 1;
                f(&ToolCall {
                    id,
                    tool,
                    command,
                    day,
                    session,
                    cwd,
                    sidechain,
                });
            }
        }
    }
    Ok(stats)
}
