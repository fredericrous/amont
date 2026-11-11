//! The two host sizing keys, resolved once for every client (ADR-0021).
//! `resolve` is pure: the environment, the `git config --show-scope` rows and
//! the detected core count are passed in, and it returns the values, where
//! each came from, and the warnings. The caller prints the warnings, so this
//! module writes nothing.
//!
//! Precedence, as greengate's `host_integer_or` has it: a valid environment
//! variable, then the last `global` or `system` row for the key, then the
//! detected default. An invalid environment value is a warning and the next
//! source is read; an invalid git value is a warning and the default is used.
//! A key set only in a repository's own config is a warning, never used.

use std::ops::RangeInclusive;
use std::sync::OnceLock;

use crate::Sizing;

/// The git key for the core count, and its environment name.
pub const CORES_KEY: &str = "greengate.hostCores";
pub const CORES_ENV: &str = "GREENGATE_HOST_CORES";
/// The git key for the slot count (unset auto, `0` off, `N` fixed), and its
/// environment name.
pub const SLOTS_KEY: &str = "greengate.hostSlots";
pub const SLOTS_ENV: &str = "GREENGATE_HOST_SLOTS";

/// Accepted range of the core count.
pub const CORES_RANGE: RangeInclusive<i64> = 1..=1024;
/// Accepted range of the slot count.
pub const SLOTS_RANGE: RangeInclusive<i64> = 0..=64;

/// One `git config --show-scope --get-regexp` row: (scope, lowercased key,
/// value).
pub type Row = (String, String, String);

/// Where a resolved value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Source {
    /// The environment variable.
    Env,
    /// `git config --global`.
    Global,
    /// `git config --system`.
    System,
    /// Detected: the physical cores, or automatic sizing for the slots.
    Detected,
    /// Detected, but the OS reported no physical count: the logical count.
    LogicalFallback,
    /// Detected, but an affinity or cgroup limit allows fewer cores than the
    /// physical count: that limit.
    LimitedByAffinity,
}

impl Source {
    /// `env`, `global`, `system`, `detected`, `detected (logical fallback)`,
    /// `limited by affinity`.
    pub fn name(self) -> &'static str {
        match self {
            Source::Env => "env",
            Source::Global => "global",
            Source::System => "system",
            Source::Detected => "detected",
            Source::LogicalFallback => "detected (logical fallback)",
            Source::LimitedByAffinity => "limited by affinity",
        }
    }

    /// Whether a person set this value (an override), rather than detection.
    pub fn is_override(self) -> bool {
        matches!(self, Source::Env | Source::Global | Source::System)
    }
}

/// A value and the source it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved<T> {
    pub value: T,
    pub source: Source,
}

/// Both host sizing keys, resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSizing {
    pub cores: Resolved<u32>,
    /// The slot sizing; `None` when `greengate.hostSlots 0` turns slots off.
    pub slots: Resolved<Option<Sizing>>,
    /// Problems found while resolving, each with the fix for its source.
    pub warnings: Vec<String>,
}

/// The core count the slots use when nothing overrides it, and how it was
/// found: the physical count, capped at the logical one (which also honours a
/// cgroup or affinity limit).
pub fn choose_cores(physical: Option<u32>, logical: u32) -> Resolved<u32> {
    let logical = logical.max(1);
    match physical {
        None => Resolved {
            value: logical,
            source: Source::LogicalFallback,
        },
        Some(p) if p > logical => Resolved {
            value: logical,
            source: Source::LimitedByAffinity,
        },
        Some(p) => Resolved {
            value: p.max(1),
            source: Source::Detected,
        },
    }
}

/// Resolve both keys from the environment `env`, the git `rows`, and the
/// `detected` core count.
pub fn resolve(
    env: impl Fn(&str) -> Option<String>,
    rows: &[Row],
    detected: Resolved<u32>,
) -> HostSizing {
    let (cores, cores_warnings) = read_key(
        CORES_KEY,
        CORES_ENV,
        CORES_RANGE,
        &env,
        rows,
        &format!("{} detected cores", detected.value),
    );
    let (slots, slots_warnings) = read_key(SLOTS_KEY, SLOTS_ENV, SLOTS_RANGE, &env, rows, "auto");
    let cores = match cores {
        Some(r) => Resolved {
            value: u32::try_from(r.value).unwrap_or(u32::MAX),
            source: r.source,
        },
        None => detected,
    };
    let slots = match slots {
        Some(Resolved { value: 0, source }) => Resolved {
            value: None,
            source,
        },
        Some(Resolved { value, source }) => Resolved {
            value: Some(Sizing::Fixed(u32::try_from(value).unwrap_or(u32::MAX))),
            source,
        },
        None => Resolved {
            value: Some(Sizing::Auto),
            source: Source::Detected,
        },
    };
    HostSizing {
        cores,
        slots,
        warnings: cores_warnings.into_iter().chain(slots_warnings).collect(),
    }
}

/// One key: the value a person set and where, or `None` for the default, and
/// any warnings. `using` names the default in a warning (`6 detected cores`).
fn read_key(
    key: &str,
    env_name: &str,
    range: RangeInclusive<i64>,
    env: &impl Fn(&str) -> Option<String>,
    rows: &[Row],
    using: &str,
) -> (Option<Resolved<i64>>, Vec<String>) {
    let mut warnings = Vec::new();
    let problem = |raw: &str| -> Option<String> {
        match raw.trim().parse::<i64>() {
            Ok(v) if range.contains(&v) => None,
            Ok(v) => Some(format!("{v}, outside {}..={}", range.start(), range.end())),
            Err(_) => Some(format!("{raw:?}, not a whole number")),
        }
    };

    if let Some(raw) = env(env_name).filter(|v| !v.trim().is_empty()) {
        match problem(&raw) {
            None => {
                return (
                    Some(Resolved {
                        value: raw.trim().parse().unwrap_or_default(),
                        source: Source::Env,
                    }),
                    warnings,
                )
            }
            Some(why) => warnings.push(format!(
                "{env_name} is {why}; reading git config instead\n  Fix: unset {env_name}"
            )),
        }
    }

    let wanted = key.to_ascii_lowercase();
    let host = rows
        .iter()
        .rev()
        .find(|(scope, k, _)| *k == wanted && (scope == "global" || scope == "system"));
    if host.is_none() && rows.iter().any(|(_, k, _)| *k == wanted) {
        warnings.push(format!(
            "{key} is set in this repository's config, which a host key never reads; \
             using {using}\n  Fix: git config --unset {key}, then git config --global {key} <n>"
        ));
    }
    let Some((scope, _, raw)) = host else {
        return (None, warnings);
    };
    let source = if scope == "system" {
        Source::System
    } else {
        Source::Global
    };
    match problem(raw) {
        None => (
            Some(Resolved {
                value: raw.trim().parse().unwrap_or_default(),
                source,
            }),
            warnings,
        ),
        Some(why) => {
            warnings.push(format!(
                "{key} in git config --{scope} is {why}; using {using}\n  \
                 Fix: git config --{scope} --unset {key}"
            ));
            (None, warnings)
        }
    }
}

/// The rows of a `git config --show-scope --get-regexp` run, from its exit
/// code and stdout. git exits 1 when no key matches, which is no rows, not a
/// failure; any other failure is `None`, git cannot report scopes.
pub fn rows_from(code: Option<i32>, stdout: &[u8]) -> Option<Vec<Row>> {
    match code {
        Some(0) => Some(parse_rows(&String::from_utf8_lossy(stdout))),
        Some(1) => Some(Vec::new()),
        _ => None,
    }
}

/// `scope<TAB>key value` lines, the key lowercased (git keys are
/// case-insensitive).
pub fn parse_rows(text: &str) -> Vec<Row> {
    text.lines()
        .filter_map(|line| {
            let (scope, rest) = line.split_once('\t')?;
            let (k, v) = rest.split_once(' ').unwrap_or((rest, ""));
            Some((scope.to_string(), k.to_ascii_lowercase(), v.to_string()))
        })
        .collect()
}

/// The arguments of the one git call that lists the host keys.
pub const GIT_ARGS: [&str; 4] = ["config", "--show-scope", "--get-regexp", r"^greengate\."];

/// The git rows, read once per process for a client with no rows of its own.
fn git_rows() -> Option<&'static [Row]> {
    static ROWS: OnceLock<Option<Vec<Row>>> = OnceLock::new();
    ROWS.get_or_init(|| {
        let out = std::process::Command::new("git")
            .args(GIT_ARGS)
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        rows_from(out.status.code(), &out.stdout)
    })
    .as_deref()
}

/// Both keys, resolved from this process's environment and one cached git
/// call, against the `detected` core count (`crate::detect_cores()`).
pub fn from_host(detected: Resolved<u32>) -> HostSizing {
    let rows = git_rows();
    let mut sizing = resolve(
        |name| std::env::var(name).ok(),
        rows.unwrap_or(&[]),
        detected,
    );
    if rows.is_none() {
        sizing.warnings.push(
            "git cannot list config with scopes, so the host keys are not read; using the \
             detected sizing\n  Fix: check that git runs (git config --global --list)"
                .to_string(),
        );
    }
    sizing
}

/// The physical core count from a `/proc/cpuinfo` text: the distinct
/// (physical id, core id) pairs. `None` when the text carries neither field
/// (aarch64 does not), so the caller falls back to the logical count.
/// Linux reads it; other platforms compile it for its tests only.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn physical_from_cpuinfo(text: &str) -> Option<u32> {
    let mut pairs = std::collections::BTreeSet::new();
    for block in text.split("\n\n") {
        let mut physical = None;
        let mut core = None;
        for line in block.lines() {
            if let Some((k, v)) = line.split_once(':') {
                match k.trim() {
                    "physical id" => physical = Some(v.trim().to_string()),
                    "core id" => core = Some(v.trim().to_string()),
                    _ => {}
                }
            }
        }
        if let Some(core) = core {
            pairs.insert((physical.unwrap_or_default(), core));
        }
    }
    (!pairs.is_empty()).then(|| u32::try_from(pairs.len()).unwrap_or(u32::MAX))
}

/// The physical core count from sysfs `core_cpus_list` entries: each physical
/// core lists its hardware threads, so the distinct lists are the cores.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn physical_from_core_lists<'a>(
    lists: impl IntoIterator<Item = &'a str>,
) -> Option<u32> {
    let distinct: std::collections::BTreeSet<&str> = lists
        .into_iter()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    (!distinct.is_empty()).then(|| u32::try_from(distinct.len()).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(scope: &str, key: &str, value: &str) -> Row {
        (
            scope.to_string(),
            key.to_ascii_lowercase(),
            value.to_string(),
        )
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    const SIX: Resolved<u32> = Resolved {
        value: 6,
        source: Source::Detected,
    };

    #[test]
    fn detected_when_nothing_is_set() {
        let s = resolve(no_env, &[], SIX);
        assert_eq!(s.cores, SIX);
        assert_eq!(
            s.slots,
            Resolved {
                value: Some(Sizing::Auto),
                source: Source::Detected
            }
        );
        assert!(s.warnings.is_empty());
    }

    #[test]
    fn an_override_of_two_gives_two() {
        let env = |n: &str| (n == CORES_ENV).then(|| "2".to_string());
        assert_eq!(
            resolve(env, &[], SIX).cores,
            Resolved {
                value: 2,
                source: Source::Env
            }
        );
    }

    #[test]
    fn env_beats_git() {
        let env = |n: &str| (n == CORES_ENV).then(|| "4".to_string());
        let rows = [row("global", CORES_KEY, "8")];
        let s = resolve(env, &rows, SIX);
        assert_eq!(
            s.cores,
            Resolved {
                value: 4,
                source: Source::Env
            }
        );
    }

    #[test]
    fn invalid_env_falls_through_to_git_with_a_warning() {
        let env = |n: &str| (n == CORES_ENV).then(|| "abc".to_string());
        let rows = [row("global", CORES_KEY, "3")];
        let s = resolve(env, &rows, SIX);
        assert_eq!(
            s.cores,
            Resolved {
                value: 3,
                source: Source::Global
            }
        );
        assert_eq!(s.warnings.len(), 1);
        assert!(
            s.warnings[0].starts_with("GREENGATE_HOST_CORES is \"abc\", not a whole number;"),
            "{}",
            s.warnings[0]
        );
        assert!(s.warnings[0].contains("Fix: unset GREENGATE_HOST_CORES"));
    }

    #[test]
    fn last_global_row_wins_and_system_is_used_when_global_is_absent() {
        let rows = [row("system", SLOTS_KEY, "2")];
        assert_eq!(
            resolve(no_env, &rows, SIX).slots,
            Resolved {
                value: Some(Sizing::Fixed(2)),
                source: Source::System
            }
        );
        let rows = [row("global", SLOTS_KEY, "1"), row("global", SLOTS_KEY, "5")];
        assert_eq!(
            resolve(no_env, &rows, SIX).slots.value,
            Some(Sizing::Fixed(5))
        );
    }

    #[test]
    fn slots_zero_turns_slots_off() {
        let rows = [row("global", SLOTS_KEY, "0")];
        assert_eq!(resolve(no_env, &rows, SIX).slots.value, None);
    }

    #[test]
    fn out_of_range_cores_warns_and_uses_detected() {
        let rows = [row("global", CORES_KEY, "0")];
        let s = resolve(no_env, &rows, SIX);
        assert_eq!(s.cores, SIX);
        assert_eq!(s.warnings.len(), 1);
        assert!(s.warnings[0].contains("using 6 detected cores"));
    }

    #[test]
    fn slots_65_warns_and_uses_auto() {
        let rows = [row("global", SLOTS_KEY, "65")];
        let s = resolve(no_env, &rows, SIX);
        assert_eq!(s.slots.value, Some(Sizing::Auto));
        assert_eq!(s.warnings.len(), 1);
        assert!(s.warnings[0].contains("using auto"));
    }

    #[test]
    fn repo_only_key_is_warned_and_ignored() {
        let rows = [row("local", CORES_KEY, "4")];
        let s = resolve(no_env, &rows, SIX);
        assert_eq!(s.cores, SIX);
        assert_eq!(s.warnings.len(), 1);
        assert!(s.warnings[0].contains("repository"));
    }

    #[test]
    fn non_numeric_git_value_warns_and_uses_detected() {
        let rows = [row("global", CORES_KEY, "abc")];
        let s = resolve(no_env, &rows, SIX);
        assert_eq!(s.cores, SIX);
        assert_eq!(s.warnings.len(), 1);
    }

    /// greengate 4.0 reads only the new names: an old git key and an old
    /// environment variable are both ignored.
    #[test]
    fn the_old_names_are_ignored() {
        let env = |n: &str| (n == "AMONT_HOST_CORES").then(|| "2".to_string());
        let rows = [row("global", "amont.hostCores", "3")];
        let s = resolve(env, &rows, SIX);
        assert_eq!(s.cores, SIX);
        assert!(s.warnings.is_empty());
    }

    #[test]
    fn detection_reports_how_it_found_the_cores() {
        assert_eq!(
            choose_cores(Some(6), 12),
            Resolved {
                value: 6,
                source: Source::Detected
            }
        );
        assert_eq!(
            choose_cores(None, 12),
            Resolved {
                value: 12,
                source: Source::LogicalFallback
            }
        );
        // An affinity limit of 4 on a 6-core host: the limit wins.
        assert_eq!(
            choose_cores(Some(6), 4),
            Resolved {
                value: 4,
                source: Source::LimitedByAffinity
            }
        );
    }

    #[test]
    fn rows_from_reads_no_match_as_no_rows_and_failure_as_none() {
        let out = b"global\tgreengate.hostCores 4\nsystem\tgreengate.hostSlots 2\n";
        assert_eq!(
            rows_from(Some(0), out),
            Some(vec![
                row("global", "greengate.hostcores", "4"),
                row("system", "greengate.hostslots", "2"),
            ])
        );
        assert_eq!(rows_from(Some(1), b""), Some(Vec::new()));
        assert_eq!(rows_from(Some(128), b""), None);
        assert_eq!(rows_from(None, b""), None);
    }

    #[test]
    fn cpuinfo_on_a_6_core_12_thread_host_gives_6() {
        // Twelve logical CPUs, two hyper-threads per core: core ids 0-5 twice.
        let mut text = String::new();
        for cpu in 0..12 {
            text.push_str(&format!(
                "processor\t: {cpu}\nphysical id\t: 0\ncore id\t\t: {}\n\n",
                cpu % 6
            ));
        }
        assert_eq!(physical_from_cpuinfo(&text), Some(6));
        // Two packages with the same core ids: the physical id tells them apart.
        let two = "physical id: 0\ncore id: 0\n\nphysical id: 1\ncore id: 0\n";
        assert_eq!(physical_from_cpuinfo(two), Some(2));
    }

    #[test]
    fn cpuinfo_without_core_fields_is_none() {
        assert_eq!(
            physical_from_cpuinfo("processor : 0\nBogoMIPS : 48\n"),
            None
        );
        assert_eq!(physical_from_cpuinfo(""), None);
    }

    #[test]
    fn sysfs_core_lists_count_distinct_cores() {
        // Siblings list the same pair: 6 cores with 2 threads each.
        let lists = [
            "0,6", "1,7", "2,8", "3,9", "4,10", "5,11", "0,6", "1,7", "2,8", "3,9", "4,10", "5,11",
        ];
        assert_eq!(physical_from_core_lists(lists), Some(6));
        assert_eq!(physical_from_core_lists(Vec::<&str>::new()), None);
    }
}
