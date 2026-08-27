//! The shadow-mode ledger — what a check found when it was set not to block.
//!
//! The feature exists so a team lead can answer "will this annoy my team?"
//! with a fortnight of evidence instead of a guess. Every test here is about
//! keeping that number honest: it must count what really happened at a commit,
//! and nothing else. A ledger inflated by rehearsals, or by problems that
//! actually blocked, would argue for a rollout decision on numbers that mean
//! something other than what they say.

mod common;

use common::Repo;

/// A staged file that trips `pre-commit-ban-terms`.
const OFFENDING: &str = "function f() {\n  debugger;\n}\n";

fn ledger(repo: &Repo) -> String {
    std::fs::read_to_string(repo.path(".git/amont-downgrades")).unwrap_or_default()
}

/// Event lines, header excluded.
fn events(repo: &Repo) -> Vec<String> {
    ledger(repo)
        .lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect()
}

/// A repo with one commit behind it, so HEAD resolves.
fn seeded() -> Repo {
    let repo = Repo::new();
    repo.stage("seed.txt", "seed\n");
    repo.commit("feat: seed");
    repo
}

fn warn_the_stage(repo: &Repo) {
    repo.git(&["config", "amont.severity.pre-commit", "warn"]);
}

#[test]
fn a_downgraded_failure_is_recorded_with_its_origin() {
    let repo = seeded();
    warn_the_stage(&repo);
    repo.stage("app.js", OFFENDING);
    let run = repo.hook("pre-commit", &[]);

    assert!(run.passed(), "warn must not block: {}", run.output());
    let events = events(&repo);
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(events[0].contains("pre-commit-ban-terms"), "{events:?}");
    // `config`, not `declared`: ban-terms ships as `block`, so this one really
    // would have stopped the commit. That distinction is the entire number.
    assert!(events[0].ends_with(" config"), "{events:?}");
}

/// A check that actually blocked has nothing to report — it did its job. If a
/// blocked commit were recorded, the headline would double-count exactly the
/// thing it exists to measure.
#[test]
fn a_blocking_failure_records_nothing() {
    let repo = seeded();
    repo.stage("app.js", OFFENDING);
    let run = repo.hook("pre-commit", &[]);

    assert!(!run.passed(), "expected a block: {}", run.output());
    assert_eq!(ledger(&repo), "", "a blocked commit must leave no event");
}

/// THE test that keeps the numbers meaningful. `amont run` is a rehearsal, not
/// a commit; `--all-files` rehearses over content nobody is committing at all.
/// Recording either would make "41 commits" a number about something else.
#[test]
fn a_rehearsal_records_nothing() {
    let repo = seeded();
    warn_the_stage(&repo);
    repo.stage("app.js", OFFENDING);

    let run = repo.run(&["run"]);
    assert!(
        run.says("Unwanted terms"),
        "the check must have run: {}",
        run.output()
    );
    assert_eq!(ledger(&repo), "", "`amont run` must not record");

    repo.run(&["run", "--all-files"]);
    assert_eq!(ledger(&repo), "", "`amont run --all-files` must not record");

    // …and the hook still does, so the emptiness above is the rehearsal being
    // excluded rather than the feature being broken.
    repo.hook("pre-commit", &[]);
    assert_eq!(events(&repo).len(), 1);
}

/// Events and commits are different facts, and the report shows both. Three
/// attempts at one commit is one person losing an afternoon; three attempts
/// across three commits is a check the team disagrees with.
#[test]
fn repeated_attempts_at_one_commit_share_a_commit_oid() {
    let repo = seeded();
    warn_the_stage(&repo);
    repo.stage("app.js", OFFENDING);
    repo.hook("pre-commit", &[]);
    repo.hook("pre-commit", &[]);
    repo.hook("pre-commit", &[]);

    let events = events(&repo);
    assert_eq!(events.len(), 3, "three events: {events:?}");
    let oids: std::collections::BTreeSet<&str> =
        events.iter().filter_map(|l| l.split(' ').nth(1)).collect();
    assert_eq!(oids.len(), 1, "all against the same HEAD: {events:?}");
}

#[test]
fn the_off_switch_stops_the_counting() {
    let repo = seeded();
    warn_the_stage(&repo);
    repo.git(&["config", "amont.recordDowngrades", "false"]);
    repo.stage("app.js", OFFENDING);

    let run = repo.hook("pre-commit", &[]);
    assert!(
        run.says("Unwanted terms"),
        "the check still runs: {}",
        run.output()
    );
    assert_eq!(
        ledger(&repo),
        "",
        "recordDowngrades false must record nothing"
    );
}

/// The ledger is amont's own bookkeeping and leaves with the hooks.
#[test]
fn uninstall_takes_the_ledger_with_it() {
    let repo = seeded();
    warn_the_stage(&repo);
    repo.stage("app.js", OFFENDING);
    repo.hook("pre-commit", &[]);
    assert!(!ledger(&repo).is_empty(), "precondition: an event exists");

    let run = repo.run(&["uninstall"]);
    assert!(
        !repo.path(".git/amont-downgrades").exists(),
        "uninstall left the ledger behind: {}",
        run.output()
    );
}

#[test]
fn list_reports_the_tally_in_both_renderings() {
    let repo = seeded();
    warn_the_stage(&repo);
    repo.stage("app.js", OFFENDING);
    repo.hook("pre-commit", &[]);

    let text = repo.run(&["list"]);
    assert!(
        text.says("problems that did not block"),
        "{}",
        text.output()
    );
    assert!(text.says("pre-commit-ban-terms"), "{}", text.output());
    assert!(
        text.says("would have blocked"),
        "the rollout line is the point: {}",
        text.output()
    );

    let json = repo.run(&["list", "--json"]);
    assert!(json.says(r#""downgrades""#), "{}", json.output());
    assert!(json.says(r#""would_block":1"#), "{}", json.output());
    assert!(json.says(r#""commits":1"#), "{}", json.output());
}

/// A clean repository's output is unchanged — the section appears only when
/// there is something to say.
#[test]
fn a_repository_with_nothing_to_report_says_nothing() {
    let repo = seeded();
    repo.stage("ok.js", "const x = 1;\n");
    repo.hook("pre-commit", &[]);

    let run = repo.run(&["list"]);
    assert!(
        !run.says("problems that did not block"),
        "no section without events: {}",
        run.output()
    );
}
