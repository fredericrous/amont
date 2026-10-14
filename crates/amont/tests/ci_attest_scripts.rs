//! The scripts CI uses to decide what a signature already covers.
//!
//! They sit between a signed note and a skipped job, so a bug in one is a skip
//! nobody checked. The properties pinned here: nothing is covered unless a
//! verified block named the gate on the right platform; a result is signed only
//! for the tree the signing job stands on, on the platform the workflow
//! declares, and only when it succeeded; every doubt reads as "run it".

mod common;

use common::{missing, Repo};
use std::path::{Path, PathBuf};
use std::process::Command;

fn script(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts")
        .join(name)
}

/// Run `name` with `env`, in `dir`, and return (exit code, GITHUB_OUTPUT text,
/// stdout).
fn run(name: &str, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
    static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let out = std::env::temp_dir().join(format!(
        "ci-attest-{}-{}-{}.out",
        std::process::id(),
        CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        name.replace('.', "-")
    ));
    let _ = std::fs::remove_file(&out);
    std::fs::write(&out, "").unwrap();
    let mut cmd = Command::new("sh");
    cmd.arg(script(name))
        .args(args)
        .current_dir(dir)
        .env("GITHUB_OUTPUT", &out)
        .env("GITHUB_STEP_SUMMARY", "/dev/null")
        .env("RUNNER_TEMP", dir)
        .env_remove("EVENT")
        .env_remove("LINUX")
        .env_remove("MACOS")
        .env_remove("WINDOWS");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let o = cmd.output().expect("run script");
    let outputs = std::fs::read_to_string(&out).unwrap_or_default();
    let _ = std::fs::remove_file(&out);
    (
        o.status.code().unwrap_or(-1),
        outputs,
        String::from_utf8_lossy(&o.stdout).into_owned(),
    )
}

fn value(outputs: &str, key: &str) -> String {
    outputs
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("no {key} in:\n{outputs}"))
        .to_string()
}

fn verify(env: &[(&str, &str)]) -> (String, String) {
    let dir = std::env::temp_dir();
    let (code, outputs, stdout) = run("ci-attest-verify-plan.sh", &dir, &[], env);
    assert_eq!(code, 0, "{stdout}");
    (outputs, stdout)
}

const ALL_LINUX: &str = r#"["ci-rust","ci-msrv","ci-deps","ci-hooks-ubuntu"]"#;

#[test]
fn nothing_covered_runs_everything_and_says_why() {
    if missing("jq") {
        return;
    }
    let (o, said) = verify(&[("EVENT", "pull_request")]);
    for k in [
        "rust_covered",
        "msrv_covered",
        "deps_covered",
        "windows_test_covered",
    ] {
        assert_eq!(value(&o, k), "false", "{k}");
    }
    assert_eq!(value(&o, "test_any"), "true");
    assert!(
        said.contains("ci-rust: running, no valid signature"),
        "{said}"
    );
    assert!(value(&o, "test_matrix").contains("macos-latest"));
}

#[test]
fn everything_covered_skips_every_job_and_says_why() {
    if missing("jq") {
        return;
    }
    let (o, said) = verify(&[
        ("EVENT", "pull_request"),
        ("LINUX", ALL_LINUX),
        ("MACOS", r#"["ci-hooks-macos"]"#),
        ("WINDOWS", r#"["ci-windows-test"]"#),
    ]);
    for k in [
        "rust_covered",
        "msrv_covered",
        "deps_covered",
        "windows_test_covered",
    ] {
        assert_eq!(value(&o, k), "true", "{k}");
    }
    assert_eq!(value(&o, "test_matrix"), "[]");
    assert_eq!(value(&o, "test_any"), "false");
    assert!(
        said.contains(
            "ci-hooks-macos: skipped, a signature covers this tree's declared inputs (ADR-0024)"
        ),
        "{said}"
    );
}

#[test]
fn only_the_uncovered_test_leg_runs() {
    if missing("jq") {
        return;
    }
    let (o, _) = verify(&[("EVENT", "push"), ("LINUX", ALL_LINUX)]);
    assert_eq!(
        value(&o, "test_matrix"),
        r#"[{"os":"macos-latest","gate":"ci-hooks-macos"}]"#
    );
    assert_eq!(value(&o, "test_any"), "true");
}

#[test]
fn a_scheduled_run_covers_nothing() {
    if missing("jq") {
        return;
    }
    let (o, said) = verify(&[
        ("EVENT", "schedule"),
        ("LINUX", ALL_LINUX),
        ("MACOS", r#"["ci-hooks-macos"]"#),
        ("WINDOWS", r#"["ci-windows-test"]"#),
    ]);
    assert_eq!(value(&o, "rust_covered"), "false");
    assert_eq!(value(&o, "windows_test_covered"), "false");
    assert_eq!(value(&o, "test_any"), "true");
    assert!(said.contains("a scheduled run covers nothing"), "{said}");
}

#[test]
fn a_gate_covered_on_the_wrong_platform_covers_nothing() {
    if missing("jq") {
        return;
    }
    // ci-rust arrives in the macOS array: the verifier was asked about macOS,
    // so that is not evidence about the Linux job.
    let (o, _) = verify(&[("EVENT", "push"), ("MACOS", r#"["ci-rust"]"#)]);
    assert_eq!(value(&o, "rust_covered"), "false");
}

#[test]
fn unreadable_input_reads_as_not_covered() {
    if missing("jq") {
        return;
    }
    let (o, said) = verify(&[("EVENT", "push"), ("LINUX", "not json at all")]);
    assert_eq!(value(&o, "rust_covered"), "false");
    assert_eq!(value(&o, "test_any"), "true");
    // Said as what it is, not as "no valid signature".
    assert!(
        said.contains("ci-rust: running, the verifier's output was unreadable"),
        "{said}"
    );
    // The platform whose output WAS readable keeps its honest reason.
    assert!(
        said.contains("ci-hooks-macos: running, no valid signature"),
        "{said}"
    );
}

#[test]
fn a_local_windows_signature_still_covers_the_windows_suite() {
    if missing("jq") {
        return;
    }
    let (o, _) = verify(&[("EVENT", "push"), ("WINDOWS", r#"["pre-push-cargo-test"]"#)]);
    assert_eq!(value(&o, "windows_test_covered"), "true");
}

// ---- signing ---------------------------------------------------------------

fn repo_with_commit() -> (Repo, String) {
    let repo = Repo::new();
    repo.stage("a.txt", "a");
    repo.commit("fixture");
    let tree = String::from_utf8_lossy(&repo.git(&["rev-parse", "HEAD^{tree}"]).stdout)
        .trim()
        .to_string();
    (repo, tree)
}

/// A results directory beside the fixture repository, unique to it: tests run
/// in parallel and each must see only its own results.
fn results_name(repo: &Repo) -> String {
    let dir = repo.path("");
    format!(
        "{}-results",
        dir.file_name().and_then(|n| n.to_str()).unwrap_or("repo")
    )
}

fn result(repo: &Repo, gate: &str, platform: &str, tree: &str, outcome: &str) {
    result_in(
        repo,
        &format!("attest-result-{gate}"),
        gate,
        platform,
        tree,
        outcome,
    );
}

/// A result file whose directory (the artifact name) is `dir`, not necessarily
/// the one its gate would give it.
fn result_in(repo: &Repo, dir: &str, gate: &str, platform: &str, tree: &str, outcome: &str) {
    repo.write(
        &format!("../{}/{dir}/attest-result-{gate}.json", results_name(repo)),
        &format!(
            r#"{{"gate":"{gate}","platform":"{platform}","tree":"{tree}","sha":"x","outcome":"{outcome}"}}"#
        ),
    );
}

fn results_dir(repo: &Repo) -> PathBuf {
    repo.path(&format!("../{}", results_name(repo)))
}

fn plan(repo: &Repo) -> (String, String) {
    let dir = results_dir(repo);
    let (code, outputs, said) = run(
        "ci-attest-sign-plan.sh",
        &repo.path(""),
        &[dir.to_str().unwrap()],
        &[],
    );
    assert_eq!(code, 0, "{said}");
    (outputs, said)
}

#[test]
fn a_result_for_this_tree_on_the_declared_platform_is_signed() {
    if missing("jq") {
        return;
    }
    let (repo, tree) = repo_with_commit();
    result(&repo, "ci-rust", "x86_64-linux", &tree, "success");
    result(&repo, "ci-hooks-macos", "aarch64-macos", &tree, "success");
    result(&repo, "ci-windows-test", "x86_64-windows", &tree, "success");
    let (o, _) = plan(&repo);
    assert_eq!(value(&o, "linux"), "ci-rust");
    assert_eq!(value(&o, "macos"), "ci-hooks-macos");
    assert_eq!(value(&o, "windows"), "ci-windows-test");
    let _ = std::fs::remove_dir_all(results_dir(&repo));
}

#[test]
fn a_result_for_another_tree_is_not_signed() {
    if missing("jq") {
        return;
    }
    let (repo, _) = repo_with_commit();
    let other = "0".repeat(40);
    result(&repo, "ci-rust", "x86_64-linux", &other, "success");
    let (o, said) = plan(&repo);
    assert_eq!(value(&o, "linux"), "");
    assert!(said.contains("ci-rust: result is for tree"), "{said}");
    let _ = std::fs::remove_dir_all(results_dir(&repo));
}

#[test]
fn a_result_from_the_wrong_platform_or_a_failure_is_not_signed() {
    if missing("jq") {
        return;
    }
    let (repo, tree) = repo_with_commit();
    // The macOS suite claiming to be a Linux result, and a failed Linux job.
    result(&repo, "ci-hooks-ubuntu", "aarch64-macos", &tree, "success");
    result(&repo, "ci-deps", "x86_64-linux", &tree, "failure");
    let (o, said) = plan(&repo);
    assert_eq!(value(&o, "linux"), "");
    assert!(
        said.contains("the workflow declares 'x86_64-linux'"),
        "{said}"
    );
    assert!(said.contains("outcome 'failure'"), "{said}");
    let _ = std::fs::remove_dir_all(results_dir(&repo));
}

#[test]
fn an_unknown_gate_and_garbage_are_ignored() {
    if missing("jq") {
        return;
    }
    let (repo, tree) = repo_with_commit();
    result(&repo, "publish", "x86_64-linux", &tree, "success");
    repo.write(
        &format!(
            "../results-{}/b/attest-result-ci-rust.json",
            std::process::id()
        ),
        "not json",
    );
    let (o, said) = plan(&repo);
    assert_eq!(value(&o, "linux"), "");
    assert!(said.contains("not a gate this workflow signs"), "{said}");
    let _ = std::fs::remove_dir_all(results_dir(&repo));
}

#[test]
fn a_result_must_sit_in_the_artifact_named_for_its_gate() {
    if missing("jq") {
        return;
    }
    let (repo, tree) = repo_with_commit();
    // A result claiming ci-rust but uploaded as another gate's artifact.
    result_in(
        &repo,
        "attest-result-ci-deps",
        "ci-rust",
        "x86_64-linux",
        &tree,
        "success",
    );
    // And one whose artifact directory has no relation to any gate.
    result_in(&repo, "a", "ci-msrv", "x86_64-linux", &tree, "success");
    let (o, said) = plan(&repo);
    assert_eq!(value(&o, "linux"), "");
    assert!(
        said.contains("names gate ci-rust but is not artifact"),
        "{said}"
    );
    assert!(
        said.contains("names gate ci-msrv but is not artifact"),
        "{said}"
    );
    let _ = std::fs::remove_dir_all(results_dir(&repo));
}

#[test]
fn no_results_signs_nothing() {
    if missing("jq") {
        return;
    }
    let (repo, _) = repo_with_commit();
    let (o, _) = plan(&repo);
    assert_eq!(value(&o, "linux"), "");
    assert_eq!(value(&o, "macos"), "");
    assert_eq!(value(&o, "windows"), "");
}

#[test]
fn the_result_a_job_writes_is_the_one_the_plan_reads() {
    if missing("jq") {
        return;
    }
    let (repo, tree) = repo_with_commit();
    let (code, _, said) = run("ci-attest-result.sh", &repo.path(""), &["ci-rust"], &[]);
    assert_eq!(code, 0, "{said}");
    let file = repo.path("attest-result-ci-rust.json");
    let body = std::fs::read_to_string(&file).expect("result written");
    assert!(body.contains(&format!(r#""tree":"{tree}""#)), "{body}");
    assert!(body.contains(r#""gate":"ci-rust""#), "{body}");
    assert!(body.contains(r#""outcome":"success""#), "{body}");
    // And it is valid JSON the sign plan can parse.
    let ok = Command::new("jq")
        .args(["-e", ".tree"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(ok.status.success());
}

// ---- drift -----------------------------------------------------------------

#[test]
fn every_ci_gate_the_makefile_requires_is_known_to_every_script_and_the_workflow() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let makefile = std::fs::read_to_string(root.join("Makefile")).unwrap();
    let gates: Vec<&str> = makefile
        .split("ATTEST_GATES :=")
        .nth(1)
        .expect("ATTEST_GATES in the Makefile")
        .lines()
        .take(2)
        .flat_map(|l| l.trim_end_matches('\\').split_whitespace())
        .filter(|g| g.starts_with("ci-"))
        .collect();
    assert_eq!(gates.len(), 6, "{gates:?}");
    let sign = std::fs::read_to_string(script("ci-attest-sign-plan.sh")).unwrap();
    let verify = std::fs::read_to_string(script("ci-attest-verify-plan.sh")).unwrap();
    let workflow = std::fs::read_to_string(root.join(".github/workflows/ci.yaml")).unwrap();
    let spec = std::fs::read_to_string(root.join(".github/attest-inputs")).unwrap();
    for g in gates {
        assert!(sign.contains(g), "{g} is not in ci-attest-sign-plan.sh");
        assert!(verify.contains(g), "{g} is not in ci-attest-verify-plan.sh");
        assert!(workflow.contains(g), "{g} is not in ci.yaml");
        assert!(
            spec.lines().any(|l| l.starts_with(&format!("{g} "))),
            "{g} is not declared in .github/attest-inputs"
        );
    }
}
