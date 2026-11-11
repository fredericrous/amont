//! A `platform=any` gate run on the homelab cluster, as one Kubernetes Job
//! (ADR-0016).
//!
//! A trusted manifest marks a gate whose verdict does not depend on the
//! platform (`platform=any`) and says `set executor cluster`; the machine
//! names a kubeconfig and an image pinned by digest. Then the gate runs in a
//! pod instead of on this laptop, and takes no host slot (ADR-0014).
//!
//! **The cluster proves only passes.** The verdict is read from what
//! Kubernetes recorded — the Job's `Complete` condition, each container's
//! exit code, `fetch`'s termination message naming the tree, the pod's
//! `imageID` — never from the logs. Anything else, a failure of the gate
//! included, re-runs the gate here, unchanged, and the local outcome is the
//! verdict: a gate never fails because of the cluster. Each such fallback
//! appends `<epoch> <gate> <reason>` to `greengate/cluster.log` in the common git
//! directory.
//!
//! A pass is recorded under its own identity
//! ([`crate::gate_identity::cluster_token`]) in its own ref
//! ([`crate::gate_stamp::CLUSTER_REF`]) and never as a local token: the
//! hooks consult [`proved`] so a cluster-derived pass never reaches the
//! commit marker, a push stamp or the laptop's attestation.
//!
//! Every `kubectl` call goes through `--kubeconfig <path>` (never the default
//! context), with stdin null — except the one `exec -i` that streams the
//! bundle — and under one deadline taken before the Job is created.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::json_read::Value;
use crate::manifest::{External, Kind};

mod detached;
pub(crate) use detached::{collect, settled_lines, status_lines, stop_pending};
pub use detached::{collect_command, detach, wanted, Ask, Detached};

/// [`collect`] for the hooks and `amont stamps`, which run in every repository:
/// only where the manifest declares a `platform=any` gate, the only kind a
/// Workflow can hold a pass for, so a repository without one pays nothing.
pub fn collect_if_declared(manifest: &crate::manifest::Manifest) {
    if manifest.externals.iter().any(is_platform_any) {
        collect();
    }
}

/// The node the gate pods run on, and the names the homelab contract fixes.
const NODE: &str = "talos-wk-3-gpu";
const PRIORITY_CLASS: &str = "gate-batch";
const PULL_SECRET: &str = "forgejo-registry-pull";
const JOB_PREFIX: &str = "amont-gate-slot-";
const CACHE_PREFIX: &str = "gate-cache-";
const TEMP_REF_PREFIX: &str = "refs/amont/cluster/";
/// The platform a cluster pass is recorded under.
pub(crate) const PLATFORM: &str = "x86_64-linux";
/// The main container's memory, request equal to limit, and CPU limit.
const MEMORY: &str = "4Gi";
const CPU_LIMIT: &str = "2";
/// One short kubectl call: its own request timeout plus room for the exec
/// credential plugin. Never more than the time left.
const CALL_SECS: u64 = 20;
/// The shared budget for deleting live Jobs when amont is interrupted.
const CANCEL_SECS: u64 = 10;
/// The bound on a quiet clean-up by label after a `create` timed out.
const LABEL_DELETE_SECS: u64 = 5;
/// The label every Job carries, valued by its run ([`run_label`]).
const RUN_LABEL_KEY: &str = crate::names::RUN_LABEL;
const POLL: Duration = Duration::from_millis(500);
const MIB: u64 = 1024 * 1024;

/// Passes the cluster proved in this process, by (check id, tree): run there,
/// or reused from a cluster token. Such a pass never becomes a local token,
/// stamp or attestation for that tree.
static PROVED: Mutex<BTreeSet<(String, String)>> = Mutex::new(BTreeSet::new());
/// Set once the apiserver did not answer: the later gates of this hook
/// process run here without asking it again.
static UNREACHABLE: AtomicBool = AtomicBool::new(false);
/// A misconfiguration is said once per run, not once per gate.
static WARNED: AtomicBool = AtomicBool::new(false);
/// An unreachable apiserver is said once per run; each gate still logs it.
static UNREACHABLE_SAID: AtomicBool = AtomicBool::new(false);
/// What an interrupt must clean up, by run.
static LIVE: Mutex<Vec<LiveRun>> = Mutex::new(Vec::new());
/// Serialises writes of the cluster note: concurrent gates of one stage each
/// read, merge and rewrite it.
static STAMP_LOCK: Mutex<()> = Mutex::new(());
static SEQ: AtomicU64 = AtomicU64::new(0);
/// Temporary manifest files of detached submissions, for an interrupt to
/// delete. Files only: a detached Workflow outlives amont on purpose.
static TEMP_FILES: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

fn track_temp_file(path: &Path) {
    TEMP_FILES
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(path.to_path_buf());
}

fn untrack_temp_file(path: &Path) {
    TEMP_FILES
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .retain(|p| p != path);
}

/// Did the cluster prove the check `id` on `tree` in this process?
pub fn proved(id: &str, tree: &str) -> bool {
    PROVED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains(&(id.to_string(), tree.to_string()))
}

/// Did the cluster prove the check `id` on any tree in this process? For a
/// caller that covers several trees at once (an attestation of a push).
pub fn proved_anywhere(id: &str) -> bool {
    PROVED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .any(|(i, _)| i == id)
}

/// Did the cluster prove the check `id` on the index's tree? Spawns nothing
/// unless the cluster proved `id` somewhere; when git cannot name the index's
/// tree, the answer is yes, so no local token is written on a guess.
pub fn proved_on_index(id: &str) -> bool {
    proved_anywhere(id) && tree_of(Judged::Index).map_or(true, |tree| proved(id, &tree))
}

/// The ids of the manifest's `platform=any` gates. A `Runnable` exists only
/// for a trusted manifest, so these are the trusted declarations.
pub(crate) fn platform_any_ids(manifest: &crate::manifest::Manifest) -> Vec<String> {
    manifest
        .externals
        .iter()
        .filter(|e| is_platform_any(e))
        .map(|e| e.id.clone())
        .collect()
}

fn mark_proved(id: &str, tree: &str) {
    PROVED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert((id.to_string(), tree.to_string()));
}

/// One cluster run, as an interrupt sees it.
#[derive(Clone)]
struct LiveRun {
    run: u64,
    kubeconfig: PathBuf,
    namespace: String,
    scratch: PathBuf,
    /// The `greengate.run` label of every Job this run creates: how a Job whose
    /// `create` is still in flight, or whose answer was lost, is found.
    run_label: String,
    /// Where this run's Job stands, as a cancel must know it.
    created: Created,
    gate_ref: Option<String>,
}

/// A run's Job, as far as an interrupt can tell.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Created {
    /// No `create` sent, or the Job already deleted: nothing on the cluster
    /// is this run's.
    NotSent,
    /// A `create` was sent; its Job, if any, is found by the run's label.
    Sent,
    /// The Job this run holds, by name.
    Named(String),
}

fn live_update(run: u64, update: impl FnOnce(&mut LiveRun)) {
    let mut live = LIVE.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(entry) = live.iter_mut().find(|l| l.run == run) {
        update(entry);
    }
}

/// The signal watcher's hook, after the index is restored: say so, ask the
/// apiserver to drop every live Job without waiting for it, all under one
/// shared budget, and remove the temporary files and refs. A second signal
/// meanwhile ends amont at once and leaves the Job to its own deadline.
pub fn cancel_live() {
    for file in std::mem::take(&mut *TEMP_FILES.lock().unwrap_or_else(|p| p.into_inner())) {
        let removed = std::fs::remove_file(&file); // amont is dying: nothing to tell
        drop(removed);
    }
    let live: Vec<LiveRun> = std::mem::take(&mut *LIVE.lock().unwrap_or_else(|p| p.into_inner()));
    if live.is_empty() {
        return;
    }
    let _leaving = crate::staged_only::leave_on_second_signal();
    let deadline = Instant::now() + Duration::from_secs(CANCEL_SECS);
    for run in live {
        if let Some(gate_ref) = &run.gate_ref {
            let _ = crate::git::succeeds(&["update-ref", "-d", gate_ref]); // best-effort: amont is dying
        }
        let _ = std::fs::remove_dir_all(&run.scratch); // best-effort: amont is dying
        let kube = Kube {
            kubeconfig: run.kubeconfig.clone(),
            namespace: run.namespace.clone(),
        };
        let left = secs_left(deadline, CANCEL_SECS);
        match &run.created {
            Created::NotSent => {}
            Created::Named(job) => {
                eprintln!("cancelling cluster Job {job}; Ctrl-C again to leave it");
                if let Err(why) = kube.bounded(&["delete", "job", job, "--wait=false"], left) {
                    eprintln!(
                        "could not cancel cluster Job {job}: {}",
                        crate::ui::sanitize(&why)
                    );
                }
            }
            // Not named yet: `create` may be in flight or its answer lost. The
            // label finds the Job if it exists, and deletes nothing otherwise.
            Created::Sent => {
                let selector = run_selector(&run.run_label);
                eprintln!("cancelling cluster Jobs of this run; Ctrl-C again to leave them");
                if let Err(why) =
                    kube.bounded(&["delete", "job", "-l", &selector, "--wait=false"], left)
                {
                    eprintln!(
                        "could not cancel the cluster Jobs of this run: {}",
                        crate::ui::sanitize(&why)
                    );
                }
            }
        }
    }
}

/// Whole seconds until `deadline`, at most `cap`, at least 1.
fn secs_left(deadline: Instant, cap: u64) -> u64 {
    deadline
        .saturating_duration_since(Instant::now())
        .as_secs()
        .clamp(1, cap.max(1))
}

/// Why the cluster did not prove a gate. Every one runs the gate here.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Fallback {
    /// A required machine key is unset: this machine has no cluster. Silent
    /// on the terminal, so a repository that says `set executor cluster`
    /// does not warn on every hook of a machine without one.
    Unconfigured(&'static str),
    /// A machine key is set but cannot be used. Said once per run, with the
    /// fix.
    Misconfigured {
        why: String,
        fix: String,
    },
    /// The apiserver did not answer, or kubectl could not run.
    Unreachable(String),
    /// The apiserver did not answer an earlier gate of this run.
    UnreachableEarlier,
    SlotsTaken,
    QuotaFull,
    BundleTooBig {
        mib: u64,
        cap: u64,
    },
    /// The gate itself exited non-zero in the pod.
    GateFailed(String),
    /// Anything else the recorded facts do not prove.
    NotProved(String),
}

impl std::fmt::Display for Fallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fallback::Unconfigured(why) => write!(f, "{why}"),
            Fallback::Misconfigured { why, .. } => write!(f, "{why}"),
            Fallback::Unreachable(why) => write!(f, "the cluster did not answer: {why}"),
            Fallback::UnreachableEarlier => {
                write!(f, "the cluster did not answer earlier in this run")
            }
            Fallback::SlotsTaken => write!(f, "all slots taken"),
            Fallback::QuotaFull => write!(f, "the quota refused every slot"),
            Fallback::BundleTooBig { mib, cap } => {
                write!(f, "bundle-too-big: {mib} MiB, over the {cap} MiB cap")
            }
            Fallback::GateFailed(detail) => write!(f, "failed on the cluster ({detail})"),
            Fallback::NotProved(why) => write!(f, "{why}"),
        }
    }
}

/// What the cluster needs from this machine and this repository.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ClusterConfig {
    kubeconfig: PathBuf,
    namespace: String,
    slots: u32,
    image: String,
    digest: String,
    max_bundle_mib: u64,
    start_wait: Duration,
    timeout_secs: u64,
    prepare: Option<String>,
}

/// The machine's `greengate.cluster.*` keys, lowercased as git prints them.
/// `--type=path` expands a leading `~`, which is how the kubeconfig is set.
fn machine_keys() -> Vec<(String, String)> {
    crate::git::stdout(&[
        "config",
        "--type=path",
        "--get-regexp",
        r"^greengate\.cluster\.",
    ])
    .unwrap_or_default()
    .lines()
    .map(|l| {
        let (k, v) = l.split_once(' ').unwrap_or((l, ""));
        (k.to_ascii_lowercase(), v.trim().to_string())
    })
    .collect()
}

fn key<'a>(keys: &'a [(String, String)], name: &str) -> Option<&'a str> {
    keys.iter()
        .rev()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
        .filter(|v| !v.is_empty())
}

fn number(keys: &[(String, String)], name: &str, default: u64, max: u64) -> u64 {
    key(keys, name)
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|n| *n >= 1)
        .map_or(default, |n| n.min(max))
}

/// The digest of a pinned image reference, `sha256:<hex>`, or `None` for a
/// reference that is not pinned by digest.
fn digest_of(image: &str) -> Option<&str> {
    let (_, digest) = image.split_once('@')?;
    let hex = digest.strip_prefix("sha256:")?;
    (hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit())).then_some(digest)
}

/// A DNS-1123 label: what a namespace may be called.
fn is_label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

/// The trusted `set clusterPrepare` command, if any. Policy only: a local
/// git config cannot hand the pod a command the manifest did not declare.
fn prepare(settings: &crate::config::Settings) -> Option<String> {
    settings
        .policy()
        .settings
        .get("greengate.clusterPrepare")
        .cloned()
}

/// Whether this repository's TRUSTED policy says `set executor cluster`, and
/// no local `git config greengate.executor local` turned it off. Reading the
/// policy map spawns nothing, so a repository that says nothing pays nothing.
fn executor_is_cluster(settings: &crate::config::Settings) -> bool {
    settings
        .policy()
        .settings
        .get("greengate.executor")
        .is_some_and(|v| v == "cluster")
        && crate::config::enumerated_or(
            settings,
            "greengate.executor",
            &["cluster", "local"],
            "local",
        ) == "cluster"
}

fn misconfigured(why: String, key_fix: &str) -> Fallback {
    Fallback::Misconfigured {
        why,
        fix: format!("{key_fix}, or `git config greengate.executor local`"),
    }
}

/// The full configuration, or why the gate runs here instead.
fn cluster_config(settings: &crate::config::Settings) -> Result<ClusterConfig, Fallback> {
    let keys = machine_keys();
    let kubeconfig = key(&keys, "greengate.cluster.kubeconfig").ok_or(Fallback::Unconfigured(
        "greengate.cluster.kubeconfig is not set: `git config --global greengate.cluster.kubeconfig ~/.config/amont/kube/homelab.yaml`",
    ))?;
    let kubeconfig = PathBuf::from(kubeconfig);
    if !kubeconfig.is_file() {
        return Err(misconfigured(
            format!("kubeconfig {} does not exist", kubeconfig.display()),
            "`git config --global greengate.cluster.kubeconfig <path>`",
        ));
    }
    let image = key(&keys, "greengate.cluster.image")
        .ok_or(Fallback::Unconfigured(
        "greengate.cluster.image is not set: `git config --global greengate.cluster.image 'name:tag@sha256:<digest>'`",
    ))?;
    let digest = digest_of(image)
        .ok_or_else(|| {
            misconfigured(
                format!("greengate.cluster.image {image:?} is not pinned by @sha256: digest"),
                "`git config --global greengate.cluster.image <name>:<tag>@sha256:<digest>`",
            )
        })?
        .to_string();
    let namespace = key(&keys, "greengate.cluster.namespace").unwrap_or("amont-gates");
    if !is_label(namespace) {
        return Err(misconfigured(
            format!("greengate.cluster.namespace {namespace:?} is not a namespace name"),
            "`git config --global greengate.cluster.namespace <name>`",
        ));
    }
    let timeout_secs = crate::hooks::common::check_timeout(settings);
    if timeout_secs == 0 {
        return Err(misconfigured(
            "greengate.timeout is 0 (unbounded), and a Job needs a deadline".into(),
            "`git config greengate.timeout <seconds>`",
        ));
    }
    Ok(ClusterConfig {
        kubeconfig,
        namespace: namespace.to_string(),
        slots: u32::try_from(number(&keys, "greengate.cluster.slots", 2, 2)).unwrap_or(2),
        image: image.to_string(),
        digest,
        max_bundle_mib: number(&keys, "greengate.cluster.maxbundlemib", 50, 4096),
        start_wait: Duration::from_secs(number(&keys, "greengate.cluster.startwait", 30, 600)),
        timeout_secs,
        prepare: prepare(settings),
    })
}

fn is_platform_any(ext: &External) -> bool {
    matches!(
        ext.kind,
        Kind::Runnable {
            runs_on: crate::manifest::RunsOn::AnyPlatform,
            ..
        }
    )
}

/// The cluster token for `ext` as configured now — the image digest and the
/// prepare command included — or `None` where no cluster token can apply: a
/// gate that is not `platform=any`, or no pinned image on this machine.
/// Never inspects the laptop's executable.
fn token(settings: &crate::config::Settings, ext: &External) -> Option<String> {
    token_from(&machine_keys(), settings, ext, &ext.id)
}

/// [`token`] from machine keys already read, for `ext`'s program, arguments
/// and file scope under the name `name`: a caller that needs several tokens
/// reads the keys (one `git config`) once.
fn token_from(
    keys: &[(String, String)],
    settings: &crate::config::Settings,
    ext: &External,
    name: &str,
) -> Option<String> {
    if !is_platform_any(ext) {
        return None;
    }
    let digest = digest_of(key(keys, "greengate.cluster.image")?)?;
    crate::gate_identity::cluster_token(ext, name, prepare(settings).as_deref(), digest)
}

/// What the cluster is asked to judge.
#[derive(Debug, Clone, Copy)]
pub enum Judged<'a> {
    /// The index: the tree `git commit` is about to seal.
    Index,
    /// A commit's tree: a pushed tip.
    Commit(&'a str),
}

/// The tree object `judged` names, or `None` when git cannot say, or when
/// the file set is not the index and the working tree differs from it (an
/// `amont run` over a dirty tree judges content no tree holds).
fn tree_of(judged: Judged) -> Option<String> {
    match judged {
        Judged::Index => {
            if crate::hooks::common::not_the_index() {
                let dirty = crate::git::stdout(&["status", "--porcelain", "--untracked-files=no"])?;
                if !dirty.trim().is_empty() {
                    return None;
                }
            }
            write_tree()
        }
        Judged::Commit(c) => crate::git::stdout(&["rev-parse", &format!("{c}^{{tree}}")]),
    }
}

/// `git write-tree` of the index, one at a time: concurrent gates of one
/// stage each ask, and git refuses a second writer while the first holds
/// `index.lock` (it refreshes the cache-tree), so a refusal is retried.
fn write_tree() -> Option<String> {
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let _held = ONE_AT_A_TIME.lock().unwrap_or_else(|p| p.into_inner());
    for _ in 0..20 {
        if let Some(tree) = crate::git::stdout(&["write-tree"]) {
            return Some(tree);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

/// pre-commit (and `run --stamp`): is the tree about to be committed already
/// proved on the cluster for this gate, as it is declared and configured
/// now? A hit counts as a cluster pass ([`proved`]).
pub fn vouched(settings: &crate::config::Settings, ext: &External, judged: Judged) -> bool {
    let Some(token) = token(settings, ext) else {
        return false;
    };
    let Some(tree) = tree_of(judged) else {
        return false;
    };
    let hit = crate::gate_stamp::cluster_tokens(&tree).contains(&token);
    if hit {
        mark_proved(&ext.id, &tree);
    }
    hit
}

/// pre-push: does every tip carry this gate's cluster token on its tree?
pub fn vouched_on_every(
    settings: &crate::config::Settings,
    ext: &External,
    tips: &[String],
) -> bool {
    if tips.is_empty() {
        return false;
    }
    let Some(token) = token(settings, ext) else {
        return false;
    };
    vouched_trees(ext, tips, &token).is_some()
}

/// The trees of `tips`, when every one carries `token`, `ext`'s cluster token;
/// each is marked proved under `ext`'s id.
fn vouched_trees(ext: &External, tips: &[String], token: &str) -> Option<Vec<String>> {
    let stamps = crate::gate_stamp::cluster_stamps_for(tips);
    let hit = tips.iter().all(|t| {
        stamps
            .get(t)
            .is_some_and(|(_, tokens)| tokens.iter().any(|held| held == token))
    });
    if !hit {
        return None;
    }
    let trees: Vec<String> = stamps.values().map(|(tree, _)| tree.clone()).collect();
    for tree in &trees {
        mark_proved(&ext.id, tree);
    }
    Some(trees)
}

/// pre-push, a `platform=any` gate `push` whose commit-time twin `twin` is
/// also `platform=any` and declares the same suite (the same cluster identity
/// under one name: program, arguments, file scope, prepare and image): when
/// `twin`'s cluster token sits on every tip's tree, claim that pass for `push`
/// too. An action, not a lookup: it marks `push`'s id proved on those trees,
/// which is what puts it in the linux block. A twin that runs anything else
/// claims nothing, so the push gate runs.
pub(crate) fn claim_twin_cluster_pass(
    settings: &crate::config::Settings,
    push: &External,
    twin: &External,
    tips: &[String],
) -> bool {
    if tips.is_empty() || !is_platform_any(push) || !is_platform_any(twin) {
        return false;
    }
    let keys = machine_keys();
    let Some(twin_token) = token_from(&keys, settings, twin, &twin.id) else {
        return false;
    };
    // `push` as it would read under the twin's name: equal tokens mean the
    // same cluster identity.
    if token_from(&keys, settings, push, &twin.id).as_deref() != Some(twin_token.as_str()) {
        return false;
    }
    let Some(trees) = vouched_trees(twin, tips, &twin_token) else {
        return false;
    };
    for tree in &trees {
        mark_proved(&push.id, tree);
    }
    true
}

/// `amont stamps`: does `tree` carry a cluster token for any of `ids`, each
/// a trusted `platform=any` declaration?
pub fn stamped_on_tree(
    settings: &crate::config::Settings,
    externals: &[External],
    ids: &[String],
    tree: &str,
) -> bool {
    let tokens: Vec<String> = externals
        .iter()
        .filter(|e| ids.contains(&e.id))
        .filter_map(|e| token(settings, e))
        .collect();
    if tokens.is_empty() {
        return false;
    }
    let on_tree = crate::gate_stamp::cluster_tokens(tree);
    tokens.iter().any(|t| on_tree.contains(t))
}

/// post-commit: the blocking commit-time scripts whose cluster token is on
/// HEAD's tree. The bypass ledger reads them as stamped: their pass is
/// recorded, only not as a local token. Spawns nothing in a repository with
/// no `platform=any` gate.
pub fn covered_at_head(
    settings: &crate::config::Settings,
    manifest: &crate::manifest::Manifest,
) -> Vec<String> {
    if !manifest.externals.iter().any(is_platform_any) {
        return Vec::new();
    }
    let Some(tree) = crate::git::stdout(&["rev-parse", "HEAD^{tree}"]) else {
        return Vec::new();
    };
    let on_tree = crate::gate_stamp::cluster_tokens(&tree);
    crate::hooks::run_tests::blocking_commit_decls(settings, &manifest.externals)
        .into_iter()
        .filter(|d| {
            manifest
                .externals
                .iter()
                .find(|e| e.id == d.id)
                .and_then(|e| token(settings, e))
                .is_some_and(|t| on_tree.contains(&t))
        })
        .map(|d| d.script)
        .collect()
}

/// Run `ext` on the cluster if it is the cluster's to run. `true`: it passed
/// there and the pass is recorded; the caller reports `Passed`. `false`: the
/// caller runs it here, as it always has — either it was never the
/// cluster's (no line logged), or the cluster did not prove it (the reason is
/// logged).
pub fn try_run(
    settings: &crate::config::Settings,
    ext: &External,
    program: &str,
    args: &[String],
    judged: Judged,
) -> bool {
    if !is_platform_any(ext) || !executor_is_cluster(settings) {
        return false;
    }
    let outcome = cluster_config(settings).and_then(|config| {
        // Not announced once the apiserver has not answered: `run` refuses
        // it at once and the gate runs here.
        if !UNREACHABLE.load(Ordering::SeqCst) {
            crate::say!(
                "{}: running on the cluster",
                crate::ui::highlight(&ext.short_name)
            );
        }
        run(&config, ext, program, args, judged)
    });
    match outcome {
        Ok(tree) => {
            mark_proved(&ext.id, &tree);
            true
        }
        Err(fallback) => {
            // A signal killed the kubectl children: amont is dying, so no
            // local re-run may start behind the signal cleanup.
            crate::tree_run::park_if_dying();
            if matches!(fallback, Fallback::Unreachable(_)) {
                UNREACHABLE.store(true, Ordering::SeqCst);
            }
            say_fallback(&ext.short_name, &fallback);
            log_line(&ext.id, &fallback.to_string());
            false
        }
    }
}

/// The one terminal line a fallback earns, if any.
fn say_fallback(short_name: &str, fallback: &Fallback) {
    let name = crate::ui::highlight(short_name);
    match fallback {
        Fallback::Unconfigured(_) => {}
        Fallback::Misconfigured { why, fix } => {
            if !WARNED.swap(true, Ordering::SeqCst) {
                crate::hooks::common::warn(&format!(
                    "{name} runs here: {} — fix with {fix}",
                    crate::ui::sanitize(why)
                ));
            }
        }
        Fallback::GateFailed(detail) => crate::hooks::common::warn(&format!(
            "{name} failed on the cluster ({}); running it here for the verdict",
            crate::ui::sanitize(detail)
        )),
        Fallback::Unreachable(why) => {
            if !UNREACHABLE_SAID.swap(true, Ordering::SeqCst) {
                crate::hooks::common::warn(&format!(
                    "{name} and this hook's other cluster gates run here: the cluster did not answer ({})",
                    crate::ui::sanitize(why)
                ));
            }
        }
        Fallback::UnreachableEarlier => {
            if !UNREACHABLE_SAID.swap(true, Ordering::SeqCst) {
                crate::hooks::common::warn(&format!(
                    "the cluster did not answer earlier in this hook: {name} runs here"
                ));
            }
        }
        Fallback::BundleTooBig { mib, cap } => crate::hooks::common::warn(&format!(
            "{name} runs here: its bundle is {mib} MiB, over the {cap} MiB cap — raise it with `git config greengate.cluster.maxBundleMiB <MiB>`"
        )),
        Fallback::SlotsTaken | Fallback::QuotaFull | Fallback::NotProved(_) => {
            warn_unavailable(&name, fallback)
        }
    }
}

fn warn_unavailable(name: &str, fallback: &Fallback) {
    crate::hooks::common::warn(&format!(
        "{name} runs here: the cluster is unavailable ({})",
        crate::ui::sanitize(&fallback.to_string())
    ));
}

/// Append `<epoch> <gate> <reason>` to `greengate/cluster.log` in the common git
/// directory. Best-effort: the log is evidence, never a verdict.
fn log_line(gate: &str, reason: &str) {
    let reason: String = reason
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let Some(dir) =
        crate::git::stdout(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
    else {
        return;
    };
    let dir = Path::new(&dir).join(crate::names::STATE_DIR);
    let line = format!("{} {gate} {reason}\n", crate::gate_evidence::now());
    let written = std::fs::create_dir_all(&dir).and_then(|()| {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("cluster.log"))?;
        std::io::Write::write_all(&mut f, line.as_bytes())
    });
    if let Err(e) = written {
        crate::hooks::common::warn(&format!("could not append to greengate/cluster.log: {e}"));
    }
}

/// What a pass carries into its record.
struct Proof {
    image_id: String,
    uid: String,
    job: String,
    bundle_bytes: u64,
}

/// Run on the cluster and record the pass. The judged tree on a pass.
fn run(
    config: &ClusterConfig,
    ext: &External,
    program: &str,
    args: &[String],
    judged: Judged,
) -> Result<String, Fallback> {
    // A signal that killed a kubectl child must not reach a local run.
    crate::tree_run::park_if_dying();
    if UNREACHABLE.load(Ordering::SeqCst) {
        return Err(Fallback::UnreachableEarlier);
    }
    let tree = tree_of(judged)
        .ok_or_else(|| Fallback::NotProved("git would not name the tree to judge".into()))?;
    let commit = parentless_commit(&tree)
        .ok_or_else(|| Fallback::NotProved("git commit-tree failed".into()))?;
    let deadline = Instant::now() + Duration::from_secs(config.timeout_secs + 60);
    let kube = Kube::new(config);
    let scratch = Scratch::new(&ext.id, &kube)?;
    let mut argv = vec![program.to_string()];
    argv.extend(args.iter().cloned());
    let spec = Spec {
        config,
        gate: &ext.id,
        tree: &tree,
        repo_dir: &repo_dir(),
        argv: &argv,
        run_label: &scratch.run_label(),
    };
    // The gate runs on the cluster and takes no host slot (ADR-0016): a slot
    // parked for the next local gate would sit idle for the whole run.
    crate::host_slots::release_parked();
    let job = claim_slot(&kube, &spec, &scratch, deadline)?;
    let outcome = run_in(&kube, config, &job, &scratch, &commit, &tree, deadline);
    job.delete(&kube, &ext.id, deadline);
    let proof = outcome?;
    record(ext, config, &tree, &proof)?;
    Ok(tree)
}

/// Write the cluster token, with its provenance. A pass with no record is no
/// pass: the caller runs the gate here.
fn record(
    ext: &External,
    config: &ClusterConfig,
    tree: &str,
    proof: &Proof,
) -> Result<(), Fallback> {
    let token = crate::gate_identity::cluster_token(
        ext,
        &ext.id,
        config.prepare.as_deref(),
        &config.digest,
    )
    .ok_or_else(|| Fallback::NotProved("git would not hash the cluster identity".into()))?;
    let provenance = format!(
        "platform={PLATFORM} image={} job={} name={} bundle={}",
        proof.image_id, proof.uid, proof.job, proof.bundle_bytes
    );
    record_token(tree, &token, &provenance)
}

/// Write `token`, as given, on `tree`'s cluster note with its provenance. The
/// detached lane passes the token it froze at submission, never one worked out
/// from today's configuration.
fn record_token(tree: &str, token: &str, provenance: &str) -> Result<(), Fallback> {
    let _held = STAMP_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    if crate::gate_stamp::stamp_cluster(tree, token, provenance) {
        Ok(())
    } else {
        Err(Fallback::NotProved(
            "git refused to write the cluster stamp".into(),
        ))
    }
}

/// Everything after the Job exists: the bundle in, the logs out, the verdict.
fn run_in(
    kube: &Kube,
    config: &ClusterConfig,
    job: &Job,
    scratch: &Scratch,
    commit: &str,
    tree: &str,
    deadline: Instant,
) -> Result<Proof, Fallback> {
    let bundle = Bundle::create(scratch, job.slot, commit)?;
    let cap = config.max_bundle_mib;
    if bundle.bytes > cap * MIB {
        return Err(Fallback::BundleTooBig {
            mib: bundle.bytes.div_ceil(MIB),
            cap,
        });
    }
    let fetch_until = deadline.min(Instant::now() + config.start_wait);
    let pod = wait_for_fetch(kube, &job.uid, fetch_until, config.start_wait)?;
    kube.send_bundle(&pod, &bundle, deadline)?;
    wait_for_gate(kube, &job.uid, deadline)?;
    kube.stream_logs(&pod, deadline);
    let end = wait_for_job(kube, job, deadline)?;
    let pod = pod_facts(kube, &job.uid, deadline)?
        .ok_or_else(|| Fallback::NotProved("the pod is gone".into()))?;
    let image_id = judge(&end, &pod, tree, config.prepare.as_deref())?;
    Ok(Proof {
        image_id,
        uid: job.uid.clone(),
        job: job.name.clone(),
        bundle_bytes: bundle.bytes,
    })
}

/// A commit with no parent holding `tree`, fixed identity and date, so the
/// same tree is the same commit. Nothing refers to it once the bundle is
/// written.
fn parentless_commit(tree: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["commit-tree", tree, "-m", "amont cluster gate"])
        .env("GIT_AUTHOR_NAME", "amont")
        .env("GIT_AUTHOR_EMAIL", "amont@localhost")
        .env("GIT_COMMITTER_NAME", "amont")
        .env("GIT_COMMITTER_EMAIL", "amont@localhost")
        .env("GIT_AUTHOR_DATE", "@0 +0000")
        .env("GIT_COMMITTER_DATE", "@0 +0000")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let oid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !oid.is_empty()).then_some(oid)
}

/// The repository's name as a plain `[A-Za-z0-9._-]` word with no leading
/// dot: the cache subPath, so one repository's code cannot plant a cache
/// another's gate reads. From the COMMON git directory, so every worktree of
/// one repository shares it.
fn repo_dir() -> String {
    let common = crate::git::stdout(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .map(PathBuf::from)
        .unwrap_or_default();
    let name = if common.file_name().is_some_and(|n| n == ".git") {
        common.parent().and_then(Path::file_name)
    } else {
        common.file_name()
    };
    sanitize_repo_dir(
        &name
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    )
}

/// See [`repo_dir`]. Pure.
fn sanitize_repo_dir(raw: &str) -> String {
    let raw = raw.strip_suffix(".git").unwrap_or(raw);
    let clean: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let clean = clean.trim_start_matches('.');
    if clean.is_empty() {
        "repo".to_string()
    } else {
        clean.to_string()
    }
}

struct Kube {
    kubeconfig: PathBuf,
    namespace: String,
}

/// How a kubectl call ended.
enum Call {
    Ok(String),
    /// Exit non-zero, with its stderr.
    Refused(String),
    /// Could not start: nothing reached the apiserver.
    NotStarted(String),
    /// Outlived its deadline, or its wait failed: the request may have
    /// reached the apiserver all the same.
    TimedOut(String),
}

impl Kube {
    fn new(config: &ClusterConfig) -> Kube {
        Kube {
            kubeconfig: config.kubeconfig.clone(),
            namespace: config.namespace.clone(),
        }
    }

    /// `kubectl --kubeconfig <path> <request timeout> -n <ns> <args>`, stdin
    /// null, git's environment removed.
    fn command(&self, request_timeout: &str, args: &[&str]) -> Command {
        let mut cmd = Command::new(crate::hooks::common::program("kubectl"));
        cmd.arg("--kubeconfig")
            .arg(&self.kubeconfig)
            .arg(request_timeout)
            .args(["-n", &self.namespace])
            .args(args)
            .stdin(Stdio::null());
        crate::hooks::common::strip_git_env(&mut cmd);
        cmd
    }

    /// One request, under kubectl's own 10 s request timeout.
    fn short_call(&self, args: &[&str]) -> Command {
        self.command("--request-timeout=10s", args)
    }

    /// `exec` and `logs -f`, whose stream kubectl's request timeout would
    /// cut: the ceiling is ours.
    fn stream_call(&self, args: &[&str]) -> Command {
        self.command("--request-timeout=0", args)
    }

    fn capture(&self, args: &[&str], secs: u64) -> Call {
        capture(&mut self.short_call(args), secs)
    }

    /// A call whose output does not matter, only that it worked.
    fn bounded(&self, args: &[&str], secs: u64) -> Result<(), String> {
        match self.capture(args, secs) {
            Call::Ok(_) => Ok(()),
            Call::Refused(e) | Call::NotStarted(e) | Call::TimedOut(e) => Err(last_line(&e)),
        }
    }

    /// The bundle, on the one stdin that is not null; its stderr is the
    /// reason when it fails.
    fn send_bundle(&self, pod: &str, bundle: &Bundle, deadline: Instant) -> Result<(), Fallback> {
        let file = std::fs::File::open(&bundle.path)
            .map_err(|e| Fallback::NotProved(format!("bundle: {e}")))?;
        let bytes = format!("EXPECTED_BYTES={}", bundle.bytes);
        let mut cmd = self.stream_call(&[
            "exec",
            "-i",
            "-c",
            "fetch",
            pod,
            "--",
            "env",
            &bytes,
            "fetch-receive",
        ]);
        cmd.stdin(file);
        let secs = secs_left(deadline, u64::MAX);
        match capture(&mut cmd, secs) {
            Call::Ok(_) => Ok(()),
            Call::Refused(e) => Err(Fallback::NotProved(format!(
                "fetch-receive failed: {}",
                last_line(&e)
            ))),
            Call::NotStarted(e) | Call::TimedOut(e) => Err(Fallback::Unreachable(e)),
        }
    }

    /// The gate's output, as it runs, under the ceiling clock only: the
    /// silence budget does not apply to a pod. What the logs say decides
    /// nothing, so a log stream that fails is not a reason to fall back.
    fn stream_logs(&self, pod: &str, deadline: Instant) {
        let mut cmd = self.stream_call(&["logs", "-f", pod, "-c", "gate"]);
        let secs = secs_left(deadline, u64::MAX);
        match crate::live::current_sink() {
            Some((stage, idx)) => {
                cmd.stdout(Stdio::piped()).stderr(Stdio::null());
                let Ok(mut child) = cmd.spawn() else {
                    return;
                };
                if let Some(mut out) = child.stdout.take() {
                    // Detached: it ends with the pipe, and the pipe with kubectl.
                    std::thread::spawn(move || {
                        let mut buf = [0u8; 8192];
                        while let Ok(n) = std::io::Read::read(&mut out, &mut buf) {
                            if n == 0 {
                                break;
                            }
                            stage.append_raw(idx, &buf[..n]);
                        }
                    });
                }
                let waited = crate::hooks::common::wait_within(
                    &mut child,
                    crate::hooks::common::Clocks::ceiling_only(secs),
                    None,
                );
                drop(waited); // the logs decide nothing; see above
            }
            None => {
                let shown = crate::hooks::common::status_within_secs(&mut cmd, secs);
                drop(shown); // the logs decide nothing; see above
            }
        }
    }
}

/// Run `cmd` with stdout and stderr captured apart, under a ceiling of
/// `secs`, through the hooks' bounded wait. Takes no host slot.
fn capture(cmd: &mut Command, secs: u64) -> Call {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Call::NotStarted(format!("kubectl: {e}")),
    };
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let ran = crate::hooks::common::wait_within(
        &mut child,
        crate::hooks::common::Clocks::ceiling_only(secs),
        None,
    );
    let collect = |rx: std::sync::mpsc::Receiver<Vec<u8>>| {
        rx.recv_timeout(Duration::from_secs(2))
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default()
    };
    match ran {
        Ok(crate::hooks::common::Ran::Status(s)) if s.success() => Call::Ok(collect(out)),
        Ok(crate::hooks::common::Ran::Status(_)) => Call::Refused(collect(err)),
        Ok(crate::hooks::common::Ran::TimedOut(_)) => Call::TimedOut("kubectl timed out".into()),
        Err(e) => Call::TimedOut(format!("kubectl: {e}")),
    }
}

/// Read a pipe to its end on a thread, handing the bytes over once.
fn drain(pipe: Option<impl std::io::Read + Send + 'static>) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    if let Some(mut pipe) = pipe {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            // A read error ends the pipe early; what was read still counts.
            let read = pipe.read_to_end(&mut buf);
            drop(read);
            // The receiver gave up after its own timeout: nobody to tell.
            let sent = tx.send(buf);
            drop(sent);
        });
    }
    rx
}

/// What a refused `kubectl create` means for the slot.
#[derive(Debug, PartialEq, Eq)]
enum Refusal {
    /// The Job name is taken: the next slot.
    Taken,
    /// The quota is full: the next slot, then local.
    Quota,
    /// Anything else: the apiserver failing.
    Other,
}

fn refusal(stderr: &str) -> Refusal {
    if stderr.contains("exceeded quota") {
        Refusal::Quota
    } else if stderr.contains("AlreadyExists") || stderr.contains("already exists") {
        Refusal::Taken
    } else {
        Refusal::Other
    }
}

/// A Job amont created.
struct Job {
    run: u64,
    slot: u32,
    name: String,
    uid: String,
    gate: String,
}

impl Job {
    /// Foreground and waited for, so no old pod still holds the slot's PVC
    /// when the next Job takes it — within the time left; past the deadline
    /// it is only asked to go. A delete that fails is said and logged: the
    /// slot stays taken until its TTL.
    fn delete(&self, kube: &Kube, gate: &str, deadline: Instant) {
        let left = deadline.saturating_duration_since(Instant::now()).as_secs();
        let timeout = format!("--timeout={}s", left.max(1));
        let deleted = if left >= 5 {
            let args = [
                "delete",
                "job",
                &self.name,
                "--cascade=foreground",
                "--wait",
                &timeout,
            ];
            kube.bounded(&args, left)
        } else {
            kube.bounded(&["delete", "job", &self.name, "--wait=false"], 5)
        };
        if let Err(why) = deleted {
            let reason = format!("could not delete Job {}: {why}", self.name);
            crate::hooks::common::warn(&reason);
            log_line(gate, &reason);
        }
        live_update(self.run, |l| l.created = Created::NotSent);
    }
}

/// What the Job spec is built from.
struct Spec<'a> {
    config: &'a ClusterConfig,
    gate: &'a str,
    tree: &'a str,
    repo_dir: &'a str,
    argv: &'a [String],
    /// The value of the Job's `greengate.run` label.
    run_label: &'a str,
}

/// Create `amont-gate-slot-<k>` for the first free `k`.
fn claim_slot(
    kube: &Kube,
    spec: &Spec,
    scratch: &Scratch,
    deadline: Instant,
) -> Result<Job, Fallback> {
    crate::staged_only::install_signal_handler();
    let mut refusals = Vec::new();
    for slot in 0..spec.config.slots {
        let path = scratch.write(
            &format!("job-{slot}.json"),
            job_manifest(spec, slot).as_bytes(),
        )?;
        let path = path.to_string_lossy().into_owned();
        let name = format!("{JOB_PREFIX}{slot}");
        let args = ["create", "-f", &path, "-o", "jsonpath={.metadata.uid}"];
        live_update(scratch.run, |l| l.created = Created::Sent);
        let created = kube.capture(&args, secs_left(deadline, CALL_SECS));
        if crate::tree_run::is_dying() {
            // The signal watcher may have cleaned up before this `create`
            // landed: whatever it made is ours to drop, then park.
            delete_by_label(kube, &scratch.run_label(), spec.gate);
            crate::tree_run::park_if_dying();
        }
        match created {
            Call::Ok(uid) if !uid.trim().is_empty() => {
                live_update(scratch.run, |l| l.created = Created::Named(name.clone()));
                return Ok(Job {
                    run: scratch.run,
                    slot,
                    name,
                    uid: uid.trim().to_string(),
                    gate: spec.gate.to_string(),
                });
            }
            Call::Ok(_) => {
                // The Job exists, only unnamed by its UID: take it down by
                // name rather than hold the slot until its deadline.
                live_update(scratch.run, |l| l.created = Created::Named(name.clone()));
                delete_by_name(kube, &name, spec.gate);
                live_update(scratch.run, |l| l.created = Created::NotSent);
                return Err(Fallback::NotProved(
                    "kubectl create printed no Job UID".into(),
                ));
            }
            Call::Refused(err) => match refusal(&err) {
                Refusal::Taken | Refusal::Quota => refusals.push(refusal(&err)),
                Refusal::Other => return Err(Fallback::Unreachable(last_line(&err))),
            },
            Call::NotStarted(err) => return Err(Fallback::Unreachable(err)),
            // The request may have landed: drop whatever it made, briefly and
            // quietly; the Job's deadline bounds what a failed delete leaves.
            Call::TimedOut(err) => {
                delete_by_label(kube, &scratch.run_label(), spec.gate);
                return Err(Fallback::Unreachable(err));
            }
        }
    }
    if refusals.iter().all(|r| *r == Refusal::Quota) {
        Err(Fallback::QuotaFull)
    } else {
        Err(Fallback::SlotsTaken)
    }
}

/// Ask the apiserver to drop the Job `name`, without waiting for it, for a Job
/// whose UID is unknown. A delete that fails is said and logged.
fn delete_by_name(kube: &Kube, name: &str, gate: &str) {
    if let Err(why) = kube.bounded(&["delete", "job", name, "--wait=false"], CALL_SECS) {
        say_delete_failed(&format!("Job {name}"), &why, gate);
    }
}

/// Ask the apiserver to drop every Job carrying this run's label, without
/// waiting: for a `create` whose answer was lost. Bounded short and logged
/// only: the person was already told the cluster did not answer, and a Job
/// left behind ends at its own deadline.
fn delete_by_label(kube: &Kube, label: &str, gate: &str) {
    let selector = run_selector(label);
    let args = ["delete", "job", "-l", &selector, "--wait=false"];
    if let Err(why) = kube.bounded(&args, LABEL_DELETE_SECS) {
        log_line(
            gate,
            &format!(
                "could not delete the Jobs of this run: {}",
                crate::ui::sanitize(&why)
            ),
        );
    }
}

/// The label selector of one run's Jobs.
fn run_selector(label: &str) -> String {
    format!("{RUN_LABEL_KEY}={label}")
}

fn say_delete_failed(what: &str, why: &str, gate: &str) {
    let reason = format!("could not delete {what}: {}", crate::ui::sanitize(why));
    crate::hooks::common::warn(&reason);
    log_line(gate, &reason);
}

fn last_line(s: &str) -> String {
    s.lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("no output")
        .to_string()
}

fn json_string(v: &str) -> String {
    format!("\"{}\"", crate::json::escape(v))
}

fn env_var(name: &str, value: &str) -> String {
    format!(
        "{{\"name\":{},\"value\":{}}}",
        json_string(name),
        json_string(value)
    )
}

fn mount(name: &str, path: &str) -> String {
    format!(
        "{{\"name\":{},\"mountPath\":{}}}",
        json_string(name),
        json_string(path)
    )
}

fn mount_sub(name: &str, path: &str, sub: &str) -> String {
    format!(
        "{{\"name\":{},\"mountPath\":{},\"subPath\":{}}}",
        json_string(name),
        json_string(path),
        json_string(sub)
    )
}

fn args_json(argv: &[String]) -> String {
    format!(
        "[{}]",
        argv.iter()
            .map(|a| json_string(a))
            .collect::<Vec<_>>()
            .join(",")
    )
}

const SECURITY: &str = "{\"allowPrivilegeEscalation\":false,\"capabilities\":{\"drop\":[\"ALL\"]}}";

fn container(name: &str, image: &str, argv: &[String], extra: &str) -> String {
    format!(
        "{{\"name\":{},\"image\":{},\"args\":{},\"securityContext\":{SECURITY},{extra}}}",
        json_string(name),
        json_string(image),
        args_json(argv)
    )
}

fn resources(memory: &str, cpu: &str) -> String {
    format!(
        "\"resources\":{{\"requests\":{{\"memory\":{m},\"cpu\":\"500m\"}},\
         \"limits\":{{\"memory\":{m},\"cpu\":{c}}}}}",
        m = json_string(memory),
        c = json_string(cpu)
    )
}

/// The Job, as JSON `kubectl create -f` reads. `args`, never `command`: the
/// image's ENTRYPOINT is tini.
fn job_manifest(spec: &Spec, slot: u32) -> String {
    let config = spec.config;
    let gate_ref = format!("{TEMP_REF_PREFIX}{slot}");
    let fetch = container(
        "fetch",
        &config.image,
        &["/usr/local/bin/fetch".to_string()],
        &format!(
            "\"env\":[{},{},{}],\"volumeMounts\":[{},{},{}],{}",
            env_var("EXPECTED_TREE", spec.tree),
            env_var("GATE_REF", &gate_ref),
            env_var("REPO_DIR", spec.repo_dir),
            mount("cache", "/cache"),
            mount("work", "/work"),
            mount("checkout", "/checkout"),
            resources("512Mi", "1"),
        ),
    );
    let work_mounts = format!(
        "\"workingDir\":\"/checkout\",\"volumeMounts\":[{},{}],{}",
        mount_sub("cache", "/cache", spec.repo_dir),
        mount("checkout", "/checkout"),
        resources(MEMORY, CPU_LIMIT),
    );
    let mut init = vec![fetch];
    if let Some(prepare) = &config.prepare {
        let argv = ["sh".to_string(), "-c".to_string(), prepare.clone()];
        init.push(container("prepare", &config.image, &argv, &work_mounts));
    }
    let gate = container("gate", &config.image, spec.argv, &work_mounts);
    format!(
        "{{\"apiVersion\":\"batch/v1\",\"kind\":\"Job\",\
         \"metadata\":{{\"name\":{name},\"labels\":{{\"app.kubernetes.io/managed-by\":\"amont\",{run_label_key}:{run_label}}},\
         \"annotations\":{{\"greengate.dev/gate\":{gate_id},\"greengate.dev/tree\":{tree}}}}},\
         \"spec\":{{\"backoffLimit\":0,\"activeDeadlineSeconds\":{deadline},\
         \"ttlSecondsAfterFinished\":300,\"template\":{{\"spec\":{{\
         \"restartPolicy\":\"Never\",\"priorityClassName\":{prio},\
         \"nodeSelector\":{{\"kubernetes.io/hostname\":{node}}},\
         \"imagePullSecrets\":[{{\"name\":{pull}}}],\
         \"automountServiceAccountToken\":false,\
         \"securityContext\":{{\"runAsNonRoot\":true,\"runAsUser\":1000,\"runAsGroup\":1000,\
         \"fsGroup\":1000,\"seccompProfile\":{{\"type\":\"RuntimeDefault\"}}}},\
         \"volumes\":[{{\"name\":\"cache\",\"persistentVolumeClaim\":{{\"claimName\":{pvc}}}}},\
         {{\"name\":\"work\",\"emptyDir\":{{}}}},{{\"name\":\"checkout\",\"emptyDir\":{{}}}}],\
         \"initContainers\":[{init}],\"containers\":[{gate}]}}}}}}}}",
        name = json_string(&format!("{JOB_PREFIX}{slot}")),
        run_label_key = json_string(RUN_LABEL_KEY),
        run_label = json_string(spec.run_label),
        gate_id = json_string(spec.gate),
        tree = json_string(spec.tree),
        deadline = config.timeout_secs + 60,
        prio = json_string(PRIORITY_CLASS),
        node = json_string(NODE),
        pull = json_string(PULL_SECRET),
        pvc = json_string(&format!("{CACHE_PREFIX}{slot}")),
        init = init.join(","),
    )
}

/// Unique to this process and run, across every host sharing the namespace
/// (pids repeat from host to host, and in containers): the pid, a random
/// per-process nonce and the run. 45 characters at most (a 7-digit pid, 16
/// hex digits, a 20-digit run), inside a label value's 63, alphanumeric at
/// both ends.
fn run_label(run: u64) -> String {
    format!("{}-{:016x}-{run}", std::process::id(), process_nonce())
}

/// 64 random bits, drawn once per process from the standard library's
/// randomly seeded hasher: no dependency, no file to read.
fn process_nonce() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    static NONCE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *NONCE.get_or_init(|| {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        hasher.finish()
    })
}

/// A private scratch directory for one cluster run — the Job manifests and
/// the bundle — created fresh (mode 0700 on unix), so no other user can guess,
/// pre-create or redirect a path in it. Removed on drop, with its entry in
/// the interrupt list.
struct Scratch {
    run: u64,
    dir: PathBuf,
}

impl Scratch {
    fn new(gate: &str, kube: &Kube) -> Result<Scratch, Fallback> {
        let run = SEQ.fetch_add(1, Ordering::SeqCst);
        let gate: String = gate
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let dir = std::env::temp_dir().join(format!(
            "greengate-cluster-{}-{run}-{gate}",
            std::process::id()
        ));
        #[cfg(unix)]
        let builder = {
            let mut builder = std::fs::DirBuilder::new();
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            builder
        };
        #[cfg(not(unix))]
        let builder = std::fs::DirBuilder::new();
        builder
            .create(&dir)
            .map_err(|e| Fallback::NotProved(format!("scratch directory: {e}")))?;
        LIVE.lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(LiveRun {
                run,
                kubeconfig: kube.kubeconfig.clone(),
                namespace: kube.namespace.clone(),
                scratch: dir.clone(),
                run_label: run_label(run),
                created: Created::NotSent,
                gate_ref: None,
            });
        Ok(Scratch { run, dir })
    }

    /// The `greengate.run` label value of this run's Jobs.
    fn run_label(&self) -> String {
        run_label(self.run)
    }

    /// A new file in the scratch directory; refused if it exists.
    fn write(&self, name: &str, bytes: &[u8]) -> Result<PathBuf, Fallback> {
        let path = self.dir.join(name);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options
            .open(&path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, bytes))
            .map_err(|e| Fallback::NotProved(format!("scratch file: {e}")))?;
        Ok(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.dir) {
            crate::hooks::common::warn(&format!("could not remove {}: {e}", self.dir.display()));
        }
        LIVE.lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|l| l.run != self.run);
    }
}

/// The judged commit as a git bundle of one temporary ref.
struct Bundle {
    path: PathBuf,
    bytes: u64,
}

impl Bundle {
    fn create(scratch: &Scratch, slot: u32, commit: &str) -> Result<Bundle, Fallback> {
        let gate_ref = format!("{TEMP_REF_PREFIX}{slot}");
        if !crate::git::succeeds(&["update-ref", &gate_ref, commit]) {
            return Err(Fallback::NotProved(format!(
                "git update-ref {gate_ref} failed"
            )));
        }
        live_update(scratch.run, |l| l.gate_ref = Some(gate_ref.clone()));
        let path = scratch.dir.join(format!("{slot}.bundle"));
        let path_text = path.to_string_lossy().into_owned();
        let made = crate::git::succeeds(&["bundle", "create", "-q", &path_text, &gate_ref]);
        let dropped = crate::git::succeeds(&["update-ref", "-d", &gate_ref]);
        live_update(scratch.run, |l| l.gate_ref = None);
        if !dropped {
            crate::hooks::common::warn(&format!("could not delete the temporary ref {gate_ref}"));
        }
        if !made {
            return Err(Fallback::NotProved("git bundle create failed".into()));
        }
        let bytes = std::fs::metadata(&path)
            .map_err(|e| Fallback::NotProved(format!("bundle: {e}")))?
            .len();
        Ok(Bundle { path, bytes })
    }
}

/// One container's state.
#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Absent,
    Waiting,
    Running,
    Terminated {
        exit: Option<i64>,
        reason: String,
        message: String,
    },
}

impl State {
    fn of(status: Option<&Value>) -> State {
        let Some(state) = status.and_then(|s| s.get("state")) else {
            return State::Absent;
        };
        if let Some(t) = state.get("terminated") {
            let text = |k: &str| t.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let exit = match t.get("exitCode") {
                Some(Value::Num(n)) => n.parse().ok(),
                _ => None,
            };
            return State::Terminated {
                exit,
                reason: text("reason"),
                message: text("message"),
            };
        }
        if state.get("running").is_some() {
            return State::Running;
        }
        State::Waiting
    }

    fn started(&self) -> bool {
        match self {
            State::Running | State::Terminated { .. } => true,
            State::Absent | State::Waiting => false,
        }
    }

    fn passed(&self) -> bool {
        matches!(self, State::Terminated { exit: Some(0), .. })
    }

    /// `exit N` or `exit N, <reason>` for a container that ended badly.
    fn failed(&self) -> Option<String> {
        let State::Terminated { exit, reason, .. } = self else {
            return None;
        };
        if *exit == Some(0) {
            return None;
        }
        let code = exit.map_or("an unknown code".to_string(), |c| c.to_string());
        Some(match reason.as_str() {
            "" | "Error" | "Completed" => format!("exit {code}"),
            r => format!("exit {code}, {r}"),
        })
    }
}

/// The pod's phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Pending,
    Running,
    Succeeded,
    Failed,
    Unknown,
}

impl Phase {
    fn parse(s: &str) -> Phase {
        match s {
            "Pending" => Phase::Pending,
            "Running" => Phase::Running,
            "Succeeded" => Phase::Succeeded,
            "Failed" => Phase::Failed,
            _ => Phase::Unknown,
        }
    }

    fn ended(self) -> bool {
        match self {
            Phase::Succeeded | Phase::Failed => true,
            Phase::Pending | Phase::Running | Phase::Unknown => false,
        }
    }
}

/// The pod's recorded facts.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PodFacts {
    name: String,
    phase: Phase,
    disrupted: bool,
    fetch: State,
    prepare: State,
    gate: State,
    image_id: Option<String>,
}

fn named<'a>(list: Option<&'a Value>, name: &str) -> Option<&'a Value> {
    list?
        .items()
        .iter()
        .find(|c| c.get("name").and_then(Value::as_str) == Some(name))
}

fn str_at<'a>(v: Option<&'a Value>, key: &str) -> Option<&'a str> {
    v?.get(key).and_then(Value::as_str)
}

/// The first pod of `kubectl get pods -o json`, as facts. Pure. The gate's
/// container is `gate`: a Job's.
fn parse_pod(json: &str) -> Option<PodFacts> {
    let doc = crate::json_read::parse(json)?;
    parse_pod_object(doc.get("items")?.items().first()?, "gate")
}

/// One Pod object, as facts, the gate being the container `gate_container`:
/// `gate` in a Job's pod, `main` in an Argo Workflow's (Argo names the main
/// container itself). Pure.
fn parse_pod_object(pod: &Value, gate_container: &str) -> Option<PodFacts> {
    let status = pod.get("status");
    let inits = status.and_then(|s| s.get("initContainerStatuses"));
    let mains = status.and_then(|s| s.get("containerStatuses"));
    let gate = named(mains, gate_container);
    let disrupted = status.and_then(|s| s.get("conditions")).is_some_and(|c| {
        c.items().iter().any(|c| {
            str_at(Some(c), "type") == Some("DisruptionTarget")
                && str_at(Some(c), "status") == Some("True")
        })
    });
    Some(PodFacts {
        name: str_at(pod.get("metadata"), "name")?.to_string(),
        phase: Phase::parse(str_at(status, "phase").unwrap_or("")),
        disrupted,
        fetch: State::of(named(inits, "fetch")),
        prepare: State::of(named(inits, "prepare")),
        gate: State::of(gate),
        image_id: str_at(gate, "imageID")
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    })
}

/// The Job's terminal condition, if it has one.
#[derive(Debug, Clone, PartialEq, Eq)]
enum JobEnd {
    Running,
    Complete,
    Failed(String),
}

/// `kubectl get job -o json`: its UID, its gate annotation, its end. Pure.
#[derive(Debug, Clone, PartialEq, Eq)]
struct JobFacts {
    uid: Option<String>,
    gate: Option<String>,
    end: JobEnd,
}

fn parse_job(json: &str) -> Option<JobFacts> {
    let doc = crate::json_read::parse(json)?;
    let metadata = doc.get("metadata");
    let conditions = doc.get("status").and_then(|s| s.get("conditions"));
    let truthy = |c: &&Value| str_at(Some(c), "status") == Some("True");
    let mut end = JobEnd::Running;
    for c in conditions
        .map(Value::items)
        .unwrap_or(&[])
        .iter()
        .filter(truthy)
    {
        match str_at(Some(c), "type") {
            Some("Failed") => {
                let reason = str_at(Some(c), "reason").unwrap_or("Failed");
                end = JobEnd::Failed(reason.to_string());
                break;
            }
            Some("Complete") => end = JobEnd::Complete,
            _ => {}
        }
    }
    Some(JobFacts {
        uid: str_at(metadata, "uid").map(str::to_string),
        gate: str_at(
            metadata.and_then(|m| m.get("annotations")),
            crate::names::GATE_ANNOTATION,
        )
        .map(str::to_string),
        end,
    })
}

/// The verdict, from recorded facts only: the observed imageID on a pass,
/// else why not. Pure.
fn judge(
    end: &JobEnd,
    pod: &PodFacts,
    expected_tree: &str,
    prepare: Option<&str>,
) -> Result<String, Fallback> {
    let not = |why: String| Err(Fallback::NotProved(why));
    if pod.disrupted {
        return not("the pod was disrupted (DisruptionTarget)".into());
    }
    match end {
        JobEnd::Complete => {}
        JobEnd::Failed(reason) => {
            return match pod.gate.failed() {
                Some(detail) => Err(Fallback::GateFailed(detail)),
                None => not(format!("the Job failed ({reason})")),
            }
        }
        JobEnd::Running => return not("the Job has no terminal condition".into()),
    }
    let want = format!("tree {expected_tree}");
    match &pod.fetch {
        State::Terminated {
            exit: Some(0),
            message,
            ..
        } if message.trim() == want => {}
        State::Terminated { message, .. } => {
            return not(format!("fetch reported {:?}, not {want:?}", message.trim()))
        }
        State::Absent | State::Waiting | State::Running => {
            return not("fetch did not terminate".into())
        }
    }
    if prepare.is_some() && !pod.prepare.passed() {
        return not("prepare did not exit 0".into());
    }
    if !pod.gate.passed() {
        return match pod.gate.failed() {
            Some(detail) => Err(Fallback::GateFailed(detail)),
            None => not("the gate did not finish".into()),
        };
    }
    match &pod.image_id {
        Some(id) => Ok(id.clone()),
        None => not("the pod recorded no imageID".into()),
    }
}

/// The pod of the Job `uid`, by its controller label only — never by name,
/// which an older Job's pod could share.
fn pod_facts(kube: &Kube, uid: &str, deadline: Instant) -> Result<Option<PodFacts>, Fallback> {
    let selector = format!("controller-uid={uid}");
    let args = ["get", "pods", "-l", &selector, "-o", "json"];
    match kube.capture(&args, secs_left(deadline, CALL_SECS)) {
        Call::Ok(json) => Ok(parse_pod(&json)),
        Call::Refused(e) => Err(Fallback::NotProved(format!(
            "kubectl get pods: {}",
            last_line(&e)
        ))),
        Call::NotStarted(e) | Call::TimedOut(e) => Err(Fallback::Unreachable(e)),
    }
}

/// Poll until `done` answers, or `until` passes.
fn poll<T>(
    until: Instant,
    mut done: impl FnMut() -> Result<Option<T>, Fallback>,
) -> Result<Option<T>, Fallback> {
    loop {
        if let Some(v) = done()? {
            return Ok(Some(v));
        }
        if Instant::now() >= until {
            return Ok(None);
        }
        std::thread::sleep(POLL);
    }
}

/// Wait, bounded, for `fetch` — trusted image code, before any repository
/// code — to be Running. The pod's name, or why not.
fn wait_for_fetch(
    kube: &Kube,
    uid: &str,
    until: Instant,
    wait: Duration,
) -> Result<String, Fallback> {
    let found = poll(until, || {
        let Some(p) = pod_facts(kube, uid, until)? else {
            return Ok(None);
        };
        Ok(match p.fetch {
            State::Running => Some(Ok(p.name)),
            State::Terminated { .. } => Some(Err(Fallback::NotProved(
                "fetch ended before the bundle arrived".into(),
            ))),
            State::Absent | State::Waiting => None,
        })
    })?;
    found.unwrap_or_else(|| {
        Err(Fallback::NotProved(format!(
            "the pod was not Running within {} s",
            wait.as_secs()
        )))
    })
}

/// Wait until the gate container has started, or an init container failed.
fn wait_for_gate(kube: &Kube, uid: &str, deadline: Instant) -> Result<(), Fallback> {
    let found = poll(deadline, || {
        let Some(p) = pod_facts(kube, uid, deadline)? else {
            return Ok(None);
        };
        // An init container that failed is infrastructure, never the gate's
        // verdict: said first, whatever the pod's phase.
        if let Some(w) = p.fetch.failed() {
            return Ok(Some(Err(Fallback::NotProved(format!("fetch {w}")))));
        }
        if let Some(w) = p.prepare.failed() {
            return Ok(Some(Err(Fallback::NotProved(format!("prepare {w}")))));
        }
        Ok((p.gate.started() || p.phase.ended()).then_some(Ok(())))
    })?;
    found.unwrap_or_else(|| {
        Err(Fallback::NotProved(
            "the gate did not start before the deadline".into(),
        ))
    })
}

/// Wait for the Job's terminal condition — shortly after the logs end, or at
/// the deadline — on the Job amont created: its UID and gate annotation must
/// be this run's, or nothing it reports is judged.
fn wait_for_job(kube: &Kube, job: &Job, deadline: Instant) -> Result<JobEnd, Fallback> {
    let until = deadline.min(Instant::now() + Duration::from_secs(60));
    let found = poll(until, || {
        let args = ["get", "job", &job.name, "-o", "json"];
        let json = match kube.capture(&args, secs_left(deadline, CALL_SECS)) {
            Call::Ok(json) => json,
            Call::Refused(e) => {
                return Err(Fallback::NotProved(format!(
                    "kubectl get job: {}",
                    last_line(&e)
                )))
            }
            Call::NotStarted(e) | Call::TimedOut(e) => return Err(Fallback::Unreachable(e)),
        };
        let Some(facts) = parse_job(&json) else {
            return Ok(None);
        };
        if facts.uid.as_deref() != Some(job.uid.as_str())
            || facts.gate.as_deref() != Some(job.gate.as_str())
        {
            return Err(Fallback::NotProved(format!(
                "{} is not the Job this run created",
                job.name
            )));
        }
        Ok((facts.end != JobEnd::Running).then_some(facts.end))
    })?;
    found.ok_or_else(|| Fallback::NotProved("the Job reached no terminal condition".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    /// A run label is the pid, a 16-hex-digit nonce and the run: valid as a
    /// label value, distinct per run, and the same nonce within one process.
    #[test]
    fn a_run_label_is_pid_nonce_and_run() {
        let first = run_label(1);
        let parts: Vec<&str> = first.split('-').collect();
        assert_eq!(parts.len(), 3, "{first}");
        assert_eq!(parts[0], std::process::id().to_string());
        assert_eq!(parts[1].len(), 16, "{first}");
        assert!(parts[1].chars().all(|c| c.is_ascii_hexdigit()), "{first}");
        assert_eq!(parts[2], "1");
        assert!(first.len() <= 63);
        let second = run_label(2);
        assert_ne!(first, second);
        assert_eq!(second.split('-').nth(1), Some(parts[1]));
    }

    fn cluster_config(prepare: Option<&str>) -> ClusterConfig {
        ClusterConfig {
            kubeconfig: PathBuf::from("/k"),
            namespace: "amont-gates".into(),
            slots: 2,
            image: format!("reg/gate:1@sha256:{}", "a".repeat(64)),
            digest: format!("sha256:{}", "a".repeat(64)),
            max_bundle_mib: 1,
            start_wait: Duration::from_secs(30),
            timeout_secs: 3600,
            prepare: prepare.map(str::to_string),
        }
    }

    #[test]
    fn a_digest_is_sha256_and_64_hex() {
        let hex = "b".repeat(64);
        assert_eq!(
            digest_of(&format!("r/x:1@sha256:{hex}")),
            Some(format!("sha256:{hex}").as_str())
        );
        assert_eq!(digest_of("r/x:1"), None);
        assert_eq!(digest_of("r/x@sha256:short"), None);
    }

    #[test]
    fn a_repo_dir_is_a_plain_name() {
        assert_eq!(sanitize_repo_dir("amont"), "amont");
        assert_eq!(sanitize_repo_dir("my repo!"), "my-repo-");
        assert_eq!(sanitize_repo_dir("..hidden"), "hidden");
        assert_eq!(sanitize_repo_dir("x.git"), "x");
        assert_eq!(sanitize_repo_dir(""), "repo");
    }

    /// Every later wait is capped at the time the one deadline leaves.
    #[test]
    fn a_wait_never_outlives_the_deadline() {
        let soon = Instant::now() + Duration::from_secs(3);
        assert!(secs_left(soon, CALL_SECS) <= 3);
        let later = Instant::now() + Duration::from_secs(600);
        assert_eq!(secs_left(later, CALL_SECS), CALL_SECS);
        let past = Instant::now() - Duration::from_secs(1);
        assert_eq!(secs_left(past, CALL_SECS), 1, "a floor of one second");
    }

    #[test]
    fn the_job_carries_the_contract() {
        let config = cluster_config(Some("make deps"));
        let argv = vec!["scripts/lint.sh".to_string(), "--strict".to_string()];
        let spec = Spec {
            config: &config,
            gate: "pre-commit-lint",
            tree: "abc",
            repo_dir: "amont",
            argv: &argv,
            run_label: "42-7",
        };
        let json = job_manifest(&spec, 1);
        let doc = crate::json_read::parse(&json).expect("valid JSON");
        let job_spec = doc.get("spec").unwrap();
        assert_eq!(job_spec.get("backoffLimit"), Some(&Value::Num("0".into())));
        assert_eq!(
            job_spec.get("activeDeadlineSeconds"),
            Some(&Value::Num("3660".into()))
        );
        assert!(!json.contains("\"command\""));
        for needle in [
            "\"name\":\"amont-gate-slot-1\"",
            "\"claimName\":\"gate-cache-1\"",
            "\"subPath\":\"amont\"",
            "\"restartPolicy\":\"Never\"",
            "\"priorityClassName\":\"gate-batch\"",
            "\"kubernetes.io/hostname\":\"talos-wk-3-gpu\"",
            "\"automountServiceAccountToken\":false",
            "\"args\":[\"sh\",\"-c\",\"make deps\"]",
            "\"args\":[\"scripts/lint.sh\",\"--strict\"]",
            "\"value\":\"refs/amont/cluster/1\"",
            "\"greengate.dev/gate\":\"pre-commit-lint\"",
        ] {
            assert!(json.contains(needle), "{needle} missing from {json}");
        }
    }

    fn pod(fetch: State, gate: State) -> PodFacts {
        PodFacts {
            name: "p".into(),
            phase: Phase::Succeeded,
            disrupted: false,
            fetch,
            prepare: State::Absent,
            gate,
            image_id: Some("img@sha256:x".into()),
        }
    }

    fn done(exit: i64, reason: &str, message: &str) -> State {
        State::Terminated {
            exit: Some(exit),
            reason: reason.into(),
            message: message.into(),
        }
    }

    #[test]
    fn only_recorded_facts_make_a_pass() {
        let ok = pod(done(0, "", "tree t1\n"), done(0, "", ""));
        assert_eq!(
            judge(&JobEnd::Complete, &ok, "t1", None),
            Ok("img@sha256:x".into())
        );
        let failed = || JobEnd::Failed("BackoffLimitExceeded".into());
        let cases: Vec<(&str, JobEnd, PodFacts, Option<&str>)> = vec![
            (
                "fetch reported",
                JobEnd::Complete,
                pod(done(0, "", "tree t2"), done(0, "", "")),
                None,
            ),
            (
                "failed on the cluster (exit 2)",
                failed(),
                pod(done(0, "", "tree t1"), done(2, "Error", "")),
                None,
            ),
            (
                "exit 137, OOMKilled",
                failed(),
                pod(done(0, "", "tree t1"), done(137, "OOMKilled", "")),
                None,
            ),
            (
                "DeadlineExceeded",
                JobEnd::Failed("DeadlineExceeded".into()),
                pod(done(0, "", "tree t1"), State::Running),
                None,
            ),
            (
                "prepare did not exit 0",
                JobEnd::Complete,
                ok.clone(),
                Some("make"),
            ),
            ("no terminal condition", JobEnd::Running, ok.clone(), None),
            (
                "no imageID",
                JobEnd::Complete,
                PodFacts {
                    image_id: None,
                    ..ok.clone()
                },
                None,
            ),
            (
                "DisruptionTarget",
                JobEnd::Complete,
                PodFacts {
                    disrupted: true,
                    ..ok.clone()
                },
                None,
            ),
        ];
        for (why, end, p, prepare) in cases {
            let reason = judge(&end, &p, "t1", prepare).expect_err(why).to_string();
            assert!(reason.contains(why), "{why}: {reason}");
        }
    }

    #[test]
    fn facts_parse_from_kubectl_json() {
        let json = r#"{"items":[{"metadata":{"name":"amont-gate-slot-0-x"},
          "status":{"phase":"Failed","conditions":[{"type":"DisruptionTarget","status":"True"}],
          "initContainerStatuses":[{"name":"fetch","state":{"terminated":{"exitCode":0,"message":"tree t"}}}],
          "containerStatuses":[{"name":"gate","imageID":"i@sha256:1","state":{"terminated":{"exitCode":137,"reason":"OOMKilled"}}}]}}]}"#;
        let p = parse_pod(json).expect("parses");
        assert!(p.disrupted);
        assert_eq!(p.phase, Phase::Failed);
        assert_eq!(p.image_id.as_deref(), Some("i@sha256:1"));
        assert_eq!(p.gate, done(137, "OOMKilled", ""));
        assert_eq!(p.prepare, State::Absent);
        let job = r#"{"metadata":{"uid":"u1","annotations":{"greengate.dev/gate":"pre-commit-x"}},
          "status":{"conditions":[{"type":"Failed","status":"True","reason":"DeadlineExceeded"}]}}"#;
        assert_eq!(
            parse_job(job),
            Some(JobFacts {
                uid: Some("u1".into()),
                gate: Some("pre-commit-x".into()),
                end: JobEnd::Failed("DeadlineExceeded".into()),
            })
        );
        let complete = r#"{"status":{"conditions":[{"type":"Complete","status":"True"}]}}"#;
        assert_eq!(parse_job(complete).map(|j| j.end), Some(JobEnd::Complete));
        assert_eq!(
            parse_job(r#"{"status":{}}"#).map(|j| j.end),
            Some(JobEnd::Running)
        );
    }

    #[test]
    fn a_refusal_is_a_name_clash_the_quota_or_trouble() {
        assert_eq!(
            refusal("Error from server (AlreadyExists): jobs.batch \"amont-gate-slot-0\" already exists"),
            Refusal::Taken
        );
        assert_eq!(
            refusal("Error from server (Forbidden): ... exceeded quota: gates, requested: limits.memory=4Gi"),
            Refusal::Quota
        );
        assert_eq!(
            refusal("Unable to connect to the server: dial tcp: i/o timeout"),
            Refusal::Other
        );
    }

    #[test]
    fn a_bundle_over_the_cap_says_its_size_in_mib() {
        let f = Fallback::BundleTooBig { mib: 63, cap: 50 };
        assert_eq!(f.to_string(), "bundle-too-big: 63 MiB, over the 50 MiB cap");
    }
}
