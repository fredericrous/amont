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
/// Heavy wave waited); `heavy_checks`, how many Heavy checks judged this
/// change or would have; and the Heavy checks skipped, if any. A commit no
/// Heavy check applied to writes nothing.
pub(crate) fn note(light_ms: u64, heavy_checks: usize, skipped: &[&str]) {
    if heavy_checks == 0 {
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
        &format!("{} {light_ms} {heavy_checks} {skipped}\n", now_epoch()),
    );
}

/// `<common-dir>/amont-waves` — shared by every worktree of the repository.
fn ledger_path() -> Option<PathBuf> {
    let dir = crate::git::stdout(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    Some(Path::new(&dir).join(LEDGER))
}

/// Append `line`. A new file gets its header and this first line in ONE
/// write on the handle that created it, so no other writer's line can land
/// above the header.
fn append(path: &Path, line: &str) {
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut f) => {
            // best-effort ledger: a bookkeeping failure must not disturb a commit
            let _ = f.write_all(format!("{FORMAT}\n{line}").as_bytes());
        }
        Err(_) => {
            compact_if_large(path);
            // best-effort ledger: a bookkeeping failure must not disturb a commit
            let _ = std::fs::OpenOptions::new()
                .append(true)
                .open(path)
                .and_then(|mut f| f.write_all(line.as_bytes()));
        }
    }
}

/// Past [`MAX_BYTES`], keep the header and the newest [`KEEP`] lines. Written
/// to a sibling file and renamed over the ledger, so a reader or a crash sees
/// the old file or the new one, never a cut one. An append racing the
/// rewrite can lose its line: one row of a measure, never a commit.
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
    let body = compacted(&text);
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    if std::fs::write(&tmp, body).is_ok() {
        // best-effort ledger: a failed rename leaves the old, whole file
        let _ = std::fs::rename(&tmp, path);
    }
    // best-effort cleanup: after a rename the temp file is already gone
    let _ = std::fs::remove_file(&tmp);
}

/// The header and the newest [`KEEP`] lines of `text`. Pure, for the tests.
fn compacted(text: &str) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| *l != FORMAT).collect();
    let keep = &lines[lines.len().saturating_sub(KEEP)..];
    let mut body = format!("{FORMAT}\n");
    for l in keep {
        body.push_str(l);
        body.push('\n');
    }
    body
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compaction_keeps_the_header_and_the_newest_lines() {
        let mut text = format!("{FORMAT}\n");
        for n in 0..(KEEP + 5) {
            text.push_str(&format!("{n} 1 1 -\n"));
        }
        let out = compacted(&text);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], FORMAT);
        assert_eq!(lines.len(), KEEP + 1);
        assert_eq!(lines[1], "5 1 1 -");
        assert_eq!(*lines.last().unwrap(), format!("{} 1 1 -", KEEP + 4));
    }

    #[test]
    fn a_new_ledger_starts_with_its_header_and_first_line() {
        let dir = std::env::temp_dir().join(format!("amont-waves-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(LEDGER);
        let _ = std::fs::remove_file(&path); // best-effort: a leftover from a crash
        append(&path, "1 2 3 -\n");
        append(&path, "4 5 6 a,b\n");
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, format!("{FORMAT}\n1 2 3 -\n4 5 6 a,b\n"));
        let _ = std::fs::remove_dir_all(&dir); // best-effort: a scratch dir
    }
}
