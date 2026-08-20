//! The ledger of unverified commits — the bypass signal, kept instead of
//! discarded.
//!
//! [`crate::gate_stamp`] already detects the interesting event: a commit
//! that a commit-time gate declaration covered, created without that gate
//! having run — `--no-verify` is the commonest cause, a blocked attempt
//! retried with it the second, a gate whose tool was missing the third.
//! Until this module existed the detection was followed by a bare `return`:
//! the first symptom of a slow or flaky check (people routing around it) was
//! thrown away at the exact moment it was in hand.
//!
//! This module only counts. The stamp gates a check (a wrong read there
//! weakens the push gate); the ledger informs a dashboard (a wrong read here
//! miscounts). That difference in stakes is why this is not part of
//! `gate_stamp` — nothing in this file participates in any suppression
//! decision, and nothing ever may.
//!
//! The ledger is a local file, never a ref, never pushed, never sent
//! anywhere — the project's no-telemetry promise applies in full. It lives
//! in the COMMON git dir (unlike the deliberately worktree-private marker)
//! because "how often does this repository dodge its gate" is a question
//! about the repository, not about one worktree. `amont uninstall` deletes
//! it; so does `amont.recordBypasses false`, prospectively.
//!
//! Format, versioned like its siblings (`amont-gate-v1`, `amont-held-v1`):
//!
//! ```text
//! amont-bypass-v1
//! <unix-epoch> <commit-oid> <script>
//! ```
//!
//! One line per uncovered script. No paths ever appear in the file, which is
//! why newline/space delimiting is safe here where `staged_only` needed NUL.

use std::io::Write;
use std::path::{Path, PathBuf};

/// First line of the ledger. A future amont that changes the shape bumps
/// this, and an old ledger reads as empty rather than being misread.
pub const FORMAT: &str = "amont-bypass-v1";

/// The ledger's filename inside the common git dir.
const LEDGER: &str = "amont-bypasses";

/// Compact past this — roughly 1,100 events. A ledger that long has long
/// since saturated the signal it exists to carry.
const MAX_BYTES: u64 = 64 * 1024;

/// Events kept by a compaction, newest first. After a compaction the total
/// is a FLOOR, not an exact count — the recent shape survives, which is the
/// part that means anything.
const KEEP: usize = 500;

/// What the ledger says, aggregated. Everything a reader displays comes
/// through here; nobody re-parses the file.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Ledger {
    /// Events on record (a floor after compaction).
    pub total: usize,
    /// The newest event's epoch, if any.
    pub last: Option<u64>,
    /// Count descending, then script ascending — a stable order a golden
    /// render can pin.
    pub by_script: Vec<ScriptCount>,
}

/// One script's slice of the ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptCount {
    pub script: String,
    pub count: usize,
    /// The newest event for THIS script.
    pub last: u64,
}

/// One valid event line, or nothing. The shared trust boundary: `parse` and
/// compaction both refuse a line through this, so a hand-edited ledger can
/// neither skew the counts with garbage nor smuggle a control byte into a
/// terminal — script names must be short printable ASCII, oids hex.
fn event(line: &str) -> Option<(u64, &str, &str)> {
    let mut fields = line.split_whitespace();
    let (Some(epoch), Some(oid), Some(script), None) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return None;
    };
    let epoch = epoch.parse::<u64>().ok()?;
    if !(7..=64).contains(&oid.len()) || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    if !(1..=32).contains(&script.len()) || !script.bytes().all(|b| b.is_ascii_graphic()) {
        return None;
    }
    Some((epoch, oid, script))
}

/// Aggregate a ledger's text. Pure; a malformed line is skipped, never
/// guessed at, and a missing or foreign header reads as an empty ledger.
pub fn parse(text: &str) -> Ledger {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    if lines.next() != Some(FORMAT) {
        return Ledger::default();
    }
    let mut out = Ledger::default();
    for line in lines {
        let Some((epoch, _oid, script)) = event(line) else {
            continue;
        };
        out.total += 1;
        out.last = Some(out.last.map_or(epoch, |l| l.max(epoch)));
        match out.by_script.iter_mut().find(|s| s.script == script) {
            Some(s) => {
                s.count += 1;
                s.last = s.last.max(epoch);
            }
            None => out.by_script.push(ScriptCount {
                script: script.to_string(),
                count: 1,
                last: epoch,
            }),
        }
    }
    out.by_script
        .sort_by(|a, b| b.count.cmp(&a.count).then(a.script.cmp(&b.script)));
    out
}

/// The fleet's door: read the ledger under an already-resolved common dir.
/// Absent file, unreadable file, foreign format — all read as empty.
pub fn read_at(common_dir: &Path) -> Ledger {
    read_file(&common_dir.join(LEDGER))
}

/// The in-repo door: resolves the common dir itself (process cwd). Empty on
/// any failure — `amont list` in a broken repo still prints.
pub fn read() -> Ledger {
    ledger_path().map(|p| read_file(&p)).unwrap_or_default()
}

fn read_file(path: &Path) -> Ledger {
    std::fs::read_to_string(path)
        .map(|t| parse(&t))
        .unwrap_or_default()
}

/// A relative age in the largest unit that fits — integer arithmetic only,
/// no calendar. A timestamp from the future (clock skew between worktree
/// hosts) clamps to "just now" rather than underflowing.
pub fn age(now: u64, then: u64) -> String {
    let d = now.saturating_sub(then);
    if d < 60 {
        "just now".to_string()
    } else if d < 3600 {
        format!("{}m ago", d / 60)
    } else if d < 86_400 {
        format!("{}h ago", d / 3600)
    } else if d < 7 * 86_400 {
        format!("{}d ago", d / 86_400)
    } else if d < 365 * 86_400 {
        format!("{}w ago", d / (7 * 86_400))
    } else {
        format!("{}y ago", d / (365 * 86_400))
    }
}

/// post-commit: record every gate-declared script this commit's files were
/// covered by that is NOT in `stamped` (what [`crate::gate_stamp`] just
/// wrote a note for). Completely silent, like the hook it runs in — the
/// number's whole value is that it is collected without a lecture.
///
/// The ordering below is the design: a repository that declares no gate
/// pays ZERO extra git spawns, and a gated repository whose commit was
/// properly stamped pays zero too. Only a commit already known to be
/// unverified spends processes.
pub(crate) fn note_unverified(manifest: &crate::manifest::Manifest, stamped: &[String]) {
    let names = crate::hooks::run_tests::gate_names_declared(&manifest.externals);
    if names.is_empty() {
        return;
    }
    if names.iter().all(|n| stamped.iter().any(|s| s == n)) {
        return;
    }
    // Skips and severity overrides can retire a declaration from the gate;
    // an entry the push gate would not trust cannot be "bypassed". EVERY
    // blocking declaration counts, whatever its name — the ledger is about
    // dodged checks, not about npm's vocabulary.
    let declared = crate::hooks::run_tests::blocking_commit_decls(&manifest.externals);
    let missing: Vec<_> = declared
        .iter()
        .filter(|d| !stamped.contains(&d.script))
        .collect();
    if missing.is_empty() {
        return;
    }
    if !crate::config::boolean_or("amont.recordBypasses", true) {
        return;
    }
    let files = head_files();
    if files.is_empty() {
        return; // git could not tell → do not guess
    }
    let scripts: Vec<&str> = missing
        .iter()
        .filter(|d| d.scope.matches(&files))
        .map(|d| d.script.as_str())
        .collect();
    if scripts.is_empty() {
        return;
    }
    let Some(oid) = crate::git::stdout(&["rev-parse", "HEAD"]) else {
        return;
    };
    let Some(path) = ledger_path() else { return };
    append(&path, &oid, &scripts);
}

/// HEAD's own files. `--root` because a parentless commit prints NOTHING
/// without it, and the initial commit is exactly the one somebody makes with
/// `--no-verify`. `-m` because a conflict-resolution commit DOES run
/// post-commit and shows nothing without it. `stdout_paths` inserts `-z`.
fn head_files() -> Vec<String> {
    crate::git::stdout_paths(&[
        "diff-tree",
        "--no-commit-id",
        "--name-only",
        "-r",
        "-m",
        "--root",
        "HEAD",
    ])
    .unwrap_or_default()
}

/// `<common-dir>/amont-bypasses` — shared by every worktree of the repo.
fn ledger_path() -> Option<PathBuf> {
    let dir = crate::git::stdout(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    Some(Path::new(&dir).join(LEDGER))
}

/// Append one event line per script, creating the header first if the file
/// is new. Best-effort throughout: a bookkeeping failure must never disturb
/// a commit that already exists.
fn append(path: &Path, commit: &str, scripts: &[&str]) {
    // `create_new` means exactly one writer ever wins the header, whatever
    // the worktree count.
    let _ = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .and_then(|mut f| f.write_all(format!("{FORMAT}\n").as_bytes()));
    compact_if_large(path);
    let now = now_epoch();
    let mut body = String::new();
    for script in scripts {
        body.push_str(&format!("{now} {commit} {script}\n"));
    }
    // One O_APPEND write of a few short lines: atomic enough in practice,
    // and a torn line is dropped by `parse` rather than misread.
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(body.as_bytes()));
}

/// Keep the file bounded: past [`MAX_BYTES`], rewrite it as the header plus
/// the newest [`KEEP`] valid events. A concurrent appender can lose a few
/// events to the rename — acceptable for a counter, unlike for a gate.
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
    let events: Vec<&str> = text.lines().filter(|l| event(l).is_some()).collect();
    let keep = &events[events.len().saturating_sub(KEEP)..];
    let mut body = String::with_capacity(keep.len() * 64 + FORMAT.len() + 1);
    body.push_str(FORMAT);
    body.push('\n');
    for line in keep {
        body.push_str(line);
        body.push('\n');
    }
    let tmp = path.with_file_name(format!("{LEDGER}.tmp-{}", std::process::id()));
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// uninstall: the ledger is OUR bookkeeping, gone with the hooks.
pub fn forget() -> bool {
    ledger_path().is_some_and(|path| std::fs::remove_file(&path).is_ok())
}

/// The same, for a repository this process is not standing in — what the
/// fleet needs, and it must resolve the path the way git would THERE:
/// `--git-common-dir` differs per repository, and a linked worktree's
/// ledger lives with its main checkout.
pub fn forget_in(repo: &Path) -> bool {
    let Some(dir) = crate::git::stdout_in(
        repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ) else {
        return false;
    };
    std::fs::remove_file(Path::new(&dir).join(LEDGER)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger(events: &[&str]) -> String {
        let mut s = format!("{FORMAT}\n");
        for e in events {
            s.push_str(e);
            s.push('\n');
        }
        s
    }

    /// No header, no ledger — a truncated or foreign file reads as empty.
    #[test]
    fn a_ledger_without_the_header_is_ignored() {
        assert_eq!(parse("100 abcdef0 typecheck\n"), Ledger::default());
        assert_eq!(parse(""), Ledger::default());
    }

    /// A future format version reads as empty rather than being misread.
    #[test]
    fn a_ledger_in_an_unknown_format_version_reads_as_empty() {
        assert_eq!(
            parse("amont-bypass-v2\n100 abcdef0 typecheck\n"),
            Ledger::default()
        );
    }

    /// A torn or hand-mangled line is skipped; its neighbours still count.
    #[test]
    fn malformed_lines_are_skipped_and_the_rest_still_counted() {
        let text = ledger(&[
            "100 abcdef0 typecheck",
            "not an event line",
            "101 abcdef0",            // two fields
            "102 abcdef0 test extra", // four fields
            "103 nothexg typecheck",  // oid not hex
            "104 abc typecheck",      // oid too short
            "105 abcdef0 test",
        ]);
        let l = parse(&text);
        assert_eq!(l.total, 2);
        assert_eq!(l.last, Some(105));
    }

    /// The trust boundary: a script name carrying a control byte never
    /// reaches a display — the line is refused wholesale.
    #[test]
    fn a_script_name_with_a_control_byte_is_rejected() {
        let text = ledger(&["100 abcdef0 type\u{1b}check"]);
        assert_eq!(parse(&text).total, 0);
    }

    /// Counts group by script; each group keeps its own newest timestamp,
    /// and the order is count desc then name asc — stable for a render.
    #[test]
    fn counts_group_by_script_and_keep_the_latest_timestamp() {
        let text = ledger(&[
            "100 aaaaaaa typecheck",
            "200 bbbbbbb test",
            "300 ccccccc typecheck",
        ]);
        let l = parse(&text);
        assert_eq!(l.total, 3);
        assert_eq!(l.last, Some(300));
        assert_eq!(l.by_script.len(), 2);
        assert_eq!(l.by_script[0].script, "typecheck");
        assert_eq!(l.by_script[0].count, 2);
        assert_eq!(l.by_script[0].last, 300);
        assert_eq!(l.by_script[1].script, "test");
        assert_eq!(l.by_script[1].last, 200);
    }

    /// A repo that never bypassed anything has no file, and that reads as
    /// zero — not as an error.
    #[test]
    fn an_absent_ledger_reads_as_empty() {
        let dir = std::env::temp_dir().join(format!("amont-bypass-none-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        assert_eq!(read_at(&dir), Ledger::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Compaction keeps the header and the NEWEST events; the file shrinks
    /// and still parses.
    #[test]
    fn compaction_keeps_the_header_and_the_newest_events() {
        let dir = std::env::temp_dir().join(format!("amont-bypass-compact-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(LEDGER);
        let mut body = format!("{FORMAT}\n");
        // Well past MAX_BYTES: ~2,600 events of ~40 bytes.
        for i in 0..2_600u64 {
            body.push_str(&format!("{i} abcdef0123456789 typecheck\n"));
        }
        std::fs::write(&path, body).unwrap();
        compact_if_large(&path);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(FORMAT));
        let l = parse(&text);
        assert_eq!(l.total, KEEP);
        assert_eq!(l.last, Some(2_599), "the newest events survive");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Appending to a fresh path writes the header once; appending again
    /// does not duplicate it.
    #[test]
    fn appending_twice_writes_exactly_one_header() {
        let dir = std::env::temp_dir().join(format!("amont-bypass-append-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(LEDGER);
        append(&path, "abcdef0123456789", &["typecheck"]);
        append(&path, "abcdef0123456789", &["test"]);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches(FORMAT).count(), 1, "{text:?}");
        assert_eq!(parse(&text).total, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The largest unit that fits, and clean boundaries.
    #[test]
    fn age_reads_in_the_largest_unit_that_fits() {
        assert_eq!(age(1000, 990), "just now");
        assert_eq!(age(1000 + 120, 1000), "2m ago");
        assert_eq!(age(1000 + 2 * 3600, 1000), "2h ago");
        assert_eq!(age(1000 + 3 * 86_400, 1000), "3d ago");
        assert_eq!(age(1000 + 20 * 86_400, 1000), "2w ago");
        assert_eq!(age(1000 + 800 * 86_400, 1000), "2y ago");
    }

    /// Clock skew across machines can put an event in the future; that
    /// clamps to "just now" instead of underflowing to eternity.
    #[test]
    fn a_timestamp_from_the_future_does_not_underflow() {
        assert_eq!(age(100, 200), "just now");
    }
}
