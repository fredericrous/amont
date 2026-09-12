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
//! `npm audit` for JS, `pip-audit` for Python — each opted in by the
//! lockfile its tool actually audits. No lockfile, no check: an audit
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
fn conclude(tool: &str, report: Report, releasing: bool, full: &str) -> Outcome {
    match report {
        Report::Clean => {
            common::ok(&format!("{tool}: no known vulnerabilities"));
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
            if releasing {
                for line in full.lines() {
                    crate::say!("{line}");
                }
                common::fail(&format!(
                    "{tool}: known vulnerabilities in the dependency tree — a v* tag \
                     does not ship with these: {}",
                    what.join(", ")
                ));
                Outcome::Failed
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

/// `npm audit`: the summary line decides. `found 0 vulnerabilities` is
/// clean; `found N vulnerabilities` (npm appends the severity split) is
/// the finding; no recognisable summary plus a refusal to exit clean is a
/// tool that never answered.
fn read_npm_audit(exit_ok: bool, out: &str) -> Report {
    let summary = out
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with("found ") && l.contains("vulnerabilit"));
    match summary {
        Some(l) if l.starts_with("found 0 ") => Report::Clean,
        Some(l) => Report::Vulnerabilities(vec![l.to_string()]),
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
fn audited(argv: &[String]) -> Option<(bool, String)> {
    let root = common::repo_root();
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(&root)
        .stdin(std::process::Stdio::null());
    common::strip_git_env(&mut cmd);
    let (ran, out) = common::capture_within(&mut cmd)?;
    match ran {
        common::Ran::Status(s) => Some((s.success(), out)),
        common::Ran::TimedOut(budget) => {
            common::say_timed_out(&argv[0], budget);
            None
        }
    }
}

pub fn rust(refs: &[PushRef]) -> Outcome {
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
    let Some((exit_ok, out)) = audited(&argv) else {
        return Outcome::Unavailable;
    };
    conclude(
        "audit-rust",
        attribute(read_cargo_audit(exit_ok, &out), &out, cargo_tree_inverse),
        releasing(refs),
        &out,
    )
}

/// `cargo tree -i <crate>`, or None if it could not be run. Failure here is
/// never fatal: attribution is an improvement to a message, and an advisory
/// reported without it is still an advisory reported.
fn cargo_tree_inverse(krate: &str) -> Option<String> {
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
    audited(&argv).map(|(_, out)| out)
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

pub fn js(refs: &[PushRef]) -> Outcome {
    let argv = vec![common::program("npm"), "audit".into()];
    let Some((exit_ok, out)) = audited(&argv) else {
        common::warn("audit-js: npm could not run — the audit did NOT run");
        return Outcome::Unavailable;
    };
    conclude(
        "audit-js",
        read_npm_audit(exit_ok, &out),
        releasing(refs),
        &out,
    )
}

pub fn go(refs: &[PushRef]) -> Outcome {
    if common::which("govulncheck").is_none() {
        common::warn(
            "audit-go: govulncheck is not installed \
             (go install golang.org/x/vuln/cmd/govulncheck@latest) — the audit did NOT run",
        );
        return Outcome::Unavailable;
    }
    let argv = vec![common::program("govulncheck"), "./...".into()];
    let Some((exit_ok, out)) = audited(&argv) else {
        return Outcome::Unavailable;
    };
    conclude(
        "audit-go",
        read_govulncheck(exit_ok, &out),
        releasing(refs),
        &out,
    )
}

pub fn python(refs: &[PushRef]) -> Outcome {
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
    let Some((exit_ok, out)) = audited(&argv) else {
        return Outcome::Unavailable;
    };
    conclude(
        "audit-python",
        read_pip_audit(exit_ok, &out),
        releasing(refs),
        &out,
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
        assert_eq!(
            read_npm_audit(true, "up to date, audited 100 packages\n"),
            Report::Clean
        );
        assert_eq!(
            read_npm_audit(false, "npm ERR! network ENOTFOUND\n"),
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
