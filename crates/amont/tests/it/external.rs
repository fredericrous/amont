//! Checks a repository declares for itself, in `amont.conf`.
//!
//! Unit tests cover the parser, and `flows/declared.rs` walks what a declared
//! check does at commit time: trust, block and warn, scope, `hook.skip` and
//! severity, gaps, the `files` marker and tool pins. What stays here is what a
//! commit does not reach: pre-push ordering and the pushed range, judged on
//! ref lines written by hand, and the output modes (`amont.progress`,
//! `amont.quiet`).

use crate::common;
use common::Repo;
use std::io::Write;
use std::process::Stdio;

/// A committed script that records having run and exits how it is told.
///
/// Deliberately not `sh -c`: the manifest executes argv directly, and a fixture
/// that reached for a shell would be testing a shell rather than the feature.
#[cfg(unix)]
pub(crate) fn probe(r: &Repo, name: &str, exit: i32) {
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
pub(crate) fn probe(r: &Repo, name: &str, exit: i32) {
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
/// Trust is a separate decision (see `flows::declared`, step `untrusted`), so
/// every test about what a declared check does has to make it first — otherwise
/// they would all be testing the trust gate instead.
fn manifest(r: &Repo, body: &str) {
    r.stage("amont.conf", body);
    trust(r);
}

fn trust(r: &Repo) {
    let out = crate::common::amont_cmd()
        .arg("trust")
        .current_dir(&r.dir)
        .output()
        .expect("amont trust");
    assert!(out.status.success(), "could not trust the manifest");
}

fn pre_push(r: &Repo) -> (i32, String) {
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

/// `amont.progress false` is the escape hatch back to raw streaming — the
/// output is all still there, just without the one-block guarantee.
#[cfg(unix)]
#[test]
fn progress_off_restores_raw_streaming() {
    use std::os::unix::fs::PermissionsExt;
    let r = Repo::new();
    r.stage(
        "noisy.sh",
        "#!/bin/sh\necho noisy-one\necho noisy-two\nexit 0\n",
    );
    std::fs::set_permissions(r.path("noisy.sh"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
    r.git(&["add", "noisy.sh"]);
    manifest(&r, "pre-commit  noisy  *  warn  ./noisy.sh\n");
    r.git(&["config", "amont.progress", "false"]);

    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.output());
    assert!(
        run.says("noisy-one") && run.says("noisy-two"),
        "raw streaming lost the output:\n{}",
        run.output()
    );
}

/// `amont.quiet` swallows the per-check success lines and leaves one count in
/// their place — the same run, told once instead of once per check.
///
/// The count is the point. A gate that says nothing at all is
/// indistinguishable from a gate that never ran, and this crate exists
/// because that distinction is the one people get wrong.
///
/// All three values in one test, deliberately: `auto` is the default and the
/// other two are departures from it, so asserting them apart would let the
/// default drift without anything failing.
#[cfg(unix)]
#[test]
fn quiet_replaces_the_success_lines_with_their_count() {
    use std::os::unix::fs::PermissionsExt;
    let r = Repo::new();
    r.stage("pass.sh", "#!/bin/sh\nexit 0\n");
    std::fs::set_permissions(r.path("pass.sh"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
    r.git(&["add", "pass.sh"]);
    manifest(&r, "pre-commit  passes  *  block  ./pass.sh\n");

    // `never` is verbose, whoever is reading. The harness pins this value for
    // every test (see common::Repo::new), so this arm is also the proof that
    // the pin does what the rest of the suite assumes.
    let loud = r.hook("pre-commit", &[]);
    assert!(loud.passed(), "{}", loud.output());
    assert!(
        loud.says("No secrets staged"),
        "`never` lost its per-check lines:\n{}",
        loud.output()
    );

    // And `auto` is the DEFAULT, which this harness cannot reach by simply
    // unsetting: `auto` asks who is reading, and `Command::output()` means
    // nobody is. Unset and the run must go quiet on its own.
    r.git(&["config", "--unset", "amont.quiet"]);
    let defaulted = r.hook("pre-commit", &[]);
    assert!(defaulted.passed(), "{}", defaulted.output());
    assert!(
        !defaulted.says("No secrets staged"),
        "the default is `auto`, and nothing is watching a piped run:\n{}",
        defaulted.output()
    );
    assert!(
        defaulted.says("check(s) passed"),
        "the default said nothing at all, which reads as a gate that never ran:\n{}",
        defaulted.output()
    );

    r.git(&["config", "amont.quiet", "always"]);
    let quiet = r.hook("pre-commit", &[]);
    assert!(quiet.passed(), "{}", quiet.output());
    assert!(
        !quiet.says("No secrets staged"),
        "quiet kept a per-check success line:\n{}",
        quiet.output()
    );
    assert!(
        quiet.says("check(s) passed"),
        "quiet said nothing at all, which reads as a gate that never ran:\n{}",
        quiet.output()
    );
}

/// Quiet is about the uneventful path. A check that fails still says so, and
/// still blocks — the setting must never be a way to lose a refusal.
#[cfg(unix)]
#[test]
fn quiet_never_swallows_a_failure() {
    use std::os::unix::fs::PermissionsExt;
    let r = Repo::new();
    r.stage("nope.sh", "#!/bin/sh\necho the-reason\nexit 1\n");
    std::fs::set_permissions(r.path("nope.sh"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
    r.git(&["add", "nope.sh"]);
    manifest(&r, "pre-commit  refuses  *  block  ./nope.sh\n");
    r.git(&["config", "amont.quiet", "always"]);

    let run = r.hook("pre-commit", &[]);
    assert!(
        !run.passed(),
        "quiet let a blocking check through:\n{}",
        run.output()
    );
    assert!(
        run.says("the-reason") && run.says("refuses"),
        "quiet swallowed the reason a check refused:\n{}",
        run.output()
    );
}
