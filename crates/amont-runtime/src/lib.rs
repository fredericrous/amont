//! The git-templates hook logic: registry, dispatchers and every check.
//!
//! This is a library so that more than one binary can hold the same truth about
//! what a hook IS. `amont` (the commit path) executes the checks;
//! `amont-fleet` reports on how they are installed across the fleet. Before
//! the split there was no lib target at all, which is why `cargo test --lib`
//! failed outright.
//!
//! **This crate must never gain an external dependency.** The hook binary
//! depends on it, so anything added here reaches every commit transitively —
//! and the entire Rust migration existed to remove exactly that kind of
//! requirement. ratatui and friends belong in `amont-fleet`.
//!
//! Hooks are invoked through a thin `sh` shim at each hook path, which passes
//! the hooks directory it lives in:
//!
//! ```text
//! amont --hooks-dir <dir> pre-commit [args…]
//! ```

/// Serialises every test that moves the process cwd **or depends on it**.
///
/// The second half of that sentence was missing, and it cost a day. The
/// lock started as "movers only" — `gate_stamp` and `attest`, which both
/// talk to "the repository at cwd" and each carried its own mutex, so each
/// was serialised against itself and raced against the other. But a mover
/// is only half the hazard: a test that READS the ambient cwd is just as
/// exposed, because production code spawns git without `-C`. While
/// `gate_stamp` held cwd inside its fixture, `restage_distinguishes_
/// nothing_from_failure` ran `git add` — landing in that fixture and
/// taking its `.git/index.lock`, so the fixture's own `git commit` failed
/// 128 and the panic surfaced three lines later as a missing gate stamp.
/// Roughly one run in forty, and unreadable until the fixtures started
/// reporting git's exit status.
///
/// So: if a test moves the cwd, or calls anything that spawns git without
/// naming a directory, it takes this lock.
#[cfg(test)]
pub(crate) static TEST_CWD: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub mod agents_md;
pub mod attest;
pub mod bypass;
pub mod check;
pub mod commit_style;
pub mod config;
pub mod content;
pub mod dispatch;
pub mod downgrade;
pub mod finding;
pub mod gate_stamp;
pub mod git;
pub mod hookfile;
pub mod hooks;
pub mod install;
pub mod json;
pub mod live;
pub mod manifest;
pub mod pack;
pub mod policy;
pub mod pushed_tree;
pub mod pushrefs;
pub mod registry;
pub mod rehearsal;
pub mod setup;
pub mod skew;
pub mod staged_only;
pub mod trust;
pub mod ui;
pub mod vocabulary;

use std::path::Path;
use std::process::{Command, Stdio};

/// `git config --get-all hook.skip`, or empty when unset/unavailable.
/// The two triggers a check can be attached to, as they are spelled in config.
///
/// Deliberately the same strings as `Stage::as_str`, and
/// `every_id_agrees_with_its_declared_stage` keeps them that way.
pub const TRIGGERS: [&str; 2] = ["pre-commit", "pre-push"];

/// How specifically a configured value names a check.
///
/// Ordered, so that when several keys match one check the most specific wins —
/// which only matters for `amont.severity`, since a skip is a boolean.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Match {
    /// `pre-commit` — every check on that trigger.
    Trigger,
    /// `clippy` — that check, whichever trigger it is on.
    ShortName,
    /// `pre-commit-clippy` — this check and no other.
    FullId,
}

/// A check's id without its trigger — `pre-commit-clippy` → `clippy`.
///
/// For DISPLAY only, wherever the trigger is already established by a heading
/// or a neighbouring column. Under a `pre-commit` heading, printing
/// `pre-commit-clippy` on every row spends eleven columns restating what the
/// heading said. Never write this to config or compare against it: two checks
/// can share a short name, and telling them apart is what the id is for.
pub fn short_name(check: &str) -> &str {
    for trigger in TRIGGERS {
        if let Some(short) = check
            .strip_prefix(trigger)
            .and_then(|rest| rest.strip_prefix('-'))
        {
            return short;
        }
    }
    check
}

/// Does `pattern`, as written in `hook.skip` or `amont.severity.<pattern>`,
/// name `check`?
///
/// A check's id is `<trigger>-<name>`, and exactly three things name it:
///
/// | written | means |
/// |---|---|
/// | `pre-commit-clippy` | that one check |
/// | `pre-commit`        | every check on that trigger |
/// | `clippy`            | that check, on any trigger |
///
/// Three exact comparisons. **No substring.** The previous rule was
/// `check.contains(skip)`, which made `hook.skip = clippy` work by accident of
/// reach — and `hook.skip = e` disable all twenty checks by the same accident,
/// and `lint-js` silently also suppress `lint-json-yaml`. Naming the three
/// things a user actually means keeps every useful case and removes every
/// sharp edge, including the one the old doc comment called "not a bug".
///
/// This reads the trigger out of the ID, which is not the same as deriving a
/// check's stage: `Stage` remains a declared field and is what the dispatcher
/// obeys. Here we are parsing an identifier a human typed.
///
/// Defined ONCE because four callers need it — the dispatcher decides what
/// runs, the severity resolver decides what blocks, the fleet view reports
/// where a check applies, and the skip resolver computes reach. A
/// reimplementation that disagreed would have the dashboard claim a check is
/// active while the dispatcher skips it.
pub fn names_check(check: &str, pattern: &str) -> Option<Match> {
    if check == pattern {
        return Some(Match::FullId);
    }
    for trigger in TRIGGERS {
        let Some(short) = check
            .strip_prefix(trigger)
            .and_then(|rest| rest.strip_prefix('-'))
        else {
            continue;
        };
        // An id carries one trigger, so the first that matches is the answer.
        if pattern == trigger {
            return Some(Match::Trigger);
        }
        if pattern == short {
            return Some(Match::ShortName);
        }
        return None;
    }
    None
}

/// Does `skip`, as configured in `hook.skip`, suppress `check`?
pub fn skip_suppresses(check: &str, skip: &str) -> bool {
    names_check(check, skip).is_some()
}

/// One check, as reported by `amont list`.
///
/// Deliberately flat — this is what gets rendered as text or serialised to
/// JSON, and a reader (human or agent) parsing the latter should not have to
/// chase nested objects for a yes/no question.
#[derive(Debug, Clone)]
pub struct CheckListing {
    /// `<trigger>-<name>` — what `hook.skip` and `amont.severity.<key>`
    /// resolve against.
    pub id: String,
    pub short_name: String,
    pub stage: check::Stage,
    pub source: Source,
    /// What the check (or manifest line) declared.
    pub declared_severity: check::Severity,
    /// What `registry::Overrides::of` would actually apply — NOT the same as
    /// `declared_severity` once `amont.severity.*` is configured. This is
    /// the one thing the old text-only `list_checks` never reported, and the
    /// exact "declared vs. effective" gap that caused a real bug in the
    /// fleet's own severity column (see `amont-fleet/src/severities.rs`).
    pub effective_severity: check::Severity,
    pub severity_overridden: bool,
    /// Where the winning override came from, only when one applies —
    /// strictly additive next to `severity_overridden`, whose meaning is
    /// unchanged.
    pub severity_source: Option<registry::Source>,
    pub fix: check::Fix,
    pub status: Status,
    /// Empty when `status == Status::Runs`; the same prose the text output
    /// always showed for the other three states.
    pub reason: String,
    pub scope_files: Vec<String>,
    pub scope_opt_in: Vec<String>,
    /// `Some` only for a declared, `Runnable` external — a builtin has no
    /// command to show, and an `Unusable` external never got far enough to
    /// have one.
    pub command: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Builtin,
    Declared,
}

/// The four states the text output's glyphs already named. Not called
/// `Outcome` — that type means what a check concluded when it RAN; this means
/// whether it would run at all, the same question `Scope::matches` answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Runs,
    Inert,
    Skipped,
    Unusable,
}

pub struct ListOptions {
    pub json: bool,
    pub stage: Option<check::Stage>,
    pub pushed: bool,
    /// Print the inert checks too, one row each, as this always did.
    pub all: bool,
}

/// Every check that would be considered for `stage_filter` (or both stages,
/// when `None`), evaluated against `paths`.
///
/// Reads `hook.skip` and `amont.severity.*` from the current repo's git
/// config, same as the original `list_checks` did — this is why it is not
/// unit-tested in isolation; see `hooks/pull_rebase.rs`'s own split between
/// pure helpers (unit-tested) and config-dependent behaviour (integration
/// tested) for the precedent.
pub fn gather_checks(
    stage_filter: Option<check::Stage>,
    paths: &[String],
    manifest: &manifest::Manifest,
) -> Vec<CheckListing> {
    use crate::check::Stage;
    let stages: Vec<Stage> = match stage_filter {
        Some(s) => vec![s],
        None => vec![Stage::PreCommit, Stage::PrePush],
    };
    let skips = configured_skips();
    let overrides = registry::Overrides::read();
    let externals_by_id: std::collections::BTreeMap<&str, &manifest::External> = manifest
        .externals
        .iter()
        .map(|e| (e.id.as_str(), e))
        .collect();

    let mut out = Vec::new();
    for stage in stages {
        // Externals are listed here too, and marked, because the question
        // this command answers — "would this run here?" — is asked most
        // often about the check somebody just added to `amont.conf`.
        for check in registry::all_stage_checks(stage, manifest) {
            let name = check.name();
            let external = externals_by_id.get(name).copied();
            let skipped = skips.iter().any(|s| skip_suppresses(name, s));
            let applies = check.scope().matches(paths);

            let unusable_why = external.and_then(|e| match &e.kind {
                manifest::Kind::Unusable { why } => Some(why.as_str()),
                manifest::Kind::Runnable { .. } => None,
            });
            // Four states: a check that is correctly silent must never look
            // like one that is disabled, and neither must look like one
            // whose declaration could not be read.
            let (status, reason) = if let Some(w) = unusable_why {
                (Status::Unusable, format!("{} {w}", manifest::MANIFEST))
            } else if skipped {
                // Which source? Policy skips read differently from machine
                // skips — the legend and the fleet detail pane echo this
                // wording, so the three move together.
                let via = if policy::current()
                    .skips
                    .iter()
                    .any(|s| skip_suppresses(name, s))
                {
                    "skipped via amont.conf"
                } else {
                    "skipped via hook.skip"
                };
                (Status::Skipped, via.to_string())
            } else if applies {
                (Status::Runs, String::new())
            } else {
                (
                    Status::Inert,
                    format!("inert here — needs {}", describe(check.scope())),
                )
            };

            let command = external.and_then(|e| match &e.kind {
                manifest::Kind::Runnable { program, args, .. } => Some(
                    std::iter::once(program.as_str())
                        .chain(args.iter().map(String::as_str))
                        .collect::<Vec<_>>()
                        .join(" "),
                ),
                manifest::Kind::Unusable { .. } => None,
            });

            let declared_severity = check.severity();
            let effective_severity = overrides.of(check);
            let severity_source = overrides
                .applied_with_source(name)
                .map(|(_, _, src)| src)
                .filter(|_| declared_severity != effective_severity);
            out.push(CheckListing {
                id: name.to_string(),
                short_name: short_name(name).to_string(),
                stage,
                source: if external.is_some() {
                    Source::Declared
                } else {
                    Source::Builtin
                },
                declared_severity,
                effective_severity,
                severity_overridden: declared_severity != effective_severity,
                severity_source,
                fix: check.fix(),
                status,
                reason,
                scope_files: {
                    let scope = check.scope();
                    scope
                        .files
                        .iter()
                        .chain(scope.names.iter())
                        .map(|s| s.to_string())
                        .collect()
                },
                scope_opt_in: check.scope().opt_in.iter().map(|s| s.to_string()).collect(),
                command,
            });
        }
    }
    out
}

/// BYTE-IDENTICAL to what `list_checks` printed before it grew `--json`.
/// `listings` is already stage-grouped (`gather_checks` iterates stage by
/// stage), so a heading prints exactly once per stage encountered, in the
/// same order.
/// The ecosystem a scope token belongs to, for the inert summary.
///
/// Presentational and deliberately incomplete: a token nobody has mapped
/// simply does not get named, and its check is still counted. The alternative
/// — a `family` field on every `Builtin` — is thirty-seven places for a label
/// to drift out of step with the scope that actually decides anything.
///
/// Generic tokens (`.yaml`, `.json`) are absent on purpose. Four checks want
/// `.yaml` for four different reasons, so naming YAML here would tell a reader
/// their repository is "missing YAML checks" when what it is missing is
/// Kubernetes.
fn ecosystem(token: &str) -> Option<&'static str> {
    Some(match token {
        ".rs" | "Cargo.toml" | "Cargo.lock" => "Rust",
        ".go" | "go.mod" | "go.sum" => "Go",
        ".py"
        | ".pyi"
        | "requirements.txt"
        | "pyproject.toml"
        | "ruff.toml"
        | ".ruff.toml"
        | "pytest.ini"
        | "conftest.py"
        | "pyrightconfig.json"
        | "pyrightconfig.jsonc" => "Python",
        ".js" | ".jsx" | ".ts" | ".tsx" | ".vue" | "package.json" | "package-lock.json" => {
            "JavaScript"
        }
        "kustomization.yaml" | "kustomization.yml" => "Kubernetes",
        t if t.starts_with(".kube-linter") => "Kubernetes",
        t if t.starts_with(".prettierrc") || t == "prettier.config.js" => "JavaScript",
        _ => return None,
    })
}

/// Every check that would run here, and one line for the ones that would not.
///
/// The default used to print all thirty-odd rows interleaved alphabetically.
/// In a repository amont serves well that is about half inert; in one built on
/// a stack it does not cover yet it is two thirds — and every one of those rows
/// names somebody else's language. Read top to bottom it says "this tool is for
/// other people", which is the opposite of true: the checks that DO run are
/// `secrets`, `large-files` and `merge-conflict`, the ones that prevent
/// incidents in any repository at all.
///
/// So the inert rows collapse to a count and the ecosystems they belong to.
/// `--all` brings them back, because "why is clippy not running" is a real
/// question with a real answer already written.
///
/// **Skipped and unusable rows are never collapsed.** `⊘` means somebody
/// silenced a check and `✗` means a declaration is broken; both are things the
/// reader has to act on, and both would be lost in a count.
pub fn print_text(listings: &[CheckListing], all: bool) {
    let shown: Vec<&CheckListing> = listings
        .iter()
        .filter(|l| all || l.status != Status::Inert)
        .collect();

    let mut current: Option<check::Stage> = None;
    for l in &shown {
        if current != Some(l.stage) {
            println!("{}", ui::highlight(l.stage.as_str()));
            current = Some(l.stage);
        }
        let glyph = match l.status {
            Status::Unusable => '✗',
            Status::Skipped => '⊘',
            Status::Runs => '●',
            Status::Inert => '○',
        };
        // The SHORT name: this loop is already inside a `pre-commit` /
        // `pre-push` heading, so printing the trigger on all twenty rows
        // restates the heading twenty times and pushes the reason — the part
        // that differs per row — eleven columns to the right.
        //
        // Where a check CAME FROM belongs next to its name, not appended
        // after a reason that is often empty. A reader scanning this list
        // wants to know which of these their repository added.
        // A declared check's name and its reason both come from the
        // repository's manifest, and `amont list` is read at least as often
        // as the trust prompt. Sanitised before padding, so the column width is
        // computed on what is printed — see `ui::sanitize`.
        let short_name = ui::sanitize(&l.short_name);
        let label = match l.source {
            Source::Declared => format!("{short_name} (declared)"),
            Source::Builtin => short_name,
        };
        println!("  {glyph} {label:<26} {}", ui::sanitize(&l.reason));
    }

    let runs = listings.iter().filter(|l| l.status == Status::Runs).count();
    let inert: Vec<&CheckListing> = listings
        .iter()
        .filter(|l| l.status == Status::Inert)
        .collect();

    println!();
    if inert.is_empty() {
        println!("  {runs} active here.");
    } else if all {
        println!("  {runs} active here, {} inert.", inert.len());
    } else {
        let mut families: Vec<&str> = inert
            .iter()
            .flat_map(|l| l.scope_files.iter().chain(l.scope_opt_in.iter()))
            .filter_map(|t| ecosystem(t))
            .collect();
        families.sort_unstable();
        families.dedup();
        let named = if families.is_empty() {
            String::new()
        } else {
            format!(" ({})", families.join(", "))
        };
        println!(
            "  {runs} active here.  {} inert{named} — amont list --all",
            inert.len()
        );
    }
    println!("  ● runs here   ○ inert   ⊘ skipped via hook.skip   ✗ declaration unusable");
}

/// What `commit-msg` will enforce here, and where each answer came from.
///
/// Printed **always**, not only when something has been configured. The
/// defaults are the divisive part — a gitmoji in every subject, a 50-character
/// description — and somebody who wants them changed has no reason to guess
/// that four keys exist. `amont list` is where they are already looking, so
/// it is where the dial belongs.
///
/// The source column appears only for a value somebody set: repeating
/// "default" on four rows spends a column restating the line underneath.
pub fn print_commit_style(style: &commit_style::Style, rows: &[commit_style::Setting]) {
    println!();
    println!("{}", ui::highlight("commit style"));
    for r in rows {
        let origin = if r.set_here {
            format!("{} ({})", r.key, r.scope.as_str())
        } else {
            String::new()
        };
        println!("  {:<18} {:<10} {origin}", r.label, r.value);
    }
    println!();
    for w in style.warnings() {
        println!("  {} {w}", ui::warning_sign().trim());
    }
    println!("  `amont setup` to change any of these");
}

/// The commit-style block as JSON: the effective value, the shipped default,
/// whether they differ and where the answer came from — the same
/// declared-vs-effective shape `CheckListing` uses for severity.
fn commit_style_json(style: &commit_style::Style, rows: &[commit_style::Setting]) -> String {
    let setting = |r: &commit_style::Setting, value: String, default: String| {
        json::object(&[
            format!("\"value\":{value}"),
            format!("\"default\":{default}"),
            json::bool_field("overridden", r.overridden),
            json::bool_field("set_here", r.set_here),
            json::string_field("source", r.scope.as_str()),
            json::string_field("key", r.key),
        ])
    };
    let d = commit_style::Style::default();
    // `rows` is built in this order by `commit_style::describe`, and the
    // numbers are emitted as numbers so a reader can compare them.
    let quoted = |s: &str| format!("\"{}\"", json::escape(s));
    let fields: Vec<String> = rows
        .iter()
        .map(|r| {
            let (value, default) = match r.key {
                commit_style::KEY_GITMOJI => {
                    (quoted(style.gitmoji.as_str()), quoted(d.gitmoji.as_str()))
                }
                commit_style::KEY_SUBJECT_MAX => {
                    (style.subject_max.to_string(), d.subject_max.to_string())
                }
                commit_style::KEY_DESCRIPTION_MAX => (
                    style.description_max.to_string(),
                    d.description_max.to_string(),
                ),
                _ => (style.body_wrap.to_string(), d.body_wrap.to_string()),
            };
            let name = r.key.rsplit('.').next().unwrap_or(r.key);
            format!("\"{}\":{}", json::escape(name), setting(r, value, default))
        })
        .collect();

    let warnings: Vec<String> = style.warnings();
    let mut all = fields;
    all.push(json::string_array_field("warnings", &warnings));
    json::object(&all)
}

/// The format id this document declares, as its first field.
///
/// Every other machine-readable thing this tool writes carries one and
/// REFUSES what it does not recognise — `amont-gate-v1`, `amont-held-v1`,
/// `amont-skew-v1`, `amont-bypasses-v1`, and `amont-attest-v2`, whose bump
/// exists precisely so a v1 verifier reads a v2 note as no note rather than
/// misreading it. This document, the most public machine surface of the
/// three, carried none: a reader had no way to state which contract it was
/// written against, so a rename here would land as a silently different
/// answer rather than a failure. Bump the version when a field's MEANING
/// changes or one is removed; adding a field keeps it, which is what the
/// object shape below was already for.
pub const LIST_FORMAT: &str = "amont-list-v1";

/// `{"format": "amont-list-v1", "stage_filter": ..., "checks": [...]}` — an
/// object, not a bare array, so a field can be added later without changing
/// the top-level shape.
pub fn print_json(
    stage_filter: Option<check::Stage>,
    pushed: bool,
    listings: &[CheckListing],
    bypasses: &bypass::Ledger,
    downgrades: &downgrade::Ledger,
    conventions_apply: bool,
) {
    let checks: Vec<String> = listings
        .iter()
        .map(|l| {
            json::object(&[
                json::string_field("id", &l.id),
                json::string_field("short_name", &l.short_name),
                json::string_field("stage", l.stage.as_str()),
                json::string_field(
                    "source",
                    match l.source {
                        Source::Builtin => "builtin",
                        Source::Declared => "declared",
                    },
                ),
                json::string_field("declared_severity", l.declared_severity.as_str()),
                json::string_field("effective_severity", l.effective_severity.as_str()),
                json::bool_field("severity_overridden", l.severity_overridden),
                json::opt_string_field(
                    "severity_source",
                    l.severity_source.map(registry::Source::as_str),
                ),
                json::string_field("fix", l.fix.as_str()),
                json::string_field(
                    "status",
                    match l.status {
                        Status::Runs => "runs",
                        Status::Inert => "inert",
                        Status::Skipped => "skipped",
                        Status::Unusable => "unusable",
                    },
                ),
                json::string_field("reason", &l.reason),
                json::string_array_field("scope_files", &l.scope_files),
                json::string_array_field("scope_opt_in", &l.scope_opt_in),
                json::opt_string_field("command", l.command.as_deref()),
            ])
        })
        .collect();

    let (style, rows) = commit_style::describe();
    println!(
        "{}",
        json::object(&[
            json::string_field("format", LIST_FORMAT),
            json::opt_string_field("stage_filter", stage_filter.map(check::Stage::as_str)),
            json::bool_field("pushed", pushed),
            format!("\"checks\":{}", json::array(&checks)),
            format!("\"commit_style\":{}", commit_style_json(&style, &rows)),
            format!("\"branch_style\":{}", branch_style_json()),
            format!("\"bypasses\":{}", bypasses_json(bypasses)),
            format!("\"downgrades\":{}", downgrades_json(downgrades)),
            json::bool_field("conventions_apply", conventions_apply),
        ])
    );
}

/// `{"total": N, "last": <epoch|null>, "by_script": [...]}` — the ledger of
/// unverified commits, so a parsing reader (the fleet, an agent) sees the
/// same numbers `amont list` prints.
fn downgrades_json(l: &downgrade::Ledger) -> String {
    let by_check: Vec<String> = l
        .by_check
        .iter()
        .map(|c| {
            json::object(&[
                json::string_field("check", &c.check),
                json::int_field("count", c.count as i64),
                json::int_field("would_block", c.would_block as i64),
                json::int_field("last", c.last as i64),
            ])
        })
        .collect();
    json::object(&[
        json::int_field("total", l.total as i64),
        json::int_field("would_block", l.would_block as i64),
        json::int_field("commits", l.commits as i64),
        json::opt_int_field("first", l.first.map(|v| v as i64)),
        json::opt_int_field("last", l.last.map(|v| v as i64)),
        format!("\"by_check\":{}", json::array(&by_check)),
    ])
}

fn bypasses_json(l: &bypass::Ledger) -> String {
    let by_script: Vec<String> = l
        .by_script
        .iter()
        .map(|s| {
            json::object(&[
                json::string_field("script", &s.script),
                json::int_field("count", s.count as i64),
                json::int_field("last", s.last as i64),
            ])
        })
        .collect();
    json::object(&[
        json::int_field("total", l.total as i64),
        json::opt_int_field("last", l.last.map(|v| v as i64)),
        format!("\"by_script\":{}", json::array(&by_script)),
    ])
}

/// The branch contract, in the same document agents are told to consult — so
/// the pattern is knowable BEFORE a branch is created rather than discovered
/// at push time. Rendered from `vocabulary::BRANCH_PREFIXES`, the same table
/// `pre-push-branch-pattern` enforces: there is no second copy to drift.
fn branch_style_json() -> String {
    let prefixes: Vec<String> = vocabulary::BRANCH_PREFIXES
        .iter()
        .map(|p| p.name.to_string())
        .collect();
    json::object(&[
        json::string_field("shape", "<prefix>/<name>"),
        json::string_field("pattern", &vocabulary::branch_contract()),
        json::string_array_field("prefixes", &prefixes),
    ])
}

/// `git ls-files` — every check's default scope evaluation, unchanged from
/// what `list_checks` always did.
///
/// Through `git::stdout_paths`, i.e. with `-z`. A raw `ls-files` QUOTES any
/// path holding an unusual byte: `é.json` comes back as the nine-byte literal
/// `"\303\251.json"`, which ends with a quote rather than an extension, so
/// `Scope::matches` reports a check as irrelevant to a repository it plainly
/// covers. Cosmetic here (this only decides what `list` prints) and not
/// cosmetic in `dispatch::enter_all_files_mode`, which is the same bug — so
/// both ask the same way.
fn tracked_paths() -> Vec<String> {
    git::stdout_paths(&["ls-files"]).unwrap_or_default()
}

/// The pushed-range file list, computed standalone rather than from a real
/// pre-push invocation's stdin.
///
/// Reuses `pushrefs::changed_files`, which already handles zero-oid deletes,
/// merge commits and the `--stdin` trailing-newline edge case — this only
/// SYNTHESISES the one `PushRef` a standalone invocation has no other way to
/// obtain.
fn pushed_paths() -> Result<Vec<String>, String> {
    let synthetic = pushrefs::synthetic_from_upstream()?;
    Ok(pushrefs::changed_files(&[synthetic]))
}

/// `amont list`: what would run here, and why — as prose, or as
/// `--json` for a reader that wants to parse it.
pub fn list_checks(opts: ListOptions) -> i32 {
    let paths = if opts.pushed {
        match pushed_paths() {
            Ok(p) => p,
            Err(msg) => {
                if opts.json {
                    println!("{}", json::object(&[json::string_field("error", &msg)]));
                } else {
                    eprintln!("amont: {msg}");
                }
                return 2;
            }
        }
    } else {
        tracked_paths()
    };
    // Loaded HERE, with the repository this command is standing in — the
    // owned-manifest shape every entrypoint now follows. See manifest::load.
    let manifest = manifest::load(std::path::Path::new(&hooks::common::repo_root()));
    // INVARIANT: policy installed immediately after every manifest::load.
    policy::install(manifest.policy.clone());
    let listings = gather_checks(opts.stage, &paths, &manifest);
    let bypasses = bypass::read();
    let downgrades = downgrade::read();
    let conventions_apply = dispatch::conventions_apply(&manifest);
    if opts.json {
        print_json(
            opts.stage,
            opts.pushed,
            &listings,
            &bypasses,
            &downgrades,
            conventions_apply,
        );
    } else {
        print_text(&listings, opts.all);
        // Not filtered by `--stage`: commit style belongs to no stage, and
        // suppressing it for `--stage pre-push` would only hide it from the
        // reader who narrowed their question.
        let (style, rows) = commit_style::describe();
        print_commit_style(&style, &rows);
        print_bypasses(&bypasses);
        print_downgrades(&downgrades);
        if !conventions_apply {
            println!(
                "\n  ! conventions held back — no amont.conf here and amont.conventions \
                 is `declared`; only the safety net runs"
            );
        }
    }
    0
}

/// The shadow-mode worksheet, only when there is one — a repository that has
/// never warned about anything keeps its old output byte-for-byte.
///
/// This is what a fortnight of `amont.severity.pre-commit warn` is FOR: the
/// counts are the argument, and the footer carries the one action a reader
/// takes afterwards. Repeating a config line per row would bury it.
fn print_downgrades(l: &downgrade::Ledger) {
    if l.total == 0 {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    println!("\nproblems that did not block");
    let pad = l
        .by_check
        .iter()
        .map(|c| ui::sanitize(&c.check).chars().count())
        .max()
        .unwrap_or(0);
    for c in &l.by_check {
        // A check that declares `warn` itself was never going to block, and
        // saying so inline stops it being read as rollout evidence.
        let advisory = if c.would_block == 0 {
            "  (advisory)"
        } else {
            ""
        };
        println!(
            "  {:<pad$}  {:>3}   last {}{}",
            ui::sanitize(&c.check),
            c.count,
            bypass::age(now, c.last),
            advisory
        );
    }
    // Events and commits are different facts: forty commits each tripping a
    // check once is a check the team disagrees with, while one commit tripping
    // it forty times is one person losing an afternoon.
    let since = l
        .first
        .map(|f| format!(", since {}", bypass::age(now, f)))
        .unwrap_or_default();
    println!(
        "  {} event{} over {} commit{}{since}",
        l.total,
        if l.total == 1 { "" } else { "s" },
        l.commits,
        if l.commits == 1 { "" } else { "s" },
    );
    if l.would_block > 0 {
        println!(
            "  {} of them would have blocked — set amont.severity.<check> to keep one \
             advisory when you go back to block",
            l.would_block
        );
    }
}

/// The unverified-commit tally, only when there is one — a clean repository's
/// `amont list` output stays byte-identical to what it always was.
fn print_bypasses(l: &bypass::Ledger) {
    if l.total == 0 {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    println!("\nunverified commits");
    let pad = l
        .by_script
        .iter()
        .map(|s| ui::sanitize(&s.script).chars().count())
        .max()
        .unwrap_or(0);
    for s in &l.by_script {
        println!(
            "  {:<pad$}  {:>3}   last {}",
            ui::sanitize(&s.script),
            s.count,
            bypass::age(now, s.last)
        );
    }
    println!(
        "  these commits carry no record that their commit-time gate ran — the push gate ran it instead"
    );
}

fn describe(s: crate::check::Scope) -> String {
    let files = if s.is_unscoped() {
        String::new()
    } else {
        s.files
            .iter()
            .chain(s.names.iter())
            .copied()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let opt = s.opt_in.join(" | ");
    match (files.is_empty(), opt.is_empty()) {
        (false, false) => format!("{files} + {opt}"),
        (false, true) => files,
        (true, false) => opt,
        (true, true) => "nothing".into(),
    }
}

/// The machine's `hook.skip` entries PLUS the trusted policy's `skip`
/// lines — the union every resolution site sees. Callers that must tell the
/// two apart (the dispatcher announces them separately) use
/// [`skips_by_source`].
pub fn configured_skips() -> Vec<String> {
    policy::union_skips(machine_skips(), policy::current())
}

/// `(machine, policy)` — the split the announcements need: "you decided
/// this" and "your team decided this" are different things to be told.
pub fn skips_by_source() -> (Vec<String>, Vec<String>) {
    (machine_skips(), policy::current().skips.clone())
}

fn machine_skips() -> Vec<String> {
    let Ok(out) = Command::new("git")
        .args(["config", "--get-all", "hook.skip"])
        .stderr(Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Which git operations are part-way through, from the markers in `$GIT_DIR`.
///
/// Asks git directly rather than deriving a path from `hooks_dir`. That used
/// to be `hooks_dir.parent()` — correct for the main worktree, where hooks
/// live in `.git/hooks` and `.git` IS `$GIT_DIR`, but wrong for a LINKED
/// worktree: hooks dispatch from the COMMON directory's `hooks/`, shared
/// across every worktree, while `MERGE_HEAD`/`CHERRY_PICK_HEAD`/etc. live in
/// each worktree's own PRIVATE gitdir under `.git/worktrees/<name>`.
/// Conflating the two silently disabled this guard for every linked
/// worktree — the same mistake `staged_only`'s store path made, which lost
/// unstaged work outright; see its module doc.
pub fn git_states_in_progress() -> Vec<crate::check::GitState> {
    let Some(git_dir) = crate::git::stdout(&["rev-parse", "--git-dir"]) else {
        return Vec::new();
    };
    let git_dir = Path::new(&git_dir);
    crate::check::GitState::ALL
        .into_iter()
        .filter(|state| {
            state
                .markers()
                .iter()
                .any(|marker| git_dir.join(marker).exists())
        })
        .collect()
}

/// True during a cherry-pick, where the zsh `pre-commit` exited 0 immediately.
/// Superseded internally by [`git_states_in_progress`]; kept for whatever
/// still calls it directly. See that function for why this asks git rather
/// than deriving a path from `hooks_dir`.
pub fn cherry_pick_in_progress() -> bool {
    crate::git::stdout(&["rev-parse", "--git-dir"])
        .map(|d| Path::new(&d).join("CHERRY_PICK_HEAD").exists())
        .unwrap_or(false)
}

#[cfg(test)]
mod naming {
    use super::*;

    /// The three things a user can write, and what each reaches.
    #[test]
    fn three_ways_to_name_a_check() {
        assert_eq!(
            names_check("pre-commit-clippy", "pre-commit-clippy"),
            Some(Match::FullId)
        );
        assert_eq!(
            names_check("pre-commit-clippy", "pre-commit"),
            Some(Match::Trigger)
        );
        assert_eq!(
            names_check("pre-commit-clippy", "clippy"),
            Some(Match::ShortName)
        );
    }

    /// The hazards the old substring rule created, all gone by construction.
    #[test]
    fn nothing_matches_by_accident() {
        // `hook.skip = e` disabled all twenty checks. It now reaches nothing.
        for pattern in ["e", "t", "i", ""] {
            assert_eq!(
                names_check("pre-commit-clippy", pattern),
                None,
                "{pattern:?}"
            );
        }
        // A partial word is not a name.
        assert_eq!(names_check("pre-commit-clippy", "clip"), None);
        assert_eq!(names_check("pre-commit-clippy", "lint"), None);
        // The wrong trigger reaches nothing.
        assert_eq!(names_check("pre-commit-clippy", "pre-push"), None);
        // And the empty string names nothing, rather than everything — git
        // stores `hook.skip` with no value as exactly this.
        assert_eq!(names_check("pre-commit-clippy", ""), None);
    }

    /// The coupling `docs/hook-skip-management.md` warned about: `lint-js` is a
    /// substring of `lint-json-yaml`, so skipping one used to skip both.
    #[test]
    fn a_short_name_does_not_reach_a_longer_one() {
        assert!(names_check("pre-commit-lint-json-yaml", "lint-js").is_none());
        assert_eq!(
            names_check("pre-commit-lint-js", "lint-js"),
            Some(Match::ShortName)
        );
        assert_eq!(
            names_check("pre-commit-lint-json-yaml", "lint-json-yaml"),
            Some(Match::ShortName)
        );
    }

    /// The one value that exists in the real fleet.
    #[test]
    fn the_fleets_only_skip_still_resolves() {
        assert_eq!(
            names_check("pre-push-run-tests-js", "run-tests-js"),
            Some(Match::ShortName)
        );
    }

    /// A trigger reaches every check on it and none on the other.
    #[test]
    fn a_trigger_reaches_its_own_stage_only() {
        let pre_commit = registry::CHECKS
            .iter()
            .filter(|c| names_check(c.name, "pre-commit").is_some())
            .count();
        let pre_push = registry::CHECKS
            .iter()
            .filter(|c| names_check(c.name, "pre-push").is_some())
            .count();
        assert_eq!(pre_commit + pre_push, registry::CHECKS.len());
        assert!(pre_commit > 0 && pre_push > 0);
    }

    /// Specificity ordering, which decides severity when several keys apply.
    #[test]
    fn a_full_id_outranks_a_short_name_outranks_a_trigger() {
        assert!(Match::FullId > Match::ShortName);
        assert!(Match::ShortName > Match::Trigger);
    }

    /// The resolver reads the trigger out of the ID. That is only sound while
    /// every ID agrees with the stage its check actually declares — so it is
    /// checked rather than assumed.
    #[test]
    fn every_id_agrees_with_its_declared_stage() {
        for check in registry::CHECKS {
            assert_eq!(
                names_check(check.name, check.stage.as_str()),
                Some(Match::Trigger),
                "{} declares {:?} but its id says otherwise",
                check.name,
                check.stage
            );
        }
    }
}

#[cfg(test)]
mod listing {
    use super::*;

    /// The summary names ecosystems so the inert count reads as "other
    /// people's stacks" rather than "twenty-two things are broken".
    #[test]
    fn scope_tokens_map_to_the_ecosystem_a_reader_would_name() {
        assert_eq!(ecosystem(".rs"), Some("Rust"));
        assert_eq!(ecosystem("Cargo.lock"), Some("Rust"));
        assert_eq!(ecosystem("go.sum"), Some("Go"));
        assert_eq!(ecosystem("pyproject.toml"), Some("Python"));
        assert_eq!(ecosystem(".tsx"), Some("JavaScript"));
        assert_eq!(ecosystem(".prettierrc.json"), Some("JavaScript"));
        assert_eq!(ecosystem("kustomization.yaml"), Some("Kubernetes"));
        assert_eq!(ecosystem(".kube-linter.yaml"), Some("Kubernetes"));
    }

    /// The generic tokens are unmapped ON PURPOSE. Four checks want `.yaml`
    /// for four unrelated reasons, so naming YAML would tell a reader their
    /// repository is missing "YAML checks" when what it is missing is
    /// Kubernetes. An unmapped token is still COUNTED — it just goes unnamed,
    /// which is the honest failure mode for a presentational map.
    #[test]
    fn generic_tokens_are_deliberately_unnamed() {
        for t in [".yaml", ".yml", ".json", ".md", ".sh", "unknown-thing"] {
            assert_eq!(ecosystem(t), None, "{t} should not name an ecosystem");
        }
    }
}
