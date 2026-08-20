//! Replaying the transcripts through the current rules.
//!
//! This is the part that decides whether a rule is allowed to block. A rule
//! whose fire rate nobody has looked at is a rule nobody can defend, and the
//! whole shadow-first ladder is meaningless without a number to graduate on.
//!
//! Rates are per 1,000 tool calls in the same bucket, so weeks of very
//! different length compare directly. The denominator is every Bash call seen,
//! including subagents' — the hook fires for those too.

use std::collections::BTreeMap;

use amont_runtime::json;

use crate::civil::{iso, week_start, Day};
use crate::rules::{self, Rule};
use crate::shell;
use crate::transcript::{self, Scan, ScanError, Stats};

pub struct Sample {
    pub day: Day,
    pub cwd: String,
    /// Centred on the match, not on the head of the command — a sample printed
    /// from the head of a 2 KB script shows text unrelated to the match, and
    /// then human review reviews the wrong thing.
    pub excerpt: String,
}

#[derive(Default, Clone)]
pub struct Bucket {
    pub calls: u32,
    pub fires: Vec<u32>,
}

pub struct Report {
    pub rules: Vec<&'static Rule>,
    pub weeks: BTreeMap<Day, Bucket>,
    pub totals: Vec<u32>,
    pub samples: Vec<Vec<Sample>>,
    pub stats: Stats,
    pub opaque: u64,
}

pub fn run(
    scan: &Scan,
    rules: &[&'static Rule],
    samples_per_rule: usize,
) -> Result<Report, ScanError> {
    let mut weeks: BTreeMap<Day, Bucket> = BTreeMap::new();
    let mut totals = vec![0u32; rules.len()];
    let mut samples: Vec<Vec<Sample>> = rules.iter().map(|_| Vec::new()).collect();
    let mut opaque = 0u64;

    let stats = transcript::for_each_call(scan, |call| {
        let bucket = weeks.entry(week_start(call.day)).or_insert_with(|| Bucket {
            calls: 0,
            fires: vec![0; rules.len()],
        });
        bucket.calls += 1;

        if call.command.is_empty() {
            return;
        }
        let parsed = shell::lex(call.command);
        if matches!(parsed, shell::Parsed::Opaque(_)) {
            opaque += 1;
            return;
        }
        for (i, rule) in rules.iter().enumerate() {
            // `examine` only. `confirm` touches the world, and the world has
            // moved since these commands ran — replaying it would produce a
            // number that describes today rather than then.
            if let Some(found) = (rule.examine)(&parsed) {
                bucket.fires[i] += 1;
                totals[i] += 1;
                if samples[i].len() < samples_per_rule {
                    samples[i].push(Sample {
                        day: call.day,
                        cwd: short_cwd(call.cwd),
                        excerpt: excerpt(call.command, found.span.start, found.span.end),
                    });
                }
            }
        }
    })?;

    Ok(Report {
        rules: rules.to_vec(),
        weeks,
        totals,
        samples,
        stats,
        opaque,
    })
}

/// A window of the command showing the match.
///
/// When the matched span is longer than the window — a `git commit` carrying
/// two `-m` bodies before its pipe, say — the ELLIPSIS GOES IN THE MIDDLE. The
/// two ends of a span are the two things a reviewer needs to see together: for
/// `pipe-to-tail` they are the mutating verb and the sink it pipes into, and an
/// excerpt that shows only the head proves nothing about why the rule fired.
/// The first version of this printed the head and produced samples that looked
/// like false positives while being correct.
pub fn excerpt(src: &str, from: usize, to: usize) -> String {
    // Derived from the terminal, not invented here: `amont_runtime::live`
    // already owns the `$COLUMNS`-with-a-sane-floor answer, and a second copy
    // would be a second place for the fallback to drift. Samples are printed
    // under a six-space indent, hence the margin.
    let width = amont_runtime::live::term_width().saturating_sub(8).max(40);
    let from = floor_char(src, from.min(src.len()));
    let to = ceil_char(src, to.min(src.len()).max(from));
    let mid = one_line(src[from..to].trim());
    let mut out = if mid.chars().count() > width {
        let all: Vec<char> = mid.chars().collect();
        let half = width / 2;
        let head: String = all[..half].iter().collect();
        let tail: String = all[all.len() - half..].iter().collect();
        format!("{head} […] {tail}")
    } else {
        mid
    };
    if from > 0 {
        out = format!("… {out}");
    }
    if to < src.len() {
        out = format!("{out} …");
    }
    out
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn floor_char(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

fn short_cwd(cwd: &str) -> String {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && cwd.starts_with(&h) => format!("~{}", &cwd[h.len()..]),
        _ => cwd.to_string(),
    }
}

fn rate(fires: u32, calls: u32) -> f64 {
    if calls == 0 {
        0.0
    } else {
        1000.0 * f64::from(fires) / f64::from(calls)
    }
}

impl Report {
    /// The per-1000 rate for one rule across every bucket, oldest first.
    pub fn series(&self, i: usize) -> Vec<f64> {
        self.weeks
            .values()
            .map(|b| rate(b.fires[i], b.calls))
            .collect()
    }

    pub fn render(&self, roots: &transcript::Roots) -> String {
        let mut s = String::new();
        s.push_str(&format!(
            "transcripts: {} ({} files, {:.0} MB, {} calls, {} duplicates dropped)\n",
            roots
                .dirs
                .iter()
                .map(|d| short_cwd(&d.display().to_string()))
                .collect::<Vec<_>>()
                .join(", "),
            self.stats.files,
            self.stats.bytes as f64 / 1_048_576.0,
            self.stats.calls,
            self.stats.duplicates,
        ));
        s.push_str(
            "UTC weeks. Sessions run on the web are not on this machine and cannot be counted.\n\n",
        );

        // Wide enough for the longest id plus a separating space. A fixed
        // width let `gh-pr-merge-auto` run into its neighbour's header.
        let width = self.rules.iter().map(|r| r.id.len()).max().unwrap_or(8) + 2;
        s.push_str(&format!("{:<14}{:>8}", "week starting", "calls"));
        for r in &self.rules {
            s.push_str(&format!("{:>width$}", r.id));
        }
        s.push('\n');
        for (week, b) in &self.weeks {
            s.push_str(&format!("{:<14}{:>8}", iso(*week), b.calls));
            for i in 0..self.rules.len() {
                s.push_str(&format!("{:>width$.1}", rate(b.fires[i], b.calls)));
            }
            s.push('\n');
        }
        let calls: u32 = self.weeks.values().map(|b| b.calls).sum();
        s.push_str(&format!("{:<14}{:>8}", "total", calls));
        for t in &self.totals {
            s.push_str(&format!("{t:>width$}"));
        }
        s.push_str("\n\n");

        s.push_str(&format!(
            "{:<18}{:>8}{:>11}{:>13}  {}\n",
            "rule", "total", "p95/1000", "trend", "ships as"
        ));
        for (i, r) in self.rules.iter().enumerate() {
            let series = self.series(i);
            s.push_str(&format!(
                "{:<18}{:>8}{:>11.1}{:>13}  {}\n",
                r.id,
                self.totals[i],
                p95(&series),
                match r.evidence.trend {
                    rules::Trend::Flat(w) => format!("flat {w}w"),
                    rules::Trend::Improving => "improving".to_string(),
                    rules::Trend::Rare => "rare".to_string(),
                },
                r.default_stance.as_str(),
            ));
        }

        for (i, r) in self.rules.iter().enumerate() {
            if self.samples[i].is_empty() {
                continue;
            }
            s.push_str(&format!("\n{} — samples\n", r.id));
            for sample in &self.samples[i] {
                s.push_str(&format!(
                    "  {}  {}\n      {}\n",
                    iso(sample.day),
                    sample.cwd,
                    sample.excerpt
                ));
            }
        }
        s
    }

    pub fn to_json(&self, roots: &transcript::Roots) -> String {
        let rules: Vec<String> = self
            .rules
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let weeks: Vec<String> = self
                    .weeks
                    .iter()
                    .map(|(w, b)| {
                        json::object(&[
                            json::string_field("week", &iso(*w)),
                            json::int_field("calls", i64::from(b.calls)),
                            json::int_field("fires", i64::from(b.fires[i])),
                            json::string_field(
                                "per_1000",
                                &format!("{:.1}", rate(b.fires[i], b.calls)),
                            ),
                        ])
                    })
                    .collect();
                json::object(&[
                    json::string_field("id", r.id),
                    json::int_field("total", i64::from(self.totals[i])),
                    json::string_field("stance", r.default_stance.as_str()),
                    json::string_field("p95_per_1000", &format!("{:.1}", p95(&self.series(i)))),
                    format!("\"weeks\":{}", json::array(&weeks)),
                ])
            })
            .collect();
        json::object(&[
            json::string_array_field(
                "transcripts",
                &roots
                    .dirs
                    .iter()
                    .map(|d| d.display().to_string())
                    .collect::<Vec<_>>(),
            ),
            json::int_field("files", self.stats.files as i64),
            json::int_field("calls", self.stats.calls as i64),
            json::int_field("duplicates", self.stats.duplicates as i64),
            json::int_field("opaque", self.opaque as i64),
            format!("\"rules\":{}", json::array(&rules)),
            json::string_field(
                "note",
                "UTC weeks; local CLI sessions only, web sessions are not on this machine",
            ),
        ])
    }
}

/// The 95th percentile of the weekly rates — the number a graduation is judged
/// on, because a rule is intrusive at its worst week, not its average one.
pub fn p95(series: &[f64]) -> f64 {
    if series.is_empty() {
        return 0.0;
    }
    let mut v = series.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let idx = (((v.len() - 1) as f64) * 0.95).round() as usize;
    v[idx]
}
