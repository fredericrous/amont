//! pre-push-run-tests-js. git feeds pre-push one line per ref on stdin, so the
//! cases drive the binary the same way.

mod common;
use common::{missing, Repo};
use std::io::Write;
use std::process::{Command, Stdio};

/// Feed the hook a pre-push line for the range `from..to`.
///
/// A REAL range matters: passing a zero remote-oid means "new branch", and the
/// range becomes the single root commit — on which `git diff-tree` reports
/// nothing without `--root`. So no files look changed, no gate runs, and every
/// case passes for the wrong reason. Two commits and an explicit range is what
/// the zsh suite used.
fn push_range(r: &Repo, from: &str, to: &str) -> i32 {
    let line = format!("refs/heads/main {to} refs/heads/main {from}\n");
    let mut child = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-push-run-tests-js")
        .current_dir(&r.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(line.as_bytes())
        .unwrap();
    child.wait().expect("wait").code().unwrap_or(-1)
}

fn head(r: &Repo) -> String {
    String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string()
}

#[test]
fn runs_a_packages_gate_and_passes_when_it_succeeds() {
    if missing("npm") {
        return;
    }
    let r = Repo::new();
    r.stage(
        "package.json",
        r#"{"name":"t","scripts":{"test":"exit 0"}}"#,
    );
    r.commit("chore: base");
    let base = head(&r);
    r.stage("src/a.js", "const x = 1;\n");
    r.commit("feat: a");
    assert_eq!(push_range(&r, &base, &head(&r)), 0);
}

#[test]
fn fails_when_the_gate_fails() {
    if missing("npm") {
        return;
    }
    let r = Repo::new();
    r.stage(
        "package.json",
        r#"{"name":"t","scripts":{"test":"exit 1"}}"#,
    );
    r.commit("chore: base");
    let base = head(&r);
    r.stage("src/a.js", "const x = 1;\n");
    r.commit("feat: a");
    assert_ne!(push_range(&r, &base, &head(&r)), 0);
}

/// typecheck → test:unit → test, cheapest first, so a type error costs seconds
/// rather than a full suite. `lint` is deliberately absent — pre-commit-lint-js
/// already lints staged files.
///
/// The recording scripts avoid `sh -c '...'`: npm runs scripts through cmd.exe
/// on Windows, where `'` does not quote. cmd would eat the `>>` redirect itself
/// and run `sh -c 'echo typecheck`, creating ran.txt EMPTY — the gate looks like
/// it ran nothing while exiting 0. `echo x>>ran.txt` means the same thing to sh
/// and to cmd. No space before `>>`: cmd would include it in the written line.
#[test]
fn runs_the_gate_in_order_and_never_lint() {
    if missing("npm") {
        return;
    }
    let r = Repo::new();
    r.stage(
        "package.json",
        r#"{"name":"t","scripts":{"typecheck":"echo typecheck>>ran.txt","lint":"echo lint>>ran.txt","test:unit":"echo unit>>ran.txt","test":"echo test>>ran.txt"}}"#,
    );
    r.commit("chore: base");
    let base = head(&r);
    r.stage("src/a.js", "const x = 1;\n");
    r.commit("feat: a");
    assert_eq!(push_range(&r, &base, &head(&r)), 0);
    let ran = std::fs::read_to_string(r.path("ran.txt")).unwrap_or_default();
    assert_eq!(
        ran.lines().map(str::trim).collect::<Vec<_>>(),
        vec!["typecheck", "unit", "test"]
    );
    assert!(
        !ran.contains("lint"),
        "lint belongs to pre-commit, not here"
    );
}

#[test]
fn a_failure_skips_the_rest_of_the_gate() {
    if missing("npm") {
        return;
    }
    let r = Repo::new();
    r.stage(
        "package.json",
        r#"{"name":"t","scripts":{"typecheck":"exit 1","test:unit":"echo unit>>ran.txt","test":"echo test>>ran.txt"}}"#,
    );
    r.commit("chore: base");
    let base = head(&r);
    r.stage("src/a.js", "const x = 1;\n");
    r.commit("feat: a");
    assert_ne!(push_range(&r, &base, &head(&r)), 0);
    assert!(
        !r.path("ran.txt").exists(),
        "nothing after the failure should run"
    );
}

/// Feed the hook a NEW-BRANCH pre-push line: the remote oid is all zeroes.
fn push_new_branch(r: &Repo, branch: &str, tip: &str) -> i32 {
    let zero = "0".repeat(tip.len());
    let line = format!("refs/heads/{branch} {tip} refs/heads/{branch} {zero}\n");
    let mut child = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-push-run-tests-js")
        .current_dir(&r.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(line.as_bytes())
        .unwrap();
    child.wait().expect("wait").code().unwrap_or(-1)
}

/// A repo with a REAL bare origin holding `main`, so `--not --remotes` has a
/// remote-tracking ref to exclude against — without one, a new branch's whole
/// history is "new" and the case proves nothing about commit selection.
fn with_origin(r: &Repo) {
    let origin = r.path(".git/test-origin.git");
    r.git(&["init", "-q", "--bare", origin.to_str().expect("utf8")]);
    r.git(&["remote", "add", "origin", origin.to_str().expect("utf8")]);
    r.git(&["push", "-q", "--no-verify", "origin", "main"]);
}

/// A new branch carrying several commits must be judged by ALL of them.
///
/// The check used to build its own range: `remote_oid == zero` became the
/// single string `<tip>`, and `diff-tree <tip>` reports only what the TIP
/// changed against its parent. So a multi-commit new branch whose FIRST commit
/// breaks the JS package, with a docs commit on top, selected no package at all
/// and the push proceeded green with the suite never having run. This case
/// passes VACUOUSLY under the old code, which is exactly the bug.
#[test]
fn a_new_branch_is_judged_by_every_commit_not_just_its_tip() {
    if missing("npm") {
        return;
    }
    let r = Repo::new();
    r.stage(
        "package.json",
        r#"{"name":"t","scripts":{"test":"exit 1"}}"#,
    );
    r.commit("chore: base");
    with_origin(&r);

    r.git(&["checkout", "-q", "-b", "feat/x"]);
    r.stage("src/a.js", "const x = 1;\n");
    r.commit("feat: the js change");
    r.stage("README.md", "docs\n");
    r.commit("docs: on top of it");

    assert_ne!(
        push_new_branch(&r, "feat/x", &head(&r)),
        0,
        "the failing package was two commits back and the gate never ran"
    );
}

/// A file changed and reverted within one push still selects its package.
///
/// `diff-tree A..B` is a two-TREE compare, not a commit walk — unlike `log`,
/// where the identical syntax means the opposite. A `.js` file edited by one
/// commit and restored by a later one in the same push nets to "unchanged"
/// between the endpoints, so the package was never selected and its suite never
/// ran, even though the push plainly carries a commit that touched it.
#[test]
fn a_file_reverted_later_in_the_same_push_still_selects_its_package() {
    if missing("npm") {
        return;
    }
    let r = Repo::new();
    r.stage(
        "package.json",
        r#"{"name":"t","scripts":{"test":"exit 1"}}"#,
    );
    r.stage("src/a.js", "const x = 1;\n");
    r.commit("chore: base");
    let base = head(&r);

    r.stage("src/a.js", "const x = 2;\n");
    r.commit("feat: touch the js");
    r.stage("src/a.js", "const x = 1;\n");
    r.commit("revert: put it back");

    assert_ne!(
        push_range(&r, &base, &head(&r)),
        0,
        "the endpoints' trees are identical, but a pushed commit touched .js"
    );
}

/// A change with no JS/TS files runs nothing at all.
#[test]
fn a_non_js_change_runs_no_gate() {
    let r = Repo::new();
    r.stage(
        "package.json",
        r#"{"name":"t","scripts":{"test":"exit 1"}}"#,
    );
    r.commit("chore: seed");
    let base = head(&r);
    r.stage("README.md", "docs\n");
    r.commit("docs: readme");
    assert_eq!(push_range(&r, &base, &head(&r)), 0);
}

// --- what a repository has already gated at COMMIT time ---------------------
//
// `typecheck` is in GATE because nothing checks it earlier. A repository that
// moves it earlier — so a type error is heard at the commit that caused it,
// not an hour later at push — must not then be charged for it twice. These
// pin both halves: that a real declaration removes it, and that a declaration
// which would NOT run leaves it exactly where it was.
//
// The signal throughout is `"typecheck": "exit 1"`. If it runs, the push
// fails; if it is correctly skipped, `test` still runs and the push passes.
// That cannot be satisfied by accident in either direction.

/// Same as `push_range`, but keeps stdout so the message can be read.
fn push_range_out(r: &Repo, from: &str, to: &str) -> (i32, String) {
    let line = format!("refs/heads/main {to} refs/heads/main {from}\n");
    let mut child = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-push-run-tests-js")
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

/// A repo whose `typecheck` script would FAIL, plus a passing `test`, with one
/// JS commit to push. Returns the base to push from.
fn repo_with_failing_typecheck() -> (Repo, String) {
    let r = Repo::new();
    r.stage(
        "package.json",
        r#"{"name":"t","scripts":{"typecheck":"exit 1","test":"exit 0"}}"#,
    );
    r.commit("chore: base");
    let base = head(&r);
    r.stage("src/a.ts", "export const x = 1\n");
    r.commit("feat: add a ts file");
    (r, base)
}

fn trust_manifest(r: &Repo) {
    let out = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("trust")
        .current_dir(&r.dir)
        .output()
        .expect("amont trust");
    assert!(
        out.status.success(),
        "could not trust the manifest: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Wire the real hooks into the fixture, so a commit stamps.
///
/// Suppression is no longer paper-based: the push gate defers to a
/// commit-time declaration only for commits that CARRY its stamp, and the
/// stamp comes from pre-commit + post-commit actually running.
fn install_hooks(r: &Repo) {
    let out = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("init")
        .current_dir(&r.dir)
        .stdin(Stdio::null())
        .output()
        .expect("amont init");
    assert!(
        out.status.success(),
        "could not install hooks: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A commit that RUNS the hooks — unlike `Repo::commit`, which passes
/// `--no-verify` precisely so ordinary fixtures stay hook-free.
fn verified_commit(r: &Repo, msg: &str) {
    let out = r.git(&["commit", "-q", "-m", msg]);
    assert!(
        out.status.success(),
        "verified commit failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repo whose `typecheck` LOGS each run and passes, with a trusted
/// declaration and real hooks. The log length is the observable: it says
/// exactly how many times the script executed, wherever from.
///
/// The declared command is `node tc.js`, NOT `npm run typecheck`: a declared
/// external spawns its literal program, and on Windows `npm` is `npm.cmd`,
/// which `Command::new("npm")` cannot start — the check would report
/// Unavailable at commit time and nothing would ever stamp. `node` is a real
/// .exe everywhere. The push-time gate still goes through `npm run`, so both
/// spellings of "the same check" are exercised — which is exactly the
/// name-is-the-contract argument in `gated_at_commit`'s doc.
fn stamped_repo() -> (Repo, String) {
    let r = Repo::new();
    r.stage(
        "package.json",
        r#"{"name":"t","scripts":{"typecheck":"node tc.js","test":"exit 0"}}"#,
    );
    r.stage("tc.js", "require('fs').appendFileSync('tc.log','x')\n");
    r.commit("chore: base");
    let base = head(&r);
    r.stage(
        "amont.conf",
        "pre-commit  typecheck  *.ts,*.tsx  block  node tc.js\n",
    );
    r.commit("chore: typecheck at commit time");
    trust_manifest(&r);
    install_hooks(&r);
    (r, base)
}

fn typecheck_runs(r: &Repo) -> usize {
    std::fs::read_to_string(r.dir.join("tc.log"))
        .map(|s| s.len())
        .unwrap_or(0)
}

#[test]
fn a_declared_pre_commit_check_removes_that_script_from_the_push_gate() {
    if missing("npm") {
        return;
    }
    let (r, base) = stamped_repo();
    r.stage("src/a.ts", "export const x = 1\n");
    verified_commit(&r, "feat: add a ts file");
    assert_eq!(
        typecheck_runs(&r),
        1,
        "the commit itself should have run typecheck exactly once"
    );

    let (code, out) = push_range_out(&r, &base, &head(&r));
    assert_eq!(
        code, 0,
        "the push failed despite a stamped, gated typecheck:\n{out}"
    );
    // And it SAYS so. A check that stops running silently is the failure this
    // whole project is arranged against.
    assert!(
        out.contains("typecheck") && out.contains("gated at commit"),
        "the push did not say typecheck had moved:\n{out}"
    );
    assert_eq!(
        typecheck_runs(&r),
        1,
        "the push re-ran a script the commit already stamped:\n{out}"
    );
}

/// THE hole the fifth shim exists to close: `--no-verify` skips pre-commit,
/// so the check never ran — and the push gate must notice, run the script
/// itself, and say why, instead of trusting the declaration's word for it.
#[test]
fn a_no_verify_commit_brings_the_push_gate_back() {
    if missing("npm") {
        return;
    }
    let (r, base) = stamped_repo();
    r.stage("src/a.ts", "export const x = 1\n");
    r.commit("feat: dodge the hooks"); // Repo::commit is --no-verify

    let (code, out) = push_range_out(&r, &base, &head(&r));
    assert_eq!(
        code, 0,
        "typecheck passes when run — only WHO runs it moved:\n{out}"
    );
    assert!(
        out.contains("no record of it"),
        "the push must say why the gate is running:\n{out}"
    );
    assert!(
        !out.contains("gated at commit"),
        "claimed commit-time cover for a commit that dodged the hooks:\n{out}"
    );
    assert_eq!(
        typecheck_runs(&r),
        1,
        "the gate did not run typecheck for the unchecked commit:\n{out}"
    );
}

/// One unchecked commit poisons the push: the stamped commit next to it does
/// not vouch for it, so the gate runs for the ref.
#[test]
fn one_unstamped_commit_re_runs_the_gate_for_the_push() {
    if missing("npm") {
        return;
    }
    let (r, base) = stamped_repo();
    r.stage("src/a.ts", "export const x = 1\n");
    verified_commit(&r, "feat: checked");
    r.stage("src/b.ts", "export const y = 2\n");
    r.commit("feat: unchecked"); // --no-verify

    let (code, out) = push_range_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("no record of it"),
        "one unstamped commit must bring the gate back:\n{out}"
    );
    assert_eq!(
        typecheck_runs(&r),
        2,
        "commit-time once, push-time once for the unchecked commit:\n{out}"
    );
}

/// The safety half, and the one worth getting wrong only once.
///
/// An untrusted manifest does not run — so if the push gate deferred to it, a
/// repository would have types checked at NEITHER end while both ends reported
/// green. `manifest::gate` turns untrusted declarations into `Unusable`, and
/// `gated_at_commit` counts only `Runnable`; this is what holds that together.
#[test]
fn an_untrusted_declaration_leaves_the_push_gate_alone() {
    if missing("npm") {
        return;
    }
    let (r, base) = repo_with_failing_typecheck();
    // Written and committed, deliberately NOT trusted — the cloned-repo case.
    r.stage(
        "amont.conf",
        "pre-commit  typecheck  *.ts,*.tsx  block  npm run typecheck\n",
    );
    r.commit("chore: declare typecheck, untrusted");

    let (code, out) = push_range_out(&r, &base, &head(&r));
    assert_ne!(
        code, 0,
        "an untrusted declaration removed the push gate — nothing typechecks now:\n{out}"
    );
}

/// Same rule, one layer up: a declaration the author switched off is not a
/// check, so it cannot excuse the push gate either.
#[test]
fn a_skipped_declaration_leaves_the_push_gate_alone() {
    if missing("npm") {
        return;
    }
    let (r, base) = repo_with_failing_typecheck();
    r.stage(
        "amont.conf",
        "pre-commit  typecheck  *.ts,*.tsx  block  npm run typecheck\n",
    );
    r.commit("chore: typecheck at commit time");
    trust_manifest(&r);
    r.git(&["config", "--add", "hook.skip", "pre-commit-typecheck"]);

    let (code, out) = push_range_out(&r, &base, &head(&r));
    assert_ne!(
        code, 0,
        "a skipped declaration removed the push gate — nothing typechecks now:\n{out}"
    );
}

/// A declaration on the OTHER stage means nothing here: `pre-push typecheck`
/// does not gate anything at commit time, so the entry stays.
#[test]
fn a_pre_push_declaration_does_not_count_as_commit_time() {
    if missing("npm") {
        return;
    }
    let (r, base) = repo_with_failing_typecheck();
    r.stage(
        "amont.conf",
        "pre-push  typecheck  *.ts,*.tsx  block  npm run typecheck\n",
    );
    r.commit("chore: declare on the wrong stage");
    trust_manifest(&r);

    let (code, out) = push_range_out(&r, &base, &head(&r));
    assert_ne!(
        code, 0,
        "a pre-push declaration was treated as commit-time cover:\n{out}"
    );
}

/// Nothing declared is the overwhelmingly common case, and it must be exactly
/// what it was before any of this existed.
#[test]
fn with_no_declaration_the_gate_is_unchanged() {
    if missing("npm") {
        return;
    }
    let (r, base) = repo_with_failing_typecheck();
    let (code, out) = push_range_out(&r, &base, &head(&r));
    assert_ne!(code, 0, "typecheck did not run:\n{out}");
    assert!(
        !out.contains("gated at commit"),
        "said something about commit-time gating with nothing declared:\n{out}"
    );
}

/// A `warn` declaration lets a failing commit through — it cannot stand in for
/// a push check that blocks. Severity is part of "would actually run count".
#[test]
fn a_warn_declaration_leaves_the_push_gate_alone() {
    if missing("npm") {
        return;
    }
    let (r, base) = repo_with_failing_typecheck();
    r.stage(
        "amont.conf",
        "pre-commit  typecheck  *.ts,*.tsx  warn  npm run typecheck\n",
    );
    r.commit("chore: typecheck at commit time, warn only");
    trust_manifest(&r);

    let (code, out) = push_range_out(&r, &base, &head(&r));
    assert_ne!(
        code, 0,
        "a warn declaration removed the push gate — a failing commit and a green push:\n{out}"
    );
    assert!(
        !out.contains("gated at commit"),
        "claimed commit-time cover a warn declaration cannot give:\n{out}"
    );
}

/// The same downgrade arrived at sideways: the declaration says `block`, an
/// `amont.severity.*` override says `warn`. The EFFECTIVE severity is what a
/// commit actually experiences, so it is what the push gate must consult.
#[test]
fn a_severity_override_to_warn_leaves_the_push_gate_alone() {
    if missing("npm") {
        return;
    }
    let (r, base) = repo_with_failing_typecheck();
    r.stage(
        "amont.conf",
        "pre-commit  typecheck  *.ts,*.tsx  block  npm run typecheck\n",
    );
    r.commit("chore: typecheck at commit time");
    trust_manifest(&r);
    r.git(&["config", "amont.severity.pre-commit-typecheck", "warn"]);

    let (code, out) = push_range_out(&r, &base, &head(&r));
    assert_ne!(
        code, 0,
        "a severity override downgraded the commit check and the push gate still deferred to it:\n{out}"
    );
}

/// A `*.ts,*.tsx` declaration says nothing about a `.js` change. A push whose
/// JS changes fall outside the declared scope carries commits the commit-time
/// check never judged, so the full gate runs for that ref.
#[test]
fn a_push_outside_the_declared_scope_runs_the_full_gate() {
    if missing("npm") {
        return;
    }
    let r = Repo::new();
    r.stage(
        "package.json",
        r#"{"name":"t","scripts":{"typecheck":"exit 1","test":"exit 0"}}"#,
    );
    r.commit("chore: base");
    let base = head(&r);
    r.stage("src/legacy.js", "const x = 1;\n");
    r.commit("feat: a js change the ts scope never saw");
    r.stage(
        "amont.conf",
        "pre-commit  typecheck  *.ts,*.tsx  block  npm run typecheck\n",
    );
    r.commit("chore: typecheck at commit time");
    trust_manifest(&r);

    let (code, out) = push_range_out(&r, &base, &head(&r));
    assert_ne!(
        code, 0,
        "a .js push was excused by a .ts-scoped declaration — checked at neither end:\n{out}"
    );
    assert!(
        !out.contains("gated at commit"),
        "claimed commit-time cover for files outside the declared scope:\n{out}"
    );
}

/// The declared command runs at the repo ROOT. A monorepo sub-package's gate
/// is a different command in a different directory, and no root declaration
/// has judged it — so it is never skipped on the root's account.
#[test]
fn a_root_declaration_does_not_excuse_a_sub_package() {
    if missing("npm") {
        return;
    }
    let r = Repo::new();
    r.stage(
        "package.json",
        r#"{"name":"root","scripts":{"typecheck":"exit 0","test":"exit 0"}}"#,
    );
    r.stage(
        "packages/b/package.json",
        r#"{"name":"b","scripts":{"typecheck":"exit 1"}}"#,
    );
    r.commit("chore: base");
    let base = head(&r);
    r.stage("packages/b/src/a.ts", "export const x = 1\n");
    r.commit("feat: change the sub-package");
    r.stage(
        "amont.conf",
        "pre-commit  typecheck  *.ts,*.tsx  block  npm run typecheck\n",
    );
    r.commit("chore: typecheck at commit time");
    trust_manifest(&r);

    let (code, out) = push_range_out(&r, &base, &head(&r));
    assert_ne!(
        code, 0,
        "the root declaration excused packages/b's own typecheck:\n{out}"
    );
}

/// A BLOCKED commit attempt must not leave a marker that vouches for the
/// `--no-verify` retry of the same tree. pre_commit records an empty gate
/// list on a Block verdict — the design property the dispatcher's comment
/// states, pinned end to end here: gated typecheck PASSES while a sibling
/// check blocks, the same tree is then committed with --no-verify, and the
/// push must re-run the gate.
#[cfg(unix)]
#[test]
fn a_blocked_attempt_cannot_vouch_for_a_no_verify_retry() {
    if missing("npm") {
        return;
    }
    let r = Repo::new();
    r.stage(
        "package.json",
        r#"{"name":"t","scripts":{"typecheck":"node tc.js","test":"exit 0"}}"#,
    );
    r.stage("tc.js", "require('fs').appendFileSync('tc.log','x')\n");
    r.stage("deny.sh", "#!/bin/sh\nexit 1\n");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(r.path("deny.sh"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
    r.git(&["add", "deny.sh"]);
    r.commit("chore: base");
    let base = head(&r);
    r.stage(
        "amont.conf",
        "pre-commit  typecheck  *.ts,*.tsx  block  node tc.js\n\
         pre-commit  deny  *  block  ./deny.sh\n",
    );
    r.commit("chore: declare");
    trust_manifest(&r);
    install_hooks(&r);

    r.stage("src/a.ts", "export const x = 1\n");
    let attempt = r.git(&["commit", "-q", "-m", "feat: blocked attempt"]);
    assert!(
        !attempt.status.success(),
        "the deny check should have blocked the commit"
    );
    assert_eq!(typecheck_runs(&r), 1, "typecheck itself ran and passed");

    r.commit("feat: same tree, hooks dodged"); // --no-verify
    let (code, out) = push_range_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("no record of it"),
        "a blocked attempt's marker vouched for the retry:\n{out}"
    );
    assert_eq!(
        typecheck_runs(&r),
        2,
        "the push gate must re-run typecheck for the unstamped commit:\n{out}"
    );
}

/// `git commit -a` builds a TEMPORARY index and exports GIT_INDEX_FILE to
/// the hooks. gate_stamp::record's `git write-tree` claims (in a comment) to
/// inherit it and so bind the stamp to the tree that commit actually seals —
/// this is the claim, pinned.
#[test]
fn a_commit_dash_a_still_earns_its_stamp() {
    if missing("npm") {
        return;
    }
    let (r, base) = stamped_repo();
    r.stage("src/a.ts", "export const x = 1\n");
    verified_commit(&r, "feat: tracked first");
    r.write("src/a.ts", "export const x = 2\n"); // unstaged edit…
    let out = r.git(&["commit", "-q", "-am", "feat: via commit -a"]);
    assert!(
        out.status.success(),
        "commit -a failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let (code, out) = push_range_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("gated at commit") && !out.contains("no record of it"),
        "a commit -a lost its stamp — write-tree read the wrong index:\n{out}"
    );
    assert_eq!(
        typecheck_runs(&r),
        2,
        "typecheck at each commit, never re-run at push:\n{out}"
    );
}

/// The marker lives in the worktree-PRIVATE gitdir and the stamps in the
/// shared notes ref — the code's claim about linked worktrees, pinned: a
/// commit made in a linked worktree must suppress the push gate exactly as a
/// main-checkout commit does.
#[test]
fn a_linked_worktree_commit_earns_a_stamp_the_push_sees() {
    if missing("npm") {
        return;
    }
    let (r, base) = stamped_repo();
    r.stage("src/a.ts", "export const x = 1\n");
    verified_commit(&r, "feat: main checkout");

    let wt = r
        .dir
        .parent()
        .expect("parent")
        .join(format!("stamp-wt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&wt);
    let out = r.git(&[
        "worktree",
        "add",
        "-b",
        "feat/wt",
        wt.to_str().expect("utf8"),
    ]);
    assert!(out.status.success(), "worktree add failed");

    std::fs::create_dir_all(wt.join("src")).expect("mkdir");
    std::fs::write(wt.join("src/b.ts"), "export const y = 2\n").expect("write");
    let wt_git = |args: &[&str]| {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&wt).args(args);
        common::Repo::strip_git_env_impl(&mut cmd);
        cmd.output().expect("git in worktree")
    };
    assert!(wt_git(&["add", "src/b.ts"]).status.success());
    let out = wt_git(&["commit", "-q", "-m", "feat: from the worktree"]);
    assert!(
        out.status.success(),
        "worktree commit failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // Push the worktree's branch, from the worktree, through the shared hooks.
    let tip = String::from_utf8_lossy(&wt_git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    let line = format!("refs/heads/feat/wt {tip} refs/heads/feat/wt {base}\n");
    let mut child = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-push-run-tests-js")
        .current_dir(&wt)
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
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = r.git(&["worktree", "remove", "--force", wt.to_str().expect("utf8")]);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(
        text.contains("gated at commit") && !text.contains("no record of it"),
        "a worktree commit's stamp was invisible to the push:\n{text}"
    );
}
