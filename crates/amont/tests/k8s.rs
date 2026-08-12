//! The three Kubernetes hooks. Each covers its OWN logic — path scoping, kind
//! filtering, config and tool gates, kustomization-root discovery — not what
//! the external tools report, which is their business.

mod common;
use common::{missing, Repo};

const DEPLOY: &str = "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: d\n";

// ---- argo-lint ----------------------------------------------------------

#[test]
fn argo_ignores_yaml_outside_the_k8s_prefixes() {
    let r = Repo::new();
    r.stage("src/config.yaml", "kind: Workflow\n");
    assert!(r.hook("pre-commit-argo-lint", &[]).silent());
}

#[test]
fn argo_ignores_k8s_yaml_that_is_not_a_workflow() {
    let r = Repo::new();
    r.stage("kubernetes/app/deploy.yaml", DEPLOY);
    assert!(r.hook("pre-commit-argo-lint", &[]).silent());
}

/// Every Argo kind must be picked up. With `argo` absent the hook warns and
/// exits 0; with it present it lints. Either way it must NOT be silent —
/// silence would mean the kind filter missed the file.
#[test]
fn argo_recognises_every_workflow_kind() {
    for kind in [
        "Workflow",
        "CronWorkflow",
        "WorkflowTemplate",
        "ClusterWorkflowTemplate",
    ] {
        let r = Repo::new();
        r.stage(
            &format!("kubernetes/wf/{kind}.yaml"),
            &format!("kind: {kind}\n"),
        );
        assert!(
            !r.hook("pre-commit-argo-lint", &[]).silent(),
            "{kind} was not picked up"
        );
    }
}

/// The one case in this repository that needs a tool to be ABSENT.
///
/// Every other gate here reads "skip unless the tool is installed". This one is
/// inverted — it asserts the soft-fail branch, that a missing argo warns and
/// lets the commit through — so installing argo does not enable it, it DISABLES
/// it. CI therefore refuses to install argo, and says so in a step of its own.
///
/// The early return below used to be silent, which made that invisible from the
/// outside: the log of a run on a machine WITH argo was byte-identical to the
/// log of a run where this case had passed. `missing()` prints its marker only
/// on the absent path, so the present path needs one of its own — worded
/// distinctly, because CI's skip reporter looks for exactly one of these two
/// markers and treats neither appearing as the case having quietly disappeared.
#[test]
fn argo_soft_fails_without_the_cli() {
    if !missing("argo") {
        println!("  ! argo PRESENT — skipping (this case asserts the ABSENT branch)");
        return; // the gate only exists when the tool is absent
    }
    let r = Repo::new();
    r.stage("kubernetes/wf/w.yaml", "kind: Workflow\n");
    let run = r.hook("pre-commit-argo-lint", &[]);
    assert!(run.passed(), "a missing toolchain must not block a commit");
    assert!(run.says("install"));
}

// ---- kube-linter --------------------------------------------------------

#[test]
fn kube_linter_ignores_yaml_outside_the_k8s_prefixes() {
    let r = Repo::new();
    r.stage("src/app.yaml", DEPLOY);
    assert!(r.hook("pre-commit-kube-linter", &[]).silent());
}

/// Stock rules are too noisy to enforce generically, so a repo-local config is
/// the opt-in. Without one the hook does nothing AND SAYS NOTHING.
///
/// CHANGE OF MIND, recorded: this case used to be
/// `kube_linter_says_it_skipped_without_a_config`, and asserted that the notice
/// MUST exist. Two reasons it is now the opposite. `yamllint::run` returns
/// silently in precisely this situation and is the precedent — one of these
/// three checks behaving differently is a difference nobody chose. And the
/// notice fires on EVERY commit touching `kubernetes/**.yaml` in a repository
/// that has no `.kube-linter*.yaml` and never will, which is the same "a repo
/// that never wanted yamllint was told to install it" noise
/// `docs/hook-architecture.md` records these three checks being fixed for.
///
/// The sibling assertion is KEPT: the opt-in is tested before the binary, so a
/// repo with no config is still never asked to install a linter it does not
/// use.
#[test]
fn kube_linter_is_silent_without_a_config() {
    let r = Repo::new();
    r.stage("kubernetes/app/deploy.yaml", DEPLOY);
    let run = r.hook("pre-commit-kube-linter", &[]);
    assert!(run.passed());
    assert!(
        run.silent(),
        "a repo that never opted in must hear nothing:\n{}",
        run.output()
    );
    assert!(
        !run.says("install"),
        "nagged a repo that never opted in:\n{}",
        run.output()
    );
}

// ---- kubeconform --------------------------------------------------------

#[test]
fn kubeconform_ignores_yaml_outside_the_k8s_prefixes() {
    let r = Repo::new();
    r.stage("src/app.yaml", DEPLOY);
    assert!(r.hook("pre-commit-kubeconform", &[]).silent());
}

/// Raw-YAML validation is out of scope: a project either uses kustomize or it
/// does not. With no kustomization above the file there is nothing to do.
///
/// Unconditional: root discovery runs BEFORE the tool gate, so this holds on a
/// machine with neither kustomize nor kubeconform installed — which is most of
/// them, and exactly where a wrongly-ordered gate would have gone unnoticed.
#[test]
fn kubeconform_is_silent_with_no_kustomization_root() {
    let r = Repo::new();
    r.stage("kubernetes/loose/deploy.yaml", DEPLOY);
    assert!(r.hook("pre-commit-kubeconform", &[]).silent());
}

/// The walk-up: a kustomization ABOVE the staged file must be found.
#[test]
fn kubeconform_finds_a_root_above_the_staged_file() {
    let r = Repo::new();
    r.stage(
        "kubernetes/app/base/kustomization.yaml",
        "resources:\n  - deploy.yaml\n",
    );
    r.stage("kubernetes/app/base/deploy.yaml", DEPLOY);
    assert!(
        !r.hook("pre-commit-kubeconform", &[]).silent(),
        "the root above the file was not discovered"
    );
}

// ---- tool-present paths -------------------------------------------------
//
// Everything above asserts scoping, kind filtering and the opt-in gates —
// which meant two Severity::Block checks had never run their TOOL anywhere:
// a regression in argument construction or exit-code reading would ship
// undetected. These four cases execute the real binaries; CI installs them
// (pinned) on both platforms, and the skip reporter fails the run if the
// gates below ever skip there.

const CLEAN_DEPLOY: &str = "\
apiVersion: apps/v1
kind: Deployment
metadata:
  name: d
spec:
  selector:
    matchLabels:
      app: d
  template:
    metadata:
      labels:
        app: d
    spec:
      containers:
        - name: c
          image: nginx:1.25
";

/// One deterministic rule, chosen in the config rather than inherited from
/// the tool's defaults — an upstream default-set change must not move this
/// test.
const LATEST_TAG_ONLY: &str = "\
checks:
  addAllBuiltIn: false
  doNotAutoAddDefaults: true
  include:
    - latest-tag
";

#[test]
fn kube_linter_blocks_what_its_config_flags() {
    if missing("kube-linter") {
        return;
    }
    let r = Repo::new();
    r.stage(".kube-linter.yaml", LATEST_TAG_ONLY);
    r.stage(
        "kubernetes/deploy.yaml",
        &CLEAN_DEPLOY.replace("nginx:1.25", "nginx:latest"),
    );
    let run = r.hook("pre-commit-kube-linter", &[]);
    assert!(
        !run.passed(),
        "a latest-tag image sailed past the configured rule:\n{}",
        run.output()
    );
    assert!(run.says("kube-linter"), "{}", run.output());
}

#[test]
fn kube_linter_passes_a_clean_manifest() {
    if missing("kube-linter") {
        return;
    }
    let r = Repo::new();
    r.stage(".kube-linter.yaml", LATEST_TAG_ONLY);
    r.stage("kubernetes/deploy.yaml", CLEAN_DEPLOY);
    let run = r.hook("pre-commit-kube-linter", &[]);
    assert!(run.passed(), "{}", run.output());
    assert!(run.says("kube-linter passed"), "{}", run.output());
}

#[test]
fn kubeconform_blocks_a_schema_violation() {
    if missing("kubeconform") || missing("kustomize") {
        return;
    }
    let r = Repo::new();
    r.stage(
        "kubernetes/kustomization.yaml",
        "resources:\n  - deploy.yaml\n",
    );
    // kustomize builds this happily — it does not schema-check — so the
    // failure below is specifically kubeconform's verdict.
    r.stage(
        "kubernetes/deploy.yaml",
        &CLEAN_DEPLOY.replace("  selector:", "  replicas: three\n  selector:"),
    );
    let run = r.hook("pre-commit-kubeconform", &[]);
    assert!(
        !run.passed(),
        "a string replicas passed schema validation:\n{}",
        run.output()
    );
    assert!(run.says("kubeconform failed"), "{}", run.output());
}

#[test]
fn kubeconform_passes_a_valid_kustomization() {
    if missing("kubeconform") || missing("kustomize") {
        return;
    }
    let r = Repo::new();
    r.stage(
        "kubernetes/kustomization.yaml",
        "resources:\n  - deploy.yaml\n",
    );
    r.stage("kubernetes/deploy.yaml", CLEAN_DEPLOY);
    let run = r.hook("pre-commit-kubeconform", &[]);
    assert!(run.passed(), "{}", run.output());
    assert!(run.says("kubeconform passed"), "{}", run.output());
}
