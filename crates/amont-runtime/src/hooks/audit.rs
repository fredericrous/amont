//! Dependency-vulnerability audits, with the severity the push deserves.
//!
//! The same policy the release workflow enforces in CI, brought to the
//! machine where the push starts: an advisory against the dependency tree
//! is INFORMATION on a branch push — named, never blocking, retried for
//! free tomorrow — and a REFUSAL on a push that carries a `v*` tag, because
//! a tag is a release leaving the building and immutable registries do not
//! take anything back. The hook advises early; CI (for repositories that
//! have it) enforces finally.
//!
//! One check per ecosystem amont already speaks — `cargo audit` for Rust,
//! `npm audit` / `pnpm audit` for JS, `pip-audit` for Python — each opted
//! in by the lockfile its tool actually audits. No lockfile, no check: an audit
//! without a resolved tree audits a guess.
//!
//! Three verdicts, learned the hard way in ci.yaml's advisory job and kept
//! here: the tools' OUTPUT decides, not the exit code alone, because every
//! one of them conflates "found vulnerabilities" with "could not fetch the
//! advisory database" in its exit status. And "could not check" is spoken
//! loudly but never blocks — [`crate::check::Outcome::Unavailable`]'s
//! contract: a hook may be offline, and a push gate that fails on a captive
//! portal teaches `--no-verify`. The release workflow, which is never
//! offline, is where an unchecked tree refuses to ship.

use crate::check::Outcome;
use crate::pushrefs::PushRef;

use super::common;

/// What an audit's output said, before the push's stakes are applied.
#[derive(Debug, PartialEq, Eq)]
enum Report {
    Clean,
    /// Warning-class advisories (unmaintained/unsound) — named, never
    /// blocking anywhere: a gate nothing can pass is a gate people delete.
    Advisories(Vec<String>),
    /// Real vulnerabilities. Blocking iff the push carries a `v*` tag.
    Vulnerabilities(Vec<String>),
    /// The tool ran but could not answer (no network, no database).
    CouldNotCheck,
}

/// Does this push carry a release? `v` + digit, so `v1.6.6` and `v2` gate
/// while a tag that merely starts with a letter v (`vendor-drop`) does not.
/// Deletes push no code and carry nothing.
fn releasing(refs: &[PushRef]) -> bool {
    refs.iter().any(|r| {
        r.remote_ref
            .strip_prefix("refs/tags/")
            .and_then(|t| t.strip_prefix('v'))
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
    })
}

/// Apply the push's stakes to the tool's report. `full` is the captured
/// output, reprinted only when the verdict blocks — that is the moment the
/// reader needs the table, and the only moment worth the scrollback.
fn conclude(
    settings: &crate::config::Settings,
    tool: &str,
    report: Report,
    releasing: bool,
    full: &str,
) -> Outcome {
    conclude_waivable(settings, tool, report, releasing, full, true)
}

/// [`conclude`], where `waivable` is false when something in the report
/// could not be attributed (a project the audit could not answer for, a
/// finding without a recognised id): a waiver must never vouch for what
/// nobody saw.
fn conclude_waivable(
    settings: &crate::config::Settings,
    tool: &str,
    report: Report,
    releasing: bool,
    full: &str,
    waivable: bool,
) -> Outcome {
    match report {
        Report::Clean => {
            common::ok(settings, &format!("{tool}: no known vulnerabilities"));
            Outcome::Passed
        }
        Report::Advisories(ids) => {
            common::warn(&format!(
                "{tool}: advisories against the dependency tree (warnings — unmaintained/unsound): {}",
                ids.join(", ")
            ));
            Outcome::Warned
        }
        Report::Vulnerabilities(what) => {
            let waivers = Waivers::load(&common::repo_root(), today());
            // The ids the verdict names first; the tool's report when the
            // verdict is a summary line (npm, pnpm).
            let mut ids = advisory_ids(&what.join(" "));
            if ids.is_empty() {
                ids = advisory_ids(full);
            }
            let unwaived: Vec<&String> = ids.iter().filter(|id| !waivers.covers(id)).collect();
            let all_waived = waivable && !ids.is_empty() && unwaived.is_empty();
            if releasing {
                if all_waived {
                    common::warn(&format!(
                        "{tool}: known vulnerabilities shipped under a reviewed waiver \
                         ({WAIVERS_FILE}) — not blocking this release: {}",
                        ids.iter()
                            .map(|id| waivers.describe(id))
                            .collect::<Vec<_>>()
                            .join("; ")
                    ));
                    return Outcome::Warned;
                }
                for line in full.lines() {
                    crate::say!("{line}");
                }
                for note in waivers.problems() {
                    common::warn(&format!("{tool}: {note}"));
                }
                common::fail(&format!(
                    "{tool}: known vulnerabilities in the dependency tree — a v* tag \
                     does not ship with these: {}{}",
                    what.join(", "),
                    if ids.is_empty() {
                        " (no advisory id to match a waiver against)".to_string()
                    } else {
                        format!(
                            " — not waived: {}",
                            unwaived
                                .iter()
                                .map(|s| s.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    }
                ));
                Outcome::Failed
            } else if all_waived {
                common::warn(&format!(
                    "{tool}: known vulnerabilities ({}) — covered by a reviewed waiver \
                     ({WAIVERS_FILE}), so a v* tag push will pass while it holds: {}",
                    what.join(", "),
                    ids.iter()
                        .map(|id| waivers.describe(id))
                        .collect::<Vec<_>>()
                        .join("; ")
                ));
                Outcome::Warned
            } else {
                common::warn(&format!(
                    "{tool}: known vulnerabilities in the dependency tree ({}) — \
                     this will BLOCK a v* tag push",
                    what.join(", ")
                ));
                Outcome::Warned
            }
        }
        Report::CouldNotCheck => {
            common::warn(&format!(
                "{tool} could not complete — the dependency tree was NOT checked. \
                 This is not a clean result."
            ));
            Outcome::Unavailable
        }
    }
}

/// Where a repository records the advisories a release may ship anyway.
const WAIVERS_FILE: &str = ".amont-audit-waivers";

/// How far ahead a waiver may run. A waiver is a decision to revisit, not
/// an exemption: one dated further out is void, so nothing is waived for
/// good by accident.
const MAX_WAIVER_DAYS: i64 = 90;

/// One line of the waiver file: `<advisory id> <expires YYYY-MM-DD> <reason>`.
#[derive(Debug)]
struct Waiver {
    id: String,
    expires: String,
    reason: String,
    valid: bool,
}

/// The repository's reviewed waivers, judged against today.
///
/// A waiver lets a RELEASE ship a known vulnerability nobody can fix yet —
/// an advisory with no patched version, on a path the project cannot
/// replace. It is committed (so it is reviewed like code), names its
/// reason, and expires: past its date, or dated more than
/// [`MAX_WAIVER_DAYS`] ahead, it waives nothing and the tag is refused
/// again. Branch pushes never consult it — they never block.
#[derive(Debug, Default)]
struct Waivers {
    entries: Vec<Waiver>,
    problems: Vec<String>,
}

impl Waivers {
    fn load(root: &str, today: i64) -> Self {
        match std::fs::read_to_string(std::path::Path::new(root).join(WAIVERS_FILE)) {
            Ok(text) => Self::parse(&text, today),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => Self {
                entries: Vec::new(),
                problems: vec![format!(
                    "{WAIVERS_FILE} could not be read ({e}) — it waives nothing"
                )],
            },
        }
    }

    fn parse(text: &str, today: i64) -> Self {
        let mut w = Self::default();
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.splitn(3, char::is_whitespace);
            let id = parts.next().unwrap_or_default().to_string();
            let expires = parts.next().unwrap_or_default().trim().to_string();
            let reason = parts.next().unwrap_or_default().trim().to_string();
            let valid = match days_from_date(&expires) {
                None => {
                    w.problems.push(format!(
                        "waiver for {id} has no readable expiry date (YYYY-MM-DD) — it waives nothing"
                    ));
                    false
                }
                Some(_) if reason.is_empty() => {
                    w.problems.push(format!(
                        "waiver for {id} gives no reason — it waives nothing"
                    ));
                    false
                }
                Some(d) if d < today => {
                    w.problems
                        .push(format!("waiver for {id} expired on {expires}"));
                    false
                }
                Some(d) if d - today > MAX_WAIVER_DAYS => {
                    w.problems.push(format!(
                        "waiver for {id} runs to {expires}, more than {MAX_WAIVER_DAYS} days \
                         ahead — it waives nothing; date it sooner and revisit"
                    ));
                    false
                }
                Some(_) => true,
            };
            w.entries.push(Waiver {
                id,
                expires,
                reason,
                valid,
            });
        }
        w
    }

    fn covers(&self, id: &str) -> bool {
        self.entries.iter().any(|w| w.valid && w.id == id)
    }

    fn describe(&self, id: &str) -> String {
        match self.entries.iter().find(|w| w.valid && w.id == id) {
            Some(w) => format!("{id} until {} ({})", w.expires, w.reason),
            None => id.to_string(),
        }
    }

    fn problems(&self) -> &[String] {
        &self.problems
    }
}

/// Days since 1970-01-01 for a `YYYY-MM-DD` date (proleptic Gregorian).
fn days_from_date(date: &str) -> Option<i64> {
    let mut it = date.split('-');
    let (y, m, d): (i64, i64, i64) = (
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
    );
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if it.next().is_some() || !(1..=12).contains(&m) || d < 1 || d > month_days[(m - 1) as usize] {
        return None;
    }
    // Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// Today, in days since the epoch (UTC).
fn today() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_secs() / 86_400) as i64)
        .unwrap_or(0)
}

/// Every advisory id in `text`: GitHub (`GHSA-xxxx-xxxx-xxxx`), RustSec,
/// Go, PyPA and CVE ids, wherever they sit (a URL, a table cell).
fn advisory_ids(text: &str) -> Vec<String> {
    let mut ids: Vec<String> = text
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .filter(|w| {
            let parts: Vec<&str> = w.split('-').collect();
            match parts.as_slice() {
                ["GHSA", a, b, c] => [a, b, c]
                    .iter()
                    .all(|p| p.len() == 4 && p.bytes().all(|b| b.is_ascii_alphanumeric())),
                ["RUSTSEC" | "GO" | "PYSEC" | "CVE" | "OSV", year, num] => {
                    year.len() == 4
                        && year.bytes().all(|b| b.is_ascii_digit())
                        && !num.is_empty()
                        && num.bytes().all(|b| b.is_ascii_digit())
                }
                _ => false,
            }
        })
        .map(str::to_string)
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// Is this word a RUSTSEC id? `RUSTSEC-` + 4 digits + `-` + 4 digits.
fn is_advisory_id(w: &str) -> bool {
    w.len() == 17
        && w.starts_with("RUSTSEC-")
        && w[8..12].bytes().all(|b| b.is_ascii_digit())
        && w.as_bytes()[12] == b'-'
        && w[13..17].bytes().all(|b| b.is_ascii_digit())
}

/// Pair each advisory with the crate it was raised against.
///
/// `cargo audit`'s terminal report is blocks — `Crate:` … `ID:` — and the
/// id alone does not say which crate carries it, let alone which of YOUR
/// crates depends on that. Answering "is this on the commit path or only in
/// an opt-in tool?" meant running `cargo tree -i` by hand every time.
///
/// A `Crate:` binds to the next `ID:` and is then spent, so an id appearing
/// outside a block — a summary line, a URL — reports no crate rather than
/// inheriting the previous block's.
fn ids_with_crates(out: &str) -> Vec<(String, Option<String>)> {
    let mut pairs: Vec<(String, Option<String>)> = Vec::new();
    let mut pending: Option<String> = None;
    for line in out.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("Crate:") {
            pending = rest.split_whitespace().next().map(str::to_string);
            continue;
        }
        if let Some(rest) = t.strip_prefix("ID:") {
            if let Some(id) = rest.split_whitespace().next().filter(|w| is_advisory_id(w)) {
                pairs.push((id.to_string(), pending.take()));
                continue;
            }
        }
        // Ids outside a block still count — the older summary-line shapes
        // and anything cargo audit prints loose.
        for w in t.split_whitespace().filter(|w| is_advisory_id(w)) {
            pairs.push((w.to_string(), None));
        }
    }
    pairs
}

/// Which of THIS workspace's crates reach `crate`, read from `cargo tree -i`.
///
/// Cargo prints a local package with its path in parentheses and a registry
/// package without one, which is the only discriminator needed and works in
/// any repository — the hook cannot know a given workspace's member names.
///
/// An empty result is an answer, not a failure: `cargo tree` prints
/// "nothing to print" for a crate that is in `Cargo.lock` but not in the
/// build graph. `cargo audit` reads the lock file, so an advisory can name a
/// crate nothing compiles — an optional dependency of a feature nobody
/// enabled. Saying so is more useful than naming no crate at all.
fn local_dependents(tree_out: &str) -> Vec<String> {
    let mut names: Vec<String> = tree_out
        .lines()
        .filter(|l| l.contains(" (/"))
        .filter_map(|l| {
            l.split_whitespace()
                .find(|w| !w.is_empty() && w.chars().next().is_some_and(|c| c.is_alphanumeric()))
                .map(str::to_string)
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

/// One advisory, said in full: the id, the crate it is against, and which of
/// this workspace's crates actually reach it.
fn describe(id: &str, krate: Option<&str>, reaches: &[String]) -> String {
    match (krate, reaches.is_empty()) {
        (None, _) => id.to_string(),
        (Some(k), true) => format!("{id} ({k}, not in the build graph)"),
        (Some(k), false) => format!("{id} ({k} → {})", reaches.join(", ")),
    }
}

/// `cargo audit`, ci.yaml's rules verbatim: the RUSTSEC ids decide, the
/// exit code only says which class they are.
fn read_cargo_audit(exit_ok: bool, out: &str) -> Report {
    let mut ids: Vec<String> = out
        .split_whitespace()
        .filter(|w| {
            w.len() == 17
                && w.starts_with("RUSTSEC-")
                && w[8..12].bytes().all(|b| b.is_ascii_digit())
                && w.as_bytes()[12] == b'-'
                && w[13..17].bytes().all(|b| b.is_ascii_digit())
        })
        .map(|w| w.to_string())
        .collect();
    ids.sort();
    ids.dedup();
    match (ids.is_empty(), exit_ok) {
        (true, true) => Report::Clean,
        (true, false) => Report::CouldNotCheck,
        (false, true) => Report::Advisories(ids),
        (false, false) => Report::Vulnerabilities(ids),
    }
}

/// `npm audit`: the summary line decides. A clean tree is `found 0
/// vulnerabilities` on every npm. A finding is `found N vulnerabilities`
/// on npm 6 and `N vulnerabilities (a moderate, b high)` — the verb dropped,
/// `1 vulnerability` in the singular — on npm 7 and later, which is every
/// npm shipped since 2020. No recognisable summary plus a refusal to exit
/// clean is a tool that never answered.
fn read_npm_audit(exit_ok: bool, out: &str) -> Report {
    let summary = out.lines().rev().map(str::trim).find(|l| {
        l.contains("vulnerabilit")
            && (l.starts_with("found ")
                || l.split_whitespace()
                    .next()
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())))
    });
    match summary {
        Some(l) if l.starts_with("found 0 ") || l.starts_with("0 ") => Report::Clean,
        Some(l) => Report::Vulnerabilities(vec![l.to_string()]),
        None if exit_ok => Report::Clean,
        None => Report::CouldNotCheck,
    }
}

/// `pnpm audit`: the summary line decides. A clean tree is `No known
/// vulnerabilities found`; a finding is `N vulnerabilities found` followed by
/// `Severity: a low | b moderate | …`, which is carried along so the warning
/// says how bad. No recognisable summary plus a refusal to exit clean is a
/// tool that never answered (`ERR_PNPM_AUDIT_BAD_RESPONSE`, no network).
fn read_pnpm_audit(exit_ok: bool, out: &str) -> Report {
    let lines: Vec<&str> = out.lines().map(str::trim).collect();
    if lines
        .iter()
        .any(|l| l.starts_with("No known vulnerabilities found"))
    {
        return Report::Clean;
    }
    let found = lines.iter().enumerate().rev().find(|(_, l)| {
        let mut w = l.split_whitespace();
        w.next()
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            && w.next().is_some_and(|v| v.starts_with("vulnerabilit"))
            && w.next() == Some("found")
    });
    match found {
        Some((_, l)) if l.starts_with("0 ") => Report::Clean,
        Some((i, l)) => {
            let severity = lines
                .get(i + 1)
                .and_then(|n| n.strip_prefix("Severity:"))
                .map(str::trim);
            Report::Vulnerabilities(vec![match severity {
                Some(sev) => format!("{l} ({sev})"),
                None => l.to_string(),
            }])
        }
        None if exit_ok => Report::Clean,
        None => Report::CouldNotCheck,
    }
}

/// `govulncheck`: the GO- ids decide, the exit code classifies them — the
/// same split cargo-audit taught. The tool exits non-zero only when the
/// analysed CODE is affected; ids with a clean exit are the informational
/// section (vulnerable modules whose functions are never called), which is
/// advisory-grade. No ids plus a refusal to exit clean is a tool that never
/// answered (no network, no vulnerability database).
fn read_govulncheck(exit_ok: bool, out: &str) -> Report {
    let mut ids: Vec<String> = out
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-'))
        .filter(|w| {
            w.len() >= 12
                && w.starts_with("GO-")
                && w[3..7].bytes().all(|b| b.is_ascii_digit())
                && w.as_bytes()[7] == b'-'
                && w[8..].bytes().all(|b| b.is_ascii_digit())
        })
        .map(|w| w.to_string())
        .collect();
    ids.sort();
    ids.dedup();
    match (ids.is_empty(), exit_ok) {
        (true, true) => Report::Clean,
        (true, false) => Report::CouldNotCheck,
        (false, true) => Report::Advisories(ids),
        (false, false) => Report::Vulnerabilities(ids),
    }
}

/// `pip-audit`: its own closing sentence decides.
fn read_pip_audit(exit_ok: bool, out: &str) -> Report {
    if out.contains("No known vulnerabilities found") {
        return Report::Clean;
    }
    if let Some(line) = out
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("Found ") && l.contains("known vulnerabilit"))
    {
        return Report::Vulnerabilities(vec![line.to_string()]);
    }
    if exit_ok {
        Report::Clean
    } else {
        Report::CouldNotCheck
    }
}

/// The `site-packages` of the environment this project actually uses, if
/// one is on disk.
///
/// `$VIRTUAL_ENV` first — an activated environment is the one whose imports
/// are live — then the `.venv` uv and PEP 668 tooling create by convention.
/// Layout differs by platform: `lib/python3.13/site-packages` everywhere
/// except Windows, which uses `Lib/site-packages`, so the python-version
/// directory is discovered rather than guessed.
fn venv_site_packages(root: &str) -> Option<String> {
    let candidates = std::env::var_os("VIRTUAL_ENV")
        .map(std::path::PathBuf::from)
        .into_iter()
        .chain(std::iter::once(std::path::Path::new(root).join(".venv")));
    for venv in candidates {
        let windows = venv.join("Lib").join("site-packages");
        if windows.is_dir() {
            return Some(windows.to_string_lossy().into_owned());
        }
        let Ok(entries) = std::fs::read_dir(venv.join("lib")) else {
            continue;
        };
        for e in entries.flatten() {
            if !e.file_name().to_string_lossy().starts_with("python") {
                continue;
            }
            let sp = e.path().join("site-packages");
            if sp.is_dir() {
                return Some(sp.to_string_lossy().into_owned());
            }
        }
    }
    None
}

/// Run one audit tool from the repo root and read its answer.
fn audited(settings: &crate::config::Settings, argv: &[String]) -> Option<(bool, String)> {
    audited_in(settings, argv, std::path::Path::new(&common::repo_root()))
}

/// Run one audit tool from `dir` and read its answer.
fn audited_in(
    settings: &crate::config::Settings,
    argv: &[String],
    dir: &std::path::Path,
) -> Option<(bool, String)> {
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(dir)
        .stdin(std::process::Stdio::null());
    common::strip_git_env(&mut cmd);
    let (ran, out) = common::capture_within(settings, &mut cmd)?;
    match ran {
        common::Ran::Status(s) => Some((s.success(), out)),
        common::Ran::TimedOut(budget) => {
            common::say_timed_out(&argv[0], budget);
            None
        }
    }
}

pub fn rust(settings: &crate::config::Settings, refs: &[PushRef]) -> Outcome {
    if !has_lockfile("Cargo.lock") {
        return Outcome::Inert;
    }
    if common::which("cargo-audit").is_none() {
        common::warn(
            "audit-rust: cargo-audit is not installed (cargo install cargo-audit) — \
             the audit did NOT run",
        );
        return Outcome::Unavailable;
    }
    let argv = vec![
        common::program("cargo"),
        "audit".into(),
        "--color".into(),
        "never".into(),
    ];
    let Some((exit_ok, out)) = audited(settings, &argv) else {
        return Outcome::Unavailable;
    };
    let release = releasing(refs);
    let mut report = read_cargo_audit(exit_ok, &out);
    if release {
        if let Report::Vulnerabilities(ids) = report {
            let (shipped, dev_only) =
                split_shipped_crates(ids, &out, |krate| cargo_tree_shipped(settings, krate));
            warn_dev_only("audit-rust", &dev_only);
            report = if shipped.is_empty() {
                Report::Clean
            } else {
                Report::Vulnerabilities(shipped)
            };
        }
    }
    conclude(
        settings,
        "audit-rust",
        attribute(report, &out, |krate| cargo_tree_inverse(settings, krate)),
        release,
        &out,
    )
}

/// `cargo tree -i <crate>` over the edges a RELEASE ships: normal and build
/// dependencies (a dependency's build script runs on every machine that
/// compiles it), for every target and every feature, so nothing a consumer
/// could switch on is mistaken for dev-only. None if it could not run.
fn cargo_tree_shipped(settings: &crate::config::Settings, spec: &str) -> Option<String> {
    let argv = vec![
        common::program("cargo"),
        "tree".into(),
        "--invert".into(),
        spec.into(),
        "--edges".into(),
        "normal,build".into(),
        "--target".into(),
        "all".into(),
        "--all-features".into(),
        "--color".into(),
        "never".into(),
    ];
    // Unlike the message-only inverse tree, a failure here must not read as
    // "nothing reaches it": an error prints no local crate either.
    audited(settings, &argv).and_then(|(ok, out)| ok.then_some(out))
}

/// The version cargo audit reports for `krate` (the `Version:` line of its
/// block), so the tree is asked about exactly that package — a bare name is
/// ambiguous when two versions are locked, and cargo refuses it.
fn crate_version(out: &str, krate: &str) -> Option<String> {
    let mut in_block = false;
    for line in out.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("Crate:") {
            in_block = rest.split_whitespace().next() == Some(krate);
        } else if in_block {
            if let Some(rest) = line.strip_prefix("Version:") {
                return rest.split_whitespace().next().map(str::to_string);
            }
        }
    }
    None
}

/// Does a shipped-edges inverse tree say the crate is dev-only? Only on
/// positive evidence: no local crate reaches it AND cargo said there is
/// nothing to print. Anything else — an unreadable tree — ships.
fn tree_says_dev_only(tree: &str) -> bool {
    local_dependents(tree).is_empty() && tree.contains("nothing to print")
}

/// Split a release's advisory ids into those that ship and those only the
/// dev tree carries. An id whose crate is unknown, or whose tree could not
/// be read, ships: an unknown is not a pass.
fn split_shipped_crates(
    ids: Vec<String>,
    out: &str,
    shipped_tree: impl Fn(&str) -> Option<String>,
) -> (Vec<String>, Vec<String>) {
    let pairs = ids_with_crates(out);
    let mut verdicts: Vec<(String, bool)> = Vec::new();
    let (mut shipped, mut dev_only) = (Vec::new(), Vec::new());
    for id in ids {
        let krate = pairs
            .iter()
            .find(|(pid, k)| *pid == id && k.is_some())
            .and_then(|(_, k)| k.clone());
        let ships = match krate {
            None => true,
            Some(k) => match verdicts.iter().find(|(name, _)| *name == k) {
                Some((_, v)) => *v,
                None => {
                    let spec = match crate_version(out, &k) {
                        Some(v) => format!("{k}@{v}"),
                        None => k.clone(),
                    };
                    let v = shipped_tree(&spec).map_or(true, |t| !tree_says_dev_only(&t));
                    verdicts.push((k.clone(), v));
                    v
                }
            },
        };
        if ships {
            shipped.push(id);
        } else {
            dev_only.push(id);
        }
    }
    (shipped, dev_only)
}

/// Name what a release does not ship and therefore does not refuse.
fn warn_dev_only(tool: &str, dev_only: &[String]) {
    if !dev_only.is_empty() {
        common::warn(&format!(
            "{tool}: only in development dependencies, which the release does not ship — not blocking: {}",
            dev_only.join("; ")
        ));
    }
}

/// `cargo tree -i <crate>`, or None if it could not be run. Failure here is
/// never fatal: attribution is an improvement to a message, and an advisory
/// reported without it is still an advisory reported.
fn cargo_tree_inverse(settings: &crate::config::Settings, krate: &str) -> Option<String> {
    let argv = vec![
        common::program("cargo"),
        "tree".into(),
        "--invert".into(),
        krate.into(),
        "--edges".into(),
        "normal".into(),
        "--color".into(),
        "never".into(),
    ];
    audited(settings, &argv).map(|(_, out)| out)
}

/// Name the crate behind each advisory, and which of this workspace's crates
/// reach it.
///
/// Deliberately spawns NOTHING on a clean report, which is every run that
/// matters: the `cargo tree` calls happen once per distinct affected crate,
/// only when there is already something to say. A hook on the push path does
/// not pay for a message nobody will read.
fn attribute(report: Report, out: &str, tree: impl Fn(&str) -> Option<String>) -> Report {
    match report {
        Report::Advisories(ids) => Report::Advisories(described(ids, out, tree)),
        Report::Vulnerabilities(ids) => Report::Vulnerabilities(described(ids, out, tree)),
        // Clean and CouldNotCheck carry no ids, so there is nothing to
        // attribute and — the part that matters — nothing to spawn.
        other => other,
    }
}

/// Each id, rewritten with its crate and what reaches it where both are
/// known. One `cargo tree` per distinct crate, not per advisory: `lru`
/// carried two advisories in the run that prompted this.
fn described(ids: Vec<String>, out: &str, tree: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let pairs = ids_with_crates(out);
    let mut seen: Vec<(String, Vec<String>)> = Vec::new();
    ids.iter()
        .map(|id| {
            let krate = pairs
                .iter()
                .find(|(pid, k)| pid == id && k.is_some())
                .and_then(|(_, k)| k.clone());
            let Some(k) = krate else {
                return id.clone();
            };
            if let Some((_, reaches)) = seen.iter().find(|(name, _)| *name == k) {
                return describe(id, Some(&k), reaches);
            }
            let reaches = tree(&k).map(|t| local_dependents(&t)).unwrap_or_default();
            seen.push((k.clone(), reaches.clone()));
            describe(id, Some(&k), &reaches)
        })
        .collect()
}

/// Does the repository carry `lockfile` anywhere in its index? An audit
/// without a resolved tree audits a guess — and one asked about a
/// repository in another language has nothing to audit at all. That is
/// `Inert`, not "could not run": the dispatcher asks the same question
/// from the registry's scope, and this is the answer for a check invoked
/// by name.
fn has_lockfile(lockfile: &str) -> bool {
    crate::tracked_paths()
        .iter()
        .any(|p| p.rsplit('/').next().unwrap_or(p) == lockfile)
}

/// The directories holding a tracked `lockfile`, repo-relative ("" for the
/// root), sorted. npm resolves a project from its own directory, so a
/// repository whose packages live in subdirectories (`web/`, `mcp/`) has no
/// lockfile at the root at all.
fn lockfile_dirs(lockfile: &str) -> Vec<String> {
    let mut dirs: Vec<String> = crate::tracked_paths()
        .iter()
        .filter(|p| p.rsplit('/').next().unwrap_or(p) == lockfile)
        .map(|p| {
            p.rsplit_once('/')
                .map(|(d, _)| d.to_string())
                .unwrap_or_default()
        })
        .collect();
    dirs.sort();
    dirs.dedup();
    dirs
}

/// One JS package manager `audit-js` knows how to ask: the lockfile that
/// opts a directory in, the tool that reads it, and how to read its answer.
struct JsAuditor {
    lockfile: &'static str,
    tool: &'static str,
    read: fn(bool, &str) -> Report,
    /// Extra arguments for a project in `dir`.
    args: fn(&str) -> Vec<String>,
    /// The flag that limits the audit to what the project ships — its
    /// production dependencies, without the dev tree.
    prod_only: &'static str,
}

/// pnpm answers for the WORKSPACE it finds above a directory, not for the
/// directory: a standalone project with its own pnpm-lock.yaml inside a
/// workspace (a spike, an example) was reported with the workspace root's
/// findings, all of them, under its own name. `--ignore-workspace` makes it
/// read the lockfile it stands next to — except at a workspace's own root,
/// where the workspace IS the project and every member must be audited.
fn pnpm_args(dir: &str) -> Vec<String> {
    let manifest = if dir.is_empty() {
        "pnpm-workspace.yaml".to_string()
    } else {
        format!("{dir}/pnpm-workspace.yaml")
    };
    if crate::tracked_paths().contains(&manifest) {
        vec![]
    } else {
        vec!["--ignore-workspace".into()]
    }
}

const JS_AUDITORS: [JsAuditor; 2] = [
    JsAuditor {
        lockfile: "package-lock.json",
        tool: "npm",
        read: read_npm_audit,
        args: |_| vec![],
        prod_only: "--omit=dev",
    },
    JsAuditor {
        lockfile: "pnpm-lock.yaml",
        tool: "pnpm",
        read: read_pnpm_audit,
        args: pnpm_args,
        prod_only: "--prod",
    },
];

/// `npm audit` in every directory that tracks a `package-lock.json`, and
/// `pnpm audit` in every directory that tracks a `pnpm-lock.yaml`.
///
/// It used to run once at the repository root, where a repository whose
/// packages live in subdirectories has no lockfile: npm answered ENOLOCK,
/// the check said "could not complete", and a tree with 17 known
/// vulnerabilities (9 high) went unaudited release after release. Each
/// project is audited on its own; any finding counts, labelled with its
/// directory, and a project the tool could not answer for keeps the whole
/// result from reading clean.
///
/// And it only ever knew npm: a pnpm workspace has no `package-lock.json`,
/// so its whole tree — 28 vulnerable versions, two critical, in one
/// repository — was never audited and nothing said so. pnpm audits its own
/// lockfile, from the lockfile alone, the same way.
///
/// On a release, what blocks is what SHIPS: a finding is audited again with
/// the production dependencies only (`--omit=dev` / `--prod`), and one that
/// lives only in the dev tree — a build script's toolchain, a test runner —
/// is named but does not refuse the tag. Nobody who installs the package
/// installs it. A finding whose production re-audit cannot answer still
/// blocks: an unknown is not a pass.
pub fn js(settings: &crate::config::Settings, refs: &[PushRef]) -> Outcome {
    let projects: Vec<(&JsAuditor, String)> = JS_AUDITORS
        .iter()
        .flat_map(|a| lockfile_dirs(a.lockfile).into_iter().map(move |d| (a, d)))
        .collect();
    if projects.is_empty() {
        return Outcome::Inert;
    }
    let root = std::path::PathBuf::from(common::repo_root());
    let release = releasing(refs);
    let mut found = Vec::new();
    let mut dev_only = Vec::new();
    let mut unchecked = Vec::new();
    let mut full = String::new();
    for (auditor, dir) in &projects {
        let label = if dir.is_empty() {
            ".".to_string()
        } else {
            dir.clone()
        };
        let mut argv = vec![common::program(auditor.tool), "audit".into()];
        argv.extend((auditor.args)(dir));
        let Some((exit_ok, out)) = audited_in(settings, &argv, &root.join(dir)) else {
            common::warn(&format!(
                "audit-js: {} could not run in {label} — the audit did NOT run",
                auditor.tool
            ));
            return Outcome::Unavailable;
        };
        match (auditor.read)(exit_ok, &out) {
            Report::Vulnerabilities(what) if release => {
                let mut prod = argv.clone();
                prod.push(auditor.prod_only.into());
                match audited_in(settings, &prod, &root.join(dir))
                    .map(|(ok, pout)| ((auditor.read)(ok, &pout), pout))
                {
                    Some((Report::Clean | Report::Advisories(_), _)) => {
                        dev_only.extend(what.into_iter().map(|w| format!("{label}: {w}")));
                    }
                    Some((Report::Vulnerabilities(shipped), pout)) => {
                        found.extend(shipped.into_iter().map(|w| format!("{label}: {w}")));
                        full.push_str(&format!(
                            "── {} audit {} in {label}\n{pout}\n",
                            auditor.tool, auditor.prod_only
                        ));
                    }
                    Some((Report::CouldNotCheck, _)) | None => {
                        found.extend(what.into_iter().map(|w| format!("{label}: {w}")));
                        full.push_str(&format!("── {} audit in {label}\n{out}\n", auditor.tool));
                    }
                }
            }
            Report::Vulnerabilities(what) => {
                found.extend(what.into_iter().map(|w| format!("{label}: {w}")));
                full.push_str(&format!("── {} audit in {label}\n{out}\n", auditor.tool));
            }
            Report::CouldNotCheck => unchecked.push(format!("{label} ({})", auditor.tool)),
            Report::Clean | Report::Advisories(_) => {}
        }
    }
    if !unchecked.is_empty() {
        common::warn(&format!(
            "audit-js: the audit could not answer in {} — those projects were NOT checked",
            unchecked.join(", ")
        ));
    }
    warn_dev_only("audit-js", &dev_only);
    let waivable = unchecked.is_empty();
    let report = if !found.is_empty() {
        Report::Vulnerabilities(found)
    } else if !unchecked.is_empty() {
        Report::CouldNotCheck
    } else {
        Report::Clean
    };
    conclude_waivable(settings, "audit-js", report, release, &full, waivable)
}

/// Go needs no production filter: modules have no dev-dependency set, and
/// `govulncheck ./...` without `-test` reports only vulnerabilities the
/// module's non-test code can reach — what ships, already.
pub fn go(settings: &crate::config::Settings, refs: &[PushRef]) -> Outcome {
    if !has_lockfile("go.sum") {
        return Outcome::Inert;
    }
    if common::which("govulncheck").is_none() {
        common::warn(
            "audit-go: govulncheck is not installed \
             (go install golang.org/x/vuln/cmd/govulncheck@latest) — the audit did NOT run",
        );
        return Outcome::Unavailable;
    }
    let argv = vec![common::program("govulncheck"), "./...".into()];
    let Some((exit_ok, out)) = audited(settings, &argv) else {
        return Outcome::Unavailable;
    };
    conclude(
        settings,
        "audit-go",
        read_govulncheck(exit_ok, &out),
        releasing(refs),
        &out,
    )
}

pub fn python(settings: &crate::config::Settings, refs: &[PushRef]) -> Outcome {
    if !has_lockfile("requirements.txt") && !has_lockfile("pyproject.toml") {
        return Outcome::Inert;
    }
    if common::which("pip-audit").is_none() {
        common::warn(
            "audit-python: pip-audit is not installed (pip install pip-audit) — \
             the audit did NOT run",
        );
        return Outcome::Unavailable;
    }
    let root = common::repo_root();
    let argv = if std::path::Path::new(&root)
        .join("requirements.txt")
        .exists()
    {
        vec![
            common::program("pip-audit"),
            "-r".into(),
            "requirements.txt".into(),
        ]
    } else if let Some(site_packages) = venv_site_packages(&root) {
        // A uv/PEP-621 project has no requirements.txt, and EXPORTING one
        // does not work either: `uv export` emits the workspace's own
        // members and any private-index dependency, and pip-audit resolves
        // a requirements file in a throwaway venv that can reach neither —
        // it dies on "No matching distribution found". Auditing the
        // INSTALLED tree resolves nothing, and is the truer question
        // anyway: these are the versions actually imported.
        vec![
            common::program("pip-audit"),
            "--path".into(),
            site_packages,
            // Workspace members are installed editable and are not on
            // PyPI; without this each one is a line of noise.
            "--skip-editable".into(),
        ]
    } else {
        common::warn(
            "audit-python: no requirements.txt, and no virtualenv to audit \
             (looked at $VIRTUAL_ENV and .venv) — the audit did NOT run",
        );
        return Outcome::Unavailable;
    };
    let Some((exit_ok, out)) = audited(settings, &argv) else {
        return Outcome::Unavailable;
    };
    let release = releasing(refs);
    let mut report = read_pip_audit(exit_ok, &out);
    // A requirements.txt is the production list by convention (dev pins
    // live beside it, in requirements-dev.txt). A virtualenv holds the dev
    // groups too, so on a release its findings are checked against what
    // `uv export --no-dev` says ships.
    let venv_mode = argv.iter().any(|a| a == "--path");
    let mut waivable = true;
    if release && venv_mode && matches!(report, Report::Vulnerabilities(_)) {
        let rows = pip_vulnerable_rows(&out);
        if let (false, Some(prod)) = (rows.is_empty(), uv_production_names(settings)) {
            let (shipped, dev_only): (Vec<String>, Vec<String>) = rows
                .into_iter()
                .map(|(name, id)| (prod.contains(&name), format!("{name} {id}")))
                .fold((Vec::new(), Vec::new()), |(mut s, mut d), (ships, row)| {
                    if ships {
                        s.push(row);
                    } else {
                        d.push(row);
                    }
                    (s, d)
                });
            warn_dev_only("audit-python", &dev_only);
            waivable = shipped.iter().all(|row| !advisory_ids(row).is_empty());
            report = if shipped.is_empty() {
                Report::Clean
            } else {
                Report::Vulnerabilities(shipped)
            };
        }
    }
    conclude_waivable(settings, "audit-python", report, release, &out, waivable)
}

/// PEP 503 normalised: `Foo_Bar.baz` and `foo-bar-baz` are one package.
fn normalise(name: &str) -> String {
    name.to_ascii_lowercase()
        .split(['-', '_', '.'])
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// The packages pip-audit's table names, one row per advisory:
/// `Name Version ID Fix Versions`. Only rows whose third column is an
/// advisory id count, so the header, the rule and the summary never do.
fn pip_vulnerable_rows(out: &str) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = out
        .lines()
        .filter_map(|l| {
            let cols: Vec<&str> = l.split_whitespace().collect();
            let id = cols.get(2)?;
            ["PYSEC-", "GHSA-", "CVE-", "OSV-"]
                .iter()
                .any(|p| id.starts_with(p))
                .then(|| (normalise(cols[0]), (*id).to_string()))
        })
        .collect();
    rows.sort();
    rows.dedup();
    rows
}

/// What a uv project ships: `uv export --no-dev` of its lock, names only.
/// None when there is no `uv.lock`, no `uv`, or the export fails — the
/// caller then keeps every finding, because an unknown is not a pass.
fn uv_production_names(
    settings: &crate::config::Settings,
) -> Option<std::collections::HashSet<String>> {
    if !has_lockfile("uv.lock") || common::which("uv").is_none() {
        return None;
    }
    let argv = vec![
        common::program("uv"),
        "export".into(),
        "--frozen".into(),
        "--no-dev".into(),
        // Optional extras ship: a consumer who asks for one installs it.
        "--all-extras".into(),
        "--no-hashes".into(),
        "--no-emit-workspace".into(),
        "--format".into(),
        "requirements-txt".into(),
    ];
    let (ok, out) = audited(settings, &argv)?;
    if !ok {
        return None;
    }
    Some(
        out.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('-'))
            .filter_map(|l| {
                l.split(|c: char| "=<>!~;[ ".contains(c))
                    .next()
                    .map(normalise)
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// uv projects have no `requirements.txt`, so the venv is the only thing
    /// left to audit — and before this, `audit-python` looked for nothing
    /// else and reported "the audit did NOT run" forever. Six repositories
    /// in one fleet were in exactly that state, one of them carrying 53
    /// known vulnerabilities nobody had been told about.
    #[test]
    fn a_uv_project_is_audited_through_its_venv() {
        let root = std::env::temp_dir().join(format!("audit-venv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        // Nothing on disk: nothing to audit, and we say so rather than
        // inventing a target.
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(venv_site_packages(root.to_str().unwrap()), None);

        // The posix layout, with the python version DISCOVERED — hard-coding
        // `python3.13` would silently stop finding it after an upgrade.
        // Joined segment by segment, NOT as one "a/b/c" literal: on Windows
        // the literal keeps its forward slashes while the code under test
        // returns backslashes, and the test fails on a difference that is
        // only in the expectation.
        let sp = root
            .join(".venv")
            .join("lib")
            .join("python3.13")
            .join("site-packages");
        std::fs::create_dir_all(&sp).unwrap();
        assert_eq!(
            venv_site_packages(root.to_str().unwrap()),
            Some(sp.to_string_lossy().into_owned())
        );

        // The Windows layout, which has no version directory at all.
        let win = std::env::temp_dir().join(format!("audit-venv-win-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&win);
        let wsp = win.join(".venv").join("Lib").join("site-packages");
        std::fs::create_dir_all(&wsp).unwrap();
        assert_eq!(
            venv_site_packages(win.to_str().unwrap()),
            Some(wsp.to_string_lossy().into_owned())
        );

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&win);
    }

    fn tag(name: &str) -> PushRef {
        PushRef {
            local_ref: name.to_string(),
            local_oid: "a".repeat(40),
            remote_ref: name.to_string(),
            remote_oid: "0".repeat(40),
        }
    }

    /// `v` + digit gates; a branch, a bare-word tag, or a tag merely
    /// starting with the letter v does not.
    #[test]
    fn pip_audit_rows_name_their_packages_normalised() {
        let out = "Name       Version ID                  Fix Versions\n\
                   ---------- ------- ------------------- ------------\n\
                   Pytest_Cov 4.0.0   GHSA-aaaa-bbbb-cccc 4.1.0\n\
                   requests   2.31.0  PYSEC-2023-74       2.31.1\n\
                   requests   2.31.0  CVE-2024-35195      2.32.0\n\
                   Found 3 known vulnerabilities in 2 packages\n";
        let names: Vec<String> = pip_vulnerable_rows(out)
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(names, vec!["pytest-cov", "requests", "requests"]);
        assert_eq!(normalise("Foo_Bar.baz"), "foo-bar-baz");
    }

    #[test]
    fn advisory_ids_are_found_wherever_they_sit() {
        let text = "More info https://github.com/advisories/GHSA-h3mg-xc3c-68pw\n\
                    ID: RUSTSEC-2025-0001, GO-2024-2687 PYSEC-2023-74 CVE-2024-35195 OSV-2024-12 GHSA-bad";
        assert_eq!(
            advisory_ids(text),
            vec![
                "CVE-2024-35195",
                "GHSA-h3mg-xc3c-68pw",
                "GO-2024-2687",
                "OSV-2024-12",
                "PYSEC-2023-74",
                "RUSTSEC-2025-0001"
            ]
        );
    }

    #[test]
    fn a_waiver_is_reasoned_dated_and_bounded() {
        let today = days_from_date("2026-10-05").unwrap();
        let w = Waivers::parse(
            "# id expires reason\n\
             GHSA-aaaa-bbbb-cccc 2026-12-01 no patched version; build-time only\n\
             GHSA-dddd-eeee-ffff 2026-10-01 expired one\n\
             GHSA-gggg-hhhh-iiii 2027-02-01 too far ahead\n\
             GHSA-jjjj-kkkk-llll 2026-11-01\n\
             GHSA-mmmm-nnnn-oooo soon no date\n",
            today,
        );
        assert!(w.covers("GHSA-aaaa-bbbb-cccc"));
        assert!(!w.covers("GHSA-dddd-eeee-ffff"), "expired");
        assert!(!w.covers("GHSA-gggg-hhhh-iiii"), "beyond the horizon");
        assert!(!w.covers("GHSA-jjjj-kkkk-llll"), "no reason");
        assert!(!w.covers("GHSA-mmmm-nnnn-oooo"), "no date");
        assert_eq!(w.problems().len(), 4);
        assert_eq!(days_from_date("1970-01-01"), Some(0));
        assert_eq!(days_from_date("2026-02-30"), None);
        assert!(days_from_date("2028-02-29").is_some());
        assert_eq!(days_from_date("2026-13-01"), None);
    }

    #[test]
    fn a_crate_ships_unless_its_tree_says_otherwise() {
        let out = "Crate: dev\nVersion: 0.2.0\nID: RUSTSEC-2025-0001\n\
                   Crate: app\nID: RUSTSEC-2025-0002\n\
                   Crate: odd\nID: RUSTSEC-2025-0004\nRUSTSEC-2025-0003\n";
        let ids = vec![
            "RUSTSEC-2025-0001".to_string(),
            "RUSTSEC-2025-0002".to_string(),
            "RUSTSEC-2025-0003".to_string(),
            "RUSTSEC-2025-0004".to_string(),
        ];
        let (shipped, dev) = split_shipped_crates(ids, out, |spec| match spec {
            // Asked by name@version when cargo audit gave the version.
            "dev@0.2.0" => Some("warning: nothing to print.\n".into()),
            "app" => Some("app v1.0.0\n└── me v0.1.0 (/repo)\n".into()),
            // A tree that says neither: not proof of dev-only.
            "odd" => Some("odd v1.0.0\n".into()),
            _ => None,
        });
        // 0003 has no crate, 0004's tree is unclear: unknowns ship.
        assert_eq!(
            shipped,
            vec![
                "RUSTSEC-2025-0002",
                "RUSTSEC-2025-0003",
                "RUSTSEC-2025-0004"
            ]
        );
        assert_eq!(dev, vec!["RUSTSEC-2025-0001"]);
    }

    #[test]
    fn a_release_is_a_v_number_tag() {
        assert!(releasing(&[tag("refs/tags/v1.6.6")]));
        assert!(releasing(&[tag("refs/tags/v2")]));
        assert!(!releasing(&[tag("refs/tags/vendor-drop")]));
        assert!(!releasing(&[tag("refs/tags/release")]));
        assert!(!releasing(&[tag("refs/heads/v1-styles")]));
        assert!(!releasing(&[tag("refs/heads/main")]));
        // A mixed push gates: the tag is in there.
        assert!(releasing(&[tag("refs/heads/main"), tag("refs/tags/v1.0")]));
    }

    /// ci.yaml's lesson, pinned at the unit level: the ids decide, the exit
    /// code only classifies them.
    #[test]
    fn cargo_audit_ids_decide_not_the_exit_code() {
        assert_eq!(
            read_cargo_audit(true, "ok, 312 crates checked"),
            Report::Clean
        );
        assert_eq!(
            read_cargo_audit(false, "error: couldn't fetch advisory database"),
            Report::CouldNotCheck
        );
        let warn = "warning: unmaintained RUSTSEC-2024-0436 paste";
        assert_eq!(
            read_cargo_audit(true, warn),
            Report::Advisories(vec!["RUSTSEC-2024-0436".into()])
        );
        let vuln = "Crate: foo\nID: RUSTSEC-2025-0001\nerror: 1 vulnerability found\nRUSTSEC-2025-0001 again";
        assert_eq!(
            read_cargo_audit(false, vuln),
            Report::Vulnerabilities(vec!["RUSTSEC-2025-0001".into()])
        );
        // A lookalike is not an id.
        assert_eq!(read_cargo_audit(true, "RUSTSEC-20XX-0001"), Report::Clean);
    }

    /// The real report shape, trimmed from the run that prompted this: two
    /// advisories against ONE crate, and one against another.
    fn real_report() -> String {
        [
            "Crate:     paste",
            "Version:   1.0.15",
            "Warning:   unmaintained",
            "ID:        RUSTSEC-2024-0436",
            "URL:       https://rustsec.org/advisories/RUSTSEC-2024-0436",
            "",
            "Crate:     lru",
            "Version:   0.12.5",
            "Warning:   unsound",
            "ID:        RUSTSEC-2026-0253",
            "",
            "Crate:     lru",
            "Version:   0.12.5",
            "ID:        RUSTSEC-2026-0002",
            "",
            "warning: 3 allowed warnings found",
        ]
        .join("\n")
    }

    #[test]
    fn an_advisory_is_paired_with_its_crate() {
        let pairs = ids_with_crates(&real_report());
        assert_eq!(
            pairs,
            vec![
                ("RUSTSEC-2024-0436".into(), Some("paste".into())),
                ("RUSTSEC-2026-0253".into(), Some("lru".into())),
                ("RUSTSEC-2026-0002".into(), Some("lru".into())),
            ]
        );
    }

    /// A `Crate:` binds to ONE id. An id printed loose reports no crate
    /// rather than inheriting whichever block happened to precede it.
    #[test]
    fn a_loose_id_borrows_no_crate() {
        let out = "Crate:     paste\nID:        RUSTSEC-2024-0436\nsee also RUSTSEC-2025-0001";
        assert_eq!(
            ids_with_crates(out),
            vec![
                ("RUSTSEC-2024-0436".into(), Some("paste".into())),
                ("RUSTSEC-2025-0001".into(), None),
            ]
        );
    }

    /// Local crates carry a path; registry crates do not. That is the whole
    /// discriminator, and it has to work without knowing the member names.
    #[test]
    fn only_local_crates_are_named_as_reached() {
        let tree =
            "lru v0.12.5\n└── ratatui v0.29.0\n    └── amont-fleet v1.32.0 (/w/crates/amont-fleet)";
        assert_eq!(local_dependents(tree), vec!["amont-fleet".to_string()]);
        assert!(local_dependents("lru v0.12.5\n└── ratatui v0.29.0").is_empty());
    }

    /// End to end on the real report, with the tree calls faked: each id
    /// names its crate and the workspace crate that reaches it, and `lru`'s
    /// two advisories cost ONE lookup.
    #[test]
    fn the_warning_names_the_crate_and_what_reaches_it() {
        let calls = std::cell::RefCell::new(Vec::new());
        let fake = |k: &str| {
            calls.borrow_mut().push(k.to_string());
            Some(format!(
                "{k} v1\n└── ratatui v0.29.0\n    └── amont-fleet v1.32.0 (/w/crates/amont-fleet)"
            ))
        };
        let out = real_report();
        let got = attribute(read_cargo_audit(true, &out), &out, fake);
        assert_eq!(
            got,
            Report::Advisories(vec![
                "RUSTSEC-2024-0436 (paste → amont-fleet)".into(),
                "RUSTSEC-2026-0002 (lru → amont-fleet)".into(),
                "RUSTSEC-2026-0253 (lru → amont-fleet)".into(),
            ])
        );
        assert_eq!(
            calls.into_inner(),
            vec!["paste", "lru"],
            "one call per crate"
        );
    }

    /// A lock-file-only crate — an optional dependency of a feature nobody
    /// enabled — is reported as such. `cargo audit` reads Cargo.lock, so this
    /// is a real and confusing case, and naming it is the point.
    #[test]
    fn a_crate_outside_the_build_graph_says_so() {
        let out = "Crate:     wezterm-input-types\nID:        RUSTSEC-2025-0001";
        let got = attribute(read_cargo_audit(true, out), out, |_| {
            Some("warning: nothing to print.".into())
        });
        assert_eq!(
            got,
            Report::Advisories(vec![
                "RUSTSEC-2025-0001 (wezterm-input-types, not in the build graph)".into()
            ])
        );
    }

    /// Attribution is a better message, never a gate. A clean report spawns
    /// nothing, and a `cargo tree` that cannot run still reports the id.
    #[test]
    fn attribution_never_changes_the_verdict() {
        let spawned = std::cell::Cell::new(false);
        let clean = attribute(read_cargo_audit(true, "0 vulnerabilities"), "", |_| {
            spawned.set(true);
            None
        });
        assert_eq!(clean, Report::Clean);
        assert!(!spawned.get(), "a clean report must spawn nothing");

        let out = "Crate:     paste\nID:        RUSTSEC-2024-0436";
        let blind = attribute(read_cargo_audit(false, out), out, |_| None);
        assert_eq!(
            blind,
            Report::Vulnerabilities(vec![
                "RUSTSEC-2024-0436 (paste, not in the build graph)".into()
            ])
        );
    }

    #[test]
    fn npm_audit_summary_decides() {
        assert_eq!(
            read_npm_audit(true, "found 0 vulnerabilities\n"),
            Report::Clean
        );
        assert_eq!(
            read_npm_audit(false, "found 3 vulnerabilities (1 moderate, 2 high)\n"),
            Report::Vulnerabilities(vec!["found 3 vulnerabilities (1 moderate, 2 high)".into()])
        );
        // npm 7+ dropped the verb: this is what every current npm prints,
        // and what read as "never answered" until the parser learned it.
        assert_eq!(
            read_npm_audit(
                false,
                "# npm audit report\n\nvite  6.0.0 - 6.1.5\nSeverity: high\n\n\
                 17 vulnerabilities (8 moderate, 9 high)\n\nTo address all issues, run:\n  npm audit fix\n"
            ),
            Report::Vulnerabilities(vec!["17 vulnerabilities (8 moderate, 9 high)".into()])
        );
        assert_eq!(
            read_npm_audit(false, "1 vulnerability (1 high)\n"),
            Report::Vulnerabilities(vec!["1 vulnerability (1 high)".into()])
        );
        assert_eq!(
            read_npm_audit(true, "up to date, audited 100 packages\n"),
            Report::Clean
        );
        assert_eq!(
            read_npm_audit(false, "npm ERR! network ENOTFOUND\n"),
            Report::CouldNotCheck
        );
    }

    #[test]
    fn pnpm_audit_summary_decides() {
        assert_eq!(
            read_pnpm_audit(true, "No known vulnerabilities found\n"),
            Report::Clean
        );
        // What pnpm prints after its table: the count, then the severities,
        // which the finding carries so the warning says how bad.
        assert_eq!(
            read_pnpm_audit(
                false,
                "│ More info │ https://github.com/advisories/GHSA-395f │\n\
                 └───────────┴──────────────────────────────────────────┘\n\
                 107 vulnerabilities found\n\
                 Severity: 6 low | 45 moderate | 54 high | 2 critical\n"
            ),
            Report::Vulnerabilities(vec![
                "107 vulnerabilities found (6 low | 45 moderate | 54 high | 2 critical)".into()
            ])
        );
        assert_eq!(
            read_pnpm_audit(false, "1 vulnerabilities found\n"),
            Report::Vulnerabilities(vec!["1 vulnerabilities found".into()])
        );
        assert_eq!(
            read_pnpm_audit(
                false,
                " ERR_PNPM_AUDIT_BAD_RESPONSE  The audit endpoint responded with 503\n"
            ),
            Report::CouldNotCheck
        );
    }

    /// The cargo-audit split, spoken in Go: ids decide, the exit code says
    /// whether the analysed code is actually affected.
    #[test]
    fn govulncheck_ids_decide_not_the_exit_code() {
        assert_eq!(
            read_govulncheck(true, "No vulnerabilities found.\n"),
            Report::Clean
        );
        assert_eq!(
            read_govulncheck(false, "vulncheck: fetching vulnerability database: dial tcp: lookup vuln.go.dev: no such host\n"),
            Report::CouldNotCheck
        );
        // Informational: the module is vulnerable, the analysed code never
        // calls it — exit 0, ids present.
        assert_eq!(
            read_govulncheck(
                true,
                "=== Informational ===\nVulnerability #1: GO-2023-1840\n  More info: https://pkg.go.dev/vuln/GO-2023-1840\n"
            ),
            Report::Advisories(vec!["GO-2023-1840".into()])
        );
        assert_eq!(
            read_govulncheck(
                false,
                "Vulnerability #1: GO-2022-0969\n  Your code calls it.\nGO-2022-0969 again\n"
            ),
            Report::Vulnerabilities(vec!["GO-2022-0969".into()])
        );
        // A lookalike is not an id.
        assert_eq!(
            read_govulncheck(true, "GO-20XX-0001 GO-2023-1"),
            Report::Clean
        );
    }

    #[test]
    fn pip_audit_sentence_decides() {
        assert_eq!(
            read_pip_audit(true, "No known vulnerabilities found\n"),
            Report::Clean
        );
        assert_eq!(
            read_pip_audit(
                false,
                "Found 2 known vulnerabilities in 1 package\nrequests 2.0 PYSEC-2023-74\n"
            ),
            Report::Vulnerabilities(vec!["Found 2 known vulnerabilities in 1 package".into()])
        );
        assert_eq!(
            read_pip_audit(false, "ERROR: could not resolve\n"),
            Report::CouldNotCheck
        );
    }
}
