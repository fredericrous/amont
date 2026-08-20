//! The three Kubernetes hooks: argo-lint, kube-linter, kubeconform.
//!
//! All are SOFT gates — a missing toolchain warns and skips rather than
//! blocking a commit, because CI is the hard gate and not every developer has
//! kustomize/kubeconform/argo installed.

use super::common::{fail, hl, ok, program, repo_root, staged_files, warn, which};
use crate::check::Outcome;
use std::path::Path;
use std::process::{Command, Stdio};

/// The extensions all three Kubernetes checks consume. Exported so
/// `registry.rs` declares their scopes from the same constant — see
/// `lint_json_yaml::EXTS` for the drift this prevents.
pub const EXTS: &[&str] = &[".yaml", ".yml"];

/// Staged YAML under a kubernetes-ish prefix. Deliberately conservative and
/// shared by all three hooks, so they trigger on exactly the same change sets.
fn k8s_staged() -> Vec<String> {
    const PREFIXES: [&str; 7] = [
        "kubernetes/",
        "manifests/",
        "chart/",
        "charts/",
        "k8s/",
        "helm/",
        "deploy/",
    ];
    staged_files(EXTS)
        .into_iter()
        .filter(|f| PREFIXES.iter().any(|p| f.starts_with(p)))
        .collect()
}

const ARGO_KINDS: [&str; 4] = [
    "Workflow",
    "CronWorkflow",
    "WorkflowTemplate",
    "ClusterWorkflowTemplate",
];

/// `^kind: <one of the Argo kinds>$` — anchored per line, so a `kind:` nested
/// in a template body or a longer word does not qualify.
pub fn declares_argo_kind(content: &str) -> bool {
    content.lines().any(|line| {
        line.strip_prefix("kind: ")
            .map(|k| ARGO_KINDS.contains(&k.trim_end()))
            .unwrap_or(false)
    })
}

pub fn argo_lint(_args: &[std::ffi::OsString]) -> Outcome {
    let staged = k8s_staged();
    if staged.is_empty() {
        return Outcome::Passed;
    }
    let root = repo_root();
    let workflows: Vec<String> = staged
        .into_iter()
        .filter(|file| {
            std::fs::read_to_string(Path::new(&root).join(file))
                .map(|c| declares_argo_kind(&c))
                .unwrap_or(false)
        })
        .collect();
    if workflows.is_empty() {
        return Outcome::Passed;
    }
    if which("argo").is_none() {
        warn(&format!(
            "Argo workflow manifests staged. Skipping argo lint; install: {}",
            hl("argo")
        ));
        return Outcome::Unavailable;
    }
    // --offline: no cluster needed, inline templates only. Flux ${VAR}
    // postBuild placeholders are inert strings to the linter; Argo {{…}}
    // templating is what it actually checks.
    // `--` before the paths: a workflow file named e.g. `-canary.yaml` would
    // otherwise be read as a flag by argo's own parser.
    let mut argv = vec![
        "lint".to_string(),
        "--offline".to_string(),
        "--".to_string(),
    ];
    argv.extend(workflows.iter().cloned());
    let mut cmd = Command::new(program("argo"));
    cmd.args(&argv).current_dir(&root).stdin(Stdio::null());
    let okd = super::common::bounded_success(&mut cmd, "argo");
    if !okd {
        fail("argo lint failed (output above)");
        return Outcome::Failed;
    }
    let n = workflows.len();
    ok(&format!(
        "argo lint passed ({n} workflow manifest{})",
        if n > 1 { "s" } else { "" }
    ));
    Outcome::Passed
}

/// Repo-root `.kube-linter*.yaml` / `.yml`, sorted for a stable run order.
pub fn kube_linter_configs(root: &str) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out: Vec<String> = rd
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with(".kube-linter") && (n.ends_with(".yaml") || n.ends_with(".yml")))
        .collect();
    out.sort();
    out
}

pub fn kube_linter(_args: &[std::ffi::OsString]) -> Outcome {
    if k8s_staged().is_empty() {
        return Outcome::Passed;
    }
    let root = repo_root();
    // Stock kube-linter rules are too noisy to enforce generically, so a
    // repo-local config is the opt-in signal — and it is tested before the
    // binary, so a repo that never opted in is not told to install a linter it
    // does not use, and does not report a gap it does not have.
    let configs = kube_linter_configs(&root);
    if configs.is_empty() {
        // SILENT, like `yamllint::run` in exactly this situation. It used to
        // print a skip notice, which fires on EVERY commit that touches
        // `kubernetes/**.yaml` in a repository that has no `.kube-linter*.yaml`
        // and never will — the same "a repo that never wanted yamllint was told
        // to install it" noise `docs/hook-architecture.md` records these three
        // checks being fixed for.
        return Outcome::Passed;
    }
    if which("kube-linter").is_none() {
        warn(&format!(
            "This repo configures kube-linter but it is not installed. Install {}",
            hl("kube-linter")
        ));
        return Outcome::Unavailable;
    }
    // One run per config: each config's own `excludes:` and scope (set inside
    // the YAML, not on the CLI) decide which manifests it applies to, so
    // apps-vs-infra splits work without per-hook wiring.
    let mut overall = 0;
    for cfg in &configs {
        let mut cmd = Command::new(program("kube-linter"));
        cmd.args(["lint", ".", "--config", cfg])
            .current_dir(&root)
            .stdin(Stdio::null());
        let okd = super::common::bounded_success(&mut cmd, "kube-linter");
        if !okd {
            fail(&format!("kube-linter ({cfg}) found issues"));
            overall = 1;
        }
    }
    if overall != 0 {
        return Outcome::Failed;
    }
    let n = configs.len();
    ok(&format!(
        "kube-linter passed ({n} config{})",
        if n > 1 { "s" } else { "" }
    ));
    Outcome::Passed
}

/// Walk up from each staged file until a directory holding kustomization.yaml
/// (or .yml) is found, or the repo root is reached. Deduped, stable order.
pub fn kustomization_roots(root: &str, staged: &[String]) -> Vec<String> {
    let mut roots: Vec<String> = Vec::new();
    for f in staged {
        let mut dir = Path::new(f).parent();
        while let Some(d) = dir {
            let s = d.to_string_lossy().to_string();
            if s.is_empty() || s == "." {
                break;
            }
            let base = Path::new(root).join(&s);
            if base.join("kustomization.yaml").is_file() || base.join("kustomization.yml").is_file()
            {
                if !roots.contains(&s) {
                    roots.push(s);
                }
                break;
            }
            dir = d.parent();
        }
    }
    roots
}

/// Kinds from a repo-local `.kubeconform-skip` (one per line, `#` comments) —
/// the escape hatch for the day someone vendors local CRD schemas.
pub fn skip_kinds(content: &str) -> Vec<String> {
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.replace(' ', ""))
        .collect()
}

pub fn kubeconform(_args: &[std::ffi::OsString]) -> Outcome {
    let staged = k8s_staged();
    if staged.is_empty() {
        return Outcome::Passed;
    }
    let root = repo_root();
    let roots = kustomization_roots(&root, &staged);
    // Raw-YAML validation is out of scope: a project either uses kustomize or
    // it does not — and that, not the toolbox, is what decides whether this
    // check had anything to do.
    if roots.is_empty() {
        return Outcome::Passed;
    }
    let missing: Vec<&str> = ["kustomize", "kubeconform"]
        .into_iter()
        .filter(|t| which(t).is_none())
        .collect();
    if !missing.is_empty() {
        warn(&format!(
            "Kustomizations staged. Skipping kubeconform; install: {}",
            hl(&missing.join(", "))
        ));
        return Outcome::Unavailable;
    }

    let skip = std::fs::read_to_string(Path::new(&root).join(".kubeconform-skip"))
        .ok()
        .map(|c| skip_kinds(&c))
        .filter(|k| !k.is_empty())
        .map(|k| k.join(","));

    let mut overall = 0;
    for r in &roots {
        if !validate_root(&root, r, skip.as_deref()) {
            fail(&format!("kubeconform failed for {r}"));
            overall = 1;
        }
    }
    if overall != 0 {
        return Outcome::Failed;
    }
    let n = roots.len();
    ok(&format!(
        "kubeconform passed ({n} kustomization root{})",
        if n > 1 { "s" } else { "" }
    ));
    Outcome::Passed
}

/// `kustomize build <root> | kubeconform …`, with the shell's `pipefail`
/// semantics: a kustomize failure fails the check even when kubeconform would
/// happily consume the empty input.
fn validate_root(root: &str, sub: &str, skip: Option<&str>) -> bool {
    // `--` before `sub`: it is a directory name walked up from staged paths,
    // so a kustomization root named e.g. `-overlay` would otherwise be read
    // as a flag by kustomize's own (cobra) parser.
    let Ok(mut build) = Command::new(program("kustomize"))
        .args(["build", "--", sub])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
    else {
        return false;
    };
    let Some(out) = build.stdout.take() else {
        return false;
    };

    // CRDs are deliberately NOT validated against an external catalog: the
    // datree CRDs-catalog is unmaintained (Datree is EOL) and its cilium schema
    // typed CIDR fields IPv4-only, false-failing IPv6 policies. Kyverno CLI,
    // Trivy and each operator's admission webhook cover them instead.
    let mut argv = vec![
        "--strict",
        "--ignore-missing-schemas",
        "--schema-location",
        "default",
    ];
    if let Some(s) = skip {
        argv.push("--skip");
        argv.push(s);
    }
    argv.push("--summary");
    argv.push("-");

    let mut cmd = Command::new(program("kubeconform"));
    cmd.args(&argv).current_dir(root).stdin(Stdio::from(out));
    let conform = super::common::bounded_success(&mut cmd, "kubeconform");
    // kubeconform's deadline closes the pipe, which normally ends kustomize
    // too — but a kustomize hung BEFORE writing (a remote base fetching over
    // the network, say) never feels the pipe close, and an unbounded wait
    // here would inherit its hang. Same clock, and a kill on expiry.
    let built = matches!(
        super::common::wait_within(&mut build, super::common::check_timeout()),
        Ok(super::common::Ran::Status(s)) if s.success()
    );
    built && conform
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_every_argo_kind_and_nothing_else() {
        for k in ARGO_KINDS {
            assert!(
                declares_argo_kind(&format!("apiVersion: x\nkind: {k}\n")),
                "{k}"
            );
        }
        assert!(!declares_argo_kind("kind: Deployment\n"));
        assert!(!declares_argo_kind("kind: WorkflowSomethingElse\n"));
        assert!(!declares_argo_kind("  kind: Workflow\n")); // must be anchored
    }

    #[test]
    fn parses_the_skip_list() {
        let c = "# comment\nCiliumNetworkPolicy\n\n  Foo Bar \n";
        assert_eq!(skip_kinds(c), vec!["CiliumNetworkPolicy", "FooBar"]);
        assert!(skip_kinds("# only a comment\n").is_empty());
    }

    #[test]
    fn walks_up_to_the_nearest_kustomization_root() {
        let tmp = std::env::temp_dir().join("amont-kustomize-test");
        let _ = std::fs::remove_dir_all(&tmp);
        let base = tmp.join("kubernetes/app/base");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("kustomization.yaml"), "resources: []").unwrap();
        let root = tmp.to_string_lossy().to_string();

        let found = kustomization_roots(&root, &["kubernetes/app/base/deploy.yaml".into()]);
        assert_eq!(found, vec!["kubernetes/app/base".to_string()]);

        // nothing above it → no root, and therefore nothing to validate
        let none = kustomization_roots(&root, &["kubernetes/loose/deploy.yaml".into()]);
        assert!(none.is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
