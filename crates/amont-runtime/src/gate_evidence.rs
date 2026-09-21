//! What the stamps ADD UP TO — the statistics side of
//! [`crate::gate_stamp`].
//!
//! Every push that runs a gate leaves a `run` line in a note keyed by the
//! tree the gate ran against (see that module's "second half of the note").
//! Across a few weeks that is a per-gate outcome dataset, already on the
//! machine, that nothing read. This module reads it, and answers two
//! questions with it:
//!
//! 1. **Is a gate still doing its job?** The fleet audit of 2026-09-19 found
//!    four mechanisms that had stopped checking anything and said nothing
//!    about it: a nested lockfile audited from the repository root, a
//!    `govulncheck` built with a Go too old for the database, `uv` invoked
//!    without a `.venv`, and an `npm` that silently timed out. Each one went
//!    green. Each one, on the record here, also went from minutes to
//!    milliseconds on the day it broke. [`summarise`] is that comparison,
//!    made explicit.
//! 2. **Which gate should run first?** With `order = evidence`, the push
//!    gates are ordered by what the record says they cost and how often they
//!    catch something, so a push that is going to fail fails early. See
//!    [`order_by_evidence`].
//!
//! # Three rules this module obeys
//!
//! **It never skips a check.** Ordering is a permutation; nothing here
//! removes a gate, shortens one or lets one be assumed to pass. A prediction
//! that a gate will pass is not evidence that it did, and a check that does
//! not run cannot be stamped or attested — the whole chain from
//! [`crate::gate_stamp`] to [`crate::attest`] rests on a record of a run that
//! happened, and a statistical model of one is not that.
//!
//! **It abstains rather than guessing.** Under [`Thresholds::min_runs`]
//! verdicts a gate is reported with its counts and NO flags, and the report
//! says why in words (`insufficient history (2 runs)`). A dashboard that
//! calls a two-run gate "flaky" teaches people to ignore the column.
//!
//! **Its thresholds are numbers, written down.** Every flag below names the
//! comparison it made and the number it made it against, both in the report
//! and in `docs/gate-evidence.md`, and every one of them is a flag on the
//! command line. A heuristic nobody can see the inside of is a heuristic
//! nobody can argue with.

use std::collections::BTreeMap;
use std::path::Path;

use crate::gate_stamp::{Note, Run, RunOutcome, NOTES_REF};

/// Seconds since the epoch, or 0 if the clock is before it. The record is
/// local and single-machine, so this is the same clock on both sides of every
/// comparison made here.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Every run recorded in one repository, with the fingerprint each was
/// recorded against.
///
/// The fingerprint is the note's key — the git tree the gate ran on. Two runs
/// with the same fingerprint read identical content, which is what makes
/// disagreement between them evidence of flakiness rather than of a change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    pub runs: Vec<(String, Run)>,
}

impl History {
    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    /// The runs inside the window, newest last. `window_days` of 0 means
    /// "everything recorded".
    fn within(&self, now: u64, window_days: u64) -> Vec<(&str, &Run)> {
        let floor = if window_days == 0 {
            0
        } else {
            now.saturating_sub(window_days.saturating_mul(86_400))
        };
        let mut runs: Vec<(&str, &Run)> = self
            .runs
            .iter()
            .filter(|(_, r)| r.at >= floor)
            .map(|(fp, r)| (fp.as_str(), r))
            .collect();
        // Ties broken by gate name so the order is total, and the report of
        // one repository is the same on two machines reading one ref.
        runs.sort_by(|a, b| a.1.at.cmp(&b.1.at).then_with(|| a.1.gate.cmp(&b.1.gate)));
        runs
    }
}

/// Read one repository's recorded runs.
///
/// Two spawns whatever the size of the history: `notes list` names every note
/// and its object, and one `cat-file --batch` reads all the bodies. A git
/// that will not answer, an absent ref and a repository with no history are
/// the same empty answer — a report that cannot be built is reported as
/// missing, never as zero.
pub fn history_in(repo: &Path) -> History {
    let Some(list) = crate::git::stdout_in(repo, &["notes", "--ref", NOTES_REF, "list"]) else {
        return History::default();
    };
    // `<note blob> <annotated object>` per line. The annotated object is the
    // fingerprint; the blob is what `cat-file` is about to be asked for.
    let mut keyed: BTreeMap<String, String> = BTreeMap::new();
    let mut stdin = String::new();
    for line in list.lines() {
        let mut t = line.split_whitespace();
        if let (Some(blob), Some(object)) = (t.next(), t.next()) {
            keyed.insert(blob.to_string(), object.to_string());
            stdin.push_str(blob);
            stdin.push('\n');
        }
    }
    if keyed.is_empty() {
        return History::default();
    }
    let Some(batch) = crate::git::stdout_piped_in(repo, &["cat-file", "--batch"], stdin.as_bytes())
    else {
        return History::default();
    };
    let mut history = History::default();
    for (blob, body) in parse_batch(&batch) {
        let Some(fingerprint) = keyed.get(&blob) else {
            continue;
        };
        for run in Note::parse(&body).runs {
            history.runs.push((fingerprint.clone(), run));
        }
    }
    history
}

/// Split `git cat-file --batch` output into `(object id, body)`.
///
/// Line-oriented rather than byte-counted, and that is a deliberate trade.
/// The header git writes is `<oid> <type> <size>`, and a body line could in
/// principle look like one — but every body this ref holds was written by
/// [`crate::gate_stamp`], whose lines begin `amont-gate-` or `run `. The
/// alternative, honouring the byte count, cannot be written against a helper
/// that trims its output, and a note this reader misreads costs a row in a
/// report rather than a verdict.
fn parse_batch(out: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut current: Option<(String, Vec<&str>)> = None;
    for line in out.lines() {
        let mut t = line.split_whitespace();
        let (oid, kind, size) = (t.next(), t.next(), t.next());
        let is_header = matches!((oid, kind, size), (Some(o), Some("blob"), Some(s))
            if o.len() >= 40
                && o.chars().all(|c| c.is_ascii_hexdigit())
                && s.parse::<u64>().is_ok()
                && t.next().is_none());
        if is_header {
            if let Some((oid, body)) = current.take() {
                found.push((oid, body.join("\n")));
            }
            current = Some((oid.unwrap_or_default().to_string(), Vec::new()));
            continue;
        }
        if let Some((_, body)) = current.as_mut() {
            body.push(line);
        }
    }
    if let Some((oid, body)) = current.take() {
        found.push((oid, body.join("\n")));
    }
    found
}

/// The numbers every flag below is decided against. Defaults are documented
/// in `docs/gate-evidence.md` and overridable per invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thresholds {
    /// How far back the report looks, in days. 0 means the whole record.
    pub window_days: u64,
    /// Verdicts below which no flag is computed and the report says so.
    pub min_runs: usize,
    /// A last duration below this percentage of the median is a collapse.
    pub noop_ratio_percent: u64,
    /// …but only for a gate whose median is at least this many seconds. A
    /// gate that has always taken 200 ms has no collapse to detect.
    pub noop_median_secs: u64,
    /// A pass faster than this, from a gate that has never once passed this
    /// fast, is the other shape of the same failure.
    pub fast_pass_ms: u64,
    /// How many later fingerprints may go by without this gate running before
    /// it is called stale.
    pub stale_runs: usize,
    /// …or how many days, when other gates have run more recently.
    pub stale_days: u64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds {
            window_days: 90,
            min_runs: 5,
            noop_ratio_percent: 10,
            noop_median_secs: 30,
            fast_pass_ms: 1_000,
            stale_runs: 10,
            stale_days: 30,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagKind {
    /// The gate stopped doing the work it used to do.
    NoOpSuspect,
    /// One fingerprint, two answers.
    Flaky,
    /// It has not run while its neighbours have.
    Stale,
}

impl FlagKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FlagKind::NoOpSuspect => "no-op suspect",
            FlagKind::Flaky => "flaky",
            FlagKind::Stale => "stale",
        }
    }
}

/// A flag and the comparison that raised it. The reason travels WITH the
/// flag: a dashboard cell saying "no-op suspect" and nothing else is an
/// accusation, and nobody can check an accusation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flag {
    pub kind: FlagKind,
    pub why: String,
}

/// What the record says about one gate in one repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateReport {
    pub gate: String,
    /// Runs in the window, of every outcome.
    pub runs: usize,
    pub passed: usize,
    pub failed: usize,
    /// Ran and did not judge: the tool was missing, or the repository lacks
    /// what turns the gate on. Counted apart from a pass, always — this is
    /// the number the fleet audit was blind to.
    pub unavailable: usize,
    /// Median wall-clock of the runs that reached a verdict, milliseconds.
    pub median_ms: u64,
    pub last_ms: u64,
    pub last_at: u64,
    pub last_outcome: Option<RunOutcome>,
    /// Distinct fingerprints this gate ran against.
    pub fingerprints: usize,
    /// Fingerprints that other gates judged AFTER this gate last ran, and
    /// that this gate did not. The "in pushes" half of staleness.
    pub missed: usize,
    pub flags: Vec<Flag>,
    /// Set when the history is too short to say anything, with the count in
    /// words. A report that abstains says so; it does not print an empty
    /// flags column that reads as "all clear".
    pub abstained: Option<String>,
}

impl GateReport {
    /// Age of the last run in whole days.
    pub fn last_age_days(&self, now: u64) -> u64 {
        now.saturating_sub(self.last_at) / 86_400
    }
}

/// The per-gate report for one repository's history.
///
/// Pure over `now`, so a test can hand it a clock. Gates appear in name
/// order; a gate that has never run does not appear at all, because this
/// record only knows what ran — `amont list` is the answer to what is
/// declared.
pub fn summarise(history: &History, now: u64, t: &Thresholds) -> Vec<GateReport> {
    let runs = history.within(now, t.window_days);
    let mut by_gate: BTreeMap<&str, Vec<(&str, &Run)>> = BTreeMap::new();
    for &(fp, run) in &runs {
        by_gate
            .entry(run.gate.as_str())
            .or_default()
            .push((fp, run));
    }
    by_gate
        .into_iter()
        .map(|(gate, mine)| one_gate(gate, &mine, &runs, now, t))
        .collect()
}

fn one_gate(
    gate: &str,
    mine: &[(&str, &Run)],
    all: &[(&str, &Run)],
    now: u64,
    t: &Thresholds,
) -> GateReport {
    let verdicts: Vec<(&str, &Run)> = mine
        .iter()
        .filter(|(_, r)| r.outcome.is_verdict())
        .copied()
        .collect();
    let passed = verdicts
        .iter()
        .filter(|(_, r)| r.outcome == RunOutcome::Passed)
        .count();
    let failed = verdicts.len() - passed;
    let unavailable = mine
        .iter()
        .filter(|(_, r)| r.outcome == RunOutcome::Unavailable)
        .count();
    let median_ms = median(&verdicts.iter().map(|(_, r)| r.ms).collect::<Vec<_>>());
    let last = mine.last().copied();
    let mut fingerprints: Vec<&str> = mine.iter().map(|(fp, _)| *fp).collect();
    fingerprints.sort_unstable();
    fingerprints.dedup();

    // Fingerprints judged by ANY gate after this one last ran, minus the ones
    // this gate was part of. "Other gates kept working and this one did not"
    // is the whole claim, so it is counted rather than inferred from a date.
    let last_at = last.map(|(_, r)| r.at).unwrap_or(0);
    let mut later: Vec<&str> = all
        .iter()
        .filter(|(_, r)| r.at > last_at && r.gate != gate)
        .map(|(fp, _)| *fp)
        .collect();
    later.sort_unstable();
    later.dedup();
    let missed = later
        .iter()
        .filter(|fp| !fingerprints.contains(*fp))
        .count();

    let mut report = GateReport {
        gate: gate.to_string(),
        runs: mine.len(),
        passed,
        failed,
        unavailable,
        median_ms,
        last_ms: last.map(|(_, r)| r.ms).unwrap_or(0),
        last_at,
        last_outcome: last.map(|(_, r)| r.outcome),
        fingerprints: fingerprints.len(),
        missed,
        flags: Vec::new(),
        abstained: None,
    };

    // Staleness is decided first, because it is the one question a short
    // history can still answer: it is a claim about the OTHER gates' record,
    // not about the distribution of this one's.
    if let Some(flag) = stale_flag(&report, all, now, t) {
        report.flags.push(flag);
    }

    if verdicts.len() < t.min_runs {
        report.abstained = Some(format!(
            "insufficient history ({} verdict{})",
            verdicts.len(),
            if verdicts.len() == 1 { "" } else { "s" }
        ));
        return report;
    }

    if let Some(flag) = noop_flag(&verdicts, median_ms, t) {
        report.flags.push(flag);
    }
    if let Some(flag) = flaky_flag(gate, &verdicts) {
        report.flags.push(flag);
    }
    report
}

/// Did the gate's cost collapse?
///
/// Two shapes of the same failure, and both are needed: a suite that used to
/// take eleven minutes and now takes 0.4 s is caught by the ratio; a gate
/// installed broken — passing in milliseconds from its very first run — has
/// no earlier median to collapse from, and is caught by the second rule only
/// once it has enough verdicts to be sure it never once did real work.
fn noop_flag(verdicts: &[(&str, &Run)], median_ms: u64, t: &Thresholds) -> Option<Flag> {
    let (_, last) = *verdicts.last()?;
    if median_ms >= t.noop_median_secs.saturating_mul(1_000)
        && last.ms.saturating_mul(100) < median_ms.saturating_mul(t.noop_ratio_percent)
    {
        return Some(Flag {
            kind: FlagKind::NoOpSuspect,
            why: format!(
                "last run {} against a median of {} ({}% of it, threshold {}%)",
                duration(last.ms),
                duration(median_ms),
                last.ms.saturating_mul(100) / median_ms.max(1),
                t.noop_ratio_percent
            ),
        });
    }
    if last.outcome == RunOutcome::Passed
        && last.ms < t.fast_pass_ms
        && verdicts[..verdicts.len() - 1]
            .iter()
            .all(|(_, r)| r.ms >= t.fast_pass_ms)
    {
        return Some(Flag {
            kind: FlagKind::NoOpSuspect,
            why: format!(
                "passed in {}, and none of the {} earlier runs ever finished under {}",
                duration(last.ms),
                verdicts.len() - 1,
                duration(t.fast_pass_ms)
            ),
        });
    }
    None
}

/// One fingerprint, two answers. The content did not change between them, so
/// something else decided the verdict.
fn flaky_flag(gate: &str, verdicts: &[(&str, &Run)]) -> Option<Flag> {
    let mut by_fp: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for &(fp, run) in verdicts {
        let e = by_fp.entry(fp).or_insert((0, 0));
        if run.outcome == RunOutcome::Passed {
            e.0 += 1;
        } else {
            e.1 += 1;
        }
    }
    let split: Vec<(&str, usize, usize)> = by_fp
        .into_iter()
        .filter(|(_, (pass, fail))| *pass > 0 && *fail > 0)
        .map(|(fp, (pass, fail))| (fp, pass, fail))
        .collect();
    let (fp, pass, fail) = *split.first()?;
    Some(Flag {
        kind: FlagKind::Flaky,
        why: format!(
            "{gate} both passed ({pass}) and failed ({fail}) on tree {}{}",
            short(fp),
            if split.len() > 1 {
                format!(", and on {} other tree(s)", split.len() - 1)
            } else {
                String::new()
            }
        ),
    })
}

/// Has it stopped running while its neighbours kept going?
fn stale_flag(report: &GateReport, all: &[(&str, &Run)], now: u64, t: &Thresholds) -> Option<Flag> {
    if report.runs == 0 {
        return None;
    }
    let newest_other = all
        .iter()
        .filter(|(_, r)| r.gate != report.gate)
        .map(|(_, r)| r.at)
        .max()?;
    if newest_other <= report.last_at {
        return None;
    }
    if report.missed >= t.stale_runs {
        return Some(Flag {
            kind: FlagKind::Stale,
            why: format!(
                "{} later tree(s) were judged by other gates and not by this one (threshold {})",
                report.missed, t.stale_runs
            ),
        });
    }
    let age = report.last_age_days(now);
    if age >= t.stale_days {
        return Some(Flag {
            kind: FlagKind::Stale,
            why: format!(
                "last ran {age} days ago (threshold {}), while another gate ran {} days ago",
                t.stale_days,
                now.saturating_sub(newest_other) / 86_400
            ),
        });
    }
    None
}

/// The order the push gates should be attempted in, as a permutation of the
/// indices of `gates`.
///
/// **What this is not.** It is not a prediction that a gate will fail, and
/// nothing is skipped on the strength of it: every gate in `gates` appears in
/// the result exactly once. Fail-fast is what makes the order worth anything
/// — pre-push already stops at the first blocking failure — so ordering only
/// changes WHEN the news arrives, never WHETHER it does.
///
/// **The rule.** Gates that have actually failed in the window are promoted,
/// ordered by failures per unit of time: `failures / runs / median duration`,
/// compared as integers by cross-multiplication so the order is exact and the
/// same everywhere. That ratio, and not the failure rate alone, is what
/// minimises the expected time spent before a failing push fails — a gate
/// that fails one push in ten and takes five seconds is worth attempting
/// before one that fails one in three and takes twenty minutes.
///
/// Everything else — a gate that has never failed, and every gate with no
/// record at all — keeps its declared order, after the promoted ones. The
/// declared index is the final tie-break, so the result is deterministic for
/// a given history.
pub fn order_by_evidence(
    gates: &[String],
    history: &History,
    now: u64,
    window_days: u64,
) -> Vec<usize> {
    let runs = history.within(now, window_days);
    let mut stats: BTreeMap<&str, (u64, u64, Vec<u64>)> = BTreeMap::new();
    for (_, run) in &runs {
        if !run.outcome.is_verdict() {
            continue;
        }
        let e = stats.entry(run.gate.as_str()).or_insert((0, 0, Vec::new()));
        e.0 += 1;
        if run.outcome == RunOutcome::Failed {
            e.1 += 1;
        }
        e.2.push(run.ms);
    }
    // (failures, runs, median ms) per declared gate, or None with no record.
    let weight = |name: &String| -> Option<(u64, u64, u64)> {
        let (runs, fails, ms) = stats.get(name.as_str())?;
        (*fails > 0).then(|| (*fails, *runs, median(ms).max(1)))
    };
    let mut order: Vec<usize> = (0..gates.len()).collect();
    order.sort_by(|&a, &b| {
        match (weight(&gates[a]), weight(&gates[b])) {
            (Some((fa, ra, ma)), Some((fb, rb, mb))) => {
                // fa/(ra*ma) vs fb/(rb*mb), cross-multiplied in u128 so no
                // float ever decides the order of a hook.
                let left = u128::from(fa) * u128::from(rb) * u128::from(mb);
                let right = u128::from(fb) * u128::from(ra) * u128::from(ma);
                right.cmp(&left).then(a.cmp(&b))
            }
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.cmp(&b),
        }
    });
    order
}

fn median(values: &[u64]) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut v = values.to_vec();
    v.sort_unstable();
    let mid = v.len() / 2;
    if v.len() % 2 == 1 {
        v[mid]
    } else {
        // The lower of the two middles rather than their mean: the numbers
        // are durations, and an average of two invents a duration nothing
        // ever took.
        v[mid - 1]
    }
}

/// A duration a person can read. Milliseconds up to a second, then seconds,
/// then minutes — the report is scanned, not computed with.
pub fn duration(ms: u64) -> String {
    if ms < 1_000 {
        return format!("{ms} ms");
    }
    if ms < 60_000 {
        return format!("{}.{} s", ms / 1000, (ms % 1000) / 100);
    }
    format!("{} m {:02} s", ms / 60_000, (ms % 60_000) / 1000)
}

/// A fingerprint, shortened the way git shortens one.
fn short(oid: &str) -> &str {
    oid.get(..8).unwrap_or(oid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(at: u64, gate: &str, outcome: RunOutcome, ms: u64) -> Run {
        Run {
            at,
            gate: gate.to_string(),
            outcome,
            ms,
        }
    }

    fn history(rows: &[(&str, Run)]) -> History {
        History {
            runs: rows
                .iter()
                .map(|(fp, r)| ((*fp).to_string(), r.clone()))
                .collect(),
        }
    }

    /// The same, where a test builds its fingerprints rather than spelling
    /// them out.
    fn owned_history(rows: Vec<(String, Run)>) -> History {
        History { runs: rows }
    }

    const NOW: u64 = 1_800_000_000;
    const DAY: u64 = 86_400;

    fn of<'a>(reports: &'a [GateReport], gate: &str) -> &'a GateReport {
        reports
            .iter()
            .find(|r| r.gate == gate)
            .unwrap_or_else(|| panic!("no report for {gate}: {reports:?}"))
    }

    fn kinds(r: &GateReport) -> Vec<FlagKind> {
        r.flags.iter().map(|f| f.kind).collect()
    }

    /// The signature the fleet audit of 2026-09-19 found four times: a gate
    /// that used to take minutes returns in milliseconds and still passes.
    /// Nothing in the hook can see it — the exit code is 0 — and this is the
    /// only place the collapse is visible.
    #[test]
    fn a_suite_that_collapsed_to_milliseconds_is_a_no_op_suspect() {
        let mut rows: Vec<(&str, Run)> = (0..8)
            .map(|i| {
                (
                    "tree0",
                    run(
                        NOW - (10 - i) * DAY,
                        "pre-push-cargo-test",
                        RunOutcome::Passed,
                        600_000,
                    ),
                )
            })
            .collect();
        rows.push((
            "tree9",
            run(NOW - DAY, "pre-push-cargo-test", RunOutcome::Passed, 400),
        ));
        let reports = summarise(&history(&rows), NOW, &Thresholds::default());
        let r = of(&reports, "pre-push-cargo-test");
        assert_eq!(kinds(r), vec![FlagKind::NoOpSuspect]);
        assert!(
            r.flags[0].why.contains("400 ms") && r.flags[0].why.contains("threshold 10%"),
            "the flag must carry the comparison it made: {:?}",
            r.flags[0].why
        );
    }

    /// The other shape: a gate that has ALWAYS returned instantly has no
    /// collapse to measure, so the rule is "it has never once done real
    /// work", and it only fires once there is enough history to say so.
    #[test]
    fn a_gate_that_never_once_took_real_time_is_also_suspect() {
        let rows: Vec<(&str, Run)> = (0..6)
            .map(|i| {
                (
                    "tree0",
                    run(
                        NOW - (7 - i) * DAY,
                        "pre-push-audit-python",
                        RunOutcome::Passed,
                        if i == 5 { 30 } else { 4_000 },
                    ),
                )
            })
            .collect();
        let reports = summarise(&history(&rows), NOW, &Thresholds::default());
        assert_eq!(
            kinds(of(&reports, "pre-push-audit-python")),
            vec![FlagKind::NoOpSuspect]
        );
    }

    /// Two verdicts, one tree. The content could not have changed between
    /// them, so the gate did.
    #[test]
    fn one_fingerprint_with_two_answers_is_flaky() {
        let rows = [
            (
                "treeA",
                run(NOW - 6 * DAY, "pre-push-pytest", RunOutcome::Passed, 90_000),
            ),
            (
                "treeA",
                run(NOW - 5 * DAY, "pre-push-pytest", RunOutcome::Failed, 88_000),
            ),
            (
                "treeA",
                run(NOW - 4 * DAY, "pre-push-pytest", RunOutcome::Passed, 91_000),
            ),
            (
                "treeB",
                run(NOW - 3 * DAY, "pre-push-pytest", RunOutcome::Passed, 92_000),
            ),
            (
                "treeB",
                run(NOW - 2 * DAY, "pre-push-pytest", RunOutcome::Passed, 90_500),
            ),
        ];
        let r = summarise(&history(&rows), NOW, &Thresholds::default());
        let r = of(&r, "pre-push-pytest");
        assert_eq!(kinds(r), vec![FlagKind::Flaky]);
        assert!(r.flags[0].why.contains("treeA"), "{:?}", r.flags[0].why);
    }

    /// Stale is a claim about the NEIGHBOURS: other gates kept judging trees
    /// this one did not. A quiet repository where nothing ran is not stale.
    #[test]
    fn a_gate_left_behind_by_its_neighbours_is_stale() {
        let mut rows = vec![(
            "tree0".to_string(),
            run(
                NOW - 40 * DAY,
                "pre-push-go-test",
                RunOutcome::Passed,
                5_000,
            ),
        )];
        for i in 0..12u64 {
            rows.push((
                format!("tree{}", i + 1),
                run(
                    NOW - (12 - i) * DAY,
                    "pre-push-cargo-test",
                    RunOutcome::Passed,
                    5_000,
                ),
            ));
        }
        let reports = summarise(&owned_history(rows), NOW, &Thresholds::default());
        let stale = of(&reports, "pre-push-go-test");
        assert!(kinds(stale).contains(&FlagKind::Stale), "{:?}", stale.flags);
        assert!(
            of(&reports, "pre-push-cargo-test").flags.is_empty(),
            "the gate that kept running is not stale"
        );
    }

    /// The refusal that matters most: two runs say nothing, and the report
    /// says THAT rather than an empty flags column.
    #[test]
    fn too_little_history_abstains_out_loud() {
        let rows = [
            (
                "tree0",
                run(
                    NOW - 2 * DAY,
                    "pre-push-cargo-test",
                    RunOutcome::Passed,
                    600_000,
                ),
            ),
            (
                "tree1",
                run(NOW - DAY, "pre-push-cargo-test", RunOutcome::Passed, 300),
            ),
        ];
        let reports = summarise(&history(&rows), NOW, &Thresholds::default());
        let r = of(&reports, "pre-push-cargo-test");
        assert_eq!(
            r.abstained.as_deref(),
            Some("insufficient history (2 verdicts)")
        );
        assert!(
            r.flags.is_empty(),
            "a two-run history must not raise a flag: {:?}",
            r.flags
        );
        assert_eq!((r.runs, r.passed), (2, 2), "the counts are still reported");
    }

    /// A gate that could not run is not a gate that passed. The count is
    /// kept separately, because the audit this module exists for found four
    /// mechanisms whose whole failure mode was that nobody counted it.
    #[test]
    fn an_unavailable_run_is_never_counted_as_a_pass() {
        let rows = [
            (
                "tree0",
                run(
                    NOW - 2 * DAY,
                    "pre-push-audit-go",
                    RunOutcome::Unavailable,
                    200,
                ),
            ),
            (
                "tree1",
                run(NOW - DAY, "pre-push-audit-go", RunOutcome::Unavailable, 210),
            ),
        ];
        let reports = summarise(&history(&rows), NOW, &Thresholds::default());
        let r = of(&reports, "pre-push-audit-go");
        assert_eq!((r.passed, r.failed, r.unavailable), (0, 0, 2));
    }

    /// Outside the window is outside the report.
    #[test]
    fn the_window_excludes_what_is_older_than_it() {
        let rows = [
            (
                "tree0",
                run(
                    NOW - 200 * DAY,
                    "pre-push-cargo-test",
                    RunOutcome::Failed,
                    500,
                ),
            ),
            (
                "tree1",
                run(NOW - DAY, "pre-push-cargo-test", RunOutcome::Passed, 500),
            ),
        ];
        let reports = summarise(&history(&rows), NOW, &Thresholds::default());
        assert_eq!(of(&reports, "pre-push-cargo-test").runs, 1);
    }

    /// Ordering promotes the gate that catches the most per second spent —
    /// not the one that fails most often. A five-second audit failing one
    /// push in six beats a twenty-minute suite failing one in three.
    #[test]
    fn ordering_prefers_failures_per_second_not_failure_rate() {
        let gates: Vec<String> = ["suite", "audit"].iter().map(|s| s.to_string()).collect();
        let mut rows: Vec<(&str, Run)> = Vec::new();
        for i in 0..6u64 {
            rows.push((
                "tree0",
                run(
                    NOW - (10 - i) * DAY,
                    "suite",
                    if i < 2 {
                        RunOutcome::Failed
                    } else {
                        RunOutcome::Passed
                    },
                    1_200_000,
                ),
            ));
            rows.push((
                "tree0",
                run(
                    NOW - (10 - i) * DAY,
                    "audit",
                    if i < 1 {
                        RunOutcome::Failed
                    } else {
                        RunOutcome::Passed
                    },
                    5_000,
                ),
            ));
        }
        let order = order_by_evidence(&gates, &history(&rows), NOW, 90);
        assert_eq!(order, vec![1, 0], "the cheap audit runs first");
    }

    /// With no record at all, the declared order is the order. This is the
    /// fallback every repository starts in, and it must be exact.
    #[test]
    fn with_no_history_the_declared_order_is_kept() {
        let gates: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            order_by_evidence(&gates, &History::default(), NOW, 90),
            vec![0, 1, 2]
        );
    }

    /// A gate that has never failed is not promoted over one that has, and
    /// the never-failed ones keep their order among themselves.
    #[test]
    fn only_gates_that_actually_failed_are_promoted() {
        let gates: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let rows = [
            ("t", run(NOW - 3 * DAY, "a", RunOutcome::Passed, 1_000)),
            ("t", run(NOW - 3 * DAY, "b", RunOutcome::Passed, 1_000)),
            ("t", run(NOW - 2 * DAY, "c", RunOutcome::Failed, 1_000)),
            ("t", run(NOW - DAY, "c", RunOutcome::Passed, 1_000)),
        ];
        assert_eq!(
            order_by_evidence(&gates, &history(&rows), NOW, 90),
            vec![2, 0, 1]
        );
    }

    /// Every gate handed in comes back out, exactly once — the property that
    /// makes this a permutation and not a filter.
    #[test]
    fn ordering_is_a_permutation_and_never_drops_a_gate() {
        let gates: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        let rows = [
            ("t", run(NOW - DAY, "b", RunOutcome::Failed, 10)),
            ("t", run(NOW - DAY, "d", RunOutcome::Failed, 10_000)),
        ];
        let mut order = order_by_evidence(&gates, &history(&rows), NOW, 90);
        assert_eq!(order.len(), gates.len());
        order.sort_unstable();
        assert_eq!(order, vec![0, 1, 2, 3]);
    }

    /// The batch reader, against the shape git actually prints.
    #[test]
    fn cat_file_batch_output_splits_into_bodies() {
        let oid = "0123456789abcdef0123456789abcdef01234567";
        let other = "89abcdef0123456789abcdef0123456789abcdef";
        let out = format!(
            "{oid} blob 42\namont-gate-v1 pre-push-cargo-test\nrun 10 pre-push-cargo-test pass 5\n\
             {other} blob 14\namont-gate-v1\n"
        );
        let got = parse_batch(&out);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, oid);
        assert!(got[0].1.contains("run 10"));
        assert_eq!(got[1].0, other);
    }

    #[test]
    fn durations_read_like_durations() {
        assert_eq!(duration(400), "400 ms");
        assert_eq!(duration(5_400), "5.4 s");
        assert_eq!(duration(662_000), "11 m 02 s");
    }
}
