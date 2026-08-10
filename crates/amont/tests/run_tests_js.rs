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

#[test]
fn a_declared_pre_commit_check_removes_that_script_from_the_push_gate() {
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

    let (code, out) = push_range_out(&r, &base, &head(&r));
    assert_eq!(
        code, 0,
        "typecheck ran at push despite being gated at commit:\n{out}"
    );
    // And it SAYS so. A check that stops running silently is the failure this
    // whole project is arranged against.
    assert!(
        out.contains("typecheck") && out.contains("gated at commit"),
        "the push did not say typecheck had moved:\n{out}"
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
