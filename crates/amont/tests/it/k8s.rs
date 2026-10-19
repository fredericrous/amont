//! The three Kubernetes hooks. Each covers its OWN logic — path scoping, kind
//! filtering, config and tool gates, kustomization-root discovery — not what
//! the external tools report, which is their business. Scoping, the opt-in,
//! a clean and a broken manifest and a missing tool are rows of the tables in
//! `tool_tables`; what is left here is the kind filter and the walk-up.

use crate::common;
use common::Repo;

const DEPLOY: &str = "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: d\n";

// ---- argo-lint ----------------------------------------------------------

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

// ---- kubeconform --------------------------------------------------------

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
