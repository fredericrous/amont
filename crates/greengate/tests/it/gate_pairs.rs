//! A gate is a NAME declared at both stages — not an npm vocabulary.
//!
//! `typecheck`/`test:unit`/`test` gated the npm push gate from the start;
//! these tests pin the general form: declare any name at pre-commit (block)
//! and again at pre-push, and the commit-time side earns per-commit stamps
//! the push-time side defers to. A Rust repo's `cargo test`, a Python
//! repo's `pytest` — same contract, no package.json anywhere.
//!
//! The pair as a person meets it (deferred to a stamped commit, back for an
//! unstamped one, refused on request, the dodge in the ledger) is a step of
//! `flows/push.rs`. What stays here is what changes the pairing: an opt-in
//! marker, a warn or policy-downgraded side, an unpaired gate, a built-in.

use crate::common;
use common::{missing, Repo};

use std::io::Write;
use std::process::Stdio;

/// Run the pre-push DISPATCHER (the pairing lives there, not in a check)
/// over one ref line, capturing output.
fn push_out(r: &Repo, from: &str, to: &str) -> (i32, String) {
    // A feature ref, not main: this drives the whole DISPATCHER, and the
    // built-in branch-protect would (correctly) block a push to main.
    let line = format!("refs/heads/feat/x {to} refs/heads/feat/x {from}\n");
    let mut child = crate::common::amont_cmd()
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-push")
        .current_dir(&r.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(line.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("wait");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn head(r: &Repo) -> String {
    String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string()
}

fn trust_and_install(r: &Repo) {
    for verb in ["trust", "init"] {
        let out = crate::common::amont_cmd()
            .arg(verb)
            .current_dir(&r.dir)
            .stdin(Stdio::null())
            .output()
            .expect("amont");
        assert!(
            out.status.success(),
            "amont {verb}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// A commit-run log both sides share: its length says how many times the
/// command executed, wherever from.
fn runs(r: &Repo) -> usize {
    std::fs::read_to_string(r.dir.join("gate.log"))
        .map(|s| s.len())
        .unwrap_or(0)
}

/// A repo whose `check` is declared at BOTH stages — the pair — with a
/// custom name no GATE list mentions, logging each execution.
fn paired_repo(severity: &str) -> (Repo, String) {
    let r = Repo::new();
    r.stage("gate.js", "require('fs').appendFileSync('gate.log','x')\n");
    r.commit("chore: base");
    let base = head(&r);
    r.stage(
        "amont.conf",
        &format!(
            "pre-commit  check  *.txt  {severity}  node gate.js\n\
             pre-push    check  *.txt  block  node gate.js\n"
        ),
    );
    r.commit("chore: the pair");
    trust_and_install(&r);
    (r, base)
}

/// An opt-in marker on the pair (`*.txt+marker`) is a fact about the
/// REPOSITORY, and it must be judged there. Judging it per commit — against
/// the files each commit changed — answered "would never fire" for every
/// commit that left `marker` alone, which is every ordinary commit; no
/// commit counted as unstamped, the verdict was "gated at commit", and the
/// push-side twin was skipped for a commit nothing had ever checked.
#[test]
fn an_opt_in_marker_does_not_hide_an_unstamped_commit() {
    if missing("node") {
        return;
    }
    let r = Repo::new();
    // ADR-0018: this test covers the inline path, where the gate runs in the push.
    r.git(&["config", "amont.pushGates", "inline"]);
    r.stage("gate.js", "require('fs').appendFileSync('gate.log','x')\n");
    r.stage("marker", "opted in\n");
    r.commit("chore: base");
    let base = head(&r);
    r.stage(
        "amont.conf",
        "pre-commit  check  *.txt+marker  block  node gate.js\n\
         pre-push    check  *.txt+marker  block  node gate.js\n",
    );
    r.commit("chore: the pair");
    trust_and_install(&r);

    r.stage("a.txt", "hello\n");
    r.commit("feat: dodge the gate"); // Repo::commit IS --no-verify
    assert_eq!(runs(&r), 0, "nothing ran at commit");

    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(
        !out.contains("gated at commit instead"),
        "an unstamped commit was reported as gated: {out}"
    );
    assert!(
        out.contains("no record of it — running it here"),
        "the reason is named: {out}"
    );
    assert_eq!(runs(&r), 1, "the push-side twin must run");

    // And the other half, so this is not passing by never pairing at all: a
    // verified commit on top IS gated. Its gate ran on the whole tree, the
    // dodged `a.txt` included, so the tip vouches for the code being pushed
    // and the older unstamped commit changes nothing about it.
    r.stage("b.txt", "world\n");
    let out = r.git(&["commit", "-q", "-m", "feat: through the gate"]);
    assert!(out.status.success(), "verified commit failed");
    assert_eq!(runs(&r), 2, "the commit-time side ran");
    let tip = head(&r);
    let (code, out) = push_out(&r, &base, &tip);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("gated at commit instead"),
        "a stamped tip covers its ref: {out}"
    );
    assert!(
        out.contains(&format!("the code at tip {} passed at commit", &tip[..8])),
        "the line names the tip it rests on: {out}"
    );
    assert_eq!(runs(&r), 2, "the push-side twin must not run again");
}

/// A WARN commit-time side vouches for nothing: no stamp, push always runs.
#[test]
fn a_warn_severity_pair_cannot_vouch() {
    if missing("node") {
        return;
    }
    let (r, base) = paired_repo("warn");
    r.stage("a.txt", "hello\n");
    let out = r.git(&["commit", "-q", "-m", "feat: warn cannot vouch"]);
    // ADR-0018: this test covers the inline path, where the gate runs in the push.
    r.git(&["config", "amont.pushGates", "inline"]);
    assert!(out.status.success());
    assert_eq!(runs(&r), 1, "the warn check still ran at commit");

    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(
        !out.contains("gated at commit instead"),
        "a warn declaration must not suppress the push: {out}"
    );
    assert_eq!(runs(&r), 2, "the push ran it again");
}

/// An unpaired pre-push declaration runs exactly as it always has.
#[test]
fn an_unpaired_push_declaration_just_runs() {
    if missing("node") {
        return;
    }
    let r = Repo::new();
    // ADR-0018: this test covers the inline path, where the gate runs in the push.
    r.git(&["config", "amont.pushGates", "inline"]);
    r.stage("gate.js", "require('fs').appendFileSync('gate.log','x')\n");
    r.commit("chore: base");
    let base = head(&r);
    r.stage(
        "amont.conf",
        "pre-push  check  *.txt  block  node gate.js\n",
    );
    r.commit("chore: push only");
    trust_and_install(&r);
    r.stage("a.txt", "hello\n");
    r.commit("feat: change");
    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("gated at commit"), "{out}");
    assert_eq!(runs(&r), 1);
}

/// A committed `severity` line downgrading the commit-time twin removes its
/// power to vouch: an unstamped gate correctly runs again at push. The
/// policy weakened the commit side, not the push side.
#[test]
fn a_policy_downgraded_pair_cannot_vouch() {
    if missing("node") {
        return;
    }
    let r = Repo::new();
    // ADR-0018: this test covers the inline path, where the gate runs in the push.
    r.git(&["config", "amont.pushGates", "inline"]);
    r.stage("gate.js", "require('fs').appendFileSync('gate.log','x')\n");
    r.commit("chore: base");
    let base = head(&r);
    r.stage(
        "amont.conf",
        "severity pre-commit-check warn\n\
         pre-commit  check  *.txt  block  node gate.js\n\
         pre-push    check  *.txt  block  node gate.js\n",
    );
    r.commit("chore: pair plus policy");
    trust_and_install(&r);

    r.stage("a.txt", "hello\n");
    let out = r.git(&["commit", "-q", "-m", "feat: through a warn gate"]);
    assert!(out.status.success(), "commit failed");
    assert_eq!(runs(&r), 1, "the commit-time side still ran");

    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(
        !out.contains("gated at commit instead"),
        "a warn-severity run must not vouch: {out}"
    );
    assert_eq!(runs(&r), 2, "the push side ran again");
}

/// A repo whose BUILT-IN `pre-push-cargo-test` has a commit-time namesake —
/// and nothing else declared.
///
/// This is the whole ergonomic the built-in fallback buys: one line, no
/// second declaration, no `hook.skip`, no repeating `cargo test` in config
/// that amont already knows how to run.
///
/// The fixture has to look like a Rust repo, and that is not decoration: the
/// built-in's scope is `Scope::new(rust_tools::EXTS, &["Cargo.toml"])`, so
/// without a real `Cargo.toml` and a `.rs` file the check is never in the
/// push list at all — and a test asserting "it was skipped" would pass
/// having proved nothing, because there was nothing to skip.
///
/// The commit-side command is a stub. What is under test is the pairing, not
/// cargo.
fn builtin_paired_repo() -> (Repo, String) {
    let r = Repo::new();
    r.stage("gate.js", "require('fs').appendFileSync('gate.log','x')\n");
    r.stage(
        "Cargo.toml",
        "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    );
    r.stage("src/lib.rs", "pub fn f() {}\n");
    r.commit("chore: base");
    let base = head(&r);
    // ONLY the commit-time half. The push side is amont's own built-in.
    r.stage(
        "amont.conf",
        "pre-commit  cargo-test  *.rs  block  node gate.js\n",
    );
    r.commit("chore: the commit-time half");
    trust_and_install(&r);
    (r, base)
}

/// The built-in push gate defers to a commit-time declaration of its short
/// name — the case that was structurally impossible before, because the
/// pairing only ever looked at `manifest.externals` and built-ins are never
/// in there.
#[test]
fn a_builtin_push_gate_is_gated_by_its_commit_time_namesake() {
    if missing("node") {
        return;
    }
    let (r, base) = builtin_paired_repo();
    r.stage("src/added.rs", "pub fn g() {}\n");
    let out = r.git(&["commit", "-q", "-m", "feat: through the gate"]);
    assert!(out.status.success(), "verified commit failed");
    assert_eq!(runs(&r), 1, "the commit-time side ran once");

    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    // Two substrings, not one: `highlight` wraps the name in SGR codes, so
    // the name and the sentence are never contiguous in real output. The
    // existing declared-pair tests only assert the sentence — asserting the
    // NAME too is the point here, since a built-in pairing under the wrong
    // name is exactly the silent failure this feature risks.
    assert!(
        out.contains("cargo-test") && out.contains("gated at commit instead"),
        "the built-in must defer to its namesake, by name: {out}"
    );
    // Every pushed commit is stamped: the plain line, with no tip line, keeps
    // meaning exactly that (ADR-0019).
    assert!(
        !out.contains("the code at tip"),
        "a fully stamped range names no tip: {out}"
    );
}

/// And it is fail-closed: a `--no-verify` commit carries no stamp, so the
/// built-in comes back rather than trusting a declaration nobody honoured.
///
/// This is the assertion that matters. If it ever goes quiet, the pair is
/// deferring to a check that never ran, which is worse than having no pair.
#[test]
fn an_unstamped_commit_brings_the_builtin_back() {
    if missing("node") {
        return;
    }
    let (r, base) = builtin_paired_repo();
    // ADR-0018: this test covers the inline path, where the gate runs in the push.
    r.git(&["config", "amont.pushGates", "inline"]);
    r.stage("src/added.rs", "pub fn g() {}\n");
    r.commit("feat: dodge the gate"); // Repo::commit IS --no-verify
    assert_eq!(runs(&r), 0, "nothing ran at commit");

    let (_code, out) = push_out(&r, &base, &head(&r));
    // Not asserting the exit code: with no stamp the built-in really does
    // run `cargo test` here, and what that says about a fixture crate is not
    // this test's business. The contract under test is that it came back and
    // said why.
    assert!(
        out.contains("carr") && out.contains("no record of it"),
        "an unstamped commit must bring the built-in back, out loud: {out}"
    );
}

/// A commit-time declaration stands in for the push gate of its name, and the
/// push gate acts on more files than the declaration's patterns: `rustfmt.toml`
/// (like `Cargo.lock`) is `pre-push-cargo-test`'s, and `*.rs` is all the
/// declaration names. A commit changing only such a file fired neither, earned
/// no stamp, and the push silently ran the whole suite again (2026-11-10, a
/// six-crate land that held for two hours). The declaration now fires on its
/// twin's files too. `rustfmt.toml` here, not `Cargo.lock`, because a lockfile
/// push also owes `pre-push-audit-rust`, a gate of its own.
#[test]
fn a_commit_that_only_changes_its_twins_other_files_is_tested_and_stamped() {
    if missing("node") {
        return;
    }
    let (r, base) = builtin_paired_repo();
    r.stage("rustfmt.toml", "max_width = 100\n");
    let commit = r.git(&["commit", "-q", "-m", "chore: tune rustfmt"]);
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&commit.stdout),
        String::from_utf8_lossy(&commit.stderr)
    );
    assert!(commit.status.success(), "hooked commit failed: {said}");
    assert_eq!(
        runs(&r),
        1,
        "the rustfmt.toml commit ran the commit-time gate"
    );
    assert!(
        said.contains("rustfmt.toml is read by pre-push-cargo-test — testing at commit"),
        "a commit that tests only through its twin's files says why: {said}"
    );

    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("cargo-test") && out.contains("gated at commit instead"),
        "the stamped rustfmt.toml commit must gate the push: {out}"
    );
}

/// The blocker the plan's panel found: widening only the coverage question
/// would let an UNSTAMPED twin-only commit (made before the upgrade, or with
/// `--no-verify`) count as "not in the pair's scope", so the push said gated
/// over a suite that ran nowhere. The per-commit stamp count uses the widened
/// scope too.
#[test]
fn an_unstamped_twin_only_commit_still_makes_the_push_run_the_suite() {
    if missing("node") {
        return;
    }
    let (r, base) = builtin_paired_repo();
    r.git(&["config", "amont.pushGates", "inline"]);
    r.stage("rustfmt.toml", "max_width = 100\n");
    r.commit("chore: dodge the gate"); // Repo::commit IS --no-verify
    assert_eq!(runs(&r), 0, "nothing ran at commit");

    let (_code, out) = push_out(&r, &base, &head(&r));
    assert!(
        !out.contains("gated at commit instead"),
        "an unstamped rustfmt.toml commit was reported as gated: {out}"
    );
    assert!(
        out.contains("no record of it"),
        "the push must say the commit carries no record: {out}"
    );
}

/// A `files` declaration is handed the matched paths as arguments, and
/// handing a linter `Cargo.lock` is wrong, so it is never widened.
#[test]
fn a_files_pair_is_not_widened_to_its_twins_files() {
    if missing("node") {
        return;
    }
    let r = Repo::new();
    r.git(&["config", "amont.pushGates", "inline"]);
    r.stage("gate.js", "require('fs').appendFileSync('gate.log','x')\n");
    r.stage(
        "Cargo.toml",
        "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    );
    r.stage("src/lib.rs", "pub fn f() {}\n");
    r.commit("chore: base");
    let base = head(&r);
    r.stage(
        "amont.conf",
        "pre-commit  cargo-test  *.rs  block  files node gate.js\n",
    );
    r.commit("chore: the commit-time half");
    trust_and_install(&r);

    r.stage("rustfmt.toml", "max_width = 100\n");
    hooked_commit(&r, "chore: tune rustfmt");
    assert_eq!(
        runs(&r),
        0,
        "a files declaration does not fire on rustfmt.toml"
    );

    let (_code, out) = push_out(&r, &base, &head(&r));
    assert!(
        !out.contains("gated at commit instead"),
        "a narrow files pair cannot gate a rustfmt.toml push: {out}"
    );
    assert!(
        out.contains("cargo-test at commit covers *.rs, not rustfmt.toml — running it here"),
        "the push says what the declaration misses: {out}"
    );
    assert!(
        out.contains("a files declaration is not widened: add rustfmt.toml to amont.conf line 1"),
        "and which line to edit: {out}"
    );
}

/// A warn declaration vouches for nothing, so widening it would only add a
/// commit-time run nobody can rely on.
#[test]
fn a_warn_pair_is_not_widened_to_its_twins_files() {
    if missing("node") {
        return;
    }
    let r = Repo::new();
    r.git(&["config", "amont.pushGates", "inline"]);
    r.stage("gate.js", "require('fs').appendFileSync('gate.log','x')\n");
    r.stage(
        "Cargo.toml",
        "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    );
    r.stage("src/lib.rs", "pub fn f() {}\n");
    r.commit("chore: base");
    let base = head(&r);
    r.stage(
        "amont.conf",
        "pre-commit  cargo-test  *.rs  warn  node gate.js\n",
    );
    r.commit("chore: the commit-time half");
    trust_and_install(&r);

    r.stage("rustfmt.toml", "max_width = 100\n");
    hooked_commit(&r, "chore: tune rustfmt");
    assert_eq!(runs(&r), 0, "a warn declaration is not widened");

    let (_code, out) = push_out(&r, &base, &head(&r));
    assert!(
        out.contains("cargo-test at commit is a warn check, which vouches for nothing"),
        "a warn pair says so, then the push runs: {out}"
    );
}

/// The severity a repository's settings give the declaration decides it, not
/// the word on its line: a warn line overridden to block widens, and the
/// declaration then fires on a file its twin reads.
#[test]
fn a_warn_pair_overridden_to_block_is_widened() {
    if missing("node") {
        return;
    }
    let r = Repo::new();
    r.git(&["config", "amont.severity.pre-commit-cargo-test", "block"]);
    r.stage("gate.js", "require('fs').appendFileSync('gate.log','x')\n");
    r.stage(
        "Cargo.toml",
        "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    );
    r.stage("src/lib.rs", "pub fn f() {}\n");
    r.commit("chore: base");
    r.stage(
        "amont.conf",
        "pre-commit  cargo-test  *.rs  warn  node gate.js\n",
    );
    r.commit("chore: the commit-time half");
    trust_and_install(&r);

    r.stage("rustfmt.toml", "max_width = 100\n");
    hooked_commit(&r, "chore: tune rustfmt");
    assert_eq!(
        runs(&r),
        1,
        "the overridden-to-block declaration is widened"
    );
}

/// `list` says what a widened row also fires on, in a trailing field, and
/// leaves the columns before it as the declaration wrote them.
#[test]
fn list_shows_what_a_declaration_also_fires_on_because_of_its_twin() {
    if missing("node") {
        return;
    }
    let (r, _base) = builtin_paired_repo();
    let json = r.run(&["list", "--json", "--stage", "pre-commit"]).stdout;
    let row = json
        .split("{\"id\":\"pre-commit-cargo-test\"")
        .nth(1)
        .and_then(|rest| rest.split("{\"id\":").next())
        .unwrap_or_else(|| panic!("no pre-commit-cargo-test row: {json}"));
    assert!(
        row.contains("\"scope_files\":[\".rs\"]"),
        "scope_files stays what the line says: {row}"
    );
    assert!(
        row.contains("\"scope_from_twin\":[")
            && row.contains("Cargo.lock")
            && row.contains("rustfmt.toml"),
        "the twin's files are reported beside it: {row}"
    );
    let text = r.run(&["list", "--stage", "pre-commit"]).stdout;
    assert!(
        text.contains("also fires on") && text.contains("Cargo.lock"),
        "the text view says so too, with every path when piped: {text}"
    );
}

/// And the other direction: a block line a repository's settings downgrade to
/// warn vouches for nothing any more, so it is not widened.
#[test]
fn a_block_pair_overridden_to_warn_is_not_widened() {
    if missing("node") {
        return;
    }
    let r = Repo::new();
    r.git(&["config", "amont.severity.pre-commit-cargo-test", "warn"]);
    r.stage("gate.js", "require('fs').appendFileSync('gate.log','x')\n");
    r.stage(
        "Cargo.toml",
        "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    );
    r.stage("src/lib.rs", "pub fn f() {}\n");
    r.commit("chore: base");
    r.stage(
        "amont.conf",
        "pre-commit  cargo-test  *.rs  block  node gate.js\n",
    );
    r.commit("chore: the commit-time half");
    trust_and_install(&r);

    r.stage("rustfmt.toml", "max_width = 100\n");
    hooked_commit(&r, "chore: tune rustfmt");
    assert_eq!(
        runs(&r),
        0,
        "a block line downgraded to warn is not widened"
    );
}

/// The built-in twin's repository with one commit-time line, for the tests of
/// why a pair did not vouch.
fn builtin_repo_with(conf: &str) -> (Repo, String) {
    let r = Repo::new();
    r.git(&["config", "amont.pushGates", "inline"]);
    r.stage("gate.js", "require('fs').appendFileSync('gate.log','x')\n");
    r.stage(
        "Cargo.toml",
        "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    );
    r.stage("src/lib.rs", "pub fn f() {}\n");
    r.commit("chore: base");
    let base = head(&r);
    r.stage("amont.conf", conf);
    r.commit("chore: the commit-time half");
    trust_and_install(&r);
    (r, base)
}

/// `hook.skip` switches the commit-time declaration off, so it vouches for
/// nothing; the push says so instead of running in silence.
#[test]
fn a_skipped_pair_says_it_is_skipped() {
    if missing("node") {
        return;
    }
    let (r, base) = builtin_repo_with("pre-commit  cargo-test  *.rs  block  node gate.js\n");
    r.git(&["config", "--add", "hook.skip", "pre-commit-cargo-test"]);
    r.stage("src/added.rs", "pub fn g() {}\n");
    hooked_commit(&r, "feat: skipped at commit");
    assert_eq!(runs(&r), 0, "hook.skip: nothing ran at commit");
    let (_code, out) = push_out(&r, &base, &head(&r));
    assert!(
        out.contains("cargo-test at commit is skipped here (hook.skip) — running it here"),
        "{out}"
    );
}

/// A commit-time declaration whose opt-in marker the repository lacks never
/// ran, so it has nothing to vouch with, and the push says which marker.
#[test]
fn a_pair_that_is_not_opted_in_names_its_marker() {
    if missing("node") {
        return;
    }
    let (r, base) = builtin_repo_with("pre-commit  cargo-test  *.rs+marker  block  node gate.js\n");
    r.stage("src/added.rs", "pub fn g() {}\n");
    hooked_commit(&r, "feat: no marker here");
    assert_eq!(runs(&r), 0, "not opted in: nothing ran at commit");
    let (_code, out) = push_out(&r, &base, &head(&r));
    assert!(
        out.contains("cargo-test at commit is not opted in here (needs marker) — running it here"),
        "{out}"
    );
}

/// A declaration whose program cannot be found has no identity to stamp, so it
/// vouches for nothing, and the push says so.
#[test]
fn a_pair_whose_program_is_missing_says_it_cannot_be_identified() {
    if missing("node") {
        return;
    }
    let (r, base) =
        builtin_repo_with("pre-commit  cargo-test  *.rs  block  no-such-program-for-this-test\n");
    r.stage("src/added.rs", "pub fn g() {}\n");
    r.commit("feat: nothing could run"); // Repo::commit IS --no-verify
    let (_code, out) = push_out(&r, &base, &head(&r));
    assert!(
        out.contains("cargo-test at commit cannot be identified (its program was not found)"),
        "{out}"
    );
}

/// With no same-named commit-time declaration there is nothing to explain, and
/// the push stays as quiet as it always was.
#[test]
fn a_gate_with_no_pair_says_nothing_about_one() {
    if missing("node") {
        return;
    }
    let (r, base) = builtin_repo_with("pre-commit  other  *.md  block  node gate.js\n");
    r.stage("src/added.rs", "pub fn g() {}\n");
    r.commit("feat: unrelated");
    let (_code, out) = push_out(&r, &base, &head(&r));
    assert!(
        !out.contains("at commit covers") && !out.contains("vouches for nothing"),
        "{out}"
    );
}

/// A declared pre-push twin is found wherever it sits in the file, and its
/// scope is what the commit-time line widens to.
#[test]
fn a_declared_twin_later_in_the_file_widens_the_commit_time_line() {
    if missing("node") {
        return;
    }
    let r = Repo::new();
    r.stage("gate.js", "require('fs').appendFileSync('gate.log','x')\n");
    r.commit("chore: base");
    let base = head(&r);
    r.stage(
        "amont.conf",
        "pre-commit  check  *.txt  block  node gate.js\n\
         pre-push    check  *.txt,*.md  block  node gate.js\n",
    );
    r.commit("chore: the pair");
    trust_and_install(&r);

    r.stage("notes.md", "words\n");
    hooked_commit(&r, "docs: a note");
    assert_eq!(runs(&r), 1, "the markdown commit ran the commit-time gate");

    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("gated at commit instead"),
        "the stamped markdown commit must gate the push: {out}"
    );
}

/// Run the pre-push dispatcher over several ref lines at once.
fn push_lines(r: &Repo, lines: &str) -> (i32, String) {
    let mut child = crate::common::amont_cmd()
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-push")
        .current_dir(&r.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(lines.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("wait");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// A commit through the installed hooks, so the commit-time side runs and
/// stamps the tree.
fn hooked_commit(r: &Repo, msg: &str) {
    let out = r.git(&["commit", "-q", "-m", msg]);
    assert!(
        out.status.success(),
        "hooked commit failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The paired repository with an existing plan file in its base, for the
/// canonical route: a tree that differs from a stamped one only in the
/// CONTENT of an existing plan shares its canonical key (ADR-0012).
fn paired_repo_with_plan(conf: &str) -> (Repo, String) {
    let r = Repo::new();
    r.git(&["config", "amont.pushGates", "inline"]);
    r.stage("gate.js", "require('fs').appendFileSync('gate.log','x')\n");
    r.stage("docs/plans/p.md", "plan\n");
    r.commit("chore: base");
    let base = head(&r);
    r.stage("amont.conf", conf);
    r.commit("chore: the pair");
    trust_and_install(&r);
    (r, base)
}

const PAIR: &str = "pre-commit  check  *.txt  block  node gate.js\n\
                    pre-push    check  *.txt  block  node gate.js\n";

/// relais #385's shape: a hookless commit, a verified commit, then a plan
/// record. The tip's tree carries no note of its own, but it differs from the
/// verified tree only in a plan's content, so its canonical key holds the
/// pair token and the push skips the suite.
#[test]
fn a_plan_only_tip_over_a_stamped_commit_is_gated_despite_an_unstamped_one() {
    if missing("node") {
        return;
    }
    let (r, base) = paired_repo_with_plan(PAIR);
    r.stage("a.txt", "hookless\n");
    r.commit("feat: like relais commit-tree"); // no hooks, no stamp
    r.stage("b.txt", "verified\n");
    hooked_commit(&r, "feat: through the gate");
    assert_eq!(runs(&r), 1, "the commit-time side ran once");
    r.stage("docs/plans/p.md", "plan, done\n");
    hooked_commit(&r, "docs(plan): p, done");
    assert_eq!(runs(&r), 1, "a plan edit does not fire a *.txt gate");

    let tip = head(&r);
    let (code, out) = push_out(&r, &base, &tip);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("gated at commit instead"), "{out}");
    assert!(
        out.contains(&format!("the code at tip {} passed at commit", &tip[..8])),
        "{out}"
    );
    assert_eq!(runs(&r), 1, "the push-side twin must not run: {out}");
}

/// The fallback: a verified commit, then an unstamped one that touches the
/// scope at the tip. The tip vouches for nothing, so the per-commit rule runs
/// the twin and says why.
#[test]
fn an_unstamped_scoped_tip_over_a_stamped_commit_runs() {
    if missing("node") {
        return;
    }
    let (r, base) = paired_repo_with_plan(PAIR);
    r.stage("a.txt", "verified\n");
    hooked_commit(&r, "feat: through the gate");
    r.stage("b.txt", "dodged\n");
    r.commit("feat: dodge the gate");
    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("gated at commit instead"), "{out}");
    assert!(out.contains("no record of it"), "{out}");
    assert_eq!(runs(&r), 2, "the twin ran: {out}");
}

/// A `files` pair is handed only its commit's matched paths: its stamp says
/// nothing about the rest of the tree, so a stamped tip cannot cover an
/// unstamped commit below it.
#[test]
fn a_files_pair_never_lets_its_tip_vouch_for_the_range() {
    if missing("node") {
        return;
    }
    let (r, base) = paired_repo_with_plan(
        "pre-commit  check  *.txt  block  files node gate.js\n\
         pre-push    check  *.txt  block  node gate.js\n",
    );
    r.stage("a.txt", "dodged\n");
    r.commit("feat: dodge the gate");
    r.stage("b.txt", "verified\n");
    hooked_commit(&r, "feat: through the gate");
    assert_eq!(runs(&r), 1, "the commit-time side ran on b.txt");
    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("gated at commit instead"), "{out}");
    assert!(out.contains("no record of it"), "{out}");
    assert_eq!(runs(&r), 2, "the twin ran: {out}");
}

/// The tip check sits after the coverage guard: when the push side would
/// fire on a file the pair's scope never judges, the pair vouches for
/// nothing (`NotPaired`), however well stamped the tip is.
#[test]
fn a_stamped_tip_cannot_cover_files_outside_the_pairs_scope() {
    if missing("node") {
        return;
    }
    let (r, base) = paired_repo_with_plan(
        "pre-commit  check  *.txt  block  node gate.js\n\
         pre-push    check  *     block  node gate.js\n",
    );
    r.stage("a.md", "outside the pair's scope\n");
    r.stage("b.txt", "verified\n");
    hooked_commit(&r, "feat: through the gate");
    assert_eq!(runs(&r), 1);
    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("gated at commit"), "{out}");
    assert_eq!(runs(&r), 2, "the push side ran on its own word: {out}");
    // And it says why, instead of running again in silence.
    assert!(
        out.contains("check at commit covers *.txt, not a.md"),
        "{out}"
    );
    assert!(
        out.contains("its push twin acts on every change: widen its scope in amont.conf line 1"),
        "{out}"
    );
}

/// Codex's case: a repository-owned gate script changed at the tip, outside
/// the trigger scope. The tip's commit-time gate judged nothing (`Inert`), so
/// the tip's tree carries no stamp, and its canonical key differs from the
/// verified tree's (the script is not a plan): an unstamped tip tree vouches
/// for nothing, and the unstamped scoped commit below brings the twin back.
#[test]
fn a_changed_gate_script_at_the_tip_does_not_vouch() {
    if missing("node") {
        return;
    }
    let (r, base) = paired_repo_with_plan(PAIR);
    r.stage("a.txt", "dodged\n");
    r.commit("feat: dodge the gate");
    r.stage("b.txt", "verified\n");
    hooked_commit(&r, "feat: through the gate");
    r.stage(
        "gate.js",
        "require('fs').appendFileSync('gate.log','x') // edited\n",
    );
    hooked_commit(&r, "chore: edit the gate");
    assert_eq!(runs(&r), 1, "only the verified commit ran the gate");
    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("gated at commit instead"), "{out}");
    assert!(out.contains("no record of it"), "{out}");
    assert_eq!(runs(&r), 2, "the twin ran: {out}");
}

/// The declared-input route (ADR-0011): with an `attest-inputs` spec, a
/// tree's stamp travels by the fingerprint of the gate's declared inputs. A
/// tip that changes only a declared input outside the trigger scope has a new
/// fingerprint, so it vouches for nothing, and the unstamped scoped commit
/// below brings the twin back.
#[test]
fn a_changed_declared_input_at_the_tip_does_not_vouch() {
    if missing("node") {
        return;
    }
    let r = Repo::new();
    r.git(&["config", "amont.pushGates", "inline"]);
    r.stage("gate.js", "require('fs').appendFileSync('gate.log','x')\n");
    r.stage("data.in", "one\n");
    r.stage(
        ".github/attest-inputs",
        "check *.txt data.in gate.js amont.conf\n",
    );
    r.commit("chore: base");
    let base = head(&r);
    r.stage("amont.conf", PAIR);
    r.commit("chore: the pair");
    trust_and_install(&r);

    r.stage("a.txt", "dodged\n");
    r.commit("feat: dodge the gate");
    r.stage("b.txt", "verified\n");
    hooked_commit(&r, "feat: through the gate");
    assert_eq!(runs(&r), 1);
    r.stage("data.in", "two\n");
    hooked_commit(&r, "chore: change a declared input");
    assert_eq!(runs(&r), 1, "data.in does not fire a *.txt gate");

    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("gated at commit instead"), "{out}");
    assert!(out.contains("no record of it"), "{out}");
    assert_eq!(runs(&r), 2, "the twin ran: {out}");
}

/// Two refs: one covered by its stamped tip, one whose tip dodged the gate.
/// The covered ref adds nothing; the other is counted alone.
#[test]
fn two_refs_one_covered_one_not_runs_for_the_one() {
    if missing("node") {
        return;
    }
    let (r, base) = paired_repo_with_plan(PAIR);
    // Both branches start after the pair is declared: a checkout without
    // `amont.conf` declares no gate at all.
    let pair = head(&r);
    r.git(&["checkout", "-q", "-b", "feat/one"]);
    r.stage("a.txt", "dodged\n");
    r.commit("feat: dodge the gate");
    r.stage("b.txt", "verified\n");
    hooked_commit(&r, "feat: through the gate");
    let one = head(&r);
    r.git(&["checkout", "-q", "-b", "feat/two", &pair]);
    r.stage("c.txt", "dodged too\n");
    r.commit("feat: dodge again");
    let two = head(&r);
    let lines = format!(
        "refs/heads/feat/one {one} refs/heads/feat/one {base}\n\
         refs/heads/feat/two {two} refs/heads/feat/two {base}\n"
    );
    let (code, out) = push_lines(&r, &lines);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("gated at commit instead"), "{out}");
    assert!(
        out.contains("1 pushed commit carries no record of it"),
        "only the uncovered ref counts: {out}"
    );
}
