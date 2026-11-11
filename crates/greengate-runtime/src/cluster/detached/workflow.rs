//! The Argo Workflow a rehearsal submits, and the check that a Workflow or a
//! pod found on the cluster is the one that was submitted (ADR-0017).
//!
//! The shape is the one the homelab's first live run proved: one DAG task and
//! one pod per gate, `fetch` in git mode as an init container, an optional
//! `prepare`, and the gate as Argo's `main` container, run by the emissary.
//! The helpers of the Job lane build the containers; what differs is only
//! what Argo requires (`command` rather than `args` for `main`, the executor
//! service account, the ambient label) and what the homelab's quotas fix.

use crate::json_read::Value;

use super::super::{
    args_json, digest_of, env_var, json_string, mount, mount_sub, ClusterConfig, NODE, PULL_SECRET,
    RUN_LABEL_KEY, SECURITY,
};
use super::pending::{Gate, Pending};

pub(super) const PRIORITY_CLASS: &str = "gate-detached";
/// The read-only cache of detached pods: one writer at a time, because the
/// `gate-detached` quota allows one pod.
const CACHE_CLAIM: &str = "gate-cache-detached";
/// The Secret holding the `greengate-bot` read token, mounted on `fetch` only.
const TOKEN_SECRET: &str = crate::names::READ_TOKEN_SECRET;
const TOKEN_DIR: &str = "/run/secrets/forgejo";
const EXECUTOR_ACCOUNT: &str = crate::names::EXECUTOR_ACCOUNT;
/// Forgejo accepts only mesh traffic from the pod network: every pod opts in.
const AMBIENT_KEY: &str = "istio.io/dataplane-mode";
const AMBIENT_VALUE: &str = "ambient";
/// Finished Workflows, and their pods with their emptyDirs, go after 48 h.
pub(super) const TTL_SECS: u64 = 172_800;
/// Slack on top of a gate's `greengate.timeout`, as the Job lane has it.
const GATE_SLACK_SECS: u64 = 60;
/// Slack on the Workflow's deadline for the time a pod waits for the one
/// detached slot between gates.
const QUEUE_SLACK_SECS: u64 = 600;

/// The Workflow's name for a run label: deterministic, so a lost answer to
/// `create` is reconciled by `get` and never by a second `create`.
pub(super) fn workflow_name(label: &str) -> String {
    format!("{}{label}", crate::names::WORKFLOW_PREFIX)
}

/// The template of the gate whose DAG task is `task`.
pub(super) fn template_name(task: &str) -> String {
    format!("gate-{task}")
}

/// The node name Argo gives the pod of `task` in the Workflow `name`.
pub(super) fn node_name(name: &str, task: &str) -> String {
    format!("{name}.{task}")
}

/// What the Workflow is built from.
pub(super) struct Plan<'a> {
    pub(super) config: &'a ClusterConfig,
    pub(super) record: &'a Pending,
    pub(super) repo_dir: &'a str,
    /// The in-cluster URL `fetch` reads from.
    pub(super) url: &'a str,
}

fn sized(
    memory: &str,
    cpu_request: &str,
    cpu_limit: &str,
    disk_request: &str,
    disk_limit: &str,
) -> String {
    format!(
        "\"resources\":{{\"requests\":{{\"cpu\":{},\"memory\":{m},\"ephemeral-storage\":{}}},\
         \"limits\":{{\"cpu\":{},\"memory\":{m},\"ephemeral-storage\":{}}}}}",
        json_string(cpu_request),
        json_string(disk_request),
        json_string(cpu_limit),
        json_string(disk_limit),
        m = json_string(memory),
    )
}

fn fetch_resources() -> String {
    sized("512Mi", "250m", "1", "1Gi", "2Gi")
}

/// The gate's and `prepare`'s: `main` 3840Mi plus Argo's `wait` 256Mi is the
/// 4Gi the `gate-detached` quota allows. `prepare` runs alone, before `main`.
fn work_resources() -> String {
    sized("3840Mi", "500m", "2", "1Gi", "4Gi")
}

fn user_container(name: &str, image: &str, argv: &[String], extra: &str) -> String {
    format!(
        "{{\"name\":{},\"image\":{},\"args\":{},\"securityContext\":{SECURITY},{extra}}}",
        json_string(name),
        json_string(image),
        args_json(argv)
    )
}

fn read_only_mount(name: &str, path: &str) -> String {
    format!(
        "{{\"name\":{},\"mountPath\":{},\"readOnly\":true}}",
        json_string(name),
        json_string(path)
    )
}

/// One gate's template: `fetch`, `prepare` when set, and the gate as `main`.
fn template(plan: &Plan, gate: &Gate) -> String {
    let config = plan.config;
    let fetch = user_container(
        "fetch",
        &config.image,
        &["/usr/local/bin/fetch".to_string()],
        &format!(
            "\"env\":[{},{},{},{},{}],\"volumeMounts\":[{},{},{},{}],{}",
            env_var("GATE_SOURCE", "git"),
            env_var("GATE_URL", plan.url),
            env_var("GATE_REF", &plan.record.source.oid),
            env_var("EXPECTED_TREE", &plan.record.tree),
            env_var("REPO_DIR", plan.repo_dir),
            mount("cache", "/cache"),
            mount("work", "/work"),
            mount("checkout", "/checkout"),
            read_only_mount("forgejo-token", TOKEN_DIR),
            fetch_resources(),
        ),
    );
    let work_mounts = format!(
        "\"workingDir\":\"/checkout\",\"volumeMounts\":[{},{}],{}",
        mount_sub("cache", "/cache", plan.repo_dir),
        mount("checkout", "/checkout"),
        work_resources(),
    );
    let mut inits = vec![fetch];
    if let Some(prepare) = &gate.prepare {
        let argv = ["sh".to_string(), "-c".to_string(), prepare.clone()];
        inits.push(user_container(
            "prepare",
            &config.image,
            &argv,
            &work_mounts,
        ));
    }
    format!(
        "{{\"name\":{name},\"activeDeadlineSeconds\":{deadline},\
         \"metadata\":{{\"annotations\":{{\"greengate.dev/gate\":{gate_id}}}}},\
         \"initContainers\":[{inits}],\
         \"container\":{{\"image\":{image},\"command\":{command},\"securityContext\":{SECURITY},{work_mounts}}}}}",
        name = json_string(&template_name(&gate.task)),
        deadline = config.timeout_secs + GATE_SLACK_SECS,
        gate_id = json_string(&gate.id),
        inits = inits.join(","),
        image = json_string(&config.image),
        command = args_json(&gate.argv),
    )
}

/// The Workflow, as JSON `kubectl create -f` reads.
///
/// A DAG with one task per gate and `failFast: false`, so that one failing
/// gate does not stop the others from being scheduled; `parallelism: 1`, so
/// the pods run one after another inside the one detached slot. Each template
/// has its own deadline; the Workflow's covers all of them.
pub(super) fn workflow_json(plan: &Plan) -> String {
    let record = plan.record;
    let config = plan.config;
    let tasks: Vec<String> = record
        .gates
        .iter()
        .map(|g| {
            format!(
                "{{\"name\":{},\"template\":{}}}",
                json_string(&g.task),
                json_string(&template_name(&g.task))
            )
        })
        .collect();
    let mut templates = vec![format!(
        "{{\"name\":\"gates\",\"dag\":{{\"failFast\":false,\"tasks\":[{}]}}}}",
        tasks.join(",")
    )];
    templates.extend(record.gates.iter().map(|g| template(plan, g)));
    let gates = record.gates.len() as u64;
    let deadline = gates * (config.timeout_secs + GATE_SLACK_SECS) + QUEUE_SLACK_SECS;
    let managed_by = crate::names::MANAGED_BY;
    format!(
        "{{\"apiVersion\":\"argoproj.io/v1alpha1\",\"kind\":\"Workflow\",\
         \"metadata\":{{\"name\":{name},\
         \"labels\":{{\"app.kubernetes.io/managed-by\":\"{managed_by}\",{run_key}:{label}}},\
         \"annotations\":{{\"greengate.dev/tree\":{tree}}}}},\
         \"spec\":{{\"entrypoint\":\"gates\",\"serviceAccountName\":\"default\",\
         \"automountServiceAccountToken\":false,\
         \"executor\":{{\"serviceAccountName\":{executor}}},\
         \"podPriorityClassName\":{prio},\
         \"nodeSelector\":{{\"kubernetes.io/hostname\":{node}}},\
         \"imagePullSecrets\":[{{\"name\":{pull}}}],\
         \"activeDeadlineSeconds\":{deadline},\
         \"parallelism\":1,\
         \"ttlStrategy\":{{\"secondsAfterCompletion\":{TTL_SECS}}},\
         \"podMetadata\":{{\"labels\":{{{ambient_key}:{ambient_value},{run_key}:{label}}}}},\
         \"securityContext\":{{\"runAsNonRoot\":true,\"runAsUser\":1000,\"runAsGroup\":1000,\
         \"fsGroup\":1000,\"seccompProfile\":{{\"type\":\"RuntimeDefault\"}}}},\
         \"volumes\":[{{\"name\":\"work\",\"emptyDir\":{{}}}},{{\"name\":\"checkout\",\"emptyDir\":{{}}}},\
         {{\"name\":\"cache\",\"persistentVolumeClaim\":{{\"claimName\":{claim}}}}},\
         {{\"name\":\"forgejo-token\",\"secret\":{{\"secretName\":{secret}}}}}],\
         \"templates\":[{templates}]}}}}",
        name = json_string(&record.name),
        run_key = json_string(RUN_LABEL_KEY),
        label = json_string(&record.label),
        tree = json_string(&record.tree),
        executor = json_string(EXECUTOR_ACCOUNT),
        prio = json_string(PRIORITY_CLASS),
        node = json_string(NODE),
        pull = json_string(PULL_SECRET),
        ambient_key = json_string(AMBIENT_KEY),
        ambient_value = json_string(AMBIENT_VALUE),
        claim = json_string(CACHE_CLAIM),
        secret = json_string(TOKEN_SECRET),
        templates = templates.join(","),
    )
}

// Is this the Workflow, or the pod, that was submitted?

fn named<'a>(list: Option<&'a Value>, name: &str) -> Option<&'a Value> {
    list?
        .items()
        .iter()
        .find(|c| c.get("name").and_then(Value::as_str) == Some(name))
}

fn text<'a>(v: Option<&'a Value>, key: &str) -> Option<&'a str> {
    v?.get(key).and_then(Value::as_str)
}

fn env_of<'a>(container: &'a Value, name: &str) -> Option<&'a str> {
    text(named(container.get("env"), name), "value")
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.map(Value::items)
        .unwrap_or(&[])
        .iter()
        .filter_map(|a| a.as_str().map(str::to_string))
        .collect()
}

/// The digest an image reference is pinned to: after `@`. A pod spec spells
/// it `repo:tag@sha256:…`, a container status `repo@sha256:…`; only the
/// digest is the same.
fn digest_in(image: Option<&str>) -> Option<&str> {
    digest_of(image?)
}

/// The argv a `main` container runs. Argo's emissary rewrites the command of
/// a pod to `argoexec emissary <flags> -- <submitted argv>`; what follows the
/// FIRST `--` is the submitted argv, whatever `--` the argv itself holds.
pub(super) fn main_argv(main: &Value) -> Vec<String> {
    let command = strings(main.get("command"));
    let emissary = command
        .first()
        .is_some_and(|c| c == "argoexec" || c.ends_with("/argoexec"));
    match command.iter().position(|a| a == "--") {
        Some(at) if emissary => command[at + 1..].to_vec(),
        _ => command,
    }
}

/// `containers` against the submission: the containers of a template, or of a
/// pod made from it. `fetch` reads the right tree from the right commit, every
/// container runs the pinned image, `prepare` exists exactly when it was
/// submitted, and `main` runs the submitted argv.
fn check_containers(
    inits: Option<&Value>,
    main: Option<&Value>,
    gate: &Gate,
    record: &Pending,
) -> Result<(), String> {
    let main = main.ok_or("no main container")?;
    let fetch = named(inits, "fetch").ok_or("no fetch container")?;
    for (what, container) in [("fetch", Some(fetch)), ("main", Some(main))] {
        if digest_in(text(container, "image")) != Some(gate.digest.as_str()) {
            return Err(format!("{what} runs another image than the submitted one"));
        }
    }
    if env_of(fetch, "EXPECTED_TREE") != Some(record.tree.as_str()) {
        return Err("fetch expects another tree than the submitted one".into());
    }
    if env_of(fetch, "GATE_REF") != Some(record.source.oid.as_str()) {
        return Err("fetch reads another commit than the submitted one".into());
    }
    let prepare = named(inits, "prepare");
    match (&gate.prepare, prepare) {
        (None, None) => {}
        (Some(command), Some(container)) => {
            if strings(container.get("args")) != ["sh", "-c", command.as_str()] {
                return Err("prepare runs another command than the submitted one".into());
            }
            if digest_in(text(Some(container), "image")) != Some(gate.digest.as_str()) {
                return Err("prepare runs another image than the submitted one".into());
            }
        }
        (None, Some(_)) => return Err("a prepare step that was not submitted".into()),
        (Some(_), None) => return Err("the submitted prepare step is missing".into()),
    }
    if main_argv(main) != gate.argv {
        return Err("main runs another command than the submitted one".into());
    }
    Ok(())
}

/// A pod of the Workflow, against the gate it claims to run. Its containers
/// are checked as a template's are.
pub(super) fn check_pod(pod: &Value, gate: &Gate, record: &Pending) -> Result<(), String> {
    let spec = pod.get("spec");
    check_containers(
        spec.and_then(|s| s.get("initContainers")),
        named(spec.and_then(|s| s.get("containers")), "main"),
        gate,
        record,
    )
}

/// A Workflow found on the cluster, against the submission it must be: its
/// name and labels, its fence-relevant priority class, and every gate's
/// template. Run before a Workflow found by name is adopted.
pub(super) fn check_workflow(wf: &Value, record: &Pending) -> Result<(), String> {
    let metadata = wf.get("metadata");
    if text(metadata, "name") != Some(record.name.as_str()) {
        return Err("another name".into());
    }
    if text(metadata.and_then(|m| m.get("labels")), RUN_LABEL_KEY) != Some(record.label.as_str()) {
        return Err("another run label".into());
    }
    let spec = wf.get("spec");
    if text(spec, "podPriorityClassName") != Some(PRIORITY_CLASS) {
        return Err("not the detached priority class".into());
    }
    let templates = spec.and_then(|s| s.get("templates"));
    for gate in &record.gates {
        let template = named(templates, &template_name(&gate.task))
            .ok_or_else(|| format!("no template for {}", gate.id))?;
        check_containers(
            template.get("initContainers"),
            template.get("container"),
            gate,
            record,
        )
        .map_err(|why| format!("{}: {why}", gate.id))?;
    }
    let tasks = templates
        .and_then(|t| named(Some(t), "gates"))
        .and_then(|t| t.get("dag"))
        .and_then(|d| d.get("tasks"))
        .map_or(0, |t| t.items().len());
    if tasks != record.gates.len() {
        return Err("another number of gates".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::pending::tests::sample;
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    fn config() -> ClusterConfig {
        ClusterConfig {
            kubeconfig: PathBuf::from("/k"),
            namespace: "greengate-gates".into(),
            slots: 2,
            image: format!("reg/gate:1@sha256:{}", "a".repeat(64)),
            digest: format!("sha256:{}", "a".repeat(64)),
            max_bundle_mib: 1,
            start_wait: Duration::from_secs(30),
            timeout_secs: 900,
            prepare: Some("make \"deps\"".into()),
        }
    }

    fn built() -> (String, Value, Pending) {
        let record = sample(&workflow_name("7-00000000000000aa-0"));
        let plan = Plan {
            config: &config(),
            record: &record,
            repo_dir: "relais",
            url: "http://forgejo-http.forgejo.svc.cluster.local:3000/amont-snapshots/relais.git",
        };
        let json = workflow_json(&plan);
        let doc = crate::json_read::parse(&json).expect("valid JSON");
        (json, doc, record)
    }

    /// The name fits a label value (63) with the longest label there is: a
    /// 7-digit pid, 16 hex digits and a 20-digit run.
    #[test]
    fn the_workflow_name_fits_a_label_value() {
        let longest = crate::cluster::run_label(u64::MAX);
        assert!(workflow_name(&longest).len() <= 63, "{longest}");
        let worst = format!("{}-{}-{}", u32::MAX, "f".repeat(16), u64::MAX);
        assert!(workflow_name(&worst).len() <= 63, "{worst}");
    }

    #[test]
    fn the_workflow_carries_the_contract() {
        let (json, doc, record) = built();
        let spec = doc.get("spec").expect("spec");
        for needle in [
            "\"podPriorityClassName\":\"gate-detached\"",
            "\"kubernetes.io/hostname\":\"talos-wk-3-gpu\"",
            "\"automountServiceAccountToken\":false",
            "\"serviceAccountName\":\"greengate-gate-executor\"",
            "\"serviceAccountName\":\"default\"",
            "\"imagePullSecrets\":[{\"name\":\"forgejo-registry-pull\"}]",
            "\"secondsAfterCompletion\":172800",
            "\"parallelism\":1",
            "\"failFast\":false",
            "\"istio.io/dataplane-mode\":\"ambient\"",
            "\"runAsUser\":1000",
            "\"runAsGroup\":1000",
            "\"fsGroup\":1000",
            "\"runAsNonRoot\":true",
            "\"type\":\"RuntimeDefault\"",
            "\"claimName\":\"gate-cache-detached\"",
            "\"secretName\":\"greengate-bot-read-token\"",
            "\"greengate.run\":\"7-00000000000000aa-0\"",
            "\"value\":\"git\"",
            "\"memory\":\"3840Mi\"",
            "\"memory\":\"512Mi\"",
            "\"ephemeral-storage\":\"4Gi\"",
            "\"workingDir\":\"/checkout\"",
            "\"subPath\":\"relais\"",
        ] {
            assert!(json.contains(needle), "{needle} missing from {json}");
        }
        // 1 gate: 1 * (900 + 60) + 600
        assert_eq!(
            spec.get("activeDeadlineSeconds"),
            Some(&Value::Num("1560".into()))
        );
        // the pods carry the run label as well as the Workflow
        let pod_labels = spec.get("podMetadata").and_then(|p| p.get("labels"));
        assert_eq!(
            text(pod_labels, "greengate.run"),
            Some(record.label.as_str()),
            "{json}"
        );
        assert!(
            !json.contains("\"args\":[\"scripts/gate.sh\""),
            "main uses command"
        );
        assert!(
            json.contains("\"command\":[\"scripts/gate.sh\",\"--a b\"]"),
            "{json}"
        );
    }

    /// The token is mounted on `fetch` and nowhere else: not on `prepare`, not
    /// on the gate.
    #[test]
    fn the_bot_token_is_mounted_on_fetch_only() {
        let (_, doc, _) = built();
        let template =
            named(doc.get("spec").and_then(|s| s.get("templates")), "gate-g0").expect("template");
        let mounts_token = |c: &Value| {
            c.get("volumeMounts")
                .map(Value::items)
                .unwrap_or(&[])
                .iter()
                .any(|m| text(Some(m), "name") == Some("forgejo-token"))
        };
        let inits = template.get("initContainers");
        assert!(mounts_token(named(inits, "fetch").expect("fetch")));
        assert!(!mounts_token(named(inits, "prepare").expect("prepare")));
        assert!(!mounts_token(template.get("container").expect("main")));
    }

    #[test]
    fn a_built_workflow_checks_against_its_own_submission() {
        let (_, doc, record) = built();
        assert_eq!(check_workflow(&doc, &record), Ok(()));
        let mut other = record.clone();
        other.gates[0].argv = vec!["scripts/other.sh".into()];
        assert!(check_workflow(&doc, &other).is_err());
        let mut other = record.clone();
        other.gates[0].prepare = None;
        assert!(check_workflow(&doc, &other).is_err());
        let mut other = record.clone();
        other.gates[0].digest = format!("sha256:{}", "b".repeat(64));
        assert!(check_workflow(&doc, &other).is_err());
        let mut other = record;
        other.tree = "0".repeat(40);
        assert!(check_workflow(&doc, &other).is_err());
    }

    /// Argo rewrites `main`'s command; the submitted argv is what follows the
    /// emissary's first `--`.
    #[test]
    fn the_emissary_command_yields_the_submitted_argv() {
        let pod = crate::json_read::parse(include_str!(
            "../../../../greengate/tests/it/fixtures/argo/pod-succeeded.json"
        ))
        .expect("fixture");
        let main = named(pod.get("spec").and_then(|s| s.get("containers")), "main").expect("main");
        assert_eq!(
            main_argv(main),
            ["python3", "scripts/check-module-cycles.py"]
        );
        let direct = crate::json_read::parse(r#"{"command":["a","--","b"]}"#).expect("json");
        assert_eq!(main_argv(&direct), ["a", "--", "b"], "not an emissary");
        let tricky = crate::json_read::parse(
            r#"{"command":["/var/run/argo/argoexec","emissary","--","x","--","y"]}"#,
        )
        .expect("json");
        assert_eq!(main_argv(&tricky), ["x", "--", "y"]);
    }

    #[test]
    fn an_image_is_compared_by_digest_alone() {
        let hex = "f".repeat(64);
        assert_eq!(
            digest_in(Some(&format!("repo:tag@sha256:{hex}"))),
            digest_in(Some(&format!("repo@sha256:{hex}")))
        );
        assert_eq!(digest_in(Some("repo:tag")), None);
    }
}
