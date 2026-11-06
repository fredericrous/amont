//! The pre-commit wave ledger (ADR-0020): one line per commit attempt whose
//! stage had a Heavy check, saying how long the Light wave held the Heavy
//! checks back and which Heavy checks a blocking Light failure skipped.
//!
//! It is the data the fail-fast decision is measured on — minutes saved
//! against the start delay every passing commit pays — and nothing reads it
//! to decide anything. Written from the HOOK only, never from `amont run`,
//! like [`crate::downgrade`]'s ledger and for the same reason: a rehearsal is
//! not a commit, and counting it would inflate both numbers.
//!
//! Best-effort and silent throughout: a bookkeeping failure must never
//! disturb a commit.

use std::io::Write;
use std::path::{Path, PathBuf};

const LEDGER: &str = "amont-waves";
const FORMAT: &str = "amont-waves-v1";
/// Past this size the file is rewritten as its header plus the newest
/// [`KEEP`] lines, so a busy repository's ledger stays small.
const MAX_BYTES: u64 = 256 * 1024;
const KEEP: usize = 2000;

/// Record one attempt: `light_ms`, the slowest Light check (how long the
/// Heavy wave waited), and the Heavy checks skipped, if any.
pub(crate) fn note(light_ms: u64, heavy: usize, skipped: &[&str]) {
    if heavy == 0 {
        return;
    }
    let Some(path) = ledger_path() else { return };
    let skipped = if skipped.is_empty() {
        "-".to_string()
    } else {
        skipped.join(",")
    };
    append(
        &path,
        &format!("{} {light_ms} {heavy} {skipped}\n", now_epoch()),
    );
}

/// `<common-dir>/amont-waves` — shared by every worktree of the repository.
fn ledger_path() -> Option<PathBuf> {
    let dir = crate::git::stdout(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    Some(Path::new(&dir).join(LEDGER))
}

fn append(path: &Path, line: &str) {
    // `create_new`: exactly one writer ever wins the header.
    let _ = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .and_then(|mut f| f.write_all(format!("{FORMAT}\n").as_bytes()));
    compact_if_large(path);
    let _ = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(line.as_bytes()));
}

fn compact_if_large(path: &Path) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if meta.len() <= MAX_BYTES {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let lines: Vec<&str> = text.lines().filter(|l| *l != FORMAT).collect();
    let keep = &lines[lines.len().saturating_sub(KEEP)..];
    let mut body = format!("{FORMAT}\n");
    for l in keep {
        body.push_str(l);
        body.push('\n');
    }
    let _ = std::fs::write(path, body);
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
