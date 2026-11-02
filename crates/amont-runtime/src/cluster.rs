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
//! appends `<epoch> <gate> <reason>` to `amont/cluster.log` in the common git
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
//! bundle — and under a deadline, through the hooks' bounded wait.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::json_read::Value;
use crate::manifest::{External, Kind};

/// The node the gate pods run on, and the names the homelab contract fixes.
const NODE: &str = "talos-wk-3-gpu";
const PRIORITY_CLASS: &str = "gate-batch";
const PULL_SECRET: &str = "forgejo-registry-pull";
const JOB_PREFIX: &str = "amont-gate-slot-";
const CACHE_PREFIX: &str = "gate-cache-";
const TEMP_REF_PREFIX: &str = "refs/amont/cluster/";
/// The platform a cluster pass is recorded under.
pub const PLATFORM: &str = "x86_64-linux";
/// The main container's memory, request equal to limit, and CPU limit.
const MEMORY: &str = "4Gi";
const CPU_LIMIT: &str = "2";
/// One short kubectl call: its own request timeout plus room for the exec
/// credential plugin.
const CALL_SECS: u64 = 20;
const POLL: Duration = Duration::from_millis(500);

// ---------------------------------------------------------------------------
// Process-wide state: what was proved here, which Job is live, and whether
// the apiserver already failed once.
// ---------------------------------------------------------------------------

static PROVED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
static UNREACHABLE: AtomicBool = AtomicBool::new(false);
static LIVE: Mutex<Vec<(PathBuf, String, String)>> = Mutex::new(Vec::new());

/// Did a pass of the check `id` come from the cluster in this process — run
/// there, or reused from a cluster token? Such a pass never becomes a local
/// token, stamp or attestation.
pub fn proved(id: &str) -> bool {
    PROVED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains(id)
}

fn mark_proved(id: &str) {
    PROVED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(id.to_string());
}

/// The signal watcher's hook: ask the apiserver to drop every live Job,
/// without waiting for it. Bounded per call; a no-op when none is live.
pub fn cancel_live() {
    let live: Vec<(PathBuf, String, String)> =
        LIVE.lock().unwrap_or_else(|p| p.into_inner()).clone();
    for (kubeconfig, namespace, job) in live {
        let kube = Kube {
            kubeconfig,
            namespace,
        };
        let _ = kube.capture(&["delete", "job", &job, "--wait=false"], 10);
    }
}

// ---------------------------------------------------------------------------
// Configuration.
// ---------------------------------------------------------------------------

/// What the cluster needs from this machine and this repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub kubeconfig: PathBuf,
    pub namespace: String,
    pub slots: u32,
    pub image: String,
    pub digest: String,
    pub max_bundle_bytes: u64,
    pub start_wait: Duration,
    pub timeout_secs: u64,
    pub prepare: Option<String>,
}

/// The machine's `amont.cluster.*` keys, lowercased as git prints them.
/// `--type=path` expands a leading `~`, which is how the kubeconfig is set.
fn machine_keys() -> Vec<(String, String)> {
    crate::git::stdout(&[
        "config",
        "--type=path",
        "--get-regexp",
        r"^amont\.cluster\.",
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
pub fn digest_of(image: &str) -> Option<&str> {
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
        .get("amont.clusterPrepare")
        .cloned()
}

/// Whether this repository's TRUSTED policy says `set executor cluster`, and
/// no local `git config amont.executor local` turned it off. Reading the
/// policy map spawns nothing, so a repository that says nothing pays nothing.
fn executor_is_cluster(settings: &crate::config::Settings) -> bool {
    settings
        .policy()
        .settings
        .get("amont.executor")
        .is_some_and(|v| v == "cluster")
        && crate::config::enumerated_or(settings, "amont.executor", &["cluster", "local"], "local")
            == "cluster"
}

/// The full configuration, or why the gate runs here instead.
fn config(settings: &crate::config::Settings) -> Result<Config, String> {
    let keys = machine_keys();
    let kubeconfig =
        key(&keys, "amont.cluster.kubeconfig").ok_or("amont.cluster.kubeconfig is not set")?;
    let kubeconfig = PathBuf::from(kubeconfig);
    if !kubeconfig.is_file() {
        return Err(format!(
            "kubeconfig {} does not exist",
            kubeconfig.display()
        ));
    }
    let image = key(&keys, "amont.cluster.image").ok_or("amont.cluster.image is not set")?;
    let digest = digest_of(image)
        .ok_or("amont.cluster.image is not pinned by @sha256: digest")?
        .to_string();
    let namespace = key(&keys, "amont.cluster.namespace").unwrap_or("amont-gates");
    if !is_label(namespace) {
        return Err(format!(
            "amont.cluster.namespace {namespace:?} is not a namespace name"
        ));
    }
    let timeout_secs = crate::hooks::common::check_timeout(settings);
    if timeout_secs == 0 {
        return Err("amont.timeout is 0 (unbounded), and a Job needs a deadline".into());
    }
    Ok(Config {
        kubeconfig,
        namespace: namespace.to_string(),
        slots: number(&keys, "amont.cluster.slots", 2, 2) as u32,
        image: image.to_string(),
        digest,
        max_bundle_bytes: number(&keys, "amont.cluster.maxbundlemib", 50, 4096) * 1024 * 1024,
        start_wait: Duration::from_secs(number(&keys, "amont.cluster.startwait", 30, 600)),
        timeout_secs,
        prepare: prepare(settings),
    })
}

fn is_platform_any(ext: &External) -> bool {
    matches!(
        ext.kind,
        Kind::Runnable {
            platform_any: true,
            ..
        }
    )
}

/// The cluster token for `ext` as configured now — the image digest and the
/// prepare command included — or `None` where no cluster token can apply: a
/// gate that is not `platform=any`, or no pinned image on this machine.
/// Never inspects the laptop's executable.
pub fn token(settings: &crate::config::Settings, ext: &External) -> Option<String> {
    if !is_platform_any(ext) {
        return None;
    }
    let keys = machine_keys();
    let digest = digest_of(key(&keys, "amont.cluster.image")?)?;
    crate::gate_identity::cluster_token(ext, &ext.id, prepare(settings).as_deref(), digest)
}

// ---------------------------------------------------------------------------
// Lookups: a cluster token vouches only for a trusted `platform=any` gate.
// ---------------------------------------------------------------------------

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
            crate::git::stdout(&["write-tree"])
        }
        Judged::Commit(c) => crate::git::stdout(&["rev-parse", &format!("{c}^{{tree}}")]),
    }
}

/// pre-commit (and `run --stamp`): is the tree about to be committed already
/// proved on the cluster for this gate, as it is declared and configured
/// now? A hit counts as a cluster pass ([`proved`]).
pub fn vouched(settings: &crate::config::Settings, ext: &External, judged: Judged) -> bool {
    let Some(token) = token(settings, ext) else {
        return false;
    };
    let hit = tree_of(judged)
        .is_some_and(|tree| crate::gate_stamp::cluster_tokens(&tree).contains(&token));
    if hit {
        mark_proved(&ext.id);
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
    let stamps = crate::gate_stamp::cluster_stamps_for(tips);
    let hit = tips
        .iter()
        .all(|t| stamps.get(t).is_some_and(|s| s.contains(&token)));
    if hit {
        mark_proved(&ext.id);
    }
    hit
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

// ---------------------------------------------------------------------------
// The run.
// ---------------------------------------------------------------------------

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
    let outcome = config(settings).and_then(|cfg| run(&cfg, ext, program, args, judged));
    match outcome {
        Ok(()) => {
            mark_proved(&ext.id);
            crate::say!(
                "{} {} passed on the cluster ({PLATFORM})",
                crate::ui::valid_sign(),
                crate::ui::highlight(&ext.short_name),
            );
            true
        }
        Err(reason) => {
            fall_back(&ext.id, &reason);
            false
        }
    }
}

/// Say why the gate runs here, and append it to `amont/cluster.log`.
fn fall_back(gate: &str, reason: &str) {
    let reason: String = reason
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    crate::hooks::common::warn(&format!(
        "{} runs here — the cluster did not prove it: {}",
        crate::ui::highlight(gate),
        crate::ui::sanitize(&reason)
    ));
    let Some(dir) =
        crate::git::stdout(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
    else {
        return;
    };
    let dir = Path::new(&dir).join("amont");
    let _ = std::fs::create_dir_all(&dir); // best-effort: the log is evidence only
    let line = format!("{} {gate} {reason}\n", crate::gate_evidence::now());
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("cluster.log"))
        .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
}

fn run(
    cfg: &Config,
    ext: &External,
    program: &str,
    args: &[String],
    judged: Judged,
) -> Result<(), String> {
    if UNREACHABLE.load(Ordering::SeqCst) {
        return Err("the apiserver failed earlier in this run".into());
    }
    let tree = tree_of(judged).ok_or("git would not name the tree to judge")?;
    let commit = parentless_commit(&tree).ok_or("git commit-tree failed")?;
    let kube = Kube::new(cfg);
    let mut argv = vec![program.to_string()];
    argv.extend(args.iter().cloned());
    let spec = Spec {
        cfg,
        gate: &ext.id,
        tree: &tree,
        repo_dir: &repo_dir(),
        argv: &argv,
    };
    let job = claim_slot(&kube, &spec)?;
    let outcome = run_in(&kube, cfg, &job, &commit, &tree);
    job.delete(&kube);
    let (image_id, uid) = outcome?;
    let token =
        crate::gate_identity::cluster_token(ext, &ext.id, cfg.prepare.as_deref(), &cfg.digest)
            .ok_or("git would not hash the cluster identity")?;
    let provenance = format!("platform={PLATFORM} image={image_id} job={uid}");
    if !crate::gate_stamp::stamp_cluster(&tree, &token, &provenance) {
        crate::hooks::common::warn("git refused to write the cluster stamp");
    }
    Ok(())
}

/// Everything after the Job exists: the bundle in, the logs out, the verdict.
/// Returns the observed imageID and the Job UID on a pass.
fn run_in(
    kube: &Kube,
    cfg: &Config,
    job: &Job,
    commit: &str,
    tree: &str,
) -> Result<(String, String), String> {
    let deadline = Instant::now() + Duration::from_secs(cfg.timeout_secs + 60);
    let bundle = Bundle::create(job.slot, commit)?;
    crate::say!(
        "cluster: {} on {}, bundle {} bytes",
        job.name,
        PLATFORM,
        bundle.bytes
    );
    if bundle.bytes > cfg.max_bundle_bytes {
        return Err(format!(
            "bundle is {} bytes, over amont.cluster.maxBundleMiB",
            bundle.bytes
        ));
    }
    let pod = wait_for_fetch(kube, &job.uid, cfg.start_wait)?;
    kube.send_bundle(&pod, &bundle)?;
    drop(bundle);
    wait_for_gate(kube, &job.uid, deadline)?;
    kube.stream_logs(&pod, deadline);
    let facts = wait_for_job(kube, &job.name, deadline)?;
    let pod = pod_facts(kube, &job.uid)?.ok_or("the pod is gone")?;
    let image_id = judge(&facts, &pod, tree, cfg.prepare.is_some())?;
    Ok((image_id, job.uid.clone()))
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
pub fn repo_dir() -> String {
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
pub fn sanitize_repo_dir(raw: &str) -> String {
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

// ---------------------------------------------------------------------------
// kubectl.
// ---------------------------------------------------------------------------

struct Kube {
    kubeconfig: PathBuf,
    namespace: String,
}

/// How a kubectl call ended.
enum Call {
    Ok(String),
    /// Exit non-zero, with its stderr.
    Refused(String),
    /// Could not start, or outlived its deadline.
    Broken(String),
}

impl Kube {
    fn new(cfg: &Config) -> Kube {
        Kube {
            kubeconfig: cfg.kubeconfig.clone(),
            namespace: cfg.namespace.clone(),
        }
    }

    /// `kubectl --kubeconfig <path> --request-timeout=<t> -n <ns> <args>`,
    /// stdin null, git's environment removed. `streaming` lifts kubectl's own
    /// request timeout for `exec` and `logs -f`, whose ceiling is ours.
    fn command(&self, args: &[&str], streaming: bool) -> Command {
        let mut cmd = Command::new(crate::hooks::common::program("kubectl"));
        cmd.arg("--kubeconfig")
            .arg(&self.kubeconfig)
            .arg(if streaming {
                "--request-timeout=0"
            } else {
                "--request-timeout=10s"
            })
            .args(["-n", &self.namespace])
            .args(args)
            .stdin(Stdio::null());
        crate::hooks::common::strip_git_env(&mut cmd);
        cmd
    }

    fn capture(&self, args: &[&str], secs: u64) -> Call {
        let mut cmd = self.command(args, false);
        capture(&mut cmd, secs)
    }

    /// The bundle, on the one stdin that is not null.
    fn send_bundle(&self, pod: &str, bundle: &Bundle) -> Result<(), String> {
        let file = std::fs::File::open(&bundle.path).map_err(|e| format!("bundle: {e}"))?;
        let bytes = format!("EXPECTED_BYTES={}", bundle.bytes);
        let mut cmd = self.command(
            &[
                "exec",
                "-i",
                "-c",
                "fetch",
                pod,
                "--",
                "env",
                &bytes,
                "fetch-receive",
            ],
            true,
        );
        cmd.stdin(file).stdout(Stdio::null()).stderr(Stdio::null());
        match crate::hooks::common::status_within_secs(&mut cmd, 300) {
            Ok(crate::hooks::common::Ran::Status(s)) if s.success() => Ok(()),
            Ok(crate::hooks::common::Ran::Status(s)) => {
                Err(format!("fetch-receive exited {}", s.code().unwrap_or(-1)))
            }
            Ok(crate::hooks::common::Ran::TimedOut(_)) => Err("fetch-receive timed out".into()),
            Err(e) => Err(format!("kubectl exec: {e}")),
        }
    }

    /// The gate's output, as it runs, under the ceiling clock only: the
    /// silence budget does not apply to a pod. What the logs say decides
    /// nothing.
    fn stream_logs(&self, pod: &str, deadline: Instant) {
        let mut cmd = self.command(&["logs", "-f", pod, "-c", "gate"], true);
        let secs = deadline
            .saturating_duration_since(Instant::now())
            .as_secs()
            .max(1);
        match crate::live::current_sink() {
            // The live display: each chunk into this check's block as it
            // arrives.
            Some((stage, idx)) => {
                cmd.stdout(Stdio::piped()).stderr(Stdio::null());
                let Ok(mut child) = cmd.spawn() else {
                    return;
                };
                let reader = child.stdout.take().map(|mut out| {
                    std::thread::spawn(move || {
                        let mut buf = [0u8; 8192];
                        while let Ok(n) = std::io::Read::read(&mut out, &mut buf) {
                            if n == 0 {
                                break;
                            }
                            stage.append_raw(idx, &buf[..n]);
                        }
                    })
                });
                let _ = crate::hooks::common::wait_within(
                    &mut child,
                    crate::hooks::common::Clocks::ceiling_only(secs),
                    None,
                );
                drop(reader); // detached: it ends with the pipe
            }
            None => {
                let _ = crate::hooks::common::status_within_secs(&mut cmd, secs);
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
        Err(e) => return Call::Broken(format!("kubectl: {e}")),
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
        Ok(crate::hooks::common::Ran::TimedOut(_)) => Call::Broken("kubectl timed out".into()),
        Err(e) => Call::Broken(format!("kubectl: {e}")),
    }
}

/// Read a pipe to its end on a thread, handing the bytes over once.
fn drain(pipe: Option<impl std::io::Read + Send + 'static>) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    if let Some(mut pipe) = pipe {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            let _ = tx.send(buf);
        });
    }
    rx
}

/// A slot refusal (the Job name is taken, or the quota is full) is a reason
/// to try the next slot; anything else is the apiserver failing.
fn is_slot_refusal(stderr: &str) -> bool {
    stderr.contains("AlreadyExists")
        || stderr.contains("already exists")
        || stderr.contains("exceeded quota")
}

// ---------------------------------------------------------------------------
// The Job.
// ---------------------------------------------------------------------------

/// A Job amont created, deleted on every path out.
struct Job {
    slot: u32,
    name: String,
    uid: String,
}

impl Job {
    /// Foreground and waited for, so no old pod still holds the slot's PVC
    /// when the next Job takes it. Bounded.
    fn delete(&self, kube: &Kube) {
        let _ = kube.capture(
            &[
                "delete",
                "job",
                &self.name,
                "--cascade=foreground",
                "--wait",
                "--timeout=60s",
            ],
            75,
        );
        LIVE.lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|(_, _, j)| *j != self.name);
    }
}

/// What the Job spec is built from.
struct Spec<'a> {
    cfg: &'a Config,
    gate: &'a str,
    tree: &'a str,
    repo_dir: &'a str,
    argv: &'a [String],
}

/// Create `amont-gate-slot-<k>` for the first free `k`.
fn claim_slot(kube: &Kube, spec: &Spec) -> Result<Job, String> {
    crate::staged_only::install_signal_handler();
    let mut refusals = Vec::new();
    for slot in 0..spec.cfg.slots {
        let manifest = job_manifest(spec, slot);
        let file = TempFile::write(&format!("job-{slot}.json"), manifest.as_bytes())?;
        let path = file.path.to_string_lossy().into_owned();
        let name = format!("{JOB_PREFIX}{slot}");
        match kube.capture(
            &["create", "-f", &path, "-o", "jsonpath={.metadata.uid}"],
            CALL_SECS,
        ) {
            Call::Ok(uid) if !uid.trim().is_empty() => {
                LIVE.lock().unwrap_or_else(|p| p.into_inner()).push((
                    kube.kubeconfig.clone(),
                    kube.namespace.clone(),
                    name.clone(),
                ));
                return Ok(Job {
                    slot,
                    name,
                    uid: uid.trim().to_string(),
                });
            }
            Call::Refused(err) if is_slot_refusal(&err) => refusals.push(err),
            Call::Ok(_) => return Err("kubectl create printed no Job UID".into()),
            Call::Refused(err) | Call::Broken(err) => {
                UNREACHABLE.store(true, Ordering::SeqCst);
                return Err(format!("kubectl create: {}", last_line(&err)));
            }
        }
    }
    if refusals.iter().all(|e| e.contains("exceeded quota")) {
        Err("the quota refused every slot".into())
    } else {
        Err("all slots taken".into())
    }
}

fn last_line(s: &str) -> String {
    s.lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("no output")
        .to_string()
}

fn s(v: &str) -> String {
    format!("\"{}\"", crate::json::escape(v))
}

fn env_var(name: &str, value: &str) -> String {
    format!("{{\"name\":{},\"value\":{}}}", s(name), s(value))
}

fn mount(name: &str, path: &str, sub: Option<&str>) -> String {
    match sub {
        Some(sub) => format!(
            "{{\"name\":{},\"mountPath\":{},\"subPath\":{}}}",
            s(name),
            s(path),
            s(sub)
        ),
        None => format!("{{\"name\":{},\"mountPath\":{}}}", s(name), s(path)),
    }
}

fn args_json(argv: &[String]) -> String {
    format!(
        "[{}]",
        argv.iter().map(|a| s(a)).collect::<Vec<_>>().join(",")
    )
}

const SECURITY: &str = "{\"allowPrivilegeEscalation\":false,\"capabilities\":{\"drop\":[\"ALL\"]}}";

fn container(name: &str, image: &str, argv: &[String], extra: &str) -> String {
    format!(
        "{{\"name\":{},\"image\":{},\"args\":{},\"securityContext\":{SECURITY},{extra}}}",
        s(name),
        s(image),
        args_json(argv)
    )
}

fn resources(memory: &str, cpu: &str) -> String {
    format!(
        "\"resources\":{{\"requests\":{{\"memory\":{m},\"cpu\":\"500m\"}},\
         \"limits\":{{\"memory\":{m},\"cpu\":{c}}}}}",
        m = s(memory),
        c = s(cpu)
    )
}

/// The Job, as JSON `kubectl create -f` reads. `args`, never `command`: the
/// image's ENTRYPOINT is tini.
fn job_manifest(spec: &Spec, slot: u32) -> String {
    let cfg = spec.cfg;
    let gate_ref = format!("{TEMP_REF_PREFIX}{slot}");
    let fetch = container(
        "fetch",
        &cfg.image,
        &["/usr/local/bin/fetch".to_string()],
        &format!(
            "\"env\":[{},{},{}],\"volumeMounts\":[{},{},{}],{}",
            env_var("EXPECTED_TREE", spec.tree),
            env_var("GATE_REF", &gate_ref),
            env_var("REPO_DIR", spec.repo_dir),
            mount("cache", "/cache", None),
            mount("work", "/work", None),
            mount("checkout", "/checkout", None),
            resources("512Mi", "1"),
        ),
    );
    let work_mounts = format!(
        "\"workingDir\":\"/checkout\",\"volumeMounts\":[{},{}],{}",
        mount("cache", "/cache", Some(spec.repo_dir)),
        mount("checkout", "/checkout", None),
        resources(MEMORY, CPU_LIMIT),
    );
    let mut init = vec![fetch];
    if let Some(prepare) = &cfg.prepare {
        let argv = ["sh".to_string(), "-c".to_string(), prepare.clone()];
        init.push(container("prepare", &cfg.image, &argv, &work_mounts));
    }
    let gate = container("gate", &cfg.image, spec.argv, &work_mounts);
    format!(
        "{{\"apiVersion\":\"batch/v1\",\"kind\":\"Job\",\
         \"metadata\":{{\"name\":{name},\"labels\":{{\"app.kubernetes.io/managed-by\":\"amont\"}},\
         \"annotations\":{{\"amont.dev/gate\":{gate_id},\"amont.dev/tree\":{tree}}}}},\
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
        name = s(&format!("{JOB_PREFIX}{slot}")),
        gate_id = s(spec.gate),
        tree = s(spec.tree),
        deadline = cfg.timeout_secs + 60,
        prio = s(PRIORITY_CLASS),
        node = s(NODE),
        pull = s(PULL_SECRET),
        pvc = s(&format!("{CACHE_PREFIX}{slot}")),
        init = init.join(","),
    )
}

// ---------------------------------------------------------------------------
// Files.
// ---------------------------------------------------------------------------

/// A temporary file, removed on drop.
struct TempFile {
    path: PathBuf,
}

impl TempFile {
    fn path_for(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("amont-cluster-{}-{name}", std::process::id()))
    }

    fn write(name: &str, bytes: &[u8]) -> Result<TempFile, String> {
        let path = Self::path_for(name);
        std::fs::write(&path, bytes).map_err(|e| format!("temp file: {e}"))?;
        Ok(TempFile { path })
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path); // best-effort: a temp file
    }
}

/// The judged commit as a git bundle of one temporary ref.
struct Bundle {
    path: PathBuf,
    bytes: u64,
    _file: TempFile,
}

impl Bundle {
    fn create(slot: u32, commit: &str) -> Result<Bundle, String> {
        let gate_ref = format!("{TEMP_REF_PREFIX}{slot}");
        if !crate::git::succeeds(&["update-ref", &gate_ref, commit]) {
            return Err(format!("git update-ref {gate_ref} failed"));
        }
        let file = TempFile {
            path: TempFile::path_for(&format!("{slot}.bundle")),
        };
        let path = file.path.to_string_lossy().into_owned();
        let made = crate::git::succeeds(&["bundle", "create", "-q", &path, &gate_ref]);
        let _ = crate::git::succeeds(&["update-ref", "-d", &gate_ref]); // the ref was only for the bundle
        if !made {
            return Err("git bundle create failed".into());
        }
        let bytes = std::fs::metadata(&file.path)
            .map_err(|e| format!("bundle: {e}"))?
            .len();
        Ok(Bundle {
            path: file.path.clone(),
            bytes,
            _file: file,
        })
    }
}

// ---------------------------------------------------------------------------
// What Kubernetes recorded.
// ---------------------------------------------------------------------------

/// One container's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Absent,
    Waiting,
    Running,
    Terminated {
        exit: i64,
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
                Some(Value::Num(n)) => n.parse().unwrap_or(-1),
                _ => -1,
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
        matches!(self, State::Running | State::Terminated { .. })
    }

    fn failed(&self) -> Option<String> {
        match self {
            State::Terminated { exit, reason, .. } if *exit != 0 => Some(if reason.is_empty() {
                format!("exited {exit}")
            } else {
                format!("exited {exit} ({reason})")
            }),
            _ => None,
        }
    }
}

/// The pod's recorded facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PodFacts {
    pub name: String,
    pub phase: String,
    pub disrupted: bool,
    pub fetch: State,
    pub prepare: State,
    pub gate: State,
    pub image_id: String,
}

fn named<'a>(list: Option<&'a Value>, name: &str) -> Option<&'a Value> {
    list?
        .items()
        .iter()
        .find(|c| c.get("name").and_then(Value::as_str) == Some(name))
}

/// The first pod of `kubectl get pods -o json`, as facts. Pure.
pub fn parse_pod(json: &str) -> Option<PodFacts> {
    let doc = crate::json_read::parse(json)?;
    let pod = doc.get("items")?.items().first()?;
    let status = pod.get("status");
    let inits = status.and_then(|s| s.get("initContainerStatuses"));
    let mains = status.and_then(|s| s.get("containerStatuses"));
    let gate = named(mains, "gate");
    let disrupted = status
        .and_then(|s| s.get("conditions"))
        .map(|c| {
            c.items().iter().any(|c| {
                c.get("type").and_then(Value::as_str) == Some("DisruptionTarget")
                    && c.get("status").and_then(Value::as_str) == Some("True")
            })
        })
        .unwrap_or(false);
    Some(PodFacts {
        name: pod
            .get("metadata")
            .and_then(|m| m.get("name"))
            .and_then(Value::as_str)?
            .to_string(),
        phase: status
            .and_then(|s| s.get("phase"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        disrupted,
        fetch: State::of(named(inits, "fetch")),
        prepare: State::of(named(inits, "prepare")),
        gate: State::of(gate),
        image_id: gate
            .and_then(|g| g.get("imageID"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

/// The Job's terminal condition, if it has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobEnd {
    Running,
    Complete,
    Failed(String),
}

/// `kubectl get job -o json`, as its terminal condition. Pure.
pub fn parse_job(json: &str) -> Option<JobEnd> {
    let doc = crate::json_read::parse(json)?;
    let conditions = doc.get("status").and_then(|s| s.get("conditions"));
    let truthy = |c: &&Value| c.get("status").and_then(Value::as_str) == Some("True");
    let mut end = JobEnd::Running;
    for c in conditions
        .map(Value::items)
        .unwrap_or(&[])
        .iter()
        .filter(truthy)
    {
        match c.get("type").and_then(Value::as_str) {
            Some("Failed") => {
                let reason = c.get("reason").and_then(Value::as_str).unwrap_or("Failed");
                return Some(JobEnd::Failed(reason.to_string()));
            }
            Some("Complete") => end = JobEnd::Complete,
            _ => {}
        }
    }
    Some(end)
}

/// The verdict, from recorded facts only: the observed imageID on a pass,
/// else why not. Pure.
pub fn judge(
    job: &JobEnd,
    pod: &PodFacts,
    expected_tree: &str,
    has_prepare: bool,
) -> Result<String, String> {
    if pod.disrupted {
        return Err("the pod was disrupted (DisruptionTarget)".into());
    }
    match job {
        JobEnd::Complete => {}
        JobEnd::Failed(reason) => {
            return Err(match pod.gate.failed() {
                Some(why) => format!("the gate {why}"),
                None => format!("the Job failed ({reason})"),
            })
        }
        JobEnd::Running => return Err("the Job has no terminal condition".into()),
    }
    let want = format!("tree {expected_tree}");
    match &pod.fetch {
        State::Terminated {
            exit: 0, message, ..
        } if message.trim() == want => {}
        State::Terminated { message, .. } => {
            return Err(format!("fetch reported {:?}, not {want:?}", message.trim()))
        }
        _ => return Err("fetch did not terminate".into()),
    }
    if has_prepare && !matches!(pod.prepare, State::Terminated { exit: 0, .. }) {
        return Err("prepare did not exit 0".into());
    }
    if !matches!(pod.gate, State::Terminated { exit: 0, .. }) {
        return Err(pod
            .gate
            .failed()
            .map_or("the gate did not exit 0".into(), |w| {
                format!("the gate {w}")
            }));
    }
    if pod.image_id.is_empty() {
        return Err("the pod recorded no imageID".into());
    }
    Ok(pod.image_id.clone())
}

/// The pod of the Job `uid`, by its controller label only — never by name,
/// which an older Job's pod could share.
fn pod_facts(kube: &Kube, uid: &str) -> Result<Option<PodFacts>, String> {
    let selector = format!("controller-uid={uid}");
    match kube.capture(&["get", "pods", "-l", &selector, "-o", "json"], CALL_SECS) {
        Call::Ok(json) => Ok(parse_pod(&json)),
        Call::Refused(e) | Call::Broken(e) => Err(format!("kubectl get pods: {}", last_line(&e))),
    }
}

/// Poll until `done` answers, or `until` passes.
fn poll<T>(
    until: Instant,
    mut done: impl FnMut() -> Result<Option<T>, String>,
) -> Result<Option<T>, String> {
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
fn wait_for_fetch(kube: &Kube, uid: &str, wait: Duration) -> Result<String, String> {
    let found = poll(Instant::now() + wait, || {
        Ok(match pod_facts(kube, uid)? {
            Some(p) if p.fetch == State::Running => Some(Ok(p.name)),
            Some(p) if matches!(p.fetch, State::Terminated { .. }) => {
                Some(Err("fetch ended before the bundle arrived".to_string()))
            }
            _ => None,
        })
    })?;
    match found {
        Some(r) => r,
        None => Err(format!(
            "the pod was not Running within {} s",
            wait.as_secs()
        )),
    }
}

/// Wait until the gate container has started, or an init container failed.
fn wait_for_gate(kube: &Kube, uid: &str, deadline: Instant) -> Result<(), String> {
    let found = poll(deadline, || {
        let Some(p) = pod_facts(kube, uid)? else {
            return Ok(None);
        };
        // An init container that failed is infrastructure, never the gate's
        // verdict: said first, whatever the pod's phase.
        if let Some(w) = p.fetch.failed() {
            return Ok(Some(Err(format!("fetch {w}"))));
        }
        if let Some(w) = p.prepare.failed() {
            return Ok(Some(Err(format!("prepare {w}"))));
        }
        let ended = p.phase == "Failed" || p.phase == "Succeeded";
        Ok((p.gate.started() || ended).then_some(Ok(())))
    })?;
    found.unwrap_or_else(|| Err("the gate did not start before the deadline".into()))
}

/// Wait for the Job's terminal condition: shortly after the logs end, or at
/// the deadline.
fn wait_for_job(kube: &Kube, name: &str, deadline: Instant) -> Result<JobEnd, String> {
    let until = deadline.min(Instant::now() + Duration::from_secs(60));
    let found = poll(until, || {
        match kube.capture(&["get", "job", name, "-o", "json"], CALL_SECS) {
            Call::Ok(json) => Ok(parse_job(&json).filter(|e| *e != JobEnd::Running)),
            Call::Refused(e) | Call::Broken(e) => {
                Err(format!("kubectl get job: {}", last_line(&e)))
            }
        }
    })?;
    found.ok_or_else(|| "the Job reached no terminal condition".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(prepare: Option<&str>) -> Config {
        Config {
            kubeconfig: PathBuf::from("/k"),
            namespace: "amont-gates".into(),
            slots: 2,
            image: format!("reg/gate:1@sha256:{}", "a".repeat(64)),
            digest: format!("sha256:{}", "a".repeat(64)),
            max_bundle_bytes: 1,
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

    #[test]
    fn the_job_carries_the_contract() {
        let c = cfg(Some("make deps"));
        let argv = vec!["scripts/lint.sh".to_string(), "--strict".to_string()];
        let spec = Spec {
            cfg: &c,
            gate: "pre-commit-lint",
            tree: "abc",
            repo_dir: "amont",
            argv: &argv,
        };
        let json = job_manifest(&spec, 1);
        let doc = crate::json_read::parse(&json).expect("valid JSON");
        let spec_ = doc.get("spec").unwrap();
        assert_eq!(spec_.get("backoffLimit"), Some(&Value::Num("0".into())));
        assert_eq!(
            spec_.get("activeDeadlineSeconds"),
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
        ] {
            assert!(json.contains(needle), "{needle} missing from {json}");
        }
    }

    fn pod(fetch: State, gate: State) -> PodFacts {
        PodFacts {
            name: "p".into(),
            phase: "Succeeded".into(),
            disrupted: false,
            fetch,
            prepare: State::Absent,
            gate,
            image_id: "img@sha256:x".into(),
        }
    }

    fn done(exit: i64, reason: &str, message: &str) -> State {
        State::Terminated {
            exit,
            reason: reason.into(),
            message: message.into(),
        }
    }

    #[test]
    fn only_recorded_facts_make_a_pass() {
        let ok = pod(done(0, "", "tree t1\n"), done(0, "", ""));
        assert_eq!(
            judge(&JobEnd::Complete, &ok, "t1", false),
            Ok("img@sha256:x".into())
        );
        let cases: Vec<(&str, JobEnd, PodFacts, bool)> = vec![
            (
                "mismatch",
                JobEnd::Complete,
                pod(done(0, "", "tree t2"), done(0, "", "")),
                false,
            ),
            (
                "gate exit",
                JobEnd::Failed("BackoffLimitExceeded".into()),
                pod(done(0, "", "tree t1"), done(2, "Error", "")),
                false,
            ),
            (
                "oom",
                JobEnd::Failed("BackoffLimitExceeded".into()),
                pod(done(0, "", "tree t1"), done(137, "OOMKilled", "")),
                false,
            ),
            (
                "deadline",
                JobEnd::Failed("DeadlineExceeded".into()),
                pod(done(0, "", "tree t1"), State::Running),
                false,
            ),
            ("no prepare", JobEnd::Complete, ok.clone(), true),
            ("running", JobEnd::Running, ok.clone(), false),
            (
                "no image",
                JobEnd::Complete,
                PodFacts {
                    image_id: String::new(),
                    ..ok.clone()
                },
                false,
            ),
            (
                "disrupted",
                JobEnd::Complete,
                PodFacts {
                    disrupted: true,
                    ..ok.clone()
                },
                false,
            ),
        ];
        for (what, job, p, prep) in cases {
            assert!(judge(&job, &p, "t1", prep).is_err(), "{what} must not pass");
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
        assert_eq!(p.image_id, "i@sha256:1");
        assert_eq!(p.gate, done(137, "OOMKilled", ""));
        assert_eq!(p.prepare, State::Absent);
        let job = r#"{"status":{"conditions":[{"type":"Failed","status":"True","reason":"DeadlineExceeded"}]}}"#;
        assert_eq!(
            parse_job(job),
            Some(JobEnd::Failed("DeadlineExceeded".into()))
        );
        assert_eq!(
            parse_job(r#"{"status":{"conditions":[{"type":"Complete","status":"True"}]}}"#),
            Some(JobEnd::Complete)
        );
        assert_eq!(parse_job(r#"{"status":{}}"#), Some(JobEnd::Running));
    }

    #[test]
    fn a_slot_refusal_is_a_name_clash_or_the_quota() {
        assert!(is_slot_refusal(
            "Error from server (AlreadyExists): jobs.batch \"amont-gate-slot-0\" already exists"
        ));
        assert!(is_slot_refusal(
            "Error from server (Forbidden): ... exceeded quota: gates, requested: limits.memory=4Gi"
        ));
        assert!(!is_slot_refusal(
            "Unable to connect to the server: dial tcp: i/o timeout"
        ));
    }
}
