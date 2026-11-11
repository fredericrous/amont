//! pre-push-run-tests-js. git feeds pre-push one line per ref on stdin, so the
//! cases drive the binary the same way.

use crate::common;
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
    let mut child = crate::common::amont_cmd()
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
    let mut child = crate::common::amont_cmd()
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

/// A new ref whose every commit a remote already has introduces nothing.
///
/// Seeding a fork — `git remote rename origin upstream`, add the fork as
/// `origin`, push `main` — is a new ref on `origin` whose tip is already on
/// `upstream/main`. `rev-list <tip> --not --remotes` is EMPTY, and the empty
/// list used to fall through to the "no remote to compare against" fallback,
/// which diffs the tip commit alone: whatever upstream's last commit touched
/// selected its package, and a 40-minute suite ran over code this push did not
/// bring. The signal is `"test": "exit 1"` on a package the tip touches: if it
/// runs, the push fails.
#[test]
fn a_new_ref_already_on_a_remote_runs_no_gate() {
    if missing("npm") {
        return;
    }
    let r = Repo::new();
    r.stage(
        "package.json",
        r#"{"name":"t","scripts":{"test":"exit 1"}}"#,
    );
    r.commit("chore: base");
    r.stage("src/a.js", "const x = 1;\n");
    r.commit("feat: upstream's last commit touches js");
    with_origin(&r);

    assert_eq!(
        push_new_branch(&r, "mirror", &head(&r)),
        0,
        "every pushed commit is already on origin/main; the gate must not run"
    );
}

/// With no remote-tracking ref at all, `--not --remotes` excludes nothing:
/// the whole history is new and is judged. The empty-range fix must not turn
/// "no remote" into "nothing new" — pins the half that must NOT change.
#[test]
fn a_new_ref_with_no_remote_is_still_gated() {
    if missing("npm") {
        return;
    }
    let r = Repo::new();
    r.stage(
        "package.json",
        r#"{"name":"t","scripts":{"test":"exit 1"}}"#,
    );
    r.commit("chore: base");
    r.stage("src/a.js", "const x = 1;\n");
    r.commit("feat: js");

    assert_ne!(
        push_new_branch(&r, "main", &head(&r)),
        0,
        "no remote to compare against: the tip's change must still be gated"
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
    let mut child = crate::common::amont_cmd()
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
    let out = crate::common::amont_cmd()
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
    let out = crate::common::amont_cmd()
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

/// The npm push gate runs its scripts for `.js`, `.jsx`, `.vue` and `.ts`
/// alike, and a commit-time declaration named for one of those scripts stands
/// in for it, so it fires on all of them: a `.js` commit under a `*.ts`
/// declaration is typechecked at commit, earns the stamp, and the push does
/// not run the script again.
#[test]
fn a_script_declaration_also_fires_on_the_js_files_its_push_gate_runs_for() {
    if missing("npm") {
        return;
    }
    let (repo, base) = stamped_repo();
    repo.stage("src/legacy.js", "const x = 1;\n");
    verified_commit(&repo, "feat: a js file under a ts declaration");
    assert_eq!(
        typecheck_runs(&repo),
        1,
        "the commit typechecked the .js file its push twin would have"
    );

    let (code, out) = push_range_out(&repo, &base, &head(&repo));
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("typecheck") && out.contains("gated at commit"),
        "{out}"
    );
    assert_eq!(
        typecheck_runs(&repo),
        1,
        "the push re-ran the script:\n{out}"
    );
}

// A workspace root whose own `test` script runs every member. The fixture's
// scripts only LOG who ran, so the log says exactly which packages the push
// ran: `ran.log` beside `log.js`, one line per run.

/// A workspace of `packages/a` and `packages/b`, plus `tools/x` outside the
/// workspace globs, with the root's `scripts` body given, and `conf` committed
/// as `amont.conf` when it is not empty. Returns the base to push from, with
/// a `.ts` change in `packages/a` and in `tools/x` committed after it.
fn workspace_repo(root_scripts: &str, conf: &str) -> (Repo, String) {
    workspace_repo_with(root_scripts, r#"["packages/*"]"#, conf)
}

/// A root whose `test` logs `root`, and one that defines no scripts at all.
const ROOT_TEST: &str = r#""test":"node log.js root""#;
const ROOT_NO_SCRIPTS: &str = "";

/// The same workspace with the root's `scripts` body and its `workspaces` value
/// (JSON) given.
fn workspace_repo_with(root_scripts: &str, workspaces: &str, conf: &str) -> (Repo, String) {
    let repo = Repo::new();
    repo.stage(
        "log.js",
        "require('fs').appendFileSync(__dirname + '/ran.log', process.argv[2] + '\\n')\n",
    );
    repo.stage(
        "package.json",
        &format!(r#"{{"name":"root","workspaces":{workspaces},"scripts":{{{root_scripts}}}}}"#),
    );
    for (dir, name, up) in [
        ("packages/a", "a", "../.."),
        ("packages/b", "b", "../.."),
        ("tools/x", "x", "../.."),
    ] {
        repo.stage(
            &format!("{dir}/package.json"),
            &format!(r#"{{"name":"{name}","scripts":{{"test":"node {up}/log.js {name}"}}}}"#),
        );
    }
    repo.commit("chore: the workspace");
    let base = head(&repo);
    repo.stage("packages/a/src/x.ts", "export const a = 1\n");
    repo.stage("tools/x/src/y.ts", "export const x = 1\n");
    repo.commit("feat: change a member and a package outside the workspace");
    if !conf.is_empty() {
        repo.stage("amont.conf", conf);
        repo.commit("chore: declare");
        trust_manifest(&repo);
    }
    (repo, base)
}

fn ran(repo: &Repo) -> Vec<String> {
    std::fs::read_to_string(repo.dir.join("ran.log"))
        .map(|log| log.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// What a repository declares, and the workspace's globs say, decides which
/// members the push skips: the root's `test` runs once, and `packages/a` (in the
/// globs) is covered, while `tools/x` (outside them) still runs.
#[test]
fn a_declared_root_covers_its_members_and_only_them() {
    if missing("npm") || missing("node") {
        return;
    }
    let (repo, base) = workspace_repo(ROOT_TEST, "set rootRunsMembers test\n");
    let (code, out) = push_range_out(&repo, &base, &head(&repo));
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        ran(&repo),
        ["root", "x"],
        "a is covered, x is outside the globs:\n{out}"
    );
    assert!(
        out.contains(
            "1 package skipped, covered by the root's test (declared in amont.conf): `node log.js root`"
        ),
        "{out}"
    );
    assert!(
        !out.contains("a repeat if the root runs them"),
        "declared: no hint:\n{out}"
    );
}

/// With nothing declared the push cannot know the root runs its members, so it
/// runs both, once, and says how to declare it: the one place it guesses, out
/// loud, and `if`.
#[test]
fn an_undeclared_root_runs_its_members_and_says_how_to_declare_it() {
    if missing("npm") || missing("node") {
        return;
    }
    let (repo, base) = workspace_repo(ROOT_TEST, "");
    let (code, out) = push_range_out(&repo, &base, &head(&repo));
    assert_eq!(code, 0, "{out}");
    assert_eq!(ran(&repo), ["root", "a", "x"], "{out}");
    assert_eq!(
        out.matches("test ran at the root and in 2 packages — a repeat if the root runs them")
            .count(),
        1,
        "said once: {out}"
    );
    assert!(
        out.contains("declare it in amont.conf: set rootRunsMembers test   (or: none)"),
        "{out}"
    );
    assert!(
        out.contains("npm run test: 1 of 3, in the root"),
        "each run is announced: {out}"
    );
}

/// `none` says the root runs no members, and silences the hint.
#[test]
fn a_root_declared_to_run_no_members_stops_the_hint() {
    if missing("npm") || missing("node") {
        return;
    }
    let (repo, base) = workspace_repo(ROOT_TEST, "set rootRunsMembers none\n");
    let (code, out) = push_range_out(&repo, &base, &head(&repo));
    assert_eq!(code, 0, "{out}");
    assert_eq!(ran(&repo), ["root", "a", "x"], "{out}");
    assert!(!out.contains("a repeat if the root runs them"), "{out}");
}

/// The root has to run the script in THIS push for its members to be covered:
/// a root with no `test` runs nothing, so nothing covers the members.
#[test]
fn a_declared_script_the_root_does_not_define_skips_nothing() {
    if missing("npm") || missing("node") {
        return;
    }
    let (repo, base) = workspace_repo(ROOT_NO_SCRIPTS, "set rootRunsMembers test\n");
    let (code, out) = push_range_out(&repo, &base, &head(&repo));
    assert_eq!(code, 0, "{out}");
    assert_eq!(ran(&repo), ["a", "x"], "{out}");
    assert!(!out.contains("covered by the root's"), "{out}");
}

/// A local `git config` may only take names out of the committed line, never
/// put one in: a machine that could add `test` would skip members' tests that
/// no committed, trusted line vouches for.
#[test]
fn a_local_setting_can_remove_a_declared_name_and_never_add_one() {
    if missing("npm") || missing("node") {
        return;
    }
    // Committed `test`, local `none`: the removal counts, so the members run.
    let (repo, base) = workspace_repo(ROOT_TEST, "set rootRunsMembers test\n");
    repo.git(&["config", "amont.rootRunsMembers", "none"]);
    let (_, out) = push_range_out(&repo, &base, &head(&repo));
    assert_eq!(ran(&repo), ["root", "a", "x"], "{out}");

    // Committed `none`, local `test`: nothing is added, so the members run.
    let (repo, base) = workspace_repo(ROOT_TEST, "set rootRunsMembers none\n");
    repo.git(&["config", "amont.rootRunsMembers", "test"]);
    let (_, out) = push_range_out(&repo, &base, &head(&repo));
    assert_eq!(ran(&repo), ["root", "a", "x"], "{out}");

    // Nothing committed, local `test`: still nothing declared, so it hints.
    let (repo, base) = workspace_repo(ROOT_TEST, "");
    repo.git(&["config", "amont.rootRunsMembers", "test"]);
    let (_, out) = push_range_out(&repo, &base, &head(&repo));
    assert_eq!(ran(&repo), ["root", "a", "x"], "{out}");
    assert!(out.contains("a repeat if the root runs them"), "{out}");
}

/// A `set rootRunsMembers` line in an `amont.conf` nobody trusted does not
/// count, and the hint says so rather than asking for a line already there.
#[test]
fn an_untrusted_declaration_does_not_count_and_the_hint_says_why() {
    if missing("npm") || missing("node") {
        return;
    }
    let (repo, base) = workspace_repo(ROOT_TEST, "");
    repo.stage("amont.conf", "set rootRunsMembers test\n");
    repo.commit("chore: declare, untrusted");
    let (_, out) = push_range_out(&repo, &base, &head(&repo));
    assert_eq!(ran(&repo), ["root", "a", "x"], "{out}");
    assert!(
        out.contains("amont.conf is not trusted: its rootRunsMembers line does not count"),
        "{out}"
    );
}

/// An untrusted `amont.conf` with no `rootRunsMembers` line in it gets the
/// ordinary hint: "trust it" would tell the person something untrue.
#[test]
fn a_withheld_file_without_the_line_gets_the_ordinary_hint() {
    if missing("npm") || missing("node") {
        return;
    }
    let (repo, base) = workspace_repo(ROOT_TEST, "");
    repo.stage("amont.conf", "set onBusy wait\n");
    repo.commit("chore: an untrusted file without the line");
    let (_, out) = push_range_out(&repo, &base, &head(&repo));
    assert_eq!(ran(&repo), ["root", "a", "x"], "{out}");
    assert!(
        out.contains("declare it in amont.conf: set rootRunsMembers test"),
        "{out}"
    );
    assert!(
        !out.contains("is not trusted: its rootRunsMembers"),
        "{out}"
    );
}

/// A declaration the workspace cannot confirm skips nothing, and says why a
/// declared line seems to do nothing.
#[test]
fn a_declared_root_with_an_unreadable_workspace_runs_the_members_and_says_so() {
    if missing("npm") || missing("node") {
        return;
    }
    let (repo, base) = workspace_repo_with(
        ROOT_TEST,
        r#"["packages/{a,b}"]"#,
        "set rootRunsMembers test\n",
    );
    let (code, out) = push_range_out(&repo, &base, &head(&repo));
    assert_eq!(code, 0, "{out}");
    assert_eq!(ran(&repo), ["root", "a", "x"], "{out}");
    assert!(
        out.contains("rootRunsMembers is declared, but its workspace globs are not understood"),
        "{out}"
    );
    assert!(
        out.contains("running every member; check the workspace globs in package.json"),
        "{out}"
    );
    assert!(!out.contains("skipped, covered by"), "{out}");
}

/// A declaration whose program is not installed has no identity to stamp, so
/// the push says that, instead of blaming the commits for carrying no record.
#[test]
fn a_script_declaration_whose_program_is_missing_says_so() {
    if missing("npm") {
        return;
    }
    let repo = Repo::new();
    repo.stage(
        "package.json",
        r#"{"name":"t","scripts":{"typecheck":"exit 0","test":"exit 0"}}"#,
    );
    repo.commit("chore: base");
    let base = head(&repo);
    repo.stage("src/a.ts", "export const x = 1\n");
    repo.commit("feat: a ts file");
    repo.stage(
        "amont.conf",
        "pre-commit  typecheck  *.ts  block  no-such-program-for-this-test\n",
    );
    repo.commit("chore: typecheck at commit time");
    trust_manifest(&repo);

    let (code, out) = push_range_out(&repo, &base, &head(&repo));
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("typecheck at commit: its program was not found — running it here"),
        "{out}"
    );
    assert!(
        !out.contains("no record of it"),
        "re-committing would not help, so it must not say so: {out}"
    );
}

/// A `files` declaration is handed the matched paths, so it is never widened:
/// a push whose files it does not cover says what it misses and where to fix
/// it, in the words the pair gates use, then runs the script.
#[test]
fn a_files_declaration_that_misses_a_changed_file_says_so() {
    if missing("npm") {
        return;
    }
    let repo = Repo::new();
    repo.stage(
        "package.json",
        r#"{"name":"t","scripts":{"typecheck":"exit 1","test":"exit 0"}}"#,
    );
    repo.commit("chore: base");
    let base = head(&repo);
    repo.stage("src/legacy.js", "const x = 1;\n");
    repo.commit("feat: a js change the ts scope never saw");
    repo.stage(
        "amont.conf",
        "pre-commit  typecheck  *.ts  block  files node tc.js\n",
    );
    repo.commit("chore: typecheck at commit time");
    trust_manifest(&repo);

    let (code, out) = push_range_out(&repo, &base, &head(&repo));
    assert_ne!(code, 0, "the script ran and failed:\n{out}");
    assert!(
        out.contains("typecheck at commit covers *.ts, not src/legacy.js — running it here"),
        "{out}"
    );
    assert!(
        out.contains("a files declaration is not widened: add src/legacy.js to amont.conf line 1"),
        "{out}"
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
    let mut child = crate::common::amont_cmd()
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

// --- the bypass ledger -----------------------------------------------------
//
// The stamp's silent sibling: a commit a gate declaration covered, created
// without that gate having run, appends one line per dodged script to
// `.git/amont-bypasses`. These tests pin what counts and — just as load-
// bearing — what does not: the fixture's own setup commits, out-of-scope
// commits, and repositories that declare no gate must all leave NO file.

fn ledger(r: &Repo) -> Option<String> {
    std::fs::read_to_string(r.path(".git/amont-bypasses")).ok()
}

#[test]
fn a_stamped_repo_starts_with_an_empty_ledger() {
    let (r, _base) = stamped_repo();
    assert_eq!(
        ledger(&r),
        None,
        "the fixture's pre-install commits must not register"
    );
}

#[test]
fn a_no_verify_commit_a_gate_covers_is_recorded_as_unverified() {
    let (r, _base) = stamped_repo();
    r.stage("src/b.ts", "export const y = 2\n");
    r.commit("feat: dodge the gate"); // Repo::commit IS --no-verify
    let text = ledger(&r).expect("a bypassed gate leaves a ledger");
    assert!(text.starts_with("amont-bypass-v1"), "{text:?}");
    assert!(text.contains(" typecheck"), "{text:?}");
    assert_eq!(text.lines().count(), 2, "one header, one event: {text:?}");
}

#[test]
fn a_verified_commit_records_nothing() {
    if missing("node") {
        return;
    }
    let (r, _base) = stamped_repo();
    r.stage("src/b.ts", "export const y = 2\n");
    verified_commit(&r, "feat: through the gate");
    assert_eq!(ledger(&r), None, "a stamped commit is not a bypass");
}

#[test]
fn a_commit_outside_the_declarations_scope_records_nothing() {
    let (r, _base) = stamped_repo();
    r.stage("README.md", "docs only\n");
    r.commit("docs: no ts touched");
    assert_eq!(
        ledger(&r),
        None,
        "the gate never covered this commit, so nothing was dodged"
    );
}

#[test]
fn a_repo_that_declares_no_gate_records_nothing() {
    let r = Repo::new();
    r.stage("a.txt", "hello\n");
    r.commit("chore: base");
    install_hooks(&r);
    r.stage("b.txt", "more\n");
    r.commit("chore: more");
    assert_eq!(ledger(&r), None, "no declaration, no bypass to count");
}

#[test]
fn a_blocked_commit_retried_with_no_verify_is_recorded() {
    if missing("node") {
        return;
    }
    let r = Repo::new();
    r.stage("tc.js", "process.exit(1)\n");
    r.commit("chore: base");
    r.stage(
        "amont.conf",
        "pre-commit  typecheck  *.ts,*.tsx  block  node tc.js\n",
    );
    r.commit("chore: gate at commit time");
    trust_manifest(&r);
    install_hooks(&r);
    r.stage("src/a.ts", "export const x = 1\n");
    let attempt = r.git(&["commit", "-q", "-m", "feat: blocked"]);
    assert!(!attempt.status.success(), "the gate must block the attempt");
    r.commit("feat: bypassed after the block");
    let text = ledger(&r).expect("the retry dodged a gate that said no");
    assert!(text.contains(" typecheck"), "{text:?}");
}

#[test]
fn a_partially_stamped_commit_records_only_the_missing_script() {
    if missing("node") {
        return;
    }
    let (r, _base) = stamped_repo();
    // A second gate whose program does not exist: it reports Unavailable at
    // commit time (warn, never blocks), earns no stamp, and is exactly the
    // case the discard-site design missed — the commit lands stamped for
    // typecheck and unverified for test.
    r.stage(
        "amont.conf",
        "pre-commit  typecheck  *.ts,*.tsx  block  node tc.js\n\
         pre-commit  test       *.ts,*.tsx  block  amont-no-such-tool-xyz\n",
    );
    r.stage("src/b.ts", "export const y = 2\n");
    trust_manifest(&r);
    verified_commit(&r, "feat: half a gate");
    let text = ledger(&r).expect("the unavailable half goes unverified");
    assert!(text.contains(" test"), "{text:?}");
    assert!(
        !text.contains(" typecheck"),
        "the stamped half must not be counted: {text:?}"
    );
}

#[test]
fn amont_list_json_reports_the_bypass_ledger() {
    let (r, _base) = stamped_repo();
    r.stage("src/b.ts", "export const y = 2\n");
    r.commit("feat: dodge the gate");
    let out = r.run(&["list", "--json"]);
    assert_eq!(out.code, 0, "{}", out.output());
    assert!(
        out.stdout.contains("\"bypasses\":{\"total\":1"),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("\"script\":\"typecheck\",\"count\":1"),
        "{}",
        out.stdout
    );
}

#[test]
fn amont_list_text_names_the_dodged_script() {
    let (r, _base) = stamped_repo();
    r.stage("src/b.ts", "export const y = 2\n");
    r.commit("feat: dodge the gate");
    let out = r.run(&["list"]);
    assert!(out.stdout.contains("unverified commits"), "{}", out.stdout);
    assert!(out.stdout.contains("typecheck"), "{}", out.stdout);
}

#[test]
fn amont_uninstall_forgets_the_bypass_ledger() {
    let (r, _base) = stamped_repo();
    r.stage("src/b.ts", "export const y = 2\n");
    r.commit("feat: dodge the gate");
    assert!(ledger(&r).is_some());
    let out = r.run(&["uninstall"]);
    assert_eq!(out.code, 0, "{}", out.output());
    assert_eq!(ledger(&r), None, "the ledger is OUR bookkeeping — gone");
}

#[test]
fn amont_record_bypasses_false_records_nothing() {
    let (r, _base) = stamped_repo();
    r.git(&["config", "amont.recordBypasses", "false"]);
    r.stage("src/b.ts", "export const y = 2\n");
    r.commit("feat: dodge the gate, off the record");
    assert_eq!(ledger(&r), None, "the documented switch must be honoured");
}
