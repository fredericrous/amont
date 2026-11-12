//! `greengate cluster sign [--watch]`: the in-cluster signer (homelab plan
//! 2026-11-11-attest-signer, Phase 3).
//!
//! It signs finished, Workflow-owned detached gate runs from what Kubernetes
//! recorded, the facts `collect` judges on the laptop: a pod bound to its
//! Workflow by `ownerReferences` and by the Workflow's own node record, `main`'s
//! termination status, `fetch`'s `tree <sha>` message and the observed
//! imageID ([`judge`]). The gate ids and the tree are read from the WORKFLOW
//! object as greengate's own submission writes them (`workflow.rs`), never
//! from a pod: a pod's labels are anyone's to set.
//!
//! What it writes is one ConfigMap per Workflow, in the signer namespace,
//! named after the Workflow: the signed git-attest block of the gates that
//! passed (ADR-0037: git-attest renders, signs and wraps it), or an empty
//! block and the reason no gate passed. Either way the Workflow is done and
//! is never judged again, on a restart, a one-shot pass or another replica.
//!
//! Plain stderr, one line per Workflow it acted on; stdout stays empty.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::json_read::Value;

use super::super::{
    capture, capture_with, json_string, judge, last_line, parse_pod_object, refusal, Call, Refusal,
    PLATFORM, RUN_LABEL_KEY,
};
use super::collect::{end_from_main, kind_of, pod_of};
use super::pending::RecordPhase;
use super::workflow::{template_name, GATE_ANNOTATION, TREE_ANNOTATION};

/// The private key the signer signs with. Which principal that is, is the
/// verifier's `allowed_signers` business, not this code's.
const KEY_ENV: &str = "GREENGATE_SIGNER_KEY";
/// A kubeconfig, when the in-cluster ServiceAccount is not the way in.
const KUBECONFIG_ENV: &str = "GREENGATE_SIGNER_KUBECONFIG";
/// The namespace the ConfigMaps are written to.
const NAMESPACE_ENV: &str = "GREENGATE_SIGNER_NAMESPACE";
const DEFAULT_NAMESPACE: &str = "attest-signer";
/// Where detached Workflows run: greengate 4's namespace and 3.x's.
const RUNNER_NAMESPACES: [&str; 2] = ["greengate-gates", "amont-gates"];

/// The label every signer ConfigMap carries, and its value.
pub(super) const SIGNER_LABEL: &str = "greengate.signer";
pub(super) const SIGNER_LABEL_VALUE: &str = "block";
pub(super) const WORKFLOW_NAMESPACE_ANNOTATION: &str = "greengate.signer/workflow-namespace";
pub(super) const TREE_SIGNED_ANNOTATION: &str = "greengate.signer/tree";
pub(super) const EXPIRES_ANNOTATION: &str = "greengate.signer/expires";
/// A ConfigMap outlives its Workflow (48 h TTL) by days, so the sweep never
/// deletes one whose Workflow could still be signed again.
const EXPIRY_SECS: u64 = 7 * 86_400;
/// A finished Workflow without a ConfigMap after this long is backlog.
const BACKLOG_SECS: u64 = 300;
/// The pause between passes of `--watch`.
const WATCH_SECS: u64 = 30;
/// kubectl's own request timeout, and the wall deadline around each call.
const REQUEST_TIMEOUT: &str = "--request-timeout=10s";
const CALL_SECS: u64 = 20;

const USAGE: &str = "usage: greengate cluster sign [--watch]";

/// Set by SIGTERM or SIGINT under `--watch`: the run in progress finishes,
/// then the loop ends.
static STOP: AtomicBool = AtomicBool::new(false);

/// What one pass decided for one Workflow, or for one expired ConfigMap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Action {
    /// Sign `gates` (passed, in DAG order) on `tree`.
    Sign {
        workflow: String,
        namespace: String,
        label: String,
        tree: String,
        gates: Vec<String>,
    },
    /// No gate passed, or the Workflow is not one greengate submitted:
    /// recorded with an empty block so that it is not judged again.
    Skip {
        workflow: String,
        namespace: String,
        label: String,
        /// The tree the Workflow names, empty when it names none usable.
        tree: String,
        reason: String,
    },
    /// A signer ConfigMap whose expiry passed.
    Delete { configmap: String },
}

/// One pass's decisions, and the finished Workflows older than
/// [`BACKLOG_SECS`] that had no ConfigMap when it began.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Planned {
    pub actions: Vec<Action>,
    pub backlog: Vec<String>,
}

fn text<'a>(v: Option<&'a Value>, key: &str) -> Option<&'a str> {
    v?.get(key).and_then(Value::as_str)
}

fn named<'a>(list: Option<&'a Value>, name: &str) -> Option<&'a Value> {
    list?
        .items()
        .iter()
        .find(|c| text(Some(c), "name") == Some(name))
}

/// Whether a gate id can stand as ONE entry of a payload's `gates` line: a
/// single non-empty token, so whitespace cannot turn one id into two gates.
/// This is a structural guard, not the gate-name grammar: that grammar is
/// git-attest's alone (ADR-0037, ADR-0039; `spec::valid_gate` is
/// crate-private there), and only fenced runner ServiceAccounts can create
/// the Workflows these ids come from.
fn valid_gate(name: &str) -> bool {
    let mut tokens = name.split_whitespace();
    matches!((tokens.next(), tokens.next()), (Some(only), None) if only == name)
}

/// Seconds since the epoch of a Kubernetes timestamp, `YYYY-MM-DDTHH:MM:SSZ`
/// (RFC 3339 in UTC, as the apiserver writes `finishedAt`). Pure.
fn epoch_of(ts: &str) -> Option<u64> {
    let b = ts.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> {
        let s = ts.get(from..to)?;
        s.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| s.parse().ok())?
    };
    let (y, m, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hh, mm, ss) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hh * 3600 + mm * 60 + ss).ok()
}

/// One DAG task of a submitted Workflow: the gate id its template names, and
/// whether a `prepare` step was submitted with it.
struct Submitted {
    task: String,
    id: String,
    prepare: bool,
}

/// What greengate's submission says of a Workflow: its tree, and per DAG
/// task the gate id and whether a `prepare` step was submitted. `Err` names
/// why it is not a Workflow greengate submitted.
fn submitted(wf: &Value) -> Result<(String, Vec<Submitted>), String> {
    let metadata = wf.get("metadata");
    let tree = text(metadata.and_then(|m| m.get("annotations")), TREE_ANNOTATION)
        .ok_or("the Workflow names no tree")?;
    if !git_attest::format::is_oid(tree) {
        return Err("the Workflow's tree is not an object id".into());
    }
    let templates = wf.get("spec").and_then(|s| s.get("templates"));
    let tasks = named(templates, "gates")
        .and_then(|t| t.get("dag"))
        .and_then(|d| d.get("tasks"))
        .map(Value::items)
        .unwrap_or(&[]);
    if tasks.is_empty() {
        return Err("the Workflow has no gate task".into());
    }
    let mut gates: Vec<Submitted> = Vec::new();
    for task in tasks {
        let name = text(Some(task), "name").ok_or("a task without a name")?;
        let want = template_name(name);
        if text(Some(task), "template") != Some(want.as_str()) {
            return Err(format!("task {name} runs another template than {want}"));
        }
        let template = named(templates, &want).ok_or_else(|| format!("no template {want}"))?;
        let id = text(
            template.get("metadata").and_then(|m| m.get("annotations")),
            GATE_ANNOTATION,
        )
        .ok_or_else(|| format!("template {want} names no gate"))?;
        if !valid_gate(id) {
            return Err(format!("template {want} names an invalid gate id"));
        }
        if gates.iter().any(|g| g.id == id) {
            return Err(format!("gate {id} is named twice"));
        }
        let prepare = named(template.get("initContainers"), "prepare").is_some();
        gates.push(Submitted {
            task: name.to_string(),
            id: id.to_string(),
            prepare,
        });
    }
    Ok((tree.to_string(), gates))
}

/// Judge one finished Workflow from the runner namespace's list: the gates
/// that passed, in DAG order, or why none can be signed. Pure.
fn judge_workflow(list: &Value, wf: &Value) -> Result<(String, Vec<String>), (String, String)> {
    let metadata = wf.get("metadata");
    let name = text(metadata, "name").unwrap_or_default();
    let uid = text(metadata, "uid").ok_or((String::new(), "the Workflow has no UID".into()))?;
    let (tree, gates) = submitted(wf).map_err(|why| (String::new(), why))?;
    let mut passed: Vec<String> = Vec::new();
    let mut why_not: Vec<String> = Vec::new();
    for Submitted { task, id, prepare } in &gates {
        // Owned by THIS Workflow (kind, name, uid, controller) and the pod of
        // its node record: a Job's pod, or one wearing a forged run label,
        // is never found here.
        let Some(pod) = pod_of(list, wf, name, uid, task) else {
            why_not.push(format!("{id}: no pod of {task} owned by the Workflow"));
            continue;
        };
        let Some(facts) = parse_pod_object(pod, "main") else {
            why_not.push(format!("{id}: the pod's status cannot be read"));
            continue;
        };
        let end = end_from_main(&facts.gate);
        let prepare = prepare.then_some("prepare");
        match judge(&end, &facts, &tree, prepare) {
            Ok(_image_id) => passed.push(id.clone()),
            Err(fallback) => why_not.push(format!("{id}: {fallback}")),
        }
    }
    if passed.is_empty() {
        return Err((tree, format!("no gate passed ({})", why_not.join("; "))));
    }
    Ok((tree, passed))
}

/// The names of the signer ConfigMaps, and those whose expiry passed.
fn configmaps(signer: &Value, now: u64) -> (BTreeSet<String>, Vec<String>) {
    let mut names = BTreeSet::new();
    let mut expired = Vec::new();
    for cm in signer.get("items").map(Value::items).unwrap_or(&[]) {
        let metadata = cm.get("metadata");
        let Some(name) = text(metadata, "name") else {
            continue;
        };
        names.insert(name.to_string());
        let expires = text(
            metadata.and_then(|m| m.get("annotations")),
            EXPIRES_ANNOTATION,
        )
        .and_then(|e| e.parse::<u64>().ok());
        if expires.is_some_and(|e| e < now) {
            expired.push(name.to_string());
        }
    }
    (names, expired)
}

/// One pass's decisions, from the runner namespaces that answered (`(name,
/// list of Workflows and pods)`) and the signer's ConfigMaps. Pure.
pub(super) fn plan_pass(runners: &[(&str, Value)], signer: &Value, now: u64) -> Planned {
    let (signed, expired) = configmaps(signer, now);
    let mut planned = Planned::default();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for (namespace, list) in runners {
        for wf in kind_of(list, "Workflow") {
            let metadata = wf.get("metadata");
            let Some(name) = text(metadata, "name") else {
                continue;
            };
            let status = wf.get("status");
            let phase = RecordPhase::parse(text(status, "phase").unwrap_or(""));
            // Signed, skipped, or already decided in this pass (a name in both
            // runner namespaces gets one ConfigMap: the first one decided).
            if !phase.finished() || signed.contains(name) || !seen.insert(name.to_string()) {
                continue;
            }
            let finished = text(status, "finishedAt").and_then(epoch_of);
            if finished.is_some_and(|at| now.saturating_sub(at) > BACKLOG_SECS) {
                planned.backlog.push(name.to_string());
            }
            let label = text(metadata.and_then(|m| m.get("labels")), RUN_LABEL_KEY)
                .unwrap_or_default()
                .to_string();
            planned.actions.push(match judge_workflow(list, wf) {
                Ok((tree, gates)) => Action::Sign {
                    workflow: name.to_string(),
                    namespace: namespace.to_string(),
                    label,
                    tree,
                    gates,
                },
                Err((tree, reason)) => Action::Skip {
                    workflow: name.to_string(),
                    namespace: namespace.to_string(),
                    label,
                    tree,
                    reason,
                },
            });
        }
    }
    planned.actions.extend(
        expired
            .into_iter()
            .map(|configmap| Action::Delete { configmap }),
    );
    planned
}

/// The git-attest v2 payload of `gates` passed on `tree` on the cluster's
/// platform, without input lines or a canonical tree: the signer has no
/// repository to fingerprint. `producer` is the same as the laptop's blocks
/// (`attest.rs`): this crate's version.
pub(super) fn payload(tree: &str, gates: &[String]) -> String {
    let line = git_attest::produce::gate_list(&gates.join(" "));
    git_attest::format::payload(tree, &line, PLATFORM, &[], None, env!("CARGO_PKG_VERSION"))
}

/// The signed block of `payload`, as a note holds it.
fn signed_block(payload: &str, key: &Path) -> Result<String, String> {
    let sig = git_attest::sign::sign(git_attest::format::NAMESPACE, payload, key)
        .map_err(|e| format!("signing failed: {e}"))?;
    Ok(git_attest::format::block(payload, &sig))
}

/// What a ConfigMap is made of.
pub(super) struct Record<'a> {
    pub workflow: &'a str,
    pub namespace: &'a str,
    pub label: &'a str,
    pub tree: &'a str,
    pub block: &'a str,
    pub skipped: Option<&'a str>,
    pub expires: u64,
}

/// The ConfigMap, as JSON `kubectl create -f -` reads.
pub(super) fn configmap_json(signer_namespace: &str, r: &Record) -> String {
    let skipped = r
        .skipped
        .map(|why| format!(",\"skipped\":{}", json_string(why)))
        .unwrap_or_default();
    format!(
        "{{\"apiVersion\":\"v1\",\"kind\":\"ConfigMap\",\
         \"metadata\":{{\"name\":{name},\"namespace\":{ns},\
         \"labels\":{{{run_key}:{label},{signer_key}:{signer_value}}},\
         \"annotations\":{{{wf_ns_key}:{wf_ns},{tree_key}:{tree},{expires_key}:{expires}}}}},\
         \"data\":{{\"block\":{block}{skipped}}}}}",
        name = json_string(r.workflow),
        ns = json_string(signer_namespace),
        run_key = json_string(RUN_LABEL_KEY),
        label = json_string(r.label),
        signer_key = json_string(SIGNER_LABEL),
        signer_value = json_string(SIGNER_LABEL_VALUE),
        wf_ns_key = json_string(WORKFLOW_NAMESPACE_ANNOTATION),
        wf_ns = json_string(r.namespace),
        tree_key = json_string(TREE_SIGNED_ANNOTATION),
        tree = json_string(r.tree),
        expires_key = json_string(EXPIRES_ANNOTATION),
        expires = json_string(&r.expires.to_string()),
        block = json_string(r.block),
    )
}

/// How the signer reaches the cluster: its ServiceAccount (kubectl's
/// in-cluster default), or a kubeconfig when one is named.
struct Access {
    kubeconfig: Option<PathBuf>,
    namespace: String,
}

impl Access {
    fn from_env() -> Access {
        let set = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
        Access {
            kubeconfig: set(KUBECONFIG_ENV).map(PathBuf::from),
            namespace: set(NAMESPACE_ENV)
                .and_then(|v| v.into_string().ok())
                .unwrap_or_else(|| DEFAULT_NAMESPACE.to_string()),
        }
    }

    /// `kubectl [--kubeconfig <path>] <request timeout> -n <ns> <args>`, stdin
    /// null, git's environment removed: [`super::super::Kube::command`]
    /// without the kubeconfig it requires.
    fn command(&self, namespace: &str, args: &[&str]) -> Command {
        let mut cmd = Command::new(crate::hooks::common::program("kubectl"));
        if let Some(path) = &self.kubeconfig {
            cmd.arg("--kubeconfig").arg(path);
        }
        cmd.arg(REQUEST_TIMEOUT)
            .args(["-n", namespace])
            .args(args)
            .stdin(Stdio::null());
        crate::hooks::common::strip_git_env(&mut cmd);
        cmd
    }

    fn list(&self, namespace: &str, what: &str, selector: &str) -> Result<Value, String> {
        let mut cmd = self.command(namespace, &["get", what, "-l", selector, "-o", "json"]);
        match capture(&mut cmd, CALL_SECS) {
            Call::Ok(json) => crate::json_read::parse(&json)
                .ok_or_else(|| "kubectl printed something that is not JSON".to_string()),
            Call::Refused(e) | Call::NotStarted(e) | Call::TimedOut(e) => Err(last_line(&e)),
        }
    }

    /// Create the ConfigMap: `Ok(true)` when this call created it,
    /// `Ok(false)` when it already existed (another pass or replica won).
    fn create(&self, json: String) -> Result<bool, String> {
        let mut cmd = self.command(&self.namespace, &["create", "-f", "-"]);
        match capture_with(&mut cmd, Some(json.into_bytes()), CALL_SECS) {
            Call::Ok(_) => Ok(true),
            Call::Refused(e) if refusal(&e) == Refusal::Taken => Ok(false),
            Call::Refused(e) | Call::NotStarted(e) | Call::TimedOut(e) => Err(last_line(&e)),
        }
    }

    fn delete(&self, configmap: &str) -> Result<(), String> {
        let mut cmd = self.command(
            &self.namespace,
            &["delete", "configmap", configmap, "--ignore-not-found"],
        );
        match capture(&mut cmd, CALL_SECS) {
            Call::Ok(_) => Ok(()),
            Call::Refused(e) | Call::NotStarted(e) | Call::TimedOut(e) => Err(last_line(&e)),
        }
    }
}

fn say(line: &str) {
    eprintln!("{}", crate::ui::sanitize(line));
}

fn short(oid: &str) -> &str {
    oid.get(..8).unwrap_or(oid)
}

/// Touch `$HOME/heartbeat`: the liveness probe's evidence that the loop
/// turns. Best-effort; a failure is said, never a failed pass.
fn heartbeat(now: u64) {
    let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) else {
        say("error heartbeat: HOME is not set");
        return;
    };
    let path = Path::new(&home).join("heartbeat");
    if let Err(e) = std::fs::write(&path, format!("{now}\n")) {
        say(&format!("error heartbeat: {}: {e}", path.display()));
    }
}

/// One pass: read, decide, write, sweep, count, heartbeat. `true` when
/// nothing failed.
fn run_pass(access: &Access, key: &Path) -> bool {
    let now = crate::gate_evidence::now();
    let clean = pass(access, key, now);
    heartbeat(now);
    clean
}

fn pass(access: &Access, key: &Path, now: u64) -> bool {
    let signer_selector = format!("{SIGNER_LABEL}={SIGNER_LABEL_VALUE}");
    // Without the ConfigMaps nothing is known to be signed: decide nothing.
    let signer = match access.list(&access.namespace, "configmaps", &signer_selector) {
        Ok(list) => list,
        Err(why) => {
            say(&format!(
                "error {}: the signer's ConfigMaps could not be read: {why}",
                access.namespace
            ));
            return false;
        }
    };
    let mut clean = true;
    let mut runners: Vec<(&str, Value)> = Vec::new();
    for namespace in RUNNER_NAMESPACES {
        match access.list(namespace, "workflows,pods", RUN_LABEL_KEY) {
            Ok(list) => runners.push((namespace, list)),
            Err(why) => {
                say(&format!("error {namespace}: could not be read: {why}"));
                clean = false;
            }
        }
    }
    let planned = plan_pass(&runners, &signer, now);
    let mut recorded: BTreeSet<String> = BTreeSet::new();
    let expires = now + EXPIRY_SECS;
    for action in &planned.actions {
        match action {
            Action::Sign {
                workflow,
                namespace,
                label,
                tree,
                gates,
            } => {
                let block = match signed_block(&payload(tree, gates), key) {
                    Ok(block) => block,
                    Err(why) => {
                        say(&format!("error {workflow}: {why}"));
                        clean = false;
                        continue;
                    }
                };
                let json = configmap_json(
                    &access.namespace,
                    &Record {
                        workflow,
                        namespace,
                        label,
                        tree,
                        block: &block,
                        skipped: None,
                        expires,
                    },
                );
                match access.create(json) {
                    Ok(true) => {
                        recorded.insert(workflow.clone());
                        say(&format!(
                            "signed {workflow} {} tree {}",
                            gates.join(","),
                            short(tree)
                        ));
                    }
                    Ok(false) => {
                        recorded.insert(workflow.clone());
                        say(&format!(
                            "skipped {workflow}: another signer wrote its ConfigMap first"
                        ));
                    }
                    Err(why) => {
                        say(&format!(
                            "error {workflow}: the ConfigMap was not created: {why}"
                        ));
                        clean = false;
                    }
                }
            }
            Action::Skip {
                workflow,
                namespace,
                label,
                tree,
                reason,
            } => {
                let json = configmap_json(
                    &access.namespace,
                    &Record {
                        workflow,
                        namespace,
                        label,
                        tree,
                        block: "",
                        skipped: Some(reason),
                        expires,
                    },
                );
                match access.create(json) {
                    Ok(_) => {
                        recorded.insert(workflow.clone());
                        say(&format!("skipped {workflow}: {reason}"));
                    }
                    Err(why) => {
                        say(&format!(
                            "error {workflow}: the skipped record was not created: {why}"
                        ));
                        clean = false;
                    }
                }
            }
            Action::Delete { configmap } => {
                if let Err(why) = access.delete(configmap) {
                    say(&format!(
                        "error {configmap}: the expired ConfigMap was not deleted: {why}"
                    ));
                    clean = false;
                }
            }
        }
    }
    let backlog = planned
        .backlog
        .iter()
        .filter(|w| !recorded.contains(*w))
        .count();
    // Every pass, 0 included: a monitor reading the log tells "none" from
    // "the line is missing".
    say(&format!(
        "backlog {backlog}: finished Workflows older than {} minutes without a ConfigMap",
        BACKLOG_SECS / 60
    ));
    clean
}

/// SIGTERM and SIGINT set [`STOP`] and nothing else: a store to an atomic is
/// async-signal-safe, and the idle wait polls the flag every second, so no
/// self-pipe or watcher thread is needed. Never re-raised: the loop ends and
/// the process exits 0.
#[cfg(unix)]
fn install_stop_handler() {
    use crate::posix_signal::{libc_signal, SIGINT, SIGTERM};
    extern "C" fn on_signal(_sig: i32) {
        STOP.store(true, Ordering::SeqCst);
    }
    unsafe {
        for sig in [SIGINT, SIGTERM] {
            libc_signal(sig, on_signal as *const () as usize);
        }
    }
}

#[cfg(not(unix))]
fn install_stop_handler() {}

fn stopping() -> bool {
    STOP.load(Ordering::SeqCst)
}

/// The key, checked once: `Err` is the line to print.
fn key_from_env() -> Result<PathBuf, String> {
    let Some(key) = std::env::var_os(KEY_ENV).filter(|k| !k.is_empty()) else {
        return Err(format!("{KEY_ENV} is not set: the signer has no key"));
    };
    let key = PathBuf::from(key);
    git_attest::sign::public_key(&key).map_err(|e| format!("{KEY_ENV}: {}: {e}", key.display()))?;
    Ok(key)
}

/// `greengate cluster sign [--watch]`, `args` after `sign`. One pass: exit 0
/// when clean, 1 when the cluster or signing failed, 2 on usage or a key
/// problem. `--watch`: passes every [`WATCH_SECS`] until SIGTERM or SIGINT,
/// then 0; a failed pass is logged and the loop goes on.
pub(super) fn sign_command(args: &[std::ffi::OsString]) -> i32 {
    let watch = match args {
        [] => false,
        [flag] if flag == "--watch" => true,
        _ => {
            eprintln!("{USAGE}");
            return 2;
        }
    };
    let key = match key_from_env() {
        Ok(key) => key,
        Err(line) => {
            say(&line);
            return 2;
        }
    };
    let access = Access::from_env();
    if !watch {
        return if run_pass(&access, &key) { 0 } else { 1 };
    }
    install_stop_handler();
    while !stopping() {
        // A failed pass said why, line by line; the next one tries again.
        let _clean = run_pass(&access, &key);
        // `sleep` resumes after a signal: slices of a second, and the flag.
        for _ in 0..WATCH_SECS {
            if stopping() {
                break;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::super::collect::tests::{compact, fixture};
    use super::super::pending::tests::sample;
    use super::super::pending::Pending;
    use super::super::workflow::{node_name, workflow_json, workflow_name, Plan};
    use super::*;

    /// The first live run's tree and image: the pod fixture reports them.
    const LIVE_TREE: &str = "13b5ee2d54389399fa2738f084a1b653b5844028";
    const NOW: u64 = 1_800_000_000;

    fn record(label: &str) -> Pending {
        let mut r = sample(&workflow_name(label));
        r.label = label.into();
        r.tree = LIVE_TREE.into();
        r.gates[0].prepare = None;
        r
    }

    /// `main`'s termination in the pod fixture: exit 0.
    const MAIN_DONE: &str =
        "\"exitCode\":0,\"finishedAt\":\"2026-11-03T04:40:34Z\",\"reason\":\"Completed\"";
    const MAIN_FAILED: &str =
        "\"exitCode\":1,\"finishedAt\":\"2026-11-03T04:40:34Z\",\"reason\":\"Error\"";

    /// The Workflow greengate submits for `r`, as the cluster reports it once
    /// it is in `phase`, finished at `finished`: one Pod node per gate.
    fn workflow(r: &Pending, phase: &str, finished: &str) -> String {
        let config = super::super::super::ClusterConfig {
            kubeconfig: PathBuf::from("/k"),
            namespace: "greengate-gates".into(),
            slots: 1,
            image: format!("reg/gate:1@sha256:{}", "a".repeat(64)),
            digest: format!("sha256:{}", "a".repeat(64)),
            max_bundle_mib: 1,
            start_wait: Duration::from_secs(30),
            timeout_secs: 900,
            prepare: None,
        };
        let json = workflow_json(&Plan {
            config: &config,
            record: r,
            repo_dir: "relais",
            url: "http://forgejo/x.git",
        });
        let with_uid = json.replacen(
            "\"metadata\":{\"name\"",
            &format!("\"metadata\":{{\"uid\":\"uid-{}\",\"name\"", r.name),
            1,
        );
        let nodes: Vec<String> = r
            .gates
            .iter()
            .map(|g| {
                format!(
                    "\"node-{t}\":{{\"id\":\"node-{t}\",\"name\":\"{}\",\"type\":\"Pod\",\
                     \"templateName\":\"gate-{t}\",\"phase\":\"{phase}\"}}",
                    node_name(&r.name, &g.task),
                    t = g.task
                )
            })
            .collect();
        format!(
            "{},\"status\":{{\"phase\":\"{phase}\",\"finishedAt\":\"{finished}\",\
             \"nodes\":{{{}}}}}}}",
            &with_uid[..with_uid.len() - 1],
            nodes.join(",")
        )
    }

    /// The first live run's pod, made the pod of `task` of `r`'s Workflow,
    /// then `tweak`ed.
    fn pod(r: &Pending, task: &str, tweak: impl Fn(String) -> String) -> String {
        let pod = compact(&fixture("pod-succeeded.json"))
            .replace(
                "\"workflows.argoproj.io/node-name\":\"greengate-handrun-pqtbl\"",
                &format!(
                    "\"workflows.argoproj.io/node-name\":\"{}\"",
                    node_name(&r.name, task)
                ),
            )
            .replace(
                "\"workflows.argoproj.io/node-id\":\"greengate-handrun-pqtbl\"",
                &format!("\"workflows.argoproj.io/node-id\":\"node-{task}\""),
            )
            .replace(
                "b8eb52b7-8ede-4c1f-bd94-9ada529eec97",
                &format!("uid-{}", r.name),
            )
            .replace("greengate-handrun-pqtbl", &r.name)
            .replace("handrun-2", &r.label);
        assert!(pod.contains(MAIN_DONE), "the fixture's main exited 0");
        tweak(pod)
    }

    fn main_failed(pod: String) -> String {
        pod.replacen(MAIN_DONE, MAIN_FAILED, 1)
    }

    fn list(items: &[String]) -> Value {
        crate::json_read::parse(&format!(
            "{{\"kind\":\"List\",\"items\":[{}]}}",
            items.join(",")
        ))
        .expect("list json")
    }

    fn no_configmaps() -> Value {
        list(&[])
    }

    fn configmap(name: &str, expires: u64) -> String {
        format!(
            "{{\"kind\":\"ConfigMap\",\"metadata\":{{\"name\":\"{name}\",\
             \"annotations\":{{\"greengate.signer/expires\":\"{expires}\"}}}}}}"
        )
    }

    const LONG_AGO: &str = "2027-01-15T07:57:20Z"; // NOW - 160
    const OLDER: &str = "2027-01-15T07:00:00Z"; // NOW - 3600

    /// In the pod there is no kubeconfig: kubectl's in-cluster default, and
    /// no `--kubeconfig` argument at all.
    #[test]
    fn kubectl_is_called_without_a_kubeconfig_unless_one_is_named() {
        let args = |access: &Access| -> Vec<String> {
            access
                .command("greengate-gates", &["get", "pods"])
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        let in_cluster = Access {
            kubeconfig: None,
            namespace: DEFAULT_NAMESPACE.into(),
        };
        assert_eq!(
            args(&in_cluster),
            [
                "--request-timeout=10s",
                "-n",
                "greengate-gates",
                "get",
                "pods"
            ]
        );
        let named = Access {
            kubeconfig: Some(PathBuf::from("/k.yaml")),
            namespace: DEFAULT_NAMESPACE.into(),
        };
        assert_eq!(
            args(&named),
            [
                "--kubeconfig",
                "/k.yaml",
                "--request-timeout=10s",
                "-n",
                "greengate-gates",
                "get",
                "pods"
            ]
        );
    }

    #[test]
    fn a_kubernetes_timestamp_reads_as_epoch_seconds() {
        assert_eq!(epoch_of("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(epoch_of("2026-11-03T04:40:34Z"), Some(1_793_680_834));
        assert_eq!(epoch_of("2000-02-29T12:00:00Z"), Some(951_825_600));
        assert_eq!(epoch_of(LONG_AGO), Some(NOW - 160));
        for bad in [
            "",
            "2026-11-03 04:40:34Z",
            "2026-13-03T04:40:34Z",
            "2026-11-03T04:40:34+01:00",
        ] {
            assert_eq!(epoch_of(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_finished_workflow_with_a_passing_owned_pod_is_signed() {
        let r = record("7-00000000000000aa-0");
        let runner = list(&[workflow(&r, "Succeeded", LONG_AGO), pod(&r, "g0", |p| p)]);
        let planned = plan_pass(&[("greengate-gates", runner)], &no_configmaps(), NOW);
        assert_eq!(
            planned.actions,
            [Action::Sign {
                workflow: r.name.clone(),
                namespace: "greengate-gates".into(),
                label: r.label.clone(),
                tree: LIVE_TREE.into(),
                gates: vec!["pre-push-lint".into()],
            }]
        );
        assert!(planned.backlog.is_empty(), "finished 160 s ago");
    }

    #[test]
    fn a_workflow_already_signed_or_still_running_is_left_alone() {
        let r = record("7-00000000000000aa-0");
        let runner = list(&[workflow(&r, "Succeeded", OLDER), pod(&r, "g0", |p| p)]);
        let signed = list(&[configmap(&r.name, NOW + 10)]);
        let planned = plan_pass(&[("greengate-gates", runner)], &signed, NOW);
        assert_eq!(planned, Planned::default());
        let runner = list(&[workflow(&r, "Running", ""), pod(&r, "g0", |p| p)]);
        let planned = plan_pass(&[("greengate-gates", runner)], &no_configmaps(), NOW);
        assert_eq!(planned, Planned::default());
    }

    #[test]
    fn an_old_finished_workflow_without_a_configmap_is_backlog() {
        let r = record("7-00000000000000aa-0");
        let runner = list(&[workflow(&r, "Failed", OLDER), pod(&r, "g0", |p| p)]);
        let planned = plan_pass(&[("greengate-gates", runner)], &no_configmaps(), NOW);
        assert_eq!(planned.backlog, vec![r.name]);
    }

    /// A bare Job's pod, and a pod a Job owns that wears the Workflow's run
    /// label and node annotations, prove nothing for the Workflow.
    #[test]
    fn pods_not_owned_by_the_workflow_are_ignored() {
        let r = record("7-00000000000000aa-0");
        let owner = format!(
            "\"kind\":\"Workflow\",\"name\":\"{}\",\"uid\":\"uid-{}\"",
            r.name, r.name
        );
        let job_owned = pod(&r, "g0", |p| {
            p.replace(
                &owner,
                "\"kind\":\"Job\",\"name\":\"x\",\"uid\":\"uid-job\"",
            )
        });
        assert!(
            job_owned.contains("\"kind\":\"Job\""),
            "the owner was forged"
        );
        let bare = pod(&r, "g0", |p| {
            let at = p.find(",\"ownerReferences\":[").expect("owners");
            let end = p[at..].find(']').expect("end") + at + 1;
            format!("{}{}", &p[..at], &p[end..])
        });
        assert!(!bare.contains("ownerReferences"));
        for (what, pod) in [("job-owned", job_owned), ("bare", bare)] {
            let runner = list(&[workflow(&r, "Succeeded", LONG_AGO), pod]);
            let planned = plan_pass(&[("greengate-gates", runner)], &no_configmaps(), NOW);
            match planned.actions.as_slice() {
                [Action::Skip { reason, .. }] => {
                    assert!(reason.contains("no pod of g0 owned"), "{what}: {reason}")
                }
                other => panic!("{what}: {other:?}"),
            }
        }
        // a pod alone, no Workflow: nothing at all
        let runner = list(&[pod(&r, "g0", |p| p)]);
        let planned = plan_pass(&[("greengate-gates", runner)], &no_configmaps(), NOW);
        assert_eq!(planned, Planned::default());
    }

    #[test]
    fn a_gate_that_did_not_pass_is_not_listed() {
        let mut r = record("7-00000000000000aa-0");
        let mut second = r.gates[0].clone();
        second.id = "pre-push-test".into();
        second.task = "g1".into();
        r.gates.push(second);
        // g0 passes; g1's main exits 1
        let wf = workflow(&r, "Failed", LONG_AGO);
        let failing = pod(&r, "g1", main_failed);
        let runner = list(&[wf.clone(), pod(&r, "g0", |p| p), failing.clone()]);
        let planned = plan_pass(&[("greengate-gates", runner)], &no_configmaps(), NOW);
        match planned.actions.as_slice() {
            [Action::Sign { gates, .. }] => assert_eq!(gates, &["pre-push-lint"]),
            other => panic!("{other:?}"),
        }
        // with g0 failing too, nothing passed: recorded as skipped
        let runner = list(&[wf, pod(&r, "g0", main_failed), failing]);
        let planned = plan_pass(&[("greengate-gates", runner)], &no_configmaps(), NOW);
        match planned.actions.as_slice() {
            [Action::Skip { reason, tree, .. }] => {
                assert!(reason.starts_with("no gate passed"), "{reason}");
                assert!(reason.contains("pre-push-test: "), "{reason}");
                assert_eq!(tree, LIVE_TREE);
            }
            other => panic!("{other:?}"),
        }
    }

    /// The hand-run fixture names no gate and no tree: greengate did not
    /// submit it, and it is never signed.
    #[test]
    fn a_workflow_greengate_did_not_submit_is_skipped() {
        let wf = compact(&fixture("workflow-succeeded.json"));
        let runner = list(&[wf, compact(&fixture("pod-succeeded.json"))]);
        let planned = plan_pass(&[("amont-gates", runner)], &no_configmaps(), NOW);
        match planned.actions.as_slice() {
            [Action::Skip { reason, .. }] => assert!(reason.contains("no tree"), "{reason}"),
            other => panic!("{other:?}"),
        }
        // a gate id with a space would be two gates in the payload
        let r = record("7-00000000000000aa-0");
        let wf = workflow(&r, "Succeeded", LONG_AGO).replace(
            "\"greengate.dev/gate\":\"pre-push-lint\"",
            "\"greengate.dev/gate\":\"pre-push-lint other\"",
        );
        let runner = list(&[wf, pod(&r, "g0", |p| p)]);
        let planned = plan_pass(&[("greengate-gates", runner)], &no_configmaps(), NOW);
        match planned.actions.as_slice() {
            [Action::Skip { reason, .. }] => assert!(reason.contains("invalid gate"), "{reason}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_expired_configmap_is_deleted() {
        let signer = list(&[configmap("old", NOW - 1), configmap("young", NOW + 1)]);
        let planned = plan_pass(&[], &signer, NOW);
        assert_eq!(
            planned.actions,
            [Action::Delete {
                configmap: "old".into()
            }]
        );
    }

    #[test]
    fn the_payload_is_git_attest_v2_and_reads_back() {
        let text = payload(LIVE_TREE, &["a".into(), "b.c".into()]);
        assert_eq!(
            text,
            format!(
                "amont-attest-v2\ntree {LIVE_TREE}\ngates a b.c\nplatform x86_64-linux\namont {}\n",
                env!("CARGO_PKG_VERSION")
            )
        );
        use git_attest::format::field;
        assert_eq!(field(&text, "tree"), Some(LIVE_TREE));
        assert_eq!(field(&text, "gates"), Some("a b.c"));
        assert_eq!(field(&text, "platform"), Some("x86_64-linux"));
        assert_eq!(field(&text, "amont"), Some(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn the_configmap_carries_the_contract() {
        let json = configmap_json(
            "attest-signer",
            &Record {
                workflow: "greengate-rh-1",
                namespace: "greengate-gates",
                label: "1",
                tree: LIVE_TREE,
                block: "p\n\nsig",
                skipped: Some("no gate passed"),
                expires: 42,
            },
        );
        let doc = crate::json_read::parse(&json).expect("valid JSON");
        let metadata = doc.get("metadata");
        assert_eq!(text(metadata, "name"), Some("greengate-rh-1"));
        assert_eq!(text(metadata, "namespace"), Some("attest-signer"));
        let labels = metadata.and_then(|m| m.get("labels"));
        assert_eq!(text(labels, "greengate.run"), Some("1"));
        assert_eq!(text(labels, "greengate.signer"), Some("block"));
        let annotations = metadata.and_then(|m| m.get("annotations"));
        assert_eq!(
            text(annotations, "greengate.signer/workflow-namespace"),
            Some("greengate-gates")
        );
        assert_eq!(text(annotations, "greengate.signer/tree"), Some(LIVE_TREE));
        assert_eq!(text(annotations, "greengate.signer/expires"), Some("42"));
        assert_eq!(text(doc.get("data"), "block"), Some("p\n\nsig"));
        assert_eq!(text(doc.get("data"), "skipped"), Some("no gate passed"));
    }

    /// A block signed with a throwaway key splits back out of the note text
    /// and verifies for the platform-bound principal (git-attest 1.8), as
    /// collect will read it.
    #[test]
    fn a_signed_block_verifies_for_the_platform_bound_principal() {
        let dir = std::env::temp_dir().join(format!("greengate-sign-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); // best-effort: a scratch path
        std::fs::create_dir_all(&dir).unwrap();
        let key = dir.join("key");
        let made = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "t", "-f"])
            .arg(&key)
            .stdin(Stdio::null())
            .status();
        if !made.is_ok_and(|s| s.success()) {
            assert!(
                std::env::var_os("CI").is_none(),
                "ssh-keygen is missing on CI"
            );
            eprintln!("ssh-keygen unavailable — skipping");
            return;
        }
        let public = git_attest::sign::public_key(&key).expect("public key");
        let signers = dir.join("allowed_signers");
        std::fs::write(
            &signers,
            format!(
                "cluster-signer@x86_64-linux namespaces=\"{}\" {public}\n",
                git_attest::format::NAMESPACE
            ),
        )
        .unwrap();
        let text = payload(LIVE_TREE, &["pre-push-lint".into()]);
        let block = signed_block(&text, &key).expect("signed");
        let (blocks, _) = git_attest::format::split_blocks(&block);
        assert_eq!(blocks.len(), 1, "{block}");
        assert_eq!(blocks[0].payload, text, "the trailing newline comes back");
        let verify = |principal: &str| {
            git_attest::sign::verify(
                git_attest::format::NAMESPACE,
                &blocks[0].payload,
                &blocks[0].signature,
                &signers,
                Some(principal),
            )
            .is_ok()
        };
        assert!(verify("cluster-signer@x86_64-linux"));
        assert!(!verify("someone@x86_64-linux"));
        let _ = std::fs::remove_dir_all(&dir); // best-effort
    }
}
