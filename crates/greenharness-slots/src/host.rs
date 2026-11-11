//! The two host sizing keys, resolved once for every client (plan:
//! "Host slots count physical cores"). `resolve` is pure: the environment and
//! the `git config --show-scope` rows are passed in, and it returns the values,
//! where each came from, and the warnings. The caller prints the warnings, so
//! this module writes nothing.
//!
//! Precedence, as amont's `host_integer_or` has it today: a valid environment
//! variable, then the last `global` or `system` row for the key, then the
//! detected default. An invalid value is a warning and the default, except an
//! invalid environment value, which falls through to git. A key set only in a
//! repository's own config is a warning, never used.

use std::ops::RangeInclusive;
use std::sync::OnceLock;

/// The git key for the core count, and its environment name.
pub const CORES_KEY: &str = "greengate.hostCores";
pub const CORES_ENV: &str = "GREENGATE_HOST_CORES";
/// The git key for the slot count (`-1` auto, `0` off, `N` fixed), and its
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
pub enum Source {
    Env,
    Global,
    System,
    Detected,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::Env => "env",
            Source::Global => "global",
            Source::System => "system",
            Source::Detected => "detected",
        }
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
    /// `-1` auto, `0` off, `N` fixed.
    pub slots: Resolved<i64>,
    /// Problems found while resolving, each with the fix for its source.
    pub warnings: Vec<String>,
}

/// Resolve both keys from the environment `env`, the git `rows`, and the
/// detected core count `detected_cores`.
pub fn resolve(
    env: impl Fn(&str) -> Option<String>,
    rows: &[Row],
    detected_cores: u32,
) -> HostSizing {
    let mut warnings = Vec::new();
    let cores = read_key(
        CORES_KEY,
        CORES_ENV,
        CORES_RANGE,
        i64::from(detected_cores.max(1)),
        &env,
        rows,
        &mut warnings,
    );
    let slots = read_key(
        SLOTS_KEY,
        SLOTS_ENV,
        SLOTS_RANGE,
        -1,
        &env,
        rows,
        &mut warnings,
    );
    HostSizing {
        cores: Resolved {
            value: u32::try_from(cores.value).unwrap_or(u32::MAX),
            source: cores.source,
        },
        slots,
        warnings,
    }
}

fn read_key(
    key: &str,
    env_name: &str,
    range: RangeInclusive<i64>,
    default: i64,
    env: &impl Fn(&str) -> Option<String>,
    rows: &[Row],
    warnings: &mut Vec<String>,
) -> Resolved<i64> {
    let accept = |raw: &str, from: &str| -> Result<i64, String> {
        match raw.trim().parse::<i64>() {
            Ok(v) if range.contains(&v) => Ok(v),
            Ok(v) => Err(format!(
                "{from}: {key} = {v} is outside {}..={}",
                range.start(),
                range.end()
            )),
            Err(_) => Err(format!("{from}: {key} = {raw:?} is not a whole number")),
        }
    };

    if let Some(raw) = env(env_name).filter(|v| !v.trim().is_empty()) {
        match accept(&raw, env_name) {
            Ok(v) => {
                return Resolved {
                    value: v,
                    source: Source::Env,
                }
            }
            Err(problem) => warnings.push(format!(
                "{problem}; the next source is read. Fix: unset {env_name}"
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
            "{key} is set in this repository's config, but a host key is read from --global \
             or --system only; it is ignored here. Fix: git config --unset {key}"
        ));
    }
    if let Some((scope, _, raw)) = host {
        let source = if scope == "system" {
            Source::System
        } else {
            Source::Global
        };
        return match accept(raw, scope) {
            Ok(v) => Resolved { value: v, source },
            Err(problem) => {
                warnings.push(format!(
                    "{problem}; the detected default is used. Fix: git config --{scope} --unset {key}"
                ));
                Resolved {
                    value: default,
                    source: Source::Detected,
                }
            }
        };
    }
    Resolved {
        value: default,
        source: Source::Detected,
    }
}

/// The git rows for both keys, read once per process: one `git config` call.
/// `None` when git cannot report config scopes.
pub fn git_rows() -> Option<&'static [Row]> {
    static ROWS: OnceLock<Option<Vec<Row>>> = OnceLock::new();
    ROWS.get_or_init(|| {
        let out = std::process::Command::new("git")
            .args(["config", "--show-scope", "--get-regexp", r"^greengate\."])
            .output()
            .ok()?;
        if !out.status.success() && out.stdout.is_empty() {
            // `git config --get-regexp` exits 1 when nothing matches: no rows, not a failure.
            return if out.status.code() == Some(1) {
                Some(Vec::new())
            } else {
                None
            };
        }
        let text = String::from_utf8_lossy(&out.stdout);
        Some(
            text.lines()
                .filter_map(|line| {
                    let (scope, rest) = line.split_once('\t')?;
                    let (k, v) = rest.split_once(' ').unwrap_or((rest, ""));
                    Some((scope.to_string(), k.to_ascii_lowercase(), v.to_string()))
                })
                .collect(),
        )
    })
    .as_deref()
}

/// Both keys, resolved from this process's environment and git.
pub fn from_host(detected_cores: u32) -> HostSizing {
    let rows = git_rows();
    let mut sizing = resolve(
        |name| std::env::var(name).ok(),
        rows.unwrap_or(&[]),
        detected_cores,
    );
    if rows.is_none() {
        sizing
            .warnings
            .push("git cannot report config scopes; the defaults are used".to_string());
    }
    sizing
}

/// The physical core count from a `/proc/cpuinfo` text: the distinct
/// (physical id, core id) pairs. `None` when the text carries neither field
/// (aarch64 does not), so the caller falls back to the logical count.
pub fn physical_from_cpuinfo(text: &str) -> Option<u32> {
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
pub fn physical_from_core_lists<'a>(lists: impl IntoIterator<Item = &'a str>) -> Option<u32> {
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

    #[test]
    fn detected_when_nothing_is_set() {
        let s = resolve(no_env, &[], 6);
        assert_eq!(
            s.cores,
            Resolved {
                value: 6,
                source: Source::Detected
            }
        );
        assert_eq!(
            s.slots,
            Resolved {
                value: -1,
                source: Source::Detected
            }
        );
        assert!(s.warnings.is_empty());
    }

    #[test]
    fn env_beats_git() {
        let env = |n: &str| (n == CORES_ENV).then(|| "4".to_string());
        let rows = [row("global", CORES_KEY, "8")];
        let s = resolve(env, &rows, 6);
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
        let s = resolve(env, &rows, 6);
        assert_eq!(
            s.cores,
            Resolved {
                value: 3,
                source: Source::Global
            }
        );
        assert!(s
            .warnings
            .iter()
            .any(|w| w.contains("GREENGATE_HOST_CORES")));
    }

    #[test]
    fn last_global_row_wins_and_system_is_used_when_global_is_absent() {
        let rows = [row("system", SLOTS_KEY, "2")];
        let s = resolve(no_env, &rows, 6);
        assert_eq!(
            s.slots,
            Resolved {
                value: 2,
                source: Source::System
            }
        );
        let rows = [row("global", SLOTS_KEY, "1"), row("global", SLOTS_KEY, "5")];
        assert_eq!(resolve(no_env, &rows, 6).slots.value, 5);
    }

    #[test]
    fn out_of_range_cores_warns_and_uses_detected() {
        let rows = [row("global", CORES_KEY, "0")];
        let s = resolve(no_env, &rows, 6);
        assert_eq!(
            s.cores,
            Resolved {
                value: 6,
                source: Source::Detected
            }
        );
        assert!(s.warnings.iter().any(|w| w.contains("outside")));
    }

    #[test]
    fn slots_65_warns_and_uses_auto() {
        let rows = [row("global", SLOTS_KEY, "65")];
        let s = resolve(no_env, &rows, 6);
        assert_eq!(
            s.slots,
            Resolved {
                value: -1,
                source: Source::Detected
            }
        );
        assert_eq!(s.warnings.len(), 1);
    }

    #[test]
    fn repo_only_key_is_warned_and_ignored() {
        let rows = [row("local", CORES_KEY, "4")];
        let s = resolve(no_env, &rows, 6);
        assert_eq!(
            s.cores,
            Resolved {
                value: 6,
                source: Source::Detected
            }
        );
        assert!(s.warnings.iter().any(|w| w.contains("repository")));
    }

    #[test]
    fn non_numeric_git_value_warns_and_uses_detected() {
        let rows = [row("global", CORES_KEY, "abc")];
        let s = resolve(no_env, &rows, 6);
        assert_eq!(s.cores.value, 6);
        assert_eq!(s.warnings.len(), 1);
    }

    /// amont 4.0 reads only the new names: an old git key and an old
    /// environment variable are both ignored.
    #[test]
    fn the_old_names_are_ignored() {
        let env = |n: &str| (n == "AMONT_HOST_CORES").then(|| "2".to_string());
        let rows = [row("global", "amont.hostCores", "3")];
        let s = resolve(env, &rows, 6);
        assert_eq!(
            s.cores,
            Resolved {
                value: 6,
                source: Source::Detected
            }
        );
        assert!(s.warnings.is_empty());
    }

    #[test]
    fn cpuinfo_counts_distinct_physical_core_pairs() {
        // Two hyper-threads share each core id: 2 physical packages x 3 cores = 6.
        let mut text = String::new();
        for (cpu, (phys, core)) in [
            ("0", "0"),
            ("0", "1"),
            ("0", "2"),
            ("0", "0"),
            ("0", "1"),
            ("0", "2"),
        ]
        .iter()
        .enumerate()
        {
            text.push_str(&format!(
                "processor\t: {cpu}\nphysical id\t: {phys}\ncore id\t\t: {core}\n\n"
            ));
        }
        assert_eq!(physical_from_cpuinfo(&text), Some(3));
        // Two packages, the same core ids: the physical id tells them apart.
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
