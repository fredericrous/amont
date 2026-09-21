//! `amont-fleet gates` — what every repository's own record says about its
//! gates.
//!
//! The hooks have been writing a run per gate per push into
//! `refs/notes/amont-gate` (see `amont_runtime::gate_stamp`). Nothing read it
//! across repositories, and across repositories is where the question lives:
//! a suite that quietly stopped testing anything looks perfect from inside
//! the repository it broke in — green, fast, and getting faster.
//!
//! Read-only, and the statistics are NOT computed here:
//! `amont_runtime::gate_evidence` owns every threshold and every flag, for
//! the same reason `downgrades` defers to the runtime's parser — the
//! dashboard must not grow a second opinion about a record the hooks own.
//! What this module adds is the `Serialize` coat the dependency-free crate
//! cannot wear, the table, and the sanitising a fleet report owes a reader:
//! a gate id can come from somebody else's committed `amont.conf`.

use std::path::{Path, PathBuf};

use amont_runtime::gate_evidence::{self, GateReport, Thresholds};
use serde::Serialize;

/// The `--json` contract: the thresholds the flags were decided against,
/// then the repositories. The thresholds are IN the document on purpose — a
/// consumer that cannot see them cannot tell a flag from an opinion, and the
/// defaults are allowed to change between releases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Document {
    pub thresholds: ThresholdsJson,
    pub repos: Vec<RepoGates>,
}

impl Document {
    pub fn new(t: &Thresholds, repos: Vec<RepoGates>) -> Document {
        Document {
            thresholds: ThresholdsJson {
                window_days: t.window_days,
                min_runs: t.min_runs,
                noop_ratio_percent: t.noop_ratio_percent,
                noop_median_secs: t.noop_median_secs,
                fast_pass_ms: t.fast_pass_ms,
                stale_pushes: t.stale_runs,
                stale_days: t.stale_days,
            },
            repos,
        }
    }
}

/// The runtime's `Thresholds` in a coat it cannot wear itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ThresholdsJson {
    pub window_days: u64,
    pub min_runs: usize,
    pub noop_ratio_percent: u64,
    pub noop_median_secs: u64,
    pub fast_pass_ms: u64,
    pub stale_pushes: usize,
    pub stale_days: u64,
}

/// One repository's gates, or the fact that it has no record yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepoGates {
    /// Path relative to the scan root — what a human recognises.
    pub repo: PathBuf,
    pub gates: Vec<GateRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GateRow {
    pub gate: String,
    pub runs: usize,
    pub passed: usize,
    pub failed: usize,
    /// Ran and judged nothing. Never folded into `passed`.
    pub unavailable: usize,
    pub median_ms: u64,
    pub last_ms: u64,
    /// Epoch seconds of the last run, and its age in whole days.
    pub last_at: u64,
    pub last_age_days: u64,
    /// How the last run ended, in the record's own words (`pass`, `fail`,
    /// `unavailable`, …). A consumer reading the JSON should not have to
    /// infer it from the counts.
    pub last_outcome: Option<String>,
    /// Later trees other gates judged and this one did not — staleness in
    /// pushes rather than in days.
    pub missed_pushes: usize,
    pub fingerprints: usize,
    pub flags: Vec<FlagRow>,
    /// Present when the history is too short for any flag to be honest.
    pub insufficient_history: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FlagRow {
    pub flag: String,
    pub why: String,
}

fn row(r: &GateReport, now: u64) -> GateRow {
    GateRow {
        gate: amont_runtime::ui::sanitize(&r.gate),
        runs: r.runs,
        passed: r.passed,
        failed: r.failed,
        unavailable: r.unavailable,
        median_ms: r.median_ms,
        last_ms: r.last_ms,
        last_at: r.last_at,
        last_age_days: r.last_age_days(now),
        last_outcome: r.last_outcome.map(|o| o.as_str().to_string()),
        missed_pushes: r.missed,
        fingerprints: r.fingerprints,
        flags: r
            .flags
            .iter()
            .map(|f| FlagRow {
                flag: f.kind.as_str().to_string(),
                why: amont_runtime::ui::sanitize(&f.why),
            })
            .collect(),
        insufficient_history: r.abstained.clone(),
    }
}

/// Read the record of every repository in `repos` (paths relative to
/// `root`), newest-first by nothing: repositories keep the scan's order, and
/// gates within one keep the runtime's name order, so two runs of this
/// command over an unchanged fleet print the same bytes.
///
/// `on` is called before each repository is read, for the same reason the
/// scan has a progress callback: this spawns two gits per repository, and a
/// fleet-sized tree is not instant.
pub fn collect(
    root: &Path,
    repos: &[PathBuf],
    now: u64,
    t: &Thresholds,
    on: &mut dyn FnMut(&Path),
) -> Vec<RepoGates> {
    repos
        .iter()
        .map(|repo| {
            on(repo);
            let history = gate_evidence::history_in(&root.join(repo));
            RepoGates {
                repo: repo.clone(),
                gates: gate_evidence::summarise(&history, now, t)
                    .iter()
                    .map(|r| row(r, now))
                    .collect(),
            }
        })
        .collect()
}

/// The one-line statement of what the numbers below were compared against.
/// Printed every time: a threshold nobody can see is a threshold nobody can
/// argue with, and this report exists to be argued with.
pub fn thresholds_line(t: &Thresholds) -> String {
    format!(
        "window {} days · no-op suspect below {}% of a median of {}s+, or a pass under {} from a gate that never was that fast · stale after {} missed pushes or {} days · under {} verdicts nothing is flagged",
        t.window_days,
        t.noop_ratio_percent,
        t.noop_median_secs,
        gate_evidence::duration(t.fast_pass_ms),
        t.stale_runs,
        t.stale_days,
        t.min_runs,
    )
}

/// The table.
pub fn report(all: &[RepoGates], t: &Thresholds, shown: impl Fn(&Path) -> String) {
    println!("{}", thresholds_line(t));
    println!();
    let with_history: Vec<&RepoGates> = all.iter().filter(|r| !r.gates.is_empty()).collect();
    let mut flagged = 0usize;
    let mut gates = 0usize;
    for repo in &with_history {
        // The scan names a repository RELATIVE to the root, which is the
        // empty string when the root IS the repository —
        // `gates --root . --depth 1`, the single-repo form the docs
        // recommend. A blank heading over a table of real rows reads as a
        // bug; `.` is what the reader typed.
        let name = shown(&repo.repo);
        println!("{}", if name.is_empty() { "." } else { &name });
        let width = repo
            .gates
            .iter()
            .map(|g| g.gate.chars().count())
            .max()
            .unwrap_or(0)
            .max(4);
        for g in &repo.gates {
            gates += 1;
            println!(
                "  {:width$}  {:>4} runs  {:>3} pass  {:>3} fail  median {:>9}  last {:>9}  {:>4}d ago",
                g.gate,
                g.runs,
                g.passed,
                g.failed,
                gate_evidence::duration(g.median_ms),
                gate_evidence::duration(g.last_ms),
                g.last_age_days,
            );
            if g.unavailable > 0 {
                println!(
                    "  {:width$}  {} run(s) could not judge anything — not counted as passes",
                    "", g.unavailable
                );
            }
            for f in &g.flags {
                flagged += 1;
                println!("  {:width$}  {}: {}", "", f.flag.to_uppercase(), f.why);
            }
            // Said out loud rather than left as an empty flags column: an
            // empty column reads as "all clear", and it is not.
            if let Some(why) = &g.insufficient_history {
                // Staleness may still have been flagged above — it is a claim
                // about the OTHER gates' record, which a short history of
                // this one does not weaken. Saying "no flag computed" under a
                // STALE line would read as a contradiction, so the sentence
                // names what was actually withheld.
                let withheld = if g.flags.is_empty() {
                    "no flag computed"
                } else {
                    "nothing judged about its cost or its flakiness"
                };
                println!("  {:width$}  {why} — {withheld}", "");
            }
        }
        println!();
    }
    let quiet = all.len() - with_history.len();
    println!(
        "{} gate(s) with a record across {} repositor{}; {} finding(s)",
        gates,
        with_history.len(),
        if with_history.len() == 1 { "y" } else { "ies" },
        flagged,
    );
    if quiet > 0 {
        // Never silently: a repository with no record is not a repository
        // with nothing wrong, and the difference is the whole point of the
        // module this reads.
        println!(
            "{quiet} repositor{} have pushed nothing through a gate yet (or their notes have been \
             pruned) — no record, so nothing is claimed about them",
            if quiet == 1 { "y" } else { "ies" }
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amont_runtime::gate_stamp::{Run, RunOutcome};

    fn history(rows: Vec<(&str, u64, &str, RunOutcome, u64)>) -> gate_evidence::History {
        gate_evidence::History {
            runs: rows
                .into_iter()
                .map(|(fp, at, gate, outcome, ms)| {
                    (
                        fp.to_string(),
                        Run {
                            at,
                            gate: gate.to_string(),
                            outcome,
                            ms,
                        },
                    )
                })
                .collect(),
        }
    }

    /// The rows are the runtime's report, field for field — the dashboard
    /// must not be able to disagree with the hooks about a number.
    #[test]
    fn the_rows_are_the_runtimes_report() {
        let now = 1_800_000_000;
        let h = history(vec![
            (
                "t0",
                now - 3 * 86_400,
                "pre-push-cargo-test",
                RunOutcome::Passed,
                600_000,
            ),
            (
                "t1",
                now - 86_400,
                "pre-push-cargo-test",
                RunOutcome::Failed,
                590_000,
            ),
        ]);
        let theirs = gate_evidence::summarise(&h, now, &Thresholds::default());
        let ours: Vec<GateRow> = theirs.iter().map(|r| row(r, now)).collect();
        assert_eq!(ours.len(), 1);
        assert_eq!(
            (
                ours[0].runs,
                ours[0].passed,
                ours[0].failed,
                ours[0].median_ms
            ),
            (
                theirs[0].runs,
                theirs[0].passed,
                theirs[0].failed,
                theirs[0].median_ms
            )
        );
        assert_eq!(
            ours[0].insufficient_history.as_deref(),
            Some("insufficient history (2 verdicts)"),
            "and the abstention travels with them"
        );
    }

    /// A gate id can come out of somebody else's committed `amont.conf`. It
    /// is repository-controlled text and is sanitised before it is printed,
    /// exactly as `amont trust`'s listing is — a repository must not be able
    /// to choose how a fleet report renders.
    #[test]
    fn a_repository_controlled_gate_id_cannot_move_the_cursor() {
        let now = 1_800_000_000;
        let h = history(vec![(
            "t0",
            now - 86_400,
            "pre-push-\u{1b}[2Kevil",
            RunOutcome::Passed,
            10,
        )]);
        let reports = gate_evidence::summarise(&h, now, &Thresholds::default());
        let r = row(&reports[0], now);
        assert!(!r.gate.contains('\u{1b}'), "{:?}", r.gate);
    }

    /// The thresholds are printed, always, and they are the ones in force.
    #[test]
    fn the_thresholds_line_states_the_numbers_it_used() {
        let t = Thresholds {
            window_days: 7,
            noop_ratio_percent: 25,
            ..Thresholds::default()
        };
        let line = thresholds_line(&t);
        assert!(line.contains("window 7 days"), "{line}");
        assert!(line.contains("25%"), "{line}");
    }
}
