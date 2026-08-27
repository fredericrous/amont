//! The ledger of problems that did NOT block — the shadow-mode signal, kept
//! instead of discarded.
//!
//! [`crate::dispatch`] already detects the interesting event. `Report.downgraded`
//! is every check that FAILED while the severity that applies said `warn`, and
//! until this module existed the detection was followed by one line of output
//! and nothing else:
//!
//! ```text
//! ! 1 check(s) reported a problem but are set to warn: pre-commit-ban-terms
//! ```
//!
//! That line is the whole evidence base for the question a team lead actually
//! asks before adopting anything that can block a commit — *will this annoy my
//! team into switching it off?* Turning every blocking check down to `warn` for
//! a fortnight answers it exactly, and answered it into a scrollback buffer
//! that nobody kept.
//!
//! This module only counts. Nothing here participates in any verdict, and
//! nothing ever may — the same rule [`crate::bypass`] states about itself, for
//! the same reason: a wrong read in a gate weakens a gate, and a wrong read
//! here miscounts a report.
//!
//! The ledger is a local file, never a ref, never pushed, never sent anywhere —
//! the project's no-telemetry promise applies in full. It lives in the COMMON
//! git dir because "what has this repository been warning about" is a question
//! about the repository, not about one worktree. `amont uninstall` deletes it;
//! so does `amont.recordDowngrades false`, prospectively.
//!
//! A consequence worth stating rather than leaving to be discovered: this
//! **cannot aggregate across a team**. Every developer's ledger is their own
//! machine's. A lead runs the trial on their own checkout, or asks people to
//! paste. That is a deliberate limit of the no-telemetry promise, not a gap.
//!
//! Format, versioned like its siblings (`amont-bypass-v1`, `amont-gate-v1`):
//!
//! ```text
//! amont-downgrade-v1
//! <unix-epoch> <head-oid> <check-id> <origin>
//! ```
//!
//! No paths ever appear in the file, which is why space delimiting is safe here
//! where `staged_only` needed NUL.

use std::io::Write;
use std::path::{Path, PathBuf};

/// First line of the ledger. A future amont that changes the shape bumps this,
/// and an old ledger reads as empty rather than being misread.
pub const FORMAT: &str = "amont-downgrade-v1";

/// The ledger's filename inside the common git dir.
const LEDGER: &str = "amont-downgrades";

/// Compact past this — roughly 5,500 events.
///
/// Four times [`crate::bypass`]'s ceiling, deliberately. A bypass is a rare
/// act; a downgrade fires on every failing check of every commit, and the
/// whole point of the file is to survive a fortnight of exactly that. Five
/// failing checks at twenty commits a day is ~1,400 events in two weeks, and
/// compacting mid-trial would turn the total the lead is reading into a silent
/// floor at the moment they most need it to be a count.
const MAX_BYTES: u64 = 256 * 1024;

/// Events kept by a compaction, newest first. After a compaction the total is
/// a FLOOR, not an exact count — the recent shape survives, which is the part
/// that means anything.
const KEEP: usize = 2_000;

/// Why the check did not block.
///
/// This is the field that separates *"it would have blocked if you had not
/// turned it down"* — the trial signal — from *"this check is advisory by
/// design and is firing a lot"*, which is worth seeing but is not evidence
/// about a rollout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// `git config amont.severity.<check> warn` — the trial switch.
    Config,
    /// The repository's committed policy.
    Policy,
    /// The check declares `warn` itself. Nothing was overridden.
    Declared,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Config => "config",
            Origin::Policy => "policy",
            Origin::Declared => "declared",
        }
    }

    fn parse(s: &str) -> Option<Origin> {
        match s {
            "config" => Some(Origin::Config),
            "policy" => Some(Origin::Policy),
            "declared" => Some(Origin::Declared),
            _ => None,
        }
    }

    /// Would this check have blocked, had nothing been turned down?
    pub fn would_block(self) -> bool {
        matches!(self, Origin::Config | Origin::Policy)
    }
}

/// What the ledger says, aggregated. Everything a reader displays comes through
/// here; nobody re-parses the file.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Ledger {
    /// Events on record (a floor after compaction).
    pub total: usize,
    /// Of those, the ones that would have blocked but for an override.
    pub would_block: usize,
    /// DISTINCT commits those events happened against.
    ///
    /// Not the same fact as `total`, and the difference is the one a reader
    /// most needs: forty commits each tripping a check once is a check the
    /// team disagrees with, while one commit tripping it forty times is one
    /// person losing an afternoon to it. Counting only events conflates them.
    pub commits: usize,
    /// The oldest event's epoch — "since when" for the report.
    pub first: Option<u64>,
    /// The newest event's epoch, if any.
    pub last: Option<u64>,
    /// Count descending, then check ascending — a stable order a golden render
    /// can pin.
    pub by_check: Vec<CheckCount>,
}

/// One check's slice of the ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckCount {
    pub check: String,
    pub count: usize,
    /// Of `count`, the events that would have blocked but for an override.
    pub would_block: usize,
    /// The newest event for THIS check.
    pub last: u64,
}

/// One valid event line, or nothing. The shared trust boundary: `parse` and
/// compaction both refuse a line through this, so a hand-edited ledger can
/// neither skew the counts with garbage nor smuggle a control byte into a
/// terminal — check names must be short printable ASCII, oids hex.
fn event(line: &str) -> Option<(u64, &str, &str, Origin)> {
    let mut fields = line.split_whitespace();
    let (Some(epoch), Some(oid), Some(check), Some(origin), None) = (
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
    ) else {
        return None;
    };
    let epoch = epoch.parse::<u64>().ok()?;
    if !(7..=64).contains(&oid.len()) || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    // Longer than `bypass`'s 32: these are full check IDs, and
    // `pre-commit-lint-json-yaml` is already 25 characters.
    if !(1..=64).contains(&check.len()) || !check.bytes().all(|b| b.is_ascii_graphic()) {
        return None;
    }
    Some((epoch, oid, check, Origin::parse(origin)?))
}

/// Aggregate a ledger's text. Pure; a malformed line is skipped, never guessed
/// at, and a missing or foreign header reads as an empty ledger.
pub fn parse(text: &str) -> Ledger {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    if lines.next() != Some(FORMAT) {
        return Ledger::default();
    }
    let mut out = Ledger::default();
    let mut oids: Vec<&str> = Vec::new();
    for line in lines {
        let Some((epoch, oid, check, origin)) = event(line) else {
            continue;
        };
        out.total += 1;
        if origin.would_block() {
            out.would_block += 1;
        }
        out.first = Some(out.first.map_or(epoch, |f| f.min(epoch)));
        out.last = Some(out.last.map_or(epoch, |l| l.max(epoch)));
        if !oids.contains(&oid) {
            oids.push(oid);
        }
        match out.by_check.iter_mut().find(|c| c.check == check) {
            Some(c) => {
                c.count += 1;
                c.would_block += usize::from(origin.would_block());
                c.last = c.last.max(epoch);
            }
            None => out.by_check.push(CheckCount {
                check: check.to_string(),
                count: 1,
                would_block: usize::from(origin.would_block()),
                last: epoch,
            }),
        }
    }
    out.commits = oids.len();
    out.by_check
        .sort_by(|a, b| b.count.cmp(&a.count).then(a.check.cmp(&b.check)));
    out
}

/// The fleet's door: read the ledger under an already-resolved common dir.
/// Absent file, unreadable file, foreign format — all read as empty.
pub fn read_at(common_dir: &Path) -> Ledger {
    read_file(&common_dir.join(LEDGER))
}

/// The in-repo door: resolves the common dir itself (process cwd). Empty on any
/// failure — `amont list` in a broken repo still prints.
pub fn read() -> Ledger {
    ledger_path().map(|p| read_file(&p)).unwrap_or_default()
}

fn read_file(path: &Path) -> Ledger {
    std::fs::read_to_string(path)
        .map(|t| parse(&t))
        .unwrap_or_default()
}

/// Record every check that failed without blocking.
///
/// Called from the HOOK entry points only — never from `amont run`. See
/// `dispatch::pre_commit` for why that distinction is the whole integrity of
/// these numbers.
///
/// Best-effort and silent, like the hook it runs in: the number's whole value
/// is that it is collected without a lecture, and a bookkeeping failure must
/// never disturb a commit.
pub(crate) fn note(events: &[(String, Origin)]) {
    if events.is_empty() {
        return;
    }
    if !crate::config::boolean_or("amont.recordDowngrades", true) {
        return;
    }
    // The parent commit, which groups an afternoon's repeated attempts at one
    // commit together. Before the first commit there is no HEAD, and that is
    // exactly when somebody is setting a repository up — so record it against
    // a zero oid rather than dropping the event.
    let oid = crate::git::stdout(&["rev-parse", "HEAD"]).unwrap_or_else(|| "0000000".into());
    let Some(path) = ledger_path() else { return };
    append(&path, &oid, events);
}

/// `<common-dir>/amont-downgrades` — shared by every worktree of the repo.
fn ledger_path() -> Option<PathBuf> {
    let dir = crate::git::stdout(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    Some(Path::new(&dir).join(LEDGER))
}

/// Append one event line per check, creating the header first if the file is
/// new. Best-effort throughout.
fn append(path: &Path, commit: &str, events: &[(String, Origin)]) {
    // `create_new` means exactly one writer ever wins the header, whatever the
    // worktree count.
    let _ = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .and_then(|mut f| f.write_all(format!("{FORMAT}\n").as_bytes()));
    compact_if_large(path);
    let now = now_epoch();
    let mut body = String::new();
    for (check, origin) in events {
        body.push_str(&format!("{now} {commit} {check} {}\n", origin.as_str()));
    }
    // One O_APPEND write of a few short lines: atomic enough in practice, and a
    // torn line is dropped by `parse` rather than misread.
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(body.as_bytes()));
}

/// Keep the file bounded: past [`MAX_BYTES`], rewrite it as the header plus the
/// newest [`KEEP`] valid events. A concurrent appender can lose a few events to
/// the rename — acceptable for a counter, unlike for a gate.
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

/// The same, for a repository this process is not standing in — what the fleet
/// needs, and it must resolve the path the way git would THERE.
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
        assert_eq!(
            parse("100 abcdef0 pre-commit-x config\n"),
            Ledger::default()
        );
        assert_eq!(parse(""), Ledger::default());
    }

    /// A future format version reads as empty rather than being misread.
    #[test]
    fn a_ledger_in_an_unknown_format_version_reads_as_empty() {
        assert_eq!(
            parse("amont-downgrade-v2\n100 abcdef0 pre-commit-x config\n"),
            Ledger::default()
        );
    }

    /// A torn or hand-mangled line is skipped; its neighbours still count.
    #[test]
    fn malformed_lines_are_skipped_and_the_rest_still_counted() {
        let text = ledger(&[
            "100 abcdef0 pre-commit-x config",
            "not an event line",
            "101 abcdef0 pre-commit-x",             // three fields
            "102 abcdef0 pre-commit-x config more", // five fields
            "103 nothexg pre-commit-x config",      // oid not hex
            "104 abc pre-commit-x config",          // oid too short
            "105 abcdef0 pre-commit-x sideways",    // unknown origin
            "106 abcdef0 pre-commit-x policy",
        ]);
        let l = parse(&text);
        assert_eq!(l.total, 2);
        assert_eq!(l.last, Some(106));
    }

    /// The trust boundary: a check name carrying a control byte never reaches
    /// a display — the line is refused wholesale.
    #[test]
    fn a_check_name_with_a_control_byte_is_rejected() {
        assert_eq!(
            parse(&ledger(&["100 abcdef0 pre\u{1b}commit config"])).total,
            0
        );
    }

    /// Counts group by check; each group keeps its own newest timestamp, and
    /// the order is count desc then name asc — stable for a render.
    #[test]
    fn counts_group_by_check_and_keep_the_latest_timestamp() {
        let text = ledger(&[
            "100 aaaaaaa pre-commit-usual-name config",
            "200 bbbbbbb pre-commit-ban-terms config",
            "300 ccccccc pre-commit-usual-name config",
        ]);
        let l = parse(&text);
        assert_eq!(l.total, 3);
        assert_eq!(l.first, Some(100));
        assert_eq!(l.last, Some(300));
        assert_eq!(l.by_check.len(), 2);
        assert_eq!(l.by_check[0].check, "pre-commit-usual-name");
        assert_eq!(l.by_check[0].count, 2);
        assert_eq!(l.by_check[0].last, 300);
        assert_eq!(l.by_check[1].check, "pre-commit-ban-terms");
    }

    /// THE distinction the report exists to draw. Three events against one
    /// commit is one person fighting one commit; the same three spread over
    /// three commits is a check the team disagrees with. `total` cannot tell
    /// them apart and `commits` can.
    #[test]
    fn distinct_commits_are_counted_separately_from_events() {
        let one = ledger(&[
            "100 aaaaaaa pre-commit-usual-name config",
            "101 aaaaaaa pre-commit-usual-name config",
            "102 aaaaaaa pre-commit-usual-name config",
        ]);
        let l = parse(&one);
        assert_eq!((l.total, l.commits), (3, 1));

        let many = ledger(&[
            "100 aaaaaaa pre-commit-usual-name config",
            "101 bbbbbbb pre-commit-usual-name config",
            "102 ccccccc pre-commit-usual-name config",
        ]);
        let l = parse(&many);
        assert_eq!((l.total, l.commits), (3, 3));
    }

    /// A check that declares `warn` itself was never going to block, so it is
    /// counted but is NOT evidence about a rollout. Conflating the two would
    /// inflate the one number a lead reads.
    #[test]
    fn only_overridden_checks_count_as_would_have_blocked() {
        let text = ledger(&[
            "100 aaaaaaa pre-commit-ban-terms config",
            "101 aaaaaaa pre-commit-secrets policy",
            "102 aaaaaaa pre-commit-agents-md declared",
        ]);
        let l = parse(&text);
        assert_eq!(l.total, 3);
        assert_eq!(l.would_block, 2, "the declared-warn check is not evidence");
        let declared = l
            .by_check
            .iter()
            .find(|c| c.check == "pre-commit-agents-md")
            .unwrap();
        assert_eq!((declared.count, declared.would_block), (1, 0));
    }

    /// A repo that never warned about anything has no file, and that reads as
    /// zero — not as an error.
    #[test]
    fn an_absent_ledger_reads_as_empty() {
        let dir = std::env::temp_dir().join(format!("amont-downgrade-none-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        assert_eq!(read_at(&dir), Ledger::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Compaction keeps the header and the NEWEST events; the file shrinks and
    /// still parses.
    #[test]
    fn compaction_keeps_the_header_and_the_newest_events() {
        let dir =
            std::env::temp_dir().join(format!("amont-downgrade-compact-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(LEDGER);
        let mut body = format!("{FORMAT}\n");
        // Well past MAX_BYTES: ~6,000 events of ~45 bytes.
        for i in 0..6_000u64 {
            body.push_str(&format!(
                "{i} abcdef0123456789 pre-commit-ban-terms config\n"
            ));
        }
        std::fs::write(&path, body).unwrap();
        compact_if_large(&path);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(FORMAT));
        let l = parse(&text);
        assert_eq!(l.total, KEEP);
        assert_eq!(l.last, Some(5_999), "the newest events survive");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The ceiling is deliberately above a fortnight of ordinary trial volume;
    /// compacting mid-trial would turn the total into a silent floor exactly
    /// when a lead is reading it as a count.
    #[test]
    fn the_ceiling_survives_a_fortnight_of_trial_volume() {
        let per_event = "1756300000 abcdef0123456789 pre-commit-usual-name config\n".len() as u64;
        let fortnight = 5 * 20 * 14; // five checks, twenty commits a day
        assert!(
            per_event * fortnight < MAX_BYTES,
            "{fortnight} events of {per_event} bytes must fit under {MAX_BYTES}"
        );
    }

    /// Appending to a fresh path writes the header once; appending again does
    /// not duplicate it.
    #[test]
    fn appending_twice_writes_exactly_one_header() {
        let dir =
            std::env::temp_dir().join(format!("amont-downgrade-append-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(LEDGER);
        append(
            &path,
            "abcdef0123456789",
            &[("pre-commit-ban-terms".into(), Origin::Config)],
        );
        append(
            &path,
            "abcdef0123456789",
            &[("pre-commit-secrets".into(), Origin::Declared)],
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches(FORMAT).count(), 1, "{text:?}");
        let l = parse(&text);
        assert_eq!((l.total, l.would_block, l.commits), (2, 1, 1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every origin survives a write and read back as itself.
    #[test]
    fn origins_round_trip() {
        for o in [Origin::Config, Origin::Policy, Origin::Declared] {
            assert_eq!(Origin::parse(o.as_str()), Some(o));
        }
        assert_eq!(Origin::parse("warn"), None);
    }
}
