//! Collect: turn the passes of detached Workflows into cluster tokens
//! (ADR-0017), cheaply, at the start of a hook.
//!
//! With nothing pending it costs one `stat` and spawns no `kubectl`. With
//! something pending it takes the pending file's lock without waiting (busy
//! means somebody else is collecting: return), and asks the cluster ONE
//! question per hook process: `kubectl get workflows,pods -l 'amont.run in
//! (…)' -o json`, under a 1 s request timeout and a wall deadline. A failed
//! read keeps every record for the next hook.
//!
//! What a pod proves is read from its `containerStatuses` and
//! `initContainerStatuses` alone, as ADR-0016's Job does. The Workflow's and
//! its nodes' phases say only that the pod FINISHED: gate code can write what
//! Argo's executor reads, so a phase is never a pass. The pod must also be
//! the one submitted (its owner is the saved Workflow UID; its images, tree
//! and command are the frozen ones), and the token written is the one frozen
//! at submission.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::json_read::Value;

use super::super::{
    capture, judge, last_line, log_line, parse_pod_object, record_token, Call, Fallback, JobEnd,
    Kube, PodFacts, State, PLATFORM, RUN_LABEL_KEY,
};
use super::pending::{GateState, Pending, PendingError, RecordPhase, Store};
use super::workflow::{check_pod, check_workflow, node_name, template_name, TTL_SECS};

/// The wall deadline for the one batched call. `kubectl`'s own request timeout
/// is 1 s; this bounds the process around it.
const COLLECT_SECS: u64 = 5;
/// Bound on each `patch` of `--stop`, and on all of them together.
const PATCH_SECS: u64 = 10;
const STOP_SECS: u64 = 30;
/// An intent no Workflow appeared for in this long was never created.
// holds-until: an apiserver slower than this to show a created Workflow — a
// create that lands after the grace is not adopted, and its gates run at the
// push instead
const INTENT_GRACE_SECS: u64 = 600;
/// A record outlives its Workflow's TTL by this long, then goes whatever the
/// cluster says.
const EXPIRY_SLACK_SECS: u64 = 3600;

/// Once per hook process, however many callers ask. Process-scoped on
/// purpose: a hook runs as one process and asks the cluster one question, so
/// `stamps` inside a hook, or `--status` after a collect, do not repeat it;
/// the next hook is a new process and asks again.
static COLLECTED: AtomicBool = AtomicBool::new(false);

/// Why a collect learned nothing, or could not keep what it learned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Trouble {
    /// Another collect, or a submission, holds the pending file.
    Busy,
    /// The cluster did not answer.
    NotReached(String),
    /// The pending file could not be read or written.
    StoreFailed(PendingError),
}

impl std::fmt::Display for Trouble {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Trouble::Busy => write!(f, "another collect is running"),
            Trouble::NotReached(why) => write!(f, "cluster not reached: {why}"),
            Trouble::StoreFailed(why) => write!(f, "pending store failed: {why}"),
        }
    }
}

/// A Workflow that left the pending file in this collect, for `--status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Settled {
    pub name: String,
    /// The Workflow's last phase in plain words.
    pub phase: RecordPhase,
    pub passed: Vec<String>,
    pub failed: Vec<String>,
}

/// What a collect did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Collected {
    /// Passes written as cluster tokens.
    pub passes: usize,
    /// Workflows still pending afterwards.
    pub pending: usize,
    /// The Workflows this collect finished with.
    pub settled: Vec<Settled>,
    pub trouble: Option<Trouble>,
}

impl Collected {
    fn troubled(trouble: Trouble) -> Collected {
        Collected {
            trouble: Some(trouble),
            ..Collected::default()
        }
    }
}

/// A gate judged a pass: the token to write, frozen at submission.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pass {
    gate: String,
    tree: String,
    token: String,
    provenance: String,
}

/// What one record became.
#[derive(Debug)]
struct Resolution {
    /// The record as it stands now.
    record: Pending,
    /// Whether it leaves the pending file: its Workflow is finished and judged,
    /// or gone. A pass whose token is not written keeps it there all the same.
    leaves: bool,
    passes: Vec<Pass>,
    /// One `cluster.log` line per gate that is not a pass: `(gate, why)`.
    notes: Vec<(String, String)>,
}

/// Collect, once per hook process. Called at the start of pre-commit and
/// pre-push, by `amont stamps`, `amont cluster collect` and `rehearse
/// --status`.
pub(crate) fn collect() -> Collected {
    if COLLECTED.swap(true, Ordering::SeqCst) {
        return Collected::default();
    }
    Store::here().map_or_else(Collected::default, |store| collect_from(&store))
}

fn collect_from(store: &Store) -> Collected {
    if !store.exists() {
        return Collected::default();
    }
    // Busy: another hook is collecting, or a submission is writing.
    let Some(held) = store.try_lock() else {
        return Collected::troubled(Trouble::Busy);
    };
    let records = match store.read() {
        Ok(records) => records,
        Err(why) => {
            log_line("collect", &why.to_string());
            return Collected::troubled(Trouble::StoreFailed(why));
        }
    };
    if records.is_empty() {
        return Collected::default();
    }
    let now = crate::gate_evidence::now();
    let mut kept: Vec<Pending> = Vec::new();
    let mut settled: Vec<Settled> = Vec::new();
    let mut passes = 0;
    let mut unreached: Vec<String> = Vec::new();
    for group in by_cluster(records) {
        let kube = Kube {
            kubeconfig: group[0].kubeconfig.clone(),
            namespace: group[0].namespace.clone(),
        };
        match fetch(&kube, &group) {
            Ok(list) => {
                for record in group {
                    let done = settle(resolve(record, &list, now));
                    passes += done.written;
                    settled.extend(done.settled);
                    kept.extend(done.record);
                }
            }
            Err(why) => {
                log_line("collect", &format!("the cluster could not be read: {why}"));
                // The rollback says an unreadable record expires: it does,
                // even when the cluster cannot be asked.
                let (old, young): (Vec<Pending>, Vec<Pending>) =
                    group.into_iter().partition(|r| is_expired(r, now));
                for record in &old {
                    log_line(
                        record.gates.first().map_or("collect", |g| g.id.as_str()),
                        &format!(
                            "detached {}: older than the Workflow TTL; dropped",
                            record.name
                        ),
                    );
                }
                kept.extend(young);
                unreached.push(format!(
                    "{why} (namespace {}, kubeconfig {})",
                    kube.namespace,
                    kube.kubeconfig.display()
                ));
            }
        }
    }
    let trouble = match (store.write(&held, &kept), unreached) {
        (Err(why), _) => {
            log_line(
                "collect",
                &format!("cluster-pending was not updated: {why}"),
            );
            Some(Trouble::StoreFailed(why))
        }
        // Any group left unasked is named, even when another answered.
        (Ok(()), why) if !why.is_empty() => Some(Trouble::NotReached(why.join("; "))),
        (Ok(()), _) => None,
    };
    Collected {
        passes,
        pending: kept.len(),
        settled,
        trouble,
    }
}

fn is_expired(record: &Pending, now: u64) -> bool {
    now > record.submitted + TTL_SECS + EXPIRY_SLACK_SECS
}

/// The records grouped by the cluster they live on: one call each.
fn by_cluster(records: Vec<Pending>) -> Vec<Vec<Pending>> {
    let mut groups: Vec<Vec<Pending>> = Vec::new();
    for record in records {
        match groups
            .iter_mut()
            .find(|g| g[0].kubeconfig == record.kubeconfig && g[0].namespace == record.namespace)
        {
            Some(group) => group.push(record),
            None => groups.push(vec![record]),
        }
    }
    groups
}

/// The one batched call: every Workflow and pod carrying one of the records'
/// run labels.
fn fetch(kube: &Kube, records: &[Pending]) -> Result<Value, String> {
    let labels: Vec<&str> = records.iter().map(|r| r.label.as_str()).collect();
    let selector = format!("{RUN_LABEL_KEY} in ({})", labels.join(","));
    let mut cmd = kube.command(
        "--request-timeout=1s",
        &["get", "workflows,pods", "-l", &selector, "-o", "json"],
    );
    match capture(&mut cmd, COLLECT_SECS) {
        Call::Ok(json) => crate::json_read::parse(&json)
            .ok_or_else(|| "kubectl printed something that is not JSON".to_string()),
        Call::Refused(e) | Call::NotStarted(e) | Call::TimedOut(e) => Err(last_line(&e)),
    }
}

/// What settling a resolution left.
struct Settlement {
    /// The record if it stays in the file.
    record: Option<Pending>,
    /// Tokens written.
    written: usize,
    /// The record's end, when it left the file.
    settled: Option<Settled>,
}

/// [`settle_with`], writing tokens to the cluster note.
fn settle(resolution: Resolution) -> Settlement {
    settle_with(resolution, |pass| {
        record_token(&pass.tree, &pass.token, &pass.provenance)
    })
}

/// Write the passes of a resolution through `write` and log its notes. A pass
/// whose token was not written returns its gate to waiting and keeps the
/// record in the file, finished Workflow or not, so that the next collect
/// tries again.
fn settle_with(
    resolution: Resolution,
    mut write: impl FnMut(&Pass) -> Result<(), Fallback>,
) -> Settlement {
    let Resolution {
        mut record,
        leaves,
        passes,
        notes,
    } = resolution;
    for (gate, why) in &notes {
        log_line(gate, why);
    }
    let mut written = 0;
    let mut unwritten = 0;
    for pass in &passes {
        match write(pass) {
            Ok(()) => {
                written += 1;
                // stderr: `amont stamps --json` collects first, and its stdout
                // is the JSON object alone.
                eprintln!(
                    "collected the cluster pass of {} for tree {}",
                    crate::ui::sanitize(&pass.gate),
                    short(&pass.tree)
                );
            }
            Err(why) => {
                log_line(
                    &pass.gate,
                    &format!("detached: the pass was not recorded: {why}"),
                );
                unwritten += 1;
                for gate in record.gates.iter_mut().filter(|g| g.id == pass.gate) {
                    gate.state = GateState::Waiting;
                }
            }
        }
    }
    if !leaves || unwritten > 0 {
        return Settlement {
            record: Some(record),
            written,
            settled: None,
        };
    }
    let ids = |state: GateState| -> Vec<String> {
        record
            .gates
            .iter()
            .filter(|g| g.state == state)
            .map(|g| g.id.clone())
            .collect()
    };
    let settled = Settled {
        name: record.name.clone(),
        phase: record.phase,
        passed: ids(GateState::Passed),
        failed: ids(GateState::Failed),
    };
    Settlement {
        record: None,
        written,
        settled: Some(settled),
    }
}

fn short(oid: &str) -> &str {
    oid.get(..8).unwrap_or(oid)
}

fn text<'a>(v: Option<&'a Value>, key: &str) -> Option<&'a str> {
    v?.get(key).and_then(Value::as_str)
}

fn kind_of<'a>(list: &'a Value, kind: &'a str) -> impl Iterator<Item = &'a Value> {
    list.get("items")
        .map(Value::items)
        .unwrap_or(&[])
        .iter()
        .filter(move |i| text(Some(i), "kind") == Some(kind))
}

/// The pod of the Workflow `uid` that ran `node`: named so by Argo's
/// annotation, owned by that Workflow, and a Pod node of the Workflow's own
/// status of the template the gate was submitted under.
fn pod_of<'a>(list: &'a Value, wf: &Value, name: &str, uid: &str, task: &str) -> Option<&'a Value> {
    let node = node_name(name, task);
    let recorded = wf
        .get("status")
        .and_then(|s| s.get("nodes"))
        .map(Value::members)
        .unwrap_or(&[])
        .iter()
        .map(|(_, n)| n)
        .find(|n| {
            text(Some(n), "name") == Some(node.as_str())
                && text(Some(n), "type") == Some("Pod")
                && text(Some(n), "templateName") == Some(template_name(task).as_str())
        })?;
    kind_of(list, "Pod").find(|pod| {
        let metadata = pod.get("metadata");
        let annotations = metadata.and_then(|m| m.get("annotations"));
        let owned = metadata
            .and_then(|m| m.get("ownerReferences"))
            .map(Value::items)
            .unwrap_or(&[])
            .iter()
            .any(|o| {
                text(Some(o), "kind") == Some("Workflow")
                    && text(Some(o), "uid") == Some(uid)
                    && text(Some(o), "name") == Some(name)
                    && o.get("controller").is_some_and(Value::is_true)
            });
        // The node id is required, not optional: a pod that does not name its
        // node cannot be bound to the Workflow's record of it.
        let node_id = text(annotations, "workflows.argoproj.io/node-id");
        owned
            && text(annotations, "workflows.argoproj.io/node-name") == Some(node.as_str())
            && node_id.is_some()
            && node_id == text(Some(recorded), "id")
    })
}

/// The Job-shaped end of a Workflow pod, from `main`'s termination status
/// alone: exit 0 is `Complete`, any other exit is `Failed`, and a container
/// that has not terminated is `Running`. Workflow and node phases are never
/// read here: gate code can write what Argo's executor reports from.
fn end_from_main(main: &State) -> JobEnd {
    match main {
        State::Terminated { exit: Some(0), .. } => JobEnd::Complete,
        State::Terminated { reason, .. } => JobEnd::Failed(if reason.is_empty() {
            "Error".to_string()
        } else {
            reason.clone()
        }),
        State::Absent | State::Waiting | State::Running => JobEnd::Running,
    }
}

/// Why a pod that never ended `main` is no pass, as the pod says it.
fn unfinished(facts: &PodFacts) -> String {
    if let Some(w) = facts.fetch.failed() {
        format!("fetch {w}")
    } else if let Some(w) = facts.prepare.failed() {
        format!("prepare {w}")
    } else {
        "the pod ended without a verdict from main".to_string()
    }
}

/// Decide what the cluster's answer means for one record. Pure.
fn resolve(record: Pending, list: &Value, now: u64) -> Resolution {
    let mut notes: Vec<(String, String)> = Vec::new();
    let wf = kind_of(list, "Workflow").find(|w| {
        text(w.get("metadata"), "name") == Some(record.name.as_str())
            && text(
                w.get("metadata").and_then(|m| m.get("labels")),
                RUN_LABEL_KEY,
            ) == Some(record.label.as_str())
    });
    // The record ends here, its waiting gates failed, one note each.
    let drop_all = |mut record: Pending, why: &str| {
        let notes = record
            .waiting()
            .map(|g| (g.id.clone(), format!("detached {}: {why}", record.name)))
            .collect();
        for gate in record
            .gates
            .iter_mut()
            .filter(|g| g.state == GateState::Waiting)
        {
            gate.state = GateState::Failed;
        }
        record.phase = RecordPhase::Gone;
        Resolution {
            record,
            leaves: true,
            passes: Vec::new(),
            notes,
        }
    };
    let Some(wf) = wf else {
        return if record.uid.is_some() {
            drop_all(
                record,
                "the Workflow is gone (its TTL passed, or it was deleted)",
            )
        } else if is_expired(&record, now) || now > record.submitted + INTENT_GRACE_SECS {
            drop_all(record, "the Workflow never appeared")
        } else {
            // A create that may still land: the intent stays.
            Resolution {
                record,
                leaves: false,
                passes: Vec::new(),
                notes,
            }
        };
    };
    if is_expired(&record, now) {
        return drop_all(record, "older than the Workflow TTL");
    }
    let found_uid = text(wf.get("metadata"), "uid").map(str::to_string);
    let uid = match (&record.uid, &found_uid) {
        (Some(saved), Some(found)) if saved == found => saved.clone(),
        (Some(_), _) => {
            return drop_all(
                record,
                "another Workflow than the one submitted holds the name",
            )
        }
        (None, Some(found)) => match check_workflow(wf, &record) {
            Ok(()) => found.clone(),
            Err(why) => {
                return drop_all(record, &format!("not the Workflow submitted: {why}"));
            }
        },
        (None, None) => return drop_all(record, "the Workflow has no UID"),
    };
    let phase = RecordPhase::parse(text(wf.get("status"), "phase").unwrap_or(""));
    let mut passes: Vec<Pass> = Vec::new();
    let mut gates = record.gates.clone();
    for gate in gates.iter_mut().filter(|g| g.state == GateState::Waiting) {
        let Some(pod) = pod_of(list, wf, &record.name, &uid, &gate.task) else {
            if phase.finished() {
                gate.state = GateState::Failed;
                notes.push((
                    gate.id.clone(),
                    format!(
                        "detached {}: no pod of {} was found ({})",
                        record.name,
                        gate.task,
                        phase.as_str()
                    ),
                ));
            }
            continue;
        };
        if let Err(why) = check_pod(pod, gate, &record) {
            gate.state = GateState::Failed;
            notes.push((
                gate.id.clone(),
                format!(
                    "detached {}: the pod is not the one submitted: {why}",
                    record.name
                ),
            ));
            continue;
        }
        let Some(facts) = parse_pod_object(pod, "main") else {
            if phase.finished() {
                gate.state = GateState::Failed;
                notes.push((
                    gate.id.clone(),
                    format!("detached {}: the pod's status cannot be read", record.name),
                ));
            }
            continue;
        };
        let end = end_from_main(&facts.gate);
        if end == JobEnd::Running {
            if phase.finished() {
                gate.state = GateState::Failed;
                notes.push((
                    gate.id.clone(),
                    format!("detached {}: {}", record.name, unfinished(&facts)),
                ));
            }
            continue;
        }
        match judge(&end, &facts, &record.tree, gate.prepare.as_deref()) {
            Ok(image_id) => {
                gate.state = GateState::Passed;
                passes.push(Pass {
                    gate: gate.id.clone(),
                    tree: record.tree.clone(),
                    token: gate.token.clone(),
                    provenance: format!(
                        "platform={PLATFORM} image={image_id} job={uid} name={} bundle=0",
                        node_name(&record.name, &gate.task)
                    ),
                });
            }
            Err(fallback) => {
                gate.state = GateState::Failed;
                notes.push((
                    gate.id.clone(),
                    format!("detached {}: {}", record.name, fallback),
                ));
            }
        }
    }
    let settled = gates.iter().all(|g| g.state != GateState::Waiting);
    Resolution {
        leaves: settled || phase.finished(),
        record: Pending {
            uid: Some(uid),
            phase,
            gates,
            ..record
        },
        passes,
        notes,
    }
}

/// `amont cluster collect`: exit 0 when it asked the cluster (or had nothing
/// to ask, or another collect is at it), 1 when it learned nothing or could
/// not keep what it learned, 2 on usage.
pub fn collect_command(args: &[std::ffi::OsString]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("usage: {} cluster collect", crate::names::BIN);
        return 0;
    }
    match args.first().and_then(|a| a.to_str()) {
        Some("collect") if args.len() == 1 => {
            let done = collect();
            match &done.trouble {
                Some(Trouble::Busy) => {
                    eprintln!("{}; nothing was asked of the cluster", Trouble::Busy);
                    0
                }
                Some(trouble) => {
                    eprintln!("{trouble}");
                    1
                }
                None => {
                    eprintln!(
                        "collected {} cluster pass{}; {} Workflow{} still pending",
                        done.passes,
                        if done.passes == 1 { "" } else { "es" },
                        done.pending,
                        if done.pending == 1 { "" } else { "s" },
                    );
                    0
                }
            }
        }
        _ => {
            eprintln!("usage: {} cluster collect", crate::names::BIN);
            2
        }
    }
}

/// `rehearse --status`: one line per pending Workflow, its phase as the last
/// collect saw it and the gates collected so far.
pub(crate) fn status_lines() -> Vec<String> {
    let Some(store) = Store::here() else {
        return Vec::new();
    };
    match store.read() {
        Ok(records) => records.iter().map(describe).collect(),
        Err(why) => {
            eprintln!("cluster run records cannot be read: {why}");
            Vec::new()
        }
    }
}

/// The Workflows a collect finished with, for the person who asked.
pub(crate) fn settled_lines(settled: &[Settled]) -> Vec<String> {
    settled
        .iter()
        .map(|run| {
            let mut parts: Vec<String> = Vec::new();
            if !run.passed.is_empty() {
                parts.push(format!("collected: {}", run.passed.join(" ")));
            }
            if !run.failed.is_empty() {
                parts.push(format!(
                    "did not pass: {} — see cluster.log; the gate runs at the push",
                    run.failed.join(" ")
                ));
            }
            format!(
                "cluster run {} {}: {}",
                run.name,
                run.phase.plain(),
                parts.join("; ")
            )
        })
        .collect()
}

fn describe(record: &Pending) -> String {
    let named = |label: &str, state: GateState| -> Option<String> {
        let ids: Vec<&str> = record
            .gates
            .iter()
            .filter(|g| g.state == state)
            .map(|g| g.id.as_str())
            .collect();
        (!ids.is_empty()).then(|| format!("{label}: {}", ids.join(" ")))
    };
    let fields: Vec<String> = [
        named("collected", GateState::Passed),
        named("did not pass", GateState::Failed),
        named("waiting", GateState::Waiting),
    ]
    .into_iter()
    .flatten()
    .collect();
    let follow = if record.phase.finished() {
        String::new()
    } else {
        format!(
            " (follow: kubectl -n {} get wf {})",
            record.namespace, record.name
        )
    };
    format!(
        "cluster run {}: {}{}{follow}",
        record.name,
        record.phase.plain(),
        if fields.is_empty() {
            String::new()
        } else {
            format!(" — {}", fields.join("; "))
        }
    )
}

/// The command that does by hand what `--stop` does for one Workflow.
fn terminate_command(record: &Pending) -> String {
    format!(
        "kubectl --kubeconfig {} -n {} patch workflow {} --type merge -p '{{\"spec\":{{\"shutdown\":\"Terminate\"}}}}'",
        shell_quote(&record.kubeconfig.to_string_lossy()),
        record.namespace,
        record.name
    )
}

/// `text` as one word for a POSIX shell.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// What `rehearse --stop` did to the pending Workflows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Stopped {
    /// One line per Workflow asked to terminate.
    pub done: Vec<String>,
    /// One line per Workflow that was not, with the command to do it by hand.
    pub failed: Vec<String>,
}

/// `rehearse --stop`: ask the cluster to terminate every pending Workflow,
/// each call and all of them bounded. Once the overall deadline is gone the
/// rest are reported as not attempted.
pub(crate) fn stop_pending() -> Stopped {
    let mut report = Stopped::default();
    let Some(store) = Store::here() else {
        return report;
    };
    let records = match store.read() {
        Ok(records) => records,
        Err(why) => {
            report
                .failed
                .push(format!("cluster run records cannot be read: {why}"));
            return report;
        }
    };
    let until = std::time::Instant::now() + std::time::Duration::from_secs(STOP_SECS);
    for record in &records {
        let left = until.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            report.failed.push(format!(
                "cluster run {} was not attempted: the stop deadline passed — {}",
                record.name,
                terminate_command(record)
            ));
            continue;
        }
        let kube = Kube {
            kubeconfig: record.kubeconfig.clone(),
            namespace: record.namespace.clone(),
        };
        let patched = kube.bounded(
            &[
                "patch",
                "workflow",
                &record.name,
                "--type",
                "merge",
                "-p",
                "{\"spec\":{\"shutdown\":\"Terminate\"}}",
            ],
            left.as_secs().clamp(1, PATCH_SECS),
        );
        match patched {
            Ok(()) => report
                .done
                .push(format!("terminating cluster run {}", record.name)),
            Err(why) => report.failed.push(format!(
                "could not terminate cluster run {}: {why} — {}",
                record.name,
                terminate_command(record)
            )),
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::super::pending::tests::sample;
    use super::*;

    fn fixture(name: &str) -> String {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../amont/tests/it/fixtures/argo");
        std::fs::read_to_string(dir.join(name)).expect("fixture")
    }

    /// `text` without the whitespace outside strings, so a replacement does
    /// not depend on how the fixture was indented.
    fn compact(text: &str) -> String {
        let (mut out, mut in_string, mut escaped) = (String::new(), false, false);
        for c in text.chars() {
            if in_string {
                out.push(c);
                match (escaped, c) {
                    (true, _) => escaped = false,
                    (false, '\\') => escaped = true,
                    (false, '"') => in_string = false,
                    (false, _) => {}
                }
            } else if c == '"' {
                in_string = true;
                out.push(c);
            } else if !c.is_whitespace() {
                out.push(c);
            }
        }
        out
    }

    /// What the cluster answers for `record`: the first live run's pod, made
    /// the pod of task `g0` of the Workflow, and a Workflow in `phase` whose
    /// node for it is in `node_phase`. `tweak` edits the pod's compact text.
    fn list_for(
        record: &Pending,
        phase: &str,
        node_phase: &str,
        tweak: impl Fn(String) -> String,
    ) -> Value {
        let node = node_name(&record.name, "g0");
        let pod = compact(&fixture("pod-succeeded.json"))
            .replace(
                "\"workflows.argoproj.io/node-name\":\"amont-handrun-pqtbl\"",
                &format!("\"workflows.argoproj.io/node-name\":\"{node}\""),
            )
            .replace(
                "\"workflows.argoproj.io/node-id\":\"amont-handrun-pqtbl\"",
                "\"workflows.argoproj.io/node-id\":\"node-g0\"",
            )
            .replace("amont-handrun-pqtbl", &record.name)
            .replace("b8eb52b7-8ede-4c1f-bd94-9ada529eec97", "uid-1")
            .replace("handrun-2", &record.label);
        let wf = format!(
            "{{\"kind\":\"Workflow\",\"metadata\":{{\"name\":\"{}\",\"uid\":\"uid-1\",\
             \"labels\":{{\"amont.run\":\"{}\"}}}},\"status\":{{\"phase\":\"{phase}\",\
             \"nodes\":{{\"node-g0\":{{\"id\":\"node-g0\",\"name\":\"{node}\",\"type\":\"Pod\",\
             \"templateName\":\"gate-g0\",\"phase\":\"{node_phase}\"}}}}}}}}",
            record.name, record.label
        );
        let list = format!("{{\"kind\":\"List\",\"items\":[{wf},{}]}}", tweak(pod));
        crate::json_read::parse(&list).expect("list json")
    }

    #[test]
    fn a_workflow_phase_is_only_ever_finished_or_not() {
        for (phase, finished) in [
            ("Succeeded", true),
            ("Failed", true),
            ("Error", true),
            ("Running", false),
            ("Pending", false),
            ("", false),
        ] {
            assert_eq!(RecordPhase::parse(phase).finished(), finished, "{phase}");
        }
    }

    /// `main` alone decides: exit 0 is the only pass, whatever else the pod
    /// or the Workflow says.
    #[test]
    fn the_end_of_a_pod_comes_from_main_alone() {
        let ended = |exit, reason: &str| State::Terminated {
            exit: Some(exit),
            reason: reason.into(),
            message: String::new(),
        };
        assert_eq!(end_from_main(&ended(0, "Completed")), JobEnd::Complete);
        assert_eq!(
            end_from_main(&ended(1, "Error")),
            JobEnd::Failed("Error".into())
        );
        assert_eq!(
            end_from_main(&ended(137, "OOMKilled")),
            JobEnd::Failed("OOMKilled".into())
        );
        assert_eq!(end_from_main(&State::Running), JobEnd::Running);
        assert_eq!(end_from_main(&State::Waiting), JobEnd::Running);
        assert_eq!(end_from_main(&State::Absent), JobEnd::Running);
    }

    fn record_of_the_fixture() -> Pending {
        let mut record = sample("amont-rh-test");
        record.uid = Some("uid-1".into());
        record.label = "label-1".into();
        record.tree = "13b5ee2d54389399fa2738f084a1b653b5844028".into();
        record.source.oid = "7e04624939f8c93f774d570276af8643c61f0320".into();
        record.gates[0].argv = vec!["python3".into(), "scripts/check-module-cycles.py".into()];
        record.gates[0].prepare = None;
        record.gates[0].digest =
            "sha256:fa7a661059493955d9590559a3ff62bc956fedd83f41eea739e73d1996797f7c".into();
        record
    }

    fn collected(record: Pending, list: &Value) -> Resolution {
        resolve(record, list, 1_700_000_100)
    }

    /// The first live run, collected: a pass written under the FROZEN token,
    /// and the finished Workflow leaves the file.
    #[test]
    fn the_live_fixture_is_a_pass() {
        let record = record_of_the_fixture();
        let list = list_for(&record, "Succeeded", "Succeeded", |p| p);
        let done = collected(record, &list);
        assert!(done.notes.is_empty(), "{:?}", done.notes);
        assert_eq!(done.passes.len(), 1, "{done:?}");
        assert_eq!(
            done.passes[0].token, "pre-push-lint@abc",
            "the frozen token"
        );
        assert!(done.passes[0].provenance.contains("platform=x86_64-linux"));
        assert!(done.leaves, "a finished Workflow leaves the file");
    }

    /// The node and the Workflow say Succeeded; `main` exited 1. Phases only
    /// say the pod finished.
    #[test]
    fn a_succeeded_node_with_a_failed_main_is_no_pass() {
        let record = record_of_the_fixture();
        let list = list_for(&record, "Succeeded", "Succeeded", |p| {
            p.replacen(
                "\"exitCode\":0,\"finishedAt\":\"2026-11-03T04:40:34Z\"",
                "\"exitCode\":1,\"finishedAt\":\"2026-11-03T04:40:34Z\"",
                1,
            )
        });
        let done = collected(record, &list);
        assert!(done.passes.is_empty(), "{done:?}");
        assert_eq!(done.notes.len(), 1, "{done:?}");
        assert!(done.notes[0].1.contains("exit 1"), "{:?}", done.notes);
    }

    #[test]
    fn a_pod_of_another_workflow_uid_is_not_this_ones() {
        let record = record_of_the_fixture();
        let list = list_for(&record, "Succeeded", "Succeeded", |p| {
            p.replace("\"uid\":\"uid-1\"", "\"uid\":\"uid-other\"")
        });
        let done = collected(record, &list);
        assert!(done.passes.is_empty(), "{done:?}");
    }

    #[test]
    fn a_pod_with_another_image_or_command_is_not_the_submission() {
        for tweak in [
            |p: String| {
                p.replace(
                    "sha256:fa7a661059493955d9590559a3ff62bc956fedd83f41eea739e73d1996797f7c",
                    &format!("sha256:{}", "0".repeat(64)),
                )
            },
            |p: String| p.replace("scripts/check-module-cycles.py", "scripts/other.py"),
            |p: String| {
                p.replace(
                    "tree 13b5ee2d54389399fa2738f084a1b653b5844028",
                    "tree 0000000000000000000000000000000000000000",
                )
            },
        ] {
            let record = record_of_the_fixture();
            let list = list_for(&record, "Succeeded", "Succeeded", tweak);
            let done = collected(record, &list);
            assert!(done.passes.is_empty(), "{done:?}");
            assert_eq!(done.notes.len(), 1, "{done:?}");
        }
    }

    #[test]
    fn a_running_workflow_keeps_its_waiting_gates() {
        let record = record_of_the_fixture();
        let list = list_for(&record, "Running", "Running", |p| {
            // `main` has not terminated: the same state, reopened as running
            p.replace(
                "\"terminated\":{\"containerID\":\"containerd://72683744e4b32662fcdee44e7d5fd846899a8b994145dfc21a07ee4d385f3989\"",
                "\"running\":{\"startedAt\":\"t\",\"containerID\":\"containerd://72683744e4b32662fcdee44e7d5fd846899a8b994145dfc21a07ee4d385f3989\"",
            )
        });
        let done = collected(record, &list);
        assert!(done.passes.is_empty(), "{done:?}");
        assert!(!done.leaves);
        assert_eq!(done.record.gates[0].state, GateState::Waiting);
        assert_eq!(done.record.phase, RecordPhase::Running);
    }

    #[test]
    fn a_workflow_gone_after_its_ttl_drops_the_record_with_one_note() {
        let record = record_of_the_fixture();
        let empty = crate::json_read::parse("{\"kind\":\"List\",\"items\":[]}").expect("json");
        let done = collected(record, &empty);
        assert!(done.leaves);
        assert_eq!(done.record.phase, RecordPhase::Gone);
        assert_eq!(done.record.phase.plain(), "gone (TTL passed or deleted)");
        assert_eq!(done.notes.len(), 1, "{done:?}");
    }

    /// A create whose answer was lost: no Workflow yet, so the intent stays
    /// for a while, and goes once the grace has passed.
    #[test]
    fn an_intent_with_no_workflow_waits_then_goes() {
        let mut record = record_of_the_fixture();
        record.uid = None;
        let empty = crate::json_read::parse("{\"kind\":\"List\",\"items\":[]}").expect("json");
        let young = resolve(record.clone(), &empty, record.submitted + 60);
        assert!(!young.leaves);
        let old = resolve(
            record.clone(),
            &empty,
            record.submitted + INTENT_GRACE_SECS + 1,
        );
        assert!(old.leaves);
        assert_eq!(old.notes.len(), 1);
    }

    /// The Workflow finished and passed, but the token cannot be written: the
    /// record stays, its gate waiting, and the next collect writes the token.
    #[test]
    fn a_pass_whose_token_cannot_be_written_keeps_the_record() {
        let record = record_of_the_fixture();
        let list = list_for(&record, "Succeeded", "Succeeded", |p| p);
        let refuse = |_: &Pass| Err(Fallback::NotProved("git refused".into()));
        let first = settle_with(resolve(record.clone(), &list, 1_700_000_100), refuse);
        assert_eq!(first.written, 0);
        assert!(
            first.settled.is_none(),
            "the run has not ended for the person"
        );
        let kept = first.record.expect("the record survives");
        assert_eq!(kept.gates[0].state, GateState::Waiting);

        let mut tokens = Vec::new();
        let second = settle_with(resolve(kept, &list, 1_700_000_200), |pass| {
            tokens.push(pass.token.clone());
            Ok(())
        });
        assert_eq!(second.written, 1);
        assert_eq!(tokens, ["pre-push-lint@abc"]);
        assert!(second.record.is_none());
        let ended = second.settled.expect("now it ends");
        assert_eq!(ended.passed, ["pre-push-lint"]);
    }

    /// A finished Workflow whose pod status cannot be read fails the gate.
    #[test]
    fn an_unreadable_pod_status_on_a_finished_workflow_fails_the_gate() {
        let record = record_of_the_fixture();
        let list = list_for(&record, "Failed", "Failed", |p| {
            p.replace(
                "\"name\":\"amont-rh-test\",\"namespace\"",
                "\"namex\":\"amont-rh-test\",\"namespace\"",
            )
        });
        let done = collected(record, &list);
        assert!(done.passes.is_empty());
        assert_eq!(done.notes.len(), 1, "{done:?}");
        assert!(
            done.notes[0].1.contains("status cannot be read"),
            "{done:?}"
        );
    }

    /// A pod that does not name its node cannot be bound to the Workflow's.
    #[test]
    fn a_pod_without_a_node_id_is_not_bound() {
        let record = record_of_the_fixture();
        let list = list_for(&record, "Succeeded", "Succeeded", |p| {
            p.replace(
                "workflows.argoproj.io/node-id",
                "workflows.argoproj.io/node-idx",
            )
        });
        let done = collected(record, &list);
        assert!(done.passes.is_empty(), "{done:?}");
    }

    #[test]
    fn a_record_older_than_the_ttl_expires_whatever_the_cluster_says() {
        let record = record_of_the_fixture();
        assert!(!is_expired(&record, record.submitted + TTL_SECS));
        assert!(is_expired(
            &record,
            record.submitted + TTL_SECS + EXPIRY_SLACK_SECS + 1
        ));
    }
}
