//! Checks a repository declares for itself, in `amont.conf`.
//!
//! Unit tests cover the parser. These cover the part a parser cannot: that a
//! line in a committed file actually causes a command to run, actually blocks a
//! commit, and is governed by the same `hook.skip` and severity machinery as a
//! built-in — a repository that can add a check it cannot disable would be a
//! worse deal than not being able to add one.

mod common;
use common::Repo;
use std::io::Write;
use std::process::{Command, Stdio};

/// A committed script that records having run and exits how it is told.
///
/// Deliberately not `sh -c`: the manifest executes argv directly, and a fixture
/// that reached for a shell would be testing a shell rather than the feature.
#[cfg(unix)]
fn probe(r: &Repo, name: &str, exit: i32) {
    let body = format!("#!/bin/sh\necho probe-{name} ran\nexit {exit}\n");
    r.stage(name, &body);
    let p = r.path(name);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    // Re-stage AFTER the chmod: the mode is part of what is staged, and the
    // pre-commit stage now resets the tree to the index — so a probe chmod'd
    // but not re-added is correctly restored to 644 and cannot execute. Which
    // is the right behaviour: a commit would not carry the bit either.
    r.git(&["add", name]);
}

#[cfg(not(unix))]
fn probe(r: &Repo, name: &str, exit: i32) {
    // `.cmd` is directly executable on Windows without a shebang.
    let body = format!("@echo probe-{name} ran\r\n@exit /b {exit}\r\n");
    r.stage(name, &body);
}

#[cfg(unix)]
const PROBE: &str = "probe.sh";
#[cfg(not(unix))]
const PROBE: &str = "probe.cmd";

/// Write the manifest AND trust it.
///
/// Trust is a separate decision (see `an_untrusted_manifest_does_not_run`), so
/// every test about what a declared check does has to make it first — otherwise
/// they would all be testing the trust gate instead.
fn manifest(r: &Repo, body: &str) {
    r.stage("amont.conf", body);
    trust(r);
}

fn trust(r: &Repo) {
    let out = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("trust")
        .current_dir(&r.dir)
        .output()
        .expect("amont trust");
    assert!(out.status.success(), "could not trust the manifest");
}

fn pre_push(r: &Repo) -> (i32, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_amont"))
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
        .expect("stdin")
        .write_all(b"refs/heads/feat/x aaa refs/heads/feat/x bbb\n")
        .expect("write");
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

/// A push of `remote..local`, as git presents it on stdin.
fn push_range(r: &Repo, remote: &str, local: &str) -> (i32, String) {
    let line = format!("refs/heads/feat/x {local} refs/heads/feat/x {remote}\n");
    let mut child = Command::new(env!("CARGO_BIN_EXE_amont"))
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
        .expect("stdin")
        .write_all(line.as_bytes())
        .expect("write");
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

#[test]
fn a_declared_check_runs_and_can_block_the_commit() {
    let r = Repo::new();
    probe(&r, PROBE, 1);
    manifest(
        &r,
        &format!("pre-commit  shellcheck  *  block  ./{PROBE}\n"),
    );

    let run = r.hook("pre-commit", &[]);
    assert!(
        run.says("probe-"),
        "the command never ran:\n{}",
        run.output()
    );
    assert!(!run.passed(), "a blocking failure must stop the commit");
    assert!(run.says("shellcheck"), "by name:\n{}", run.output());
}

#[test]
fn a_declared_check_that_passes_lets_the_commit_through() {
    let r = Repo::new();
    probe(&r, PROBE, 0);
    manifest(
        &r,
        &format!("pre-commit  shellcheck  *  block  ./{PROBE}\n"),
    );

    let run = r.hook("pre-commit", &[]);
    assert!(run.says("probe-"), "{}", run.output());
    assert!(run.passed(), "{}", run.output());
}

/// The severity column is the author's choice, and it has to mean the same
/// thing it means for a built-in.
#[test]
fn a_declared_check_can_choose_not_to_block() {
    let r = Repo::new();
    probe(&r, PROBE, 1);
    manifest(&r, &format!("pre-commit  smoke  *  warn  ./{PROBE}\n"));

    let run = r.hook("pre-commit", &[]);
    assert!(run.says("probe-"), "it must still RUN:\n{}", run.output());
    assert!(run.passed(), "warn must not block:\n{}", run.output());
    assert!(run.says("set to warn"), "{}", run.output());
}

/// Scope is evaluated against the staged files, exactly as a built-in's is.
#[test]
fn scope_keeps_a_declared_check_out_of_unrelated_commits() {
    let r = Repo::new();
    probe(&r, PROBE, 1);
    manifest(
        &r,
        &format!("pre-commit  shellcheck  *.rs  block  ./{PROBE}\n"),
    );

    // Nothing staged ends in `.rs` — the probe and the manifest do not.
    let run = r.hook("pre-commit", &[]);
    assert!(
        !run.says("probe-"),
        "it ran on a commit it does not apply to:\n{}",
        run.output()
    );
    assert!(run.passed());

    r.stage("src/main.rs", "fn main() {}\n");
    let run = r.hook("pre-commit", &[]);
    assert!(run.says("probe-"), "it must fire now:\n{}", run.output());
}

/// A repository that can add a check it cannot disable is a worse deal than one
/// that cannot add checks at all.
#[test]
fn hook_skip_and_severity_govern_a_declared_check() {
    let r = Repo::new();
    probe(&r, PROBE, 1);
    manifest(
        &r,
        &format!("pre-commit  shellcheck  *  block  ./{PROBE}\n"),
    );
    assert!(!r.hook("pre-commit", &[]).passed(), "baseline");

    r.git(&["config", "amont.severity.shellcheck", "warn"]);
    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "severity override ignored:\n{}", run.output());
    assert!(run.says("probe-"), "downgraded, not disabled");

    r.git(&["config", "--unset", "amont.severity.shellcheck"]);
    r.git(&["config", "hook.skip", "shellcheck"]);
    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.output());
    assert!(
        !run.says("probe-"),
        "a skipped check must not run:\n{}",
        run.output()
    );
    assert!(
        run.says("skipped by"),
        "and the skip must be announced:\n{}",
        run.output()
    );
}

/// A command that cannot be STARTED has judged nothing. Reporting that as a
/// lint failure sends someone hunting for an error that does not exist.
#[test]
fn a_missing_command_is_a_gap_not_a_failure() {
    let r = Repo::new();
    manifest(
        &r,
        "pre-commit  shellcheck  *  block  ./no-such-binary-c8f2\n",
    );

    let run = r.hook("pre-commit", &[]);
    assert!(
        run.passed(),
        "an absent tool must not block:\n{}",
        run.output()
    );
    assert!(
        run.says("could not run"),
        "and must be reported as a gap:\n{}",
        run.output()
    );
}

/// The rule the module commits to: a line nobody can parse is a check that is
/// not running, and silence there is the failure `Outcome` exists to name.
#[test]
fn a_malformed_line_is_reported_on_every_commit() {
    let r = Repo::new();
    manifest(&r, "pre-commit  shellcheck  *  LOUD  ./probe.sh\n");

    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "it must not block:\n{}", run.output());
    assert!(run.says("shellcheck"), "{}", run.output());
    assert!(
        run.says("severity"),
        "say what was wrong:\n{}",
        run.output()
    );
    assert!(run.says("line 1"), "and where:\n{}", run.output());
}

/// Externals are appended, never interleaved: a third-party command must not be
/// able to delay `pre-push-branch-protect`.
#[test]
fn a_declared_pre_push_check_runs_after_the_built_ins() {
    let r = Repo::new();
    probe(&r, PROBE, 0);
    manifest(&r, &format!("pre-push  smoke  *  block  ./{PROBE}\n"));
    r.commit("init");
    // A branch name the built-in pattern check accepts, so the only thing that
    // can end the chain early is the one this test is about.
    r.git(&["checkout", "-q", "-b", "feat/x"]);

    let (code, out) = pre_push(&r);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("probe-"), "the declared check must run: {out}");

    // With a branch name the built-in rejects, it blocks first and the declared
    // check never gets a turn — which is the ordering guarantee, observed.
    //
    // The ref line carries an all-zero REMOTE oid: `branch-pattern` judges the
    // ref being pushed rather than the branch checked out, and a non-zero
    // remote oid means the branch already exists on the server and its name was
    // authorised long ago. The old line said `bbb`, which under the corrected
    // check is "already there, nothing to validate".
    r.git(&["checkout", "-q", "-b", "nonsense-branch-name"]);
    let mut child = Command::new(env!("CARGO_BIN_EXE_amont"))
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
        .expect("stdin")
        .write_all(
            b"refs/heads/nonsense aaa refs/heads/nonsense-branch-name \
              0000000000000000000000000000000000000000\n",
        )
        .expect("write");
    let out = child.wait_with_output().expect("wait");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_ne!(out.status.code(), Some(0), "{text}");
    assert!(
        !text.contains("probe-"),
        "a declared check ran despite a built-in blocking first: {text}"
    );
}

/// At push time nothing is staged, so a pre-push scope evaluated against the
/// index would match nothing and the check would never run. The range being
/// PUSHED is what it has to be judged against.
#[test]
fn a_pre_push_scope_is_judged_against_the_pushed_range() {
    let r = Repo::new();
    probe(&r, PROBE, 0);
    manifest(&r, &format!("pre-push  smoke  *.rs  block  ./{PROBE}\n"));
    r.commit("init");
    r.git(&["checkout", "-q", "-b", "feat/x"]);
    let base = String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();

    r.stage("notes.md", "nothing to compile\n");
    r.commit("docs only");
    let docs = String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    let (code, out) = push_range(&r, &base, &docs);
    assert_eq!(code, 0, "{out}");
    assert!(
        !out.contains("probe-"),
        "a docs-only push must not fire a `*.rs` check: {out}"
    );

    r.stage("src/main.rs", "fn main() {}\n");
    r.commit("some rust");
    let rust = String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    let (code, out) = push_range(&r, &docs, &rust);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("probe-"),
        "a push containing Rust must fire it: {out}"
    );
}

/// A file can be touched by an intermediate commit and reverted by a later
/// one in the SAME push, netting to no difference between the two endpoints
/// of the range — `diff-tree base..tip` is a straight two-tree comparison,
/// not a walk, so a scope keyed off that alone would see nothing to gate on
/// and never fire. The push still carries a commit that touched a `.rs`
/// file, and the check must still see it.
#[test]
fn a_reverted_change_within_the_pushed_range_still_counts() {
    let r = Repo::new();
    probe(&r, PROBE, 0);
    manifest(&r, &format!("pre-push  smoke  *.rs  block  ./{PROBE}\n"));
    r.commit("init");
    r.git(&["checkout", "-q", "-b", "feat/z"]);
    let base = String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();

    r.stage("src/temp.rs", "fn temp() {}\n");
    r.commit("add a rust file");
    r.git(&["rm", "-q", "src/temp.rs"]);
    r.commit("remove it again");
    let tip = String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();

    let (code, out) = push_range(&r, &base, &tip);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("probe-"),
        "an intermediate commit touched a .rs file even though the push \
         nets to no .rs change between its endpoints: {out}"
    );
}

/// A merge commit's own resolution can introduce a file neither parent
/// touched — resolving a conflict, or just adding something, as part of the
/// merge itself. `diff-tree` shows NOTHING for a merge commit at all unless
/// asked with `-m` — confirmed empirically, not merely documented — so
/// without it this file is invisible to a scope-gated check even though it
/// is genuinely part of what is being pushed.
#[test]
fn a_file_touched_only_by_a_merge_resolution_still_counts() {
    let r = Repo::new();
    probe(&r, PROBE, 0);
    manifest(&r, &format!("pre-push  smoke  *.rs  block  ./{PROBE}\n"));
    r.commit("init");
    let base = String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();

    r.git(&["checkout", "-q", "-b", "feat/a"]);
    r.stage("a.txt", "a\n");
    r.commit("add a");

    r.git(&["checkout", "-q", "-b", "feat/b", base.as_str()]);
    r.stage("b.txt", "b\n");
    r.commit("add b");

    // Two branches touching different files merge cleanly; the `.rs` file is
    // added ON TOP, as part of the merge commit, never on either branch.
    r.git(&["merge", "-q", "--no-ff", "--no-commit", "feat/a"]);
    r.stage("src/resolution_only.rs", "fn resolved() {}\n");
    r.commit("merge feat/a, plus a resolution-only rust file");
    let tip = String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();

    let (code, out) = push_range(&r, &base, &tip);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("probe-"),
        "a file introduced only by the merge commit itself must still be seen: {out}"
    );
}

/// The whole point of the trust gate: a manifest nobody accepted does not run,
/// and is not silently dropped either.
#[test]
fn an_untrusted_manifest_does_not_run() {
    let r = Repo::new();
    probe(&r, PROBE, 1);
    // NOT `manifest()` — that trusts it. This is the cloned-repo case.
    r.stage(
        "amont.conf",
        &format!("pre-commit  audit  *  block  ./{PROBE}\n"),
    );

    let run = r.hook("pre-commit", &[]);
    assert!(
        !run.says("probe-"),
        "an untrusted manifest executed its command:\n{}",
        run.output()
    );
    assert!(
        run.passed(),
        "and it must not block either:\n{}",
        run.output()
    );
    // Reported, not dropped — the name stays visible and so does the reason.
    assert!(run.says("audit"), "{}", run.output());
    assert!(
        run.says("trust"),
        "must say how to accept it:\n{}",
        run.output()
    );
    assert!(run.says("could not run"), "{}", run.output());

    // And after trusting, the same manifest runs.
    trust(&r);
    let run = r.hook("pre-commit", &[]);
    assert!(
        run.says("probe-"),
        "trusting did not enable it:\n{}",
        run.output()
    );
}

/// Consent is to CONTENT: a `git pull` that adds a command cannot inherit the
/// acceptance given to the file before it.
#[test]
fn editing_a_trusted_manifest_stops_it_running() {
    let r = Repo::new();
    probe(&r, PROBE, 0);
    manifest(&r, &format!("pre-commit  audit  *  block  ./{PROBE}\n"));
    assert!(r.hook("pre-commit", &[]).says("probe-"), "baseline");

    // As if a pull had added a line.
    r.stage(
        "amont.conf",
        &format!(
            "pre-commit  audit  *  block  ./{PROBE}\npre-commit  extra  *  block  ./{PROBE}\n"
        ),
    );
    let run = r.hook("pre-commit", &[]);
    assert!(
        !run.says("probe-"),
        "an edited manifest kept its trust:\n{}",
        run.output()
    );
    assert!(
        run.says("changed"),
        "must say WHICH happened:\n{}",
        run.output()
    );
}

/// No manifest is the normal case for ninety-six repositories, and it must cost
/// nothing and say nothing.
#[test]
fn a_repository_without_a_manifest_is_unaffected() {
    let r = Repo::new();
    r.stage("a.txt", "hello\n");
    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.output());
    assert!(
        !run.says("amont.conf"),
        "nothing should mention a file that does not exist:\n{}",
        run.output()
    );
}

/// The `files` marker hands the command exactly the paths its scope matched —
/// what a builtin gets, without a wrapper script re-deriving `git diff`.
#[cfg(unix)]
#[test]
fn the_files_marker_hands_the_command_its_matched_paths() {
    let r = Repo::new();
    let body = "#!/bin/sh\nprintf '%s\\n' \"$@\" > argv.txt\nexit 0\n";
    r.stage("probe.sh", body);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(r.path("probe.sh"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
    r.git(&["add", "probe.sh"]);
    r.stage("a.sh", "echo a\n");
    r.stage("b.rs", "fn main() {}\n");
    manifest(
        &r,
        "pre-commit  shellcheck  *.sh  block  files ./probe.sh\n",
    );

    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.output());
    let argv = std::fs::read_to_string(r.path("argv.txt")).expect("argv.txt");
    assert!(
        argv.lines().any(|l| l == "a.sh"),
        "matched path missing: {argv}"
    );
    assert!(
        !argv.lines().any(|l| l == "b.rs"),
        "a path outside the scope was handed over: {argv}"
    );
}

/// `$AMONT_FILES` always carries the matched set, marker or no marker — the
/// wrapper-script escape hatch that cannot diverge from the set the gate
/// judged.
#[cfg(unix)]
#[test]
fn amont_files_carries_the_matched_set_without_the_marker() {
    let r = Repo::new();
    let body = "#!/bin/sh\nprintf '%s' \"$AMONT_FILES\" > envfiles.txt\nexit 0\n";
    r.stage("probe.sh", body);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(r.path("probe.sh"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
    r.git(&["add", "probe.sh"]);
    r.stage("a.sh", "echo a\n");
    r.stage("b.rs", "fn main() {}\n");
    manifest(&r, "pre-commit  shellcheck  *.sh  block  ./probe.sh\n");

    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.output());
    let env = std::fs::read_to_string(r.path("envfiles.txt")).expect("envfiles.txt");
    assert!(env.lines().any(|l| l == "a.sh"), "{env}");
    assert!(!env.lines().any(|l| l == "b.rs"), "{env}");
}

/// A files-taking command with nothing staged has nothing to judge: running
/// it bare would make most linters error on an empty argv and block a commit
/// over nothing.
#[cfg(unix)]
#[test]
fn a_files_command_with_nothing_matched_does_not_run() {
    let r = Repo::new();
    probe(&r, PROBE, 1); // exit 1: if it runs, the stage fails loudly
    manifest(
        &r,
        &format!("pre-commit  smoke  *  block  files ./{PROBE}\n"),
    );
    r.commit("chore: park the fixture files");

    // Nothing staged at all now.
    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.output());
    assert!(
        !run.says("probe-"),
        "ran with an empty file list:\n{}",
        run.output()
    );
}

/// One hung tool must not hold the commit — and the parked unstaged work —
/// hostage. The budget kills it and the check FAILS, loudly, with the config
/// key that raises the budget named.
#[cfg(unix)]
#[test]
fn a_check_that_outlives_the_budget_is_killed_and_fails() {
    let r = Repo::new();
    // `exec`, so the sleep IS the spawned process rather than a grandchild:
    // the kill only reaches the direct child, and a grandchild inheriting the
    // harness's output pipe would hold this TEST hostage the way no real git
    // invocation can (git lends hooks its own stdio, it does not read a pipe).
    let body = "#!/bin/sh\nexec sleep 300\n";
    r.stage("slow.sh", body);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(r.path("slow.sh"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
    r.git(&["add", "slow.sh"]);
    manifest(&r, "pre-commit  slowpoke  *  block  ./slow.sh\n");
    r.git(&["config", "amont.timeout", "1"]);

    let started = std::time::Instant::now();
    let run = r.hook("pre-commit", &[]);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(60),
        "the deadline never fired"
    );
    assert!(!run.passed(), "a killed check must fail:\n{}", run.output());
    assert!(
        run.says("timed out") && run.says("amont.timeout"),
        "must say what happened and how to change the budget:\n{}",
        run.output()
    );
}

/// `tool <program> <version-substring>` — the pin that turns cross-machine
/// version skew from "the hook is flaky here" into a printed fact.
#[test]
fn a_mismatched_tool_pin_warns_and_never_blocks() {
    let r = Repo::new();
    r.stage("a.txt", "x\n");
    // git is the one tool every machine running this suite has.
    manifest(&r, "tool git 999.999.\n");
    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "skew must never block:\n{}", run.output());
    assert!(
        run.says("999.999.") && run.says("disagree with CI"),
        "the skew must be named:\n{}",
        run.output()
    );
}

#[test]
fn a_matching_tool_pin_is_silent() {
    let r = Repo::new();
    r.stage("a.txt", "x\n");
    manifest(&r, "tool git version\n"); // `git --version` prints "git version …"
    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.output());
    assert!(
        !run.says("disagree with CI") && !run.says("would not run") && !run.says("tool <program>"),
        "a satisfied pin must cost no output:\n{}",
        run.output()
    );
}

/// Verifying a pin means EXECUTING `<program> --version` for a program name
/// the repository chose — exactly the consent the trust model collects. An
/// untrusted manifest's pins are inert.
#[test]
fn an_untrusted_manifest_s_pins_run_nothing() {
    let r = Repo::new();
    r.stage("a.txt", "x\n");
    // Committed but never trusted; the program name would fail loudly if run.
    r.stage("amont.conf", "tool definitely-absent-tool-xyz 1.\n");
    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.output());
    assert!(
        !run.says("definitely-absent-tool-xyz 1."),
        "an untrusted pin must not be verified:\n{}",
        run.output()
    );
}

/// A pinned tool that is not runnable at all is its own message — the skew
/// warning would be nonsense with nothing to compare.
#[test]
fn a_pin_on_a_missing_tool_says_so() {
    let r = Repo::new();
    r.stage("a.txt", "x\n");
    manifest(&r, "tool definitely-absent-tool-xyz 1.\n");
    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.output());
    assert!(
        run.says("would not run"),
        "a missing pinned tool must be named:\n{}",
        run.output()
    );
}

/// A malformed pin nags like every other broken line — a line that cannot be
/// understood is not skipped.
#[test]
fn a_malformed_tool_pin_nags() {
    let r = Repo::new();
    r.stage("a.txt", "x\n");
    manifest(&r, "tool git\n");
    let run = r.hook("pre-commit", &[]);
    assert!(
        run.passed(),
        "a broken line warns, never blocks:\n{}",
        run.output()
    );
    assert!(
        run.says("tool <program> <version-substring>"),
        "the shape must be prescribed:\n{}",
        run.output()
    );
}

/// A bare scope token is an exact filename: the check fires when THAT file
/// is staged — anywhere in the tree — and never for a suffix look-alike.
#[cfg(unix)]
#[test]
fn a_filename_scope_fires_on_the_file_and_not_a_look_alike() {
    let r = Repo::new();
    probe(&r, PROBE, 1); // exit 1: firing is visible as a block
    manifest(
        &r,
        &format!("pre-commit  lockcheck  package.json  block  ./{PROBE}\n"),
    );

    // A suffix look-alike must not wake it.
    r.stage("not-package.json", "{}\n");
    let run = r.hook("pre-commit", &[]);
    assert!(
        !run.says("probe-") && run.passed(),
        "a look-alike woke a filename-scoped check:\n{}",
        run.output()
    );

    // The real thing, nested, does.
    r.stage("apps/web/package.json", "{}\n");
    let run = r.hook("pre-commit", &[]);
    assert!(
        run.says("probe-") && !run.passed(),
        "the named file did not wake the check:\n{}",
        run.output()
    );
}
