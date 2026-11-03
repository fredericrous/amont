//! A rehearsal's `platform=any` gates as one Argo Workflow that outlives the
//! laptop (ADR-0017).
//!
//! `amont rehearse --cluster`, or `amont.rehearse.cluster` for the
//! post-commit rehearsal, hands the gates a rehearsal would run, and that the
//! cluster may run, to one Workflow: a DAG with a pod per gate, the same pod
//! as ADR-0016's Job. The code goes in as a Forgejo snapshot commit, or is
//! read from a pull mirror that already advertises it. Submission returns
//! once the Workflow exists; [`collect`] turns its passes into cluster tokens
//! at the start of the next hook, from facts Kubernetes recorded, under the
//! identity frozen at submission. The laptop is the only signer.
//!
//! Every path that goes wrong ends in the gate running here, as before: a
//! detached run can only remove work from a push, never let it skip work
//! nobody did.

use std::time::Duration;

use crate::check::Stage;
use crate::manifest::{External, Kind, Manifest};

use super::{
    key, last_line, log_line, machine_keys, run_label, sanitize_repo_dir, token_from, Call,
    ClusterConfig, Fallback, Kube, CALL_SECS, SEQ,
};
use crate::json_read::Value;

mod collect;
mod pending;
mod workflow;

pub use collect::{
    collect, collect_command, settled_lines, status_lines, stop_pending, Collected, Settled,
    Stopped, Trouble,
};

use pending::{Gate, GateState, Pending, PendingError, RecordPhase, Source, Store};

/// The machine key that sends every rehearsal to the cluster.
const REHEARSE_CLUSTER: &str = "amont.rehearse.cluster";
/// A push of the snapshot commit, and a read of a remote's refs, are network
/// calls with a deadline each.
const PUSH_SECS: u64 = 60;
const LS_REMOTE_SECS: u64 = 20;
/// How long a submission waits for the pending file's lock.
const LOCK_WAIT: Duration = Duration::from_secs(5);

/// Who asks for a rehearsal to go to the cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    /// `rehearse --cluster`: the person asked, so what goes wrong is said.
    Flag,
    /// Only the machine key may ask (`amont.rehearse.cluster`): the
    /// post-commit rehearsal, which says nothing on the terminal.
    Setting,
}

/// Does this rehearsal go to the cluster: asked for with `--cluster`, or the
/// machine says `amont.rehearse.cluster true`?
pub fn wanted(settings: &crate::config::Settings, ask: Ask) -> bool {
    ask == Ask::Flag || crate::config::boolean_or(settings, REHEARSE_CLUSTER, false)
}

/// The gates of a rehearsal that are on the cluster for `HEAD`'s tree now.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Detached {
    pub gates: Vec<String>,
}

/// A Workflow's pod stops here for tests that kill amont at a precise point.
/// Compiled only with `test-hooks`: nothing reads the variable otherwise.
#[cfg(feature = "test-hooks")]
fn crash_point(at: &str) {
    if std::env::var("AMONT_TEST_CRASH").is_ok_and(|v| v == at) {
        std::process::exit(137);
    }
}

#[cfg(not(feature = "test-hooks"))]
fn crash_point(_at: &str) {}

/// The detached lane's machine keys, as git prints them (lowercased).
struct Machine {
    base: ClusterConfig,
    snapshot_remote: String,
    snapshot_fetch: String,
    mirror: Option<Mirror>,
}

/// A read-only pull mirror: where the laptop lists its refs, and where the
/// cluster fetches from.
struct Mirror {
    remote: String,
    fetch: String,
}

fn machine(settings: &crate::config::Settings) -> Result<Machine, Fallback> {
    let base = super::cluster_config(settings)?;
    let keys = machine_keys();
    let need = |name: &'static str, shown: &'static str| {
        key(&keys, name)
            .map(str::to_string)
            .ok_or(Fallback::Unconfigured(shown))
    };
    let snapshot_remote = need(
        "amont.cluster.snapshotremote",
        "amont.cluster.snapshotRemote is not set: `git config --global amont.cluster.snapshotRemote 'git@host:org/{repo}.git'`",
    )?;
    let snapshot_fetch = need(
        "amont.cluster.snapshotfetch",
        "amont.cluster.snapshotFetch is not set: `git config --global amont.cluster.snapshotFetch 'http://forgejo-http.forgejo.svc.cluster.local:3000/org/{repo}.git'`",
    )?;
    let mirror = match (
        key(&keys, "amont.cluster.mirrorremote"),
        key(&keys, "amont.cluster.mirror"),
    ) {
        (Some(remote), Some(fetch)) => Some(Mirror {
            remote: remote.to_string(),
            fetch: fetch.to_string(),
        }),
        _ => None,
    };
    Ok(Machine {
        base,
        snapshot_remote,
        snapshot_fetch,
        mirror,
    })
}

/// The repository's name for a URL template: the last segment of `origin`'s
/// URL, without `.git`, as a plain word.
fn repo_name() -> Result<String, String> {
    let url = crate::git::stdout(&["remote", "get-url", "origin"])
        .ok_or("the repository has no origin remote to name it by")?;
    let last = url
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or_default();
    if last.is_empty() {
        return Err(format!("cannot name the repository from {url:?}"));
    }
    Ok(sanitize_repo_dir(last))
}

fn expand(template: &str, repo: &str) -> String {
    template.replace("{repo}", repo)
}

/// The trusted `platform=any` pre-push gates among `scoped` that the cluster
/// may run: the repository says `set executor cluster`, and nothing local
/// turned it off.
fn eligible<'a>(
    settings: &crate::config::Settings,
    manifest: &'a Manifest,
    scoped: &[String],
) -> Vec<&'a External> {
    if !super::executor_is_cluster(settings) {
        return Vec::new();
    }
    scoped
        .iter()
        .filter_map(|name| {
            manifest
                .externals
                .iter()
                .find(|e| e.stage == Stage::PrePush && e.id == *name)
        })
        .filter(|e| super::is_platform_any(e))
        .collect()
}

/// The gates of the records of `tree` still waiting for a verdict, and every
/// gate any record of `tree` holds, whatever its state.
///
/// An intent whose `create` never happened (amont died between the two)
/// counts as waiting, so later rehearsals of the tree leave its gates alone.
// holds-until: INTENT_GRACE_SECS (10 minutes) in collect.rs — collect drops an
// intent no Workflow appeared for after that; until then the push, not the
// rehearsal, runs those gates
fn known_for(tree: &str) -> (Vec<String>, Vec<String>) {
    let Some(store) = Store::here() else {
        return (Vec::new(), Vec::new());
    };
    let records = match store.read() {
        Ok(records) => records,
        Err(why) => {
            log_line("detach", &format!("cluster-pending is unreadable: {why}"));
            return (Vec::new(), Vec::new());
        }
    };
    let ours = records.iter().filter(|r| r.tree == tree);
    let waiting = ours
        .clone()
        .flat_map(|r| r.waiting().map(|g| g.id.clone()))
        .collect();
    let all = ours
        .flat_map(|r| r.gates.iter().map(|g| g.id.clone()))
        .collect();
    (waiting, all)
}

/// Hand `scoped` — the gates a rehearsal of `HEAD` would run — to the
/// cluster, as far as it may take them, and say which are there now: the ones
/// a record of this tree still waits for, and the ones this call submitted.
/// The rest run here, exactly as without the lane.
///
/// `ask` is `--cluster` ([`Ask::Flag`]); `amont.rehearse.cluster` asks too.
/// With neither, nothing is submitted, but the gates an earlier call put there
/// still count. What is said goes to stderr, and only when the person asked.
pub fn detach(
    settings: &crate::config::Settings,
    manifest: &Manifest,
    scoped: &[String],
    ask: Ask,
) -> Detached {
    let Some(tree) = crate::git::stdout(&["rev-parse", "HEAD^{tree}"]) else {
        return Detached::default();
    };
    let (waiting, known) = known_for(&tree);
    let mut riding: Vec<String> = scoped
        .iter()
        .filter(|gate| waiting.contains(gate))
        .cloned()
        .collect();
    if !wanted(settings, ask) {
        return Detached { gates: riding };
    }
    let fresh: Vec<&External> = eligible(settings, manifest, scoped)
        .into_iter()
        .filter(|e| !known.contains(&e.id))
        .collect();
    if fresh.is_empty() {
        if ask == Ask::Flag {
            if riding.is_empty() {
                eprintln!(
                    "no gate of this rehearsal can go to the cluster: it needs a trusted \
                     `platform=any` pre-push gate and `set executor cluster`"
                );
            } else {
                eprintln!("already on the cluster: {}", riding.join(" "));
            }
        }
        return Detached { gates: riding };
    }
    match submit(settings, &tree, &fresh) {
        Ok(submitted) => {
            eprintln!(
                "submitted {} to the cluster: {} — the next commit or push collects the result",
                submitted.name,
                submitted.gates.join(" ")
            );
            riding.extend(submitted.gates);
        }
        Err(error) => {
            let (line, spoken) = error.render();
            for e in &fresh {
                log_line(&e.id, &format!("detached: {line}"));
            }
            if ask == Ask::Flag {
                crate::hooks::common::warn(&crate::ui::sanitize(&spoken));
            }
        }
    }
    Detached { gates: riding }
}

/// Why a submission did not end with a recorded Workflow.
#[derive(Debug)]
enum SubmitError {
    /// Nothing was created: the gates run here.
    NotSubmitted(Fallback),
    /// A Workflow may exist, or does, but this process cannot vouch for it:
    /// the gates run here as well, and a later collect adopts it.
    Unconfirmed(String),
}

impl From<Fallback> for SubmitError {
    fn from(f: Fallback) -> SubmitError {
        SubmitError::NotSubmitted(f)
    }
}

impl SubmitError {
    /// The line for `cluster.log`, and the sentence for the person.
    fn render(&self) -> (String, String) {
        match self {
            SubmitError::NotSubmitted(Fallback::Misconfigured { why, fix }) => (
                why.clone(),
                format!("not submitted to the cluster, the gates run here: {why} — fix with {fix}"),
            ),
            SubmitError::NotSubmitted(f) => (
                f.to_string(),
                format!("not submitted to the cluster, the gates run here: {f}"),
            ),
            SubmitError::Unconfirmed(why) => (
                format!("not confirmed: {why}"),
                format!(
                    "submitted, not confirmed ({why}); the gates also run here, and the next \
                     commit or push collects the Workflow if it landed"
                ),
            ),
        }
    }
}

/// A Workflow that exists on the cluster and is recorded.
struct Submitted {
    name: String,
    gates: Vec<String>,
}

/// Where the code comes from, and the URL the cluster fetches it by.
struct Origin {
    source: Source,
    url: String,
}

fn submit(
    settings: &crate::config::Settings,
    tree: &str,
    gates: &[&External],
) -> Result<Submitted, SubmitError> {
    let machine = machine(settings)?;
    let head = crate::git::stdout(&["rev-parse", "HEAD"])
        .ok_or_else(|| Fallback::NotProved("git would not name HEAD".into()))?;
    let repo = repo_name().map_err(Fallback::NotProved)?;
    let label = run_label(SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
    let frozen = freeze(settings, &machine, gates)?;
    let origin = choose_origin(&machine, &repo, &head, tree, &label)?;
    let record = Pending {
        name: workflow::workflow_name(&label),
        uid: None,
        label,
        namespace: machine.base.namespace.clone(),
        kubeconfig: machine.base.kubeconfig.clone(),
        tree: tree.to_string(),
        source: origin.source,
        submitted: crate::gate_evidence::now(),
        phase: RecordPhase::Submitting,
        gates: frozen,
    };
    let store = Store::here()
        .ok_or_else(|| Fallback::NotProved("git would not name the common directory".into()))?;
    record_intent(&store, &record)?;
    crash_point("before-create");
    let plan = workflow::Plan {
        config: &machine.base,
        record: &record,
        repo_dir: &super::repo_dir(),
        url: &origin.url,
    };
    let kube = Kube::new(&machine.base);
    let uid = create(&kube, &record, &workflow::workflow_json(&plan))?;
    crash_point("after-create");
    adopt(&store, &record.name, &uid)?;
    Ok(Submitted {
        name: record.name,
        gates: record.gates.into_iter().map(|g| g.id).collect(),
    })
}

/// Each gate's identity, frozen: the token as the push will look it up, from
/// the same function, so a pass earned under today's configuration is one only
/// today's configuration accepts.
fn freeze(
    settings: &crate::config::Settings,
    machine: &Machine,
    gates: &[&External],
) -> Result<Vec<Gate>, Fallback> {
    let keys = machine_keys();
    gates
        .iter()
        .enumerate()
        .map(|(i, ext)| {
            let Kind::Runnable {
                program,
                args,
                scope,
                ..
            } = &ext.kind
            else {
                return Err(Fallback::NotProved(format!("{} is not runnable", ext.id)));
            };
            let token = token_from(&keys, settings, ext, &ext.id).ok_or_else(|| {
                Fallback::NotProved(format!("git would not hash the identity of {}", ext.id))
            })?;
            let mut argv = vec![program.clone()];
            argv.extend(args.iter().cloned());
            Ok(Gate {
                id: ext.id.clone(),
                task: format!("g{i}"),
                argv,
                scope: format!("{scope:?}"),
                prepare: machine.base.prepare.clone(),
                digest: machine.base.digest.clone(),
                contract: crate::gate_identity::CLUSTER_CONTRACT.to_string(),
                token,
                state: GateState::Waiting,
            })
        })
        .collect()
}

/// The mirror when it advertises `head`; otherwise a snapshot commit pushed to
/// the snapshot remote and read back.
fn choose_origin(
    machine: &Machine,
    repo: &str,
    head: &str,
    tree: &str,
    label: &str,
) -> Result<Origin, Fallback> {
    if let Some(mirror) = &machine.mirror {
        match advertised(&expand(&mirror.remote, repo)) {
            Ok(oids) if oids.iter().any(|o| o == head) => {
                return Ok(Origin {
                    source: Source {
                        snapshot_ref: None,
                        oid: head.to_string(),
                    },
                    url: expand(&mirror.fetch, repo),
                })
            }
            Ok(_) => {}
            Err(why) => log_line("detach", &format!("mirror not listed: {why}")),
        }
    }
    let oid = snapshot_commit(head, tree, label)?;
    let snapshot_ref = format!("refs/snapshots/{label}");
    let remote = expand(&machine.snapshot_remote, repo);
    push_snapshot(&remote, &oid, &snapshot_ref)?;
    let seen = advertised_at(&remote, &snapshot_ref)
        .map_err(|why| Fallback::NotProved(format!("the snapshot was not read back: {why}")))?;
    if seen.as_deref() != Some(oid.as_str()) {
        return Err(Fallback::NotProved(
            "the snapshot was pushed but not read back from the remote".into(),
        ));
    }
    Ok(Origin {
        source: Source {
            snapshot_ref: Some(snapshot_ref),
            oid,
        },
        url: expand(&machine.snapshot_fetch, repo),
    })
}

/// The commit objects `git ls-remote <remote>` advertises. `ls-remote <sha>`
/// would match ref NAMES, never commits, so the answer is compared here.
fn advertised(remote: &str) -> Result<Vec<String>, String> {
    let (probe, out) = crate::git::probe_remote_stdout(
        &["ls-remote", remote, "refs/heads/*", "refs/tags/*"],
        LS_REMOTE_SECS,
    );
    match probe {
        crate::git::Probe::Exit(0) => Ok(out
            .lines()
            .filter_map(|l| l.split_whitespace().next())
            .map(str::to_string)
            .collect()),
        other => Err(format!("git ls-remote {remote}: {other:?}")),
    }
}

/// The object `ref_name` points to on `remote`, if it is there.
fn advertised_at(remote: &str, ref_name: &str) -> Result<Option<String>, String> {
    let (probe, out) =
        crate::git::probe_remote_stdout(&["ls-remote", remote, ref_name], LS_REMOTE_SECS);
    match probe {
        crate::git::Probe::Exit(0) => Ok(out
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .find(|(_, name)| *name == ref_name)
            .map(|(oid, _)| oid.trim().to_string())),
        other => Err(format!("git ls-remote {remote}: {other:?}")),
    }
}

/// A commit holding `tree` with `head` as its parent, dated NOW: the prune
/// ages a snapshot ref by its commit's committer date, so a reused date would
/// age a fresh snapshot out at once.
fn snapshot_commit(head: &str, tree: &str, label: &str) -> Result<String, Fallback> {
    let now = format!("@{} +0000", crate::gate_evidence::now());
    let out = std::process::Command::new("git")
        .args(["commit-tree", tree, "-p", head, "-m"])
        .arg(format!("amont snapshot {label}"))
        .env("GIT_AUTHOR_NAME", "amont")
        .env("GIT_AUTHOR_EMAIL", "amont@localhost")
        .env("GIT_COMMITTER_NAME", "amont")
        .env("GIT_COMMITTER_EMAIL", "amont@localhost")
        .env("GIT_AUTHOR_DATE", &now)
        .env("GIT_COMMITTER_DATE", &now)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| Fallback::NotProved(format!("git commit-tree: {e}")))?;
    let oid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() && !oid.is_empty() {
        Ok(oid)
    } else {
        Err(Fallback::NotProved("git commit-tree failed".into()))
    }
}

/// Push `oid` to `ref_name` on `remote` with the person's own credentials:
/// never a prompt, bounded, and marked so this repository's own pre-push hook
/// stands down instead of judging a snapshot.
fn push_snapshot(remote: &str, oid: &str, ref_name: &str) -> Result<(), Fallback> {
    let (mut args, mut env) = crate::git::remote_env(crate::git::configured_ssh());
    env.push((crate::attest::PUSH_GUARD, "1"));
    let refspec = format!("{oid}:{ref_name}");
    args.extend(["push", "-q", remote, &refspec]);
    match crate::git::probe_env(&args, PUSH_SECS, &env) {
        crate::git::Probe::Exit(0) => Ok(()),
        other => Err(Fallback::NotProved(format!(
            "git push of the snapshot to {remote}: {other:?}"
        ))),
    }
}

/// Write the intent: a record with no UID, durably, before anything is
/// created. Under the same lock, a record of the same tree that already holds
/// one of these gates refuses it: two rehearsals of one tree that both passed
/// the unlocked look in [`known_for`] cannot both submit.
fn record_intent(store: &Store, record: &Pending) -> Result<(), Fallback> {
    let not = |why: PendingError| Fallback::NotProved(format!("cluster-pending: {why}"));
    let held = store.lock_within(LOCK_WAIT).ok_or_else(|| {
        Fallback::NotProved("cluster-pending: busy, or no flock on this platform".into())
    })?;
    let mut records = store.read().map_err(not)?;
    if let Some(taken) = holder_of(&records, record) {
        return Err(Fallback::NotProved(format!(
            "another submission of this tree already holds {taken}"
        )));
    }
    records.push(record.clone());
    store.write(&held, &records).map_err(not)
}

/// A gate of `record` that a record of the same tree in `records` holds.
fn holder_of(records: &[Pending], record: &Pending) -> Option<String> {
    records
        .iter()
        .filter(|other| other.tree == record.tree)
        .flat_map(|other| other.gates.iter())
        .find(|held| record.gates.iter().any(|g| g.id == held.id))
        .map(|held| held.id.clone())
}

/// Set the UID of the record `name`, durably. The record may have been
/// adopted already by a collect that found the Workflow first.
fn adopt(store: &Store, name: &str, uid: &str) -> Result<(), SubmitError> {
    let not = |why: String| {
        SubmitError::Unconfirmed(format!(
            "the Workflow exists but its UID was not recorded: {why}"
        ))
    };
    let held = store
        .lock_within(LOCK_WAIT)
        .ok_or_else(|| not("cluster-pending is busy".into()))?;
    let records: Vec<Pending> = store
        .read()
        .map_err(|e| not(e.to_string()))?
        .into_iter()
        .map(|mut r| {
            if r.name == name && r.uid.is_none() {
                r.uid = Some(uid.to_string());
                r.phase = RecordPhase::Pending;
            }
            r
        })
        .collect();
    store.write(&held, &records).map_err(|e| not(e.to_string()))
}

/// How a `kubectl create` of the Workflow ended.
#[derive(Debug, PartialEq, Eq)]
enum Created {
    /// The Workflow exists, with this UID.
    Made(String),
    /// The name is taken: a Workflow of this name exists.
    Taken,
    /// The apiserver answered with an error: nothing was created.
    Rejected(String),
    /// kubectl did not start: nothing reached the apiserver.
    NotSent(String),
    /// The request may have landed: the answer was lost, or never came.
    Unknown(String),
}

fn classify(call: Call) -> Created {
    match call {
        Call::Ok(uid) if !uid.trim().is_empty() => Created::Made(uid.trim().to_string()),
        Call::Ok(_) => Created::Unknown("kubectl create printed no UID".into()),
        Call::Refused(err) if err.contains("AlreadyExists") || err.contains("already exists") => {
            Created::Taken
        }
        // "Error from server (…)" is the apiserver speaking: it refused.
        // Anything else (an EOF, a reset, a deadline) may have landed.
        Call::Refused(err) if err.contains("Error from server") => {
            Created::Rejected(last_line(&err))
        }
        Call::Refused(err) | Call::TimedOut(err) => Created::Unknown(last_line(&err)),
        Call::NotStarted(err) => Created::NotSent(err),
    }
}

/// Why a `create` may or may not have landed, for [`reconcile`].
#[derive(Debug, PartialEq, Eq)]
enum Uncertain {
    /// The name is taken: a Workflow of this name exists.
    Taken,
    /// The answer was lost, or never came.
    Lost(String),
}

/// Create the Workflow ONCE, as `amont-gate-runner` (the kubeconfig's user),
/// and return its UID. A create whose answer is lost is reconciled by `get`
/// on its deterministic name and never repeated.
fn create(kube: &Kube, record: &Pending, json: &str) -> Result<String, SubmitError> {
    let file = ManifestFile::write(&record.label, json)?;
    let path = file.path.to_string_lossy().into_owned();
    let args = ["create", "-f", &path, "-o", "jsonpath={.metadata.uid}"];
    let outcome = classify(kube.capture(&args, CALL_SECS));
    drop(file);
    match outcome {
        Created::Made(uid) => Ok(uid),
        Created::Rejected(why) => {
            forget(record);
            Err(Fallback::NotProved(format!("the cluster refused it: {why}")).into())
        }
        Created::NotSent(why) => {
            forget(record);
            Err(Fallback::Unreachable(why).into())
        }
        Created::Taken => reconcile(kube, record, Uncertain::Taken),
        Created::Unknown(why) => reconcile(kube, record, Uncertain::Lost(why)),
    }
}

/// After a create that may have landed: `get` the Workflow by its name and
/// adopt it only if its spec is the one submitted.
fn reconcile(kube: &Kube, record: &Pending, uncertain: Uncertain) -> Result<String, SubmitError> {
    let said = match uncertain {
        Uncertain::Lost(why) => why,
        Uncertain::Taken => "the name was taken".to_string(),
    };
    match kube.capture(&["get", "workflow", &record.name, "-o", "json"], CALL_SECS) {
        Call::Ok(json) => {
            let wf = crate::json_read::parse(&json).ok_or_else(|| {
                SubmitError::Unconfirmed("the Workflow found by name is not JSON".into())
            })?;
            let uid = wf
                .get("metadata")
                .and_then(|m| m.get("uid"))
                .and_then(Value::as_str)
                .ok_or_else(|| SubmitError::Unconfirmed("the Workflow found has no UID".into()))?;
            match workflow::check_workflow(&wf, record) {
                Ok(()) => Ok(uid.to_string()),
                Err(why) => {
                    forget(record);
                    Err(Fallback::NotProved(format!(
                        "a Workflow named {} exists and is not the one submitted: {why}",
                        record.name
                    ))
                    .into())
                }
            }
        }
        // Not there: one whose answer was lost keeps its intent, so that a
        // request landing late is adopted by the next collect and never
        // repeated.
        Call::Refused(err) if err.contains("NotFound") => Err(SubmitError::Unconfirmed(format!(
            "the cluster did not show the Workflow ({said}); the intent is kept"
        ))),
        Call::Refused(err) | Call::NotStarted(err) | Call::TimedOut(err) => {
            Err(SubmitError::Unconfirmed(format!(
                "the Workflow could not be looked up ({said}; {})",
                last_line(&err)
            )))
        }
    }
}

/// Drop the intent of `record`: nothing was created for it. Failing to is
/// logged, never fatal: an intent with no Workflow expires.
fn forget(record: &Pending) {
    let Some(store) = Store::here() else { return };
    let Some(held) = store.lock_within(LOCK_WAIT) else {
        log_line(
            "detach",
            &format!("could not drop the intent of {}", record.name),
        );
        return;
    };
    let kept: Result<Vec<Pending>, PendingError> = store
        .read()
        .map(|rs| rs.into_iter().filter(|r| r.name != record.name).collect());
    let written = kept.and_then(|rs| store.write(&held, &rs));
    if let Err(why) = written {
        log_line(
            "detach",
            &format!("could not drop the intent of {}: {why}", record.name),
        );
    }
}

/// The manifest handed to `kubectl create -f`, in a private file of its own:
/// created fresh and unreadable to others, removed when dropped, and listed
/// with the interrupt clean-up, which deletes this file and nothing else. Not
/// a [`super::Scratch`]: that registers a clean-up which deletes Jobs, and a
/// detached Workflow is meant to survive amont.
struct ManifestFile {
    path: std::path::PathBuf,
}

impl ManifestFile {
    fn write(label: &str, json: &str) -> Result<ManifestFile, Fallback> {
        let path = std::env::temp_dir().join(format!("amont-workflow-{label}.json"));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options
            .open(&path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, json.as_bytes()))
            .map_err(|e| Fallback::NotProved(format!("manifest file: {e}")))?;
        super::track_temp_file(&path);
        Ok(ManifestFile { path })
    }
}

impl Drop for ManifestFile {
    fn drop(&mut self) {
        super::untrack_temp_file(&self.path);
        if let Err(e) = std::fs::remove_file(&self.path) {
            crate::hooks::common::warn(&format!("could not remove {}: {e}", self.path.display()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repository_is_named_by_its_origin() {
        // the last segment, with no `.git`
        for (url, name) in [
            ("git@git.example:org/relais.git", "relais"),
            (
                "https://github.com/o/duro-design-system",
                "duro-design-system",
            ),
            ("/tmp/x/remote.git/", "remote"),
        ] {
            let last = url
                .trim_end_matches('/')
                .rsplit(['/', ':'])
                .next()
                .unwrap_or_default();
            assert_eq!(sanitize_repo_dir(last), name, "{url}");
        }
        assert_eq!(
            expand("git@h:amont-snapshots/{repo}.git", "relais"),
            "git@h:amont-snapshots/relais.git"
        );
    }

    #[test]
    fn a_create_is_only_repeated_never() {
        let made = classify(Call::Ok("uid-1\n".into()));
        assert_eq!(made, Created::Made("uid-1".into()));
        assert_eq!(
            classify(Call::Refused(
                "Error from server (AlreadyExists): workflows.argoproj.io \"x\" already exists"
                    .into()
            )),
            Created::Taken
        );
        // the apiserver spoke: nothing was created
        assert!(matches!(
            classify(Call::Refused(
                "Error from server (Forbidden): workflows is forbidden".into()
            )),
            Created::Rejected(_)
        ));
        // a lost answer may have landed
        assert!(matches!(
            classify(Call::Refused("error: unexpected EOF".into())),
            Created::Unknown(_)
        ));
        assert!(matches!(
            classify(Call::TimedOut("kubectl timed out".into())),
            Created::Unknown(_)
        ));
        assert!(matches!(
            classify(Call::Ok(String::new())),
            Created::Unknown(_)
        ));
        assert!(matches!(
            classify(Call::NotStarted("kubectl: not found".into())),
            Created::NotSent(_)
        ));
    }

    #[test]
    fn a_second_submission_of_a_tree_cannot_take_a_gate_already_held() {
        let held = super::pending::tests::sample("wf-1");
        let mut other = super::pending::tests::sample("wf-2");
        assert_eq!(
            holder_of(std::slice::from_ref(&held), &other),
            Some("pre-push-lint".to_string())
        );
        other.tree = "0".repeat(40);
        assert_eq!(holder_of(&[held], &other), None, "another tree");
    }

    /// Under the lock, a record of the same tree holding the gate refuses the
    /// second intent: two rehearsals that both looked first cannot both submit.
    #[cfg(unix)]
    #[test]
    fn record_intent_refuses_a_gate_a_record_of_the_tree_holds() {
        let dir = std::env::temp_dir().join(format!("amont-intent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let store = Store::in_dir(&dir);
        let first = super::pending::tests::sample("wf-1");
        assert!(record_intent(&store, &first).is_ok());
        let second = super::pending::tests::sample("wf-2");
        let refused = record_intent(&store, &second).expect_err("held");
        assert!(
            refused.to_string().contains("already holds pre-push-lint"),
            "{refused}"
        );
        assert_eq!(store.read().expect("read").len(), 1);
        std::fs::remove_dir_all(&dir).expect("clean");
    }
}
