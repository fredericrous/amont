//! The dependency audits, end to end: warn on a branch push, refuse a
//! `v*` tag push carrying known vulnerabilities, never block when the
//! tool is missing. Fake audit tools on a prepended PATH give each test
//! total control of output and exit code — the checks' verdicts come from
//! parsing, and parsing is what these pin.
#![cfg(unix)]

mod common;
use common::Repo;

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

/// A directory of fake tools, prepended to PATH for one invocation.
fn shim(r: &Repo, name: &str, body: &str) {
    let dir = r.path(".git/toolshims");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}")).expect("write");
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// Run ONE audit check with a single pushed ref, fake tools first on PATH.
fn push_check(r: &Repo, check: &str, remote_ref: &str) -> (i32, String) {
    let oid = "a".repeat(40);
    let line = format!("{remote_ref} {oid} {remote_ref} {}\n", "0".repeat(40));
    let path = format!(
        "{}:{}",
        r.path(".git/toolshims").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg(check)
        .current_dir(&r.dir)
        .env("PATH", path)
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

/// A fake `cargo`: `audit` reports RUSTSEC-2025-0001 against `bad`, and
/// `tree -i bad` over the shipped edges (normal,build) prints `shipped` —
/// a local crate line like `app v0.1.0 (/repo/app)` reaches it, an empty
/// string means nothing that ships does. Any other `tree` sees the dev edge.
fn cargo_reaching(shipped: &str) -> String {
    // Real cargo, for a crate only a dev edge reaches: exit 0 and
    // "warning: nothing to print." The tree is asked about `bad@1.0.0`,
    // the version cargo audit reported.
    let answer = if shipped.is_empty() {
        "echo 'warning: nothing to print.' >&2; exit 0".to_string()
    } else {
        format!("printf 'bad v1.0.0\\n%s\\n' '{shipped}'; exit 0")
    };
    format!(
        "case \"$1\" in\n\
         audit) echo 'Crate: bad'; echo 'Version: 1.0.0'; echo 'ID: RUSTSEC-2025-0001'; echo 'error: 1 vulnerability found'; exit 1;;\n\
         tree) case \"$*\" in\n\
           *bad@1.0.0*normal,build*) {answer};;\n\
           *normal,build*) echo 'error: package ID specification is ambiguous' >&2; exit 101;;\n\
           *) printf 'bad v1.0.0\\n└── devtool v0.1.0 (/repo/devtool)\\n'; exit 0;;\n\
         esac;;\n\
         esac\nexit 0"
    )
}

/// A repository that has opted every audit in: each audit runs only where
/// its lockfile is tracked, so the fixture carries all four. Content is
/// irrelevant — the fake tools on PATH decide, and parsing is what these
/// tests pin.
fn repo() -> Repo {
    let r = bare_repo();
    for lockfile in [
        "Cargo.lock",
        "package-lock.json",
        "go.sum",
        "requirements.txt",
    ] {
        r.stage(lockfile, "# fixture\n");
    }
    r.commit("chore: opt every audit in");
    r
}

/// No lockfile at all: what a repository in some other language looks like
/// to the audits.
fn bare_repo() -> Repo {
    let r = Repo::new();
    r.stage("a.txt", "x\n");
    r.commit("chore: base");
    r
}

/// The point of the whole design: the same finding is a warning on a
/// branch and a refusal on a release.
#[test]
fn vulnerabilities_warn_on_a_branch_and_refuse_a_v_tag() {
    let r = repo();
    shim(&r, "cargo-audit", "exit 0"); // exists, so the check proceeds
    shim(&r, "cargo", &cargo_reaching("app v0.1.0 (/repo/app)"));

    let (code, out) = push_check(&r, "pre-push-audit-rust", "refs/heads/feat/x");
    assert_eq!(code, 0, "a branch push must not block: {out}");
    assert!(out.contains("will BLOCK a v* tag push"), "{out}");
    assert!(out.contains("RUSTSEC-2025-0001"), "{out}");

    let (code, out) = push_check(&r, "pre-push-audit-rust", "refs/tags/v1.0.0");
    assert_ne!(code, 0, "a release does not ship with these: {out}");
    assert!(out.contains("does not ship"), "{out}");
}

/// A tag that merely starts with the letter v is not a release.
#[test]
fn a_vendor_tag_is_not_a_release() {
    let r = repo();
    shim(&r, "cargo-audit", "exit 0");
    shim(&r, "cargo", "echo 'ID: RUSTSEC-2025-0002'\nexit 1");
    let (code, out) = push_check(&r, "pre-push-audit-rust", "refs/tags/vendor-drop");
    assert_eq!(code, 0, "{out}");
}

/// Clean trees pass a release push, and say so.
#[test]
fn a_clean_tree_passes_a_tag_push() {
    let r = repo();
    shim(&r, "cargo-audit", "exit 0");
    shim(&r, "cargo", "echo 'ok, 312 crates checked'\nexit 0");
    let (code, out) = push_check(&r, "pre-push-audit-rust", "refs/tags/v2.0.0");
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("no known vulnerabilities"), "{out}");
}

/// Warning-class advisories never block, even on a release — the tree
/// carries unmaintained crates today and a gate nothing passes gets
/// deleted.
#[test]
fn warning_class_advisories_do_not_block_a_release() {
    let r = repo();
    shim(&r, "cargo-audit", "exit 0");
    shim(
        &r,
        "cargo",
        "echo 'warning: unmaintained RUSTSEC-2024-0436 paste'\nexit 0",
    );
    let (code, out) = push_check(&r, "pre-push-audit-rust", "refs/tags/v3.0.0");
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("RUSTSEC-2024-0436"),
        "named, not hidden: {out}"
    );
}

/// A missing tool is loud and non-blocking — the offline case must not
/// teach --no-verify.
#[test]
fn a_missing_audit_tool_warns_and_never_blocks() {
    // No shims at all: cargo-audit absent from the fake dir. The lockfile
    // opts the audit in but is not one cargo-audit can read, so that on a
    // machine with a REAL cargo-audit on PATH — which the shim dir cannot
    // hide — the check takes the could-not-check path instead. Both
    // phrasings honour the same contract this test pins: loud, and never
    // blocking.
    let r = bare_repo();
    r.stage("Cargo.lock", "this is not a lockfile\n");
    r.commit("chore: unreadable lockfile");
    std::fs::create_dir_all(r.path(".git/toolshims")).unwrap();
    let (code, out) = push_check(&r, "pre-push-audit-rust", "refs/tags/v1.0.0");
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("did NOT run") || out.contains("could not run") || out.contains("NOT checked"),
        "{out}"
    );
}

/// npm's summary line decides, both ways.
#[test]
fn npm_audit_summary_decides_both_ways() {
    let r = repo();
    shim(
        &r,
        "npm",
        "echo 'found 3 vulnerabilities (1 moderate, 2 high)'\nexit 1",
    );
    let (code, out) = push_check(&r, "pre-push-audit-js", "refs/tags/v1.0.0");
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("found 3 vulnerabilities"), "{out}");

    // npm 7+ dropped the verb — the summary every current npm prints. Until
    // the parser learned it, a real finding read as "could not complete"
    // and a v* tag shipped over it: the audit check was fail-open exactly
    // when it had something to say.
    shim(
        &r,
        "npm",
        "echo '17 vulnerabilities (8 moderate, 9 high)'\nexit 1",
    );
    let (code, out) = push_check(&r, "pre-push-audit-js", "refs/tags/v1.0.0");
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("17 vulnerabilities"), "{out}");
    assert!(!out.contains("could not complete"), "{out}");

    shim(&r, "npm", "echo 'found 0 vulnerabilities'\nexit 0");
    let (code, out) = push_check(&r, "pre-push-audit-js", "refs/tags/v1.0.0");
    assert_eq!(code, 0, "{out}");
}

/// govulncheck's GO- ids decide; the exit code says whether the analysed
/// code is affected or the finding is informational.
#[test]
fn govulncheck_ids_decide_both_ways() {
    let r = repo();
    shim(
        &r,
        "govulncheck",
        "echo 'Vulnerability #1: GO-2022-0969'\nexit 3",
    );
    let (code, out) = push_check(&r, "pre-push-audit-go", "refs/tags/v1.0.0");
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("GO-2022-0969"), "{out}");

    shim(
        &r,
        "govulncheck",
        "echo 'No vulnerabilities found.'\nexit 0",
    );
    let (code, out) = push_check(&r, "pre-push-audit-go", "refs/tags/v1.0.0");
    assert_eq!(code, 0, "{out}");

    // Informational — the module is vulnerable, the code never calls it:
    // named, never blocking, even on a release tag.
    shim(
        &r,
        "govulncheck",
        "echo '=== Informational ==='\necho 'Vulnerability #1: GO-2023-1840'\nexit 0",
    );
    let (code, out) = push_check(&r, "pre-push-audit-go", "refs/tags/v1.0.0");
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("GO-2023-1840"), "{out}");
}

/// pip-audit's closing sentence decides.
#[test]
fn pip_audit_sentence_decides() {
    let r = repo();
    // The pip-style path: a requirements file is what gets audited.
    std::fs::write(r.path("requirements.txt"), "requests==2.0.0\n").expect("write");
    shim(
        &r,
        "pip-audit",
        "echo 'Found 2 known vulnerabilities in 1 package'\nexit 1",
    );
    let (code, out) = push_check(&r, "pre-push-audit-python", "refs/tags/v1.0.0");
    assert_ne!(code, 0, "{out}");

    shim(
        &r,
        "pip-audit",
        "echo 'No known vulnerabilities found'\nexit 0",
    );
    let (code, out) = push_check(&r, "pre-push-audit-python", "refs/tags/v1.0.0");
    assert_eq!(code, 0, "{out}");
}

/// A uv project has no `requirements.txt` — it has a venv. Before this, the
/// check looked only for the file, found none, and reported "the audit did
/// NOT run" on every push forever; a whole fleet of Python repositories was
/// in that state with nobody told. Driven through the real binary, so the
/// argv it builds is what is being pinned.
#[test]
fn a_uv_project_audits_its_venv_not_a_missing_requirements_file() {
    let r = bare_repo();
    r.stage("pyproject.toml", "[project]\nname = \"x\"\n");
    r.commit("chore: uv project");
    std::fs::create_dir_all(r.path(".venv/lib/python3.13/site-packages")).expect("mkdir");
    // The shim proves the tool was actually invoked: it only speaks when
    // asked about a --path, so reaching this output means the venv route
    // was taken rather than the requirements one.
    shim(
        &r,
        "pip-audit",
        "case \"$*\" in *--path*) echo 'Found 3 known vulnerabilities in 2 packages'; exit 1;; \
         *) echo 'wrong invocation: '\"$*\"; exit 2;; esac",
    );
    let (code, out) = push_check(&r, "pre-push-audit-python", "refs/tags/v1.0.0");
    assert_ne!(code, 0, "a release must refuse a vulnerable tree: {out}");
    assert!(
        out.contains("Found 3 known vulnerabilities"),
        "the tool's own sentence should decide: {out}"
    );

    // A Python project with neither a requirements file nor a venv to
    // audit: it still says so rather than inventing one.
    let bare = bare_repo();
    bare.stage("pyproject.toml", "[project]\nname = \"x\"\n");
    bare.commit("chore: uv project, no venv");
    shim(
        &bare,
        "pip-audit",
        "echo 'No known vulnerabilities found'\nexit 0",
    );
    let (code, out) = push_check(&bare, "pre-push-audit-python", "refs/tags/v1.0.0");
    assert_eq!(code, 0, "unavailable never blocks: {out}");
    assert!(
        out.contains("did NOT run"),
        "silence would be the bug this fixes: {out}"
    );
}

/// The registry calls an audit "inert here — needs go.sum" in a repository
/// without one; the push gate used to run it anyway, find govulncheck
/// missing, and warn on every push that it "could not run". A repository
/// in another language is not an audit that could not run: it is nothing
/// to audit, and nothing is said.
#[test]
fn an_audit_whose_lockfile_the_repository_lacks_is_silent_not_could_not_run() {
    let r = bare_repo();
    r.stage("Cargo.lock", "# only rust here\n");
    r.commit("chore: rust only");
    // Every tool present and vulnerable-by-default: if any of the three
    // foreign audits ran, it would say so loudly.
    for tool in ["govulncheck", "npm", "pnpm", "pip-audit"] {
        shim(
            &r,
            tool,
            "echo 'Vulnerability #1: GO-2024-0001 found 9 vulnerabilities'; exit 1",
        );
    }
    shim(
        &r,
        "cargo-audit",
        "echo 'Success No vulnerable packages found'; exit 0",
    );
    shim(
        &r,
        "cargo",
        "echo 'Success No vulnerable packages found'; exit 0",
    );
    for check in [
        "pre-push-audit-go",
        "pre-push-audit-js",
        "pre-push-audit-python",
    ] {
        let (code, out) = push_check(&r, check, "refs/tags/v1.0.0");
        assert_eq!(code, 0, "{check}: inert never blocks: {out}");
        assert!(
            !out.contains("could not run")
                && !out.contains("did NOT run")
                && !out.contains("NOT checked")
                && !out.contains("vulnerabilit"),
            "{check} has no lockfile here and should say nothing: {out}"
        );
    }
    // The one audit the repository DID opt into still runs.
    let (code, out) = push_check(&r, "pre-push-audit-rust", "refs/tags/v1.0.0");
    assert_eq!(code, 0, "{out}");
    assert!(
        !out.contains("could not run") && !out.contains("did NOT run"),
        "audit-rust has its lockfile and a tool: {out}"
    );
}

/// npm resolves a project from its own directory. A repository whose
/// packages live in subdirectories has no lockfile at the root, so the one
/// root-level `npm audit` answered ENOLOCK and the check said "could not
/// complete" — a tree with 17 real vulnerabilities (cluster-vision's web/)
/// was never audited. Each lockfile's directory is audited, and a finding
/// in any of them decides.
#[test]
fn audit_js_runs_in_every_lockfile_directory() {
    let r = bare_repo();
    r.stage("web/package-lock.json", "{}\n");
    r.stage("mcp/package-lock.json", "{}\n");
    r.commit("chore: two npm projects, none at the root");

    // The fake npm answers by the directory it runs in, and refuses the
    // root like the real one (no lockfile there).
    shim(
        &r,
        "npm",
        r#"case "$PWD" in
  */web) echo '17 vulnerabilities (8 moderate, 9 high)'; exit 1 ;;
  */mcp) echo 'found 0 vulnerabilities'; exit 0 ;;
  *) echo 'npm error code ENOLOCK' >&2; exit 1 ;;
esac"#,
    );
    let (code, out) = push_check(&r, "pre-push-audit-js", "refs/tags/v1.0.0");
    assert_ne!(
        code, 0,
        "a v* tag must not ship over web/'s findings: {out}"
    );
    assert!(out.contains("web: 17 vulnerabilities"), "{out}");
    assert!(!out.contains("could not complete"), "{out}");

    shim(
        &r,
        "npm",
        r#"case "$PWD" in
  */web|*/mcp) echo 'found 0 vulnerabilities'; exit 0 ;;
  *) echo 'npm error code ENOLOCK' >&2; exit 1 ;;
esac"#,
    );
    let (code, out) = push_check(&r, "pre-push-audit-js", "refs/tags/v1.0.0");
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("could not complete"), "{out}");
}

/// The same bug, still live for Rust (amont#305): a repository whose only
/// `Cargo.lock` is in a subdirectory — a Tauri app's `apps/ui/src-tauri` —
/// was audited at the root, where cargo-audit exits 2 ("entity not found"),
/// and the check said "could not complete" with no reason, over a tree that
/// carried 19 known vulnerabilities.
#[test]
fn audit_rust_runs_in_every_lockfile_directory() {
    let r = bare_repo();
    r.stage("apps/ui/src-tauri/Cargo.lock", "# fixture\n");
    r.commit("chore: a nested cargo project, none at the root");
    shim(&r, "cargo-audit", "exit 0");
    shim(
        &r,
        "cargo",
        r#"case "$1" in
  audit) case "$PWD" in
    */src-tauri) echo 'Crate: bad'; echo 'Version: 1.0.0'; echo 'ID: RUSTSEC-2025-0001'; echo 'error: 1 vulnerability found'; exit 1 ;;
    *) echo 'error: I/O operation failed: entity not found' >&2; exit 2 ;;
  esac ;;
  tree) printf 'bad v1.0.0\n└── app v0.1.0 (/repo/app)\n'; exit 0 ;;
esac
exit 0"#,
    );
    let (code, out) = push_check(&r, "pre-push-audit-rust", "refs/heads/feat/x");
    assert_eq!(code, 0, "a branch push must not block: {out}");
    assert!(
        out.contains("apps/ui/src-tauri: RUSTSEC-2025-0001"),
        "{out}"
    );
    assert!(!out.contains("could not complete"), "{out}");

    let (code, out) = push_check(&r, "pre-push-audit-rust", "refs/tags/v1.0.0");
    assert_ne!(
        code, 0,
        "a v* tag must not ship over the nested findings: {out}"
    );
}

/// Go and Python had the same root-only runner.
#[test]
fn audit_go_and_python_run_in_every_lockfile_directory() {
    let r = bare_repo();
    r.stage("svc/go.sum", "# fixture\n");
    r.stage("tools/requirements.txt", "# fixture\n");
    r.commit("chore: a nested go module and python project");
    shim(
        &r,
        "govulncheck",
        r#"case "$PWD" in
  */svc) echo 'Vulnerability #1: GO-2025-0001'; echo 'Your code is affected by 1 vulnerability'; exit 3 ;;
  *) echo 'go: no go.mod file found' >&2; exit 1 ;;
esac"#,
    );
    let (_, out) = push_check(&r, "pre-push-audit-go", "refs/heads/feat/x");
    assert!(out.contains("svc: GO-2025-0001"), "{out}");
    assert!(!out.contains("could not complete"), "{out}");

    shim(
        &r,
        "pip-audit",
        r#"case "$PWD" in
  */tools) echo 'Found 1 known vulnerability in 1 package'; echo 'Name Version ID Fix Versions'; echo 'jinja2 2.0 PYSEC-2025-1 3.0'; exit 1 ;;
  *) echo 'ERROR: requirements.txt not found' >&2; exit 1 ;;
esac"#,
    );
    let (_, out) = push_check(&r, "pre-push-audit-python", "refs/heads/feat/x");
    assert!(out.contains("tools: Found 1 known vulnerability"), "{out}");
    assert!(!out.contains("could not complete"), "{out}");
}

/// "Could not complete" says why: the tool's own last error line.
#[test]
fn an_audit_that_cannot_answer_names_the_tools_error() {
    let r = bare_repo();
    r.stage("Cargo.lock", "# fixture\n");
    r.commit("chore: a cargo project");
    shim(&r, "cargo-audit", "exit 0");
    shim(
        &r,
        "cargo",
        "echo 'Fetching advisory database'; echo 'error: failed to fetch advisory database: network unreachable' >&2; exit 1",
    );
    let (code, out) = push_check(&r, "pre-push-audit-rust", "refs/heads/feat/x");
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("could not complete"), "{out}");
    assert!(
        out.contains("failed to fetch advisory database: network unreachable"),
        "the reason must be printed: {out}"
    );
}

/// A pnpm workspace has no package-lock.json, so audit-js — which only knew
/// npm — never audited it and said nothing: one such tree carried 28
/// vulnerable versions, two critical, release after release. pnpm audits
/// its own lockfile; its summary decides, both ways.
#[test]
fn audit_js_audits_a_pnpm_workspace() {
    let r = bare_repo();
    r.stage("pnpm-lock.yaml", "lockfileVersion: '9.0'\n");
    r.commit("chore: a pnpm workspace");
    // npm must not be asked: there is no package-lock.json.
    shim(&r, "npm", "echo 'npm must not run here' >&2; exit 1");
    shim(
        &r,
        "pnpm",
        "echo '107 vulnerabilities found'\necho 'Severity: 6 low | 45 moderate | 54 high | 2 critical'\nexit 1",
    );

    let (code, out) = push_check(&r, "pre-push-audit-js", "refs/heads/feat/x");
    assert_eq!(code, 0, "a branch push must not block: {out}");
    assert!(out.contains("will BLOCK a v* tag push"), "{out}");
    assert!(
        out.contains(".: 107 vulnerabilities found (6 low | 45 moderate | 54 high | 2 critical)"),
        "{out}"
    );
    assert!(!out.contains("npm must not run"), "{out}");

    let (code, out) = push_check(&r, "pre-push-audit-js", "refs/tags/v1.0.0");
    assert_ne!(code, 0, "a release does not ship with these: {out}");

    shim(&r, "pnpm", "echo 'No known vulnerabilities found'\nexit 0");
    let (code, out) = push_check(&r, "pre-push-audit-js", "refs/tags/v1.0.0");
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("no known vulnerabilities"), "{out}");
}

/// Both package managers in one repository: each lockfile's directory is
/// audited by its own tool, and a finding in either decides.
#[test]
fn audit_js_audits_npm_and_pnpm_projects_side_by_side() {
    let r = bare_repo();
    r.stage("pnpm-lock.yaml", "lockfileVersion: '9.0'\n");
    r.stage("spike/package-lock.json", "{}\n");
    r.commit("chore: a pnpm workspace with an npm spike");
    shim(&r, "pnpm", "echo 'No known vulnerabilities found'\nexit 0");
    shim(
        &r,
        "npm",
        r#"case "$PWD" in
  */spike) echo '1 vulnerability (1 high)'; exit 1 ;;
  *) echo 'npm error code ENOLOCK' >&2; exit 1 ;;
esac"#,
    );
    let (code, out) = push_check(&r, "pre-push-audit-js", "refs/tags/v1.0.0");
    assert_ne!(code, 0, "the npm spike's finding still decides: {out}");
    assert!(out.contains("spike: 1 vulnerability (1 high)"), "{out}");
}

/// pnpm answers for the workspace it finds above a directory: a standalone
/// project with its own pnpm-lock.yaml inside a workspace (a spike) was
/// reported with the WORKSPACE ROOT's findings under its own name. Such a
/// project is audited with --ignore-workspace; the workspace root — which
/// tracks pnpm-workspace.yaml — is audited as the workspace, members and all.
#[test]
fn a_nested_pnpm_project_is_audited_on_its_own_lockfile() {
    let r = bare_repo();
    r.stage("pnpm-workspace.yaml", "packages:\n  - apps/*\n");
    r.stage("pnpm-lock.yaml", "lockfileVersion: '9.0'\n");
    r.stage("spikes/s3/pnpm-lock.yaml", "lockfileVersion: '9.0'\n");
    r.commit("chore: a workspace with a standalone spike");
    // The fake pnpm answers like the real one: without --ignore-workspace it
    // reports the workspace, wherever it runs.
    shim(
        &r,
        "pnpm",
        r#"case "$*" in
  *--ignore-workspace*) case "$PWD" in
      */spikes/s3) echo '1 vulnerabilities found'; echo 'Severity: 1 moderate'; exit 1 ;;
      *) echo 'root audited with --ignore-workspace' >&2; exit 2 ;;
    esac ;;
  *) echo '107 vulnerabilities found'; echo 'Severity: 2 critical'; exit 1 ;;
esac"#,
    );
    let (_, out) = push_check(&r, "pre-push-audit-js", "refs/heads/feat/x");
    assert!(
        out.contains(".: 107 vulnerabilities found (2 critical)"),
        "the root is audited as the workspace: {out}"
    );
    assert!(
        out.contains("spikes/s3: 1 vulnerabilities found (1 moderate)"),
        "the spike reads its own lockfile: {out}"
    );
    assert!(!out.contains("spikes/s3: 107"), "{out}");
    assert!(
        !out.contains("root audited with --ignore-workspace"),
        "{out}"
    );
}

/// A release blocks on what it SHIPS. A finding only in the dev tree — the
/// registry builder's ts-morph pulling an unpatched `braces` — is named, but
/// nobody installing the package installs it, so the tag goes out.
#[test]
fn a_release_is_not_blocked_by_dev_only_findings() {
    let r = bare_repo();
    r.stage("pnpm-lock.yaml", "lockfileVersion: '9.0'\n");
    r.commit("chore: a pnpm workspace");
    shim(
        &r,
        "pnpm",
        "case \" $* \" in *' --prod '*) echo 'No known vulnerabilities found'; exit 0;; esac\n\
         echo '5 vulnerabilities found'\necho 'Severity: 4 moderate | 1 high'\nexit 1",
    );
    let (code, out) = push_check(&r, "pre-push-audit-js", "refs/tags/v4.3.0");
    assert_eq!(
        code, 0,
        "dev-only findings must not refuse a release: {out}"
    );
    assert!(out.contains("only in development dependencies"), "{out}");
    assert!(out.contains("5 vulnerabilities found"), "{out}");
}

/// What ships still blocks, and the refusal shows the production audit.
#[test]
fn a_release_is_blocked_by_shipped_findings() {
    let r = bare_repo();
    r.stage("pnpm-lock.yaml", "lockfileVersion: '9.0'\n");
    r.commit("chore: a pnpm workspace");
    shim(
        &r,
        "pnpm",
        "case \" $* \" in *' --prod '*) echo '1 vulnerabilities found'; echo 'Severity: 1 high'; exit 1;; esac\n\
         echo '5 vulnerabilities found'\necho 'Severity: 4 moderate | 1 high'\nexit 1",
    );
    let (code, out) = push_check(&r, "pre-push-audit-js", "refs/tags/v4.3.0");
    assert_ne!(code, 0, "a shipped vulnerability refuses the tag: {out}");
    assert!(out.contains("1 vulnerabilities found (1 high)"), "{out}");
    assert!(out.contains("audit --prod"), "{out}");
}

/// A production re-audit that cannot answer is not a pass: the full
/// finding still blocks.
#[test]
fn a_release_whose_prod_audit_cannot_answer_still_blocks() {
    let r = bare_repo();
    r.stage("pnpm-lock.yaml", "lockfileVersion: '9.0'\n");
    r.commit("chore: a pnpm workspace");
    shim(
        &r,
        "pnpm",
        "case \" $* \" in *' --prod '*) echo 'ERR_PNPM_AUDIT_BAD_RESPONSE' >&2; exit 1;; esac\n\
         echo '5 vulnerabilities found'\necho 'Severity: 4 moderate | 1 high'\nexit 1",
    );
    let (code, out) = push_check(&r, "pre-push-audit-js", "refs/tags/v4.3.0");
    assert_ne!(code, 0, "an unknown is not a pass: {out}");
}

/// npm takes `--omit=dev` for the same question.
#[test]
fn an_npm_release_audits_what_it_ships() {
    let r = bare_repo();
    r.stage("package-lock.json", "{}\n");
    r.commit("chore: an npm project");
    shim(
        &r,
        "npm",
        "case \" $* \" in *' --omit=dev '*) echo 'found 0 vulnerabilities'; exit 0;; esac\n\
         echo '2 vulnerabilities (1 moderate, 1 high)'\nexit 1",
    );
    let (code, out) = push_check(&r, "pre-push-audit-js", "refs/tags/v1.0.0");
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("only in development dependencies"), "{out}");

    // A branch push is unchanged: named, never blocking, no second audit.
    let (code, out) = push_check(&r, "pre-push-audit-js", "refs/heads/feat/x");
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("will BLOCK a v* tag push"), "{out}");
}

/// Rust: a crate only a dev-dependency reaches does not refuse a release.
#[test]
fn a_rust_release_is_not_blocked_by_a_dev_only_crate() {
    let r = repo();
    shim(&r, "cargo-audit", "exit 0");
    shim(&r, "cargo", &cargo_reaching(""));
    let (code, out) = push_check(&r, "pre-push-audit-rust", "refs/tags/v1.0.0");
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("only in development dependencies"), "{out}");
    assert!(out.contains("RUSTSEC-2025-0001"), "{out}");
}

/// Python: a vulnerable package outside `uv export --no-dev` is dev-only.
#[test]
fn a_python_release_is_audited_on_what_uv_ships() {
    let r = bare_repo();
    r.stage("pyproject.toml", "[project]\nname = 'app'\n");
    r.stage("uv.lock", "version = 1\n");
    r.commit("chore: a uv project");
    std::fs::create_dir_all(r.path(".venv/lib/python3.12/site-packages")).unwrap();
    let table = "echo 'Name    Version ID                  Fix Versions'\n\
                 echo '------- ------- ------------------- ------------'\n\
                 echo 'pytest_cov 4.0.0 GHSA-aaaa-bbbb-cccc 4.1.0'\n\
                 echo 'Found 1 known vulnerability in 1 package'\nexit 1";
    shim(&r, "pip-audit", table);
    shim(
        &r,
        "uv",
        "echo 'requests==2.32.3'\necho '    # via app'\nexit 0",
    );
    let (code, out) = push_check(&r, "pre-push-audit-python", "refs/tags/v1.0.0");
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("only in development dependencies"), "{out}");
    assert!(out.contains("pytest-cov"), "{out}");

    // The same package in the production set refuses the tag.
    shim(&r, "uv", "echo 'pytest-cov==4.0.0'\nexit 0");
    let (code, out) = push_check(&r, "pre-push-audit-python", "refs/tags/v1.0.0");
    assert_ne!(code, 0, "{out}");

    // No uv to ask: an unknown is not a pass.
    std::fs::remove_file(r.path(".git/toolshims/uv")).unwrap();
    let (code, out) = push_check(&r, "pre-push-audit-python", "refs/tags/v1.0.0");
    assert_ne!(code, 0, "{out}");
}

/// Go is audited on what ships already: no `-test`, so test-only code and
/// its imports are never counted.
#[test]
fn go_is_audited_without_test_code() {
    let r = repo();
    shim(
        &r,
        "govulncheck",
        "case \" $* \" in *' -test '*) echo 'must not audit tests'; exit 3;; esac\necho 'No vulnerabilities found.'\nexit 0",
    );
    let (code, out) = push_check(&r, "pre-push-audit-go", "refs/tags/v1.0.0");
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("must not audit tests"), "{out}");
}

/// `YYYY-MM-DD`, `offset` days from today (UTC) — Hinnant's civil_from_days.
fn date_in(offset: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        / 86_400;
    let z = now + offset + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// A pnpm workspace whose PRODUCTION audit names one GHSA advisory.
fn shipped_braces_repo(waivers: Option<String>) -> Repo {
    let r = bare_repo();
    r.stage("pnpm-lock.yaml", "lockfileVersion: '9.0'\n");
    if let Some(w) = waivers {
        r.stage(".amont-audit-waivers", &w);
    }
    r.commit("chore: a pnpm workspace");
    shim(
        &r,
        "pnpm",
        "echo '│ More info │ https://github.com/advisories/GHSA-3gc7-fjrx-p6mg │'\n\
         echo '1 vulnerabilities found'\necho 'Severity: 1 high'\nexit 1",
    );
    r
}

/// An unfixable shipped advisory passes a release under a dated, reasoned
/// waiver — and the push says so.
#[test]
fn a_waived_advisory_ships_until_its_waiver_expires() {
    let r = shipped_braces_repo(Some(format!(
        "# id expires reason\nGHSA-3gc7-fjrx-p6mg {} braces via react-strict-dom; no patched version\n",
        date_in(30)
    )));
    let (code, out) = push_check(&r, "pre-push-audit-js", "refs/tags/v4.3.0");
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("under a reviewed waiver"), "{out}");
    assert!(out.contains("GHSA-3gc7-fjrx-p6mg until"), "{out}");
}

/// Expired, or waiving some other advisory: the tag is refused, and why.
#[test]
fn an_expired_or_unrelated_waiver_refuses_the_release() {
    let r = shipped_braces_repo(Some(format!(
        "GHSA-3gc7-fjrx-p6mg {} braces; no patch\n",
        date_in(-1)
    )));
    let (code, out) = push_check(&r, "pre-push-audit-js", "refs/tags/v4.3.0");
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("expired on"), "{out}");
    assert!(out.contains("not waived: GHSA-3gc7-fjrx-p6mg"), "{out}");

    let r = shipped_braces_repo(Some(format!(
        "GHSA-aaaa-bbbb-cccc {} something else\n",
        date_in(30)
    )));
    let (code, out) = push_check(&r, "pre-push-audit-js", "refs/tags/v4.3.0");
    assert_ne!(code, 0, "{out}");

    let r = shipped_braces_repo(None);
    let (code, _) = push_check(&r, "pre-push-audit-js", "refs/tags/v4.3.0");
    assert_ne!(code, 0);
}

/// A shipped-edges tree that cannot be read is not "dev-only": cargo
/// erroring (an ambiguous spec, a stale lock) prints no local crate either.
#[test]
fn a_rust_release_blocks_when_the_tree_cannot_answer() {
    let r = repo();
    shim(&r, "cargo-audit", "exit 0");
    shim(
        &r,
        "cargo",
        "case \"$1\" in\n\
         audit) echo 'Crate: bad'; echo 'Version: 1.0.0'; echo 'ID: RUSTSEC-2025-0001'; echo 'error: 1 vulnerability found'; exit 1;;\n\
         tree) echo 'error: failed to load manifest' >&2; exit 101;;\n\
         esac\nexit 0",
    );
    let (code, out) = push_check(&r, "pre-push-audit-rust", "refs/tags/v1.0.0");
    assert_ne!(code, 0, "an unknown is not a pass: {out}");
}

/// A waiver never vouches for a project the audit could not check.
#[test]
fn a_waiver_does_not_cover_an_unchecked_project() {
    let r = bare_repo();
    r.stage("pnpm-workspace.yaml", "packages:\n  - apps/*\n");
    r.stage("pnpm-lock.yaml", "lockfileVersion: '9.0'\n");
    r.stage("spikes/s3/pnpm-lock.yaml", "lockfileVersion: '9.0'\n");
    r.stage(
        ".amont-audit-waivers",
        &format!("GHSA-3gc7-fjrx-p6mg {} braces; no patch\n", date_in(30)),
    );
    r.commit("chore: a workspace with a spike");
    shim(
        &r,
        "pnpm",
        "case \" $* \" in *' --ignore-workspace '*) echo 'ERR_PNPM_AUDIT_BAD_RESPONSE' >&2; exit 1;; esac\n\
         echo 'https://github.com/advisories/GHSA-3gc7-fjrx-p6mg'\n\
         echo '1 vulnerabilities found'\necho 'Severity: 1 high'\nexit 1",
    );
    let (code, out) = push_check(&r, "pre-push-audit-js", "refs/tags/v1.0.0");
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("NOT checked"), "{out}");
}

/// A branch push names the waiver instead of promising a refusal.
#[test]
fn a_branch_push_names_the_waiver() {
    let r = shipped_braces_repo(Some(format!(
        "GHSA-3gc7-fjrx-p6mg {} braces; no patch\n",
        date_in(30)
    )));
    let (code, out) = push_check(&r, "pre-push-audit-js", "refs/heads/feat/x");
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("a v* tag push will pass while it holds"),
        "{out}"
    );
    assert!(!out.contains("will BLOCK"), "{out}");
}
