//! The pending file: what the laptop owes itself about the Workflows it
//! submitted (ADR-0017).
//!
//! `$GIT_COMMON_DIR/amont/cluster-pending` holds one record per submitted
//! Workflow, written BEFORE `kubectl create` so that a crash at any point
//! leaves the intent on disk, and a Workflow nobody recorded cannot exist.
//! Every writer takes the same flock on `cluster-pending.lock`; a reader that
//! does not write (`--status`) reads the file whole, which is safe because a
//! write replaces it by rename.
//!
//! A record carries the FROZEN identity of each gate: the token earned by a
//! pass is the one worked out at submission, never one recomputed from the
//! configuration of the day a pass is collected.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::json_read::Value;

/// The layout of the file: an older or newer amont reads a file it did not
/// write as empty and says so in `cluster.log`.
const FORMAT: i64 = 1;
/// How often a writer polls for the lock.
const LOCK_POLL: Duration = Duration::from_millis(50);

/// Where one gate of a record stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GateState {
    /// Its pod has not ended yet, or has not been looked at.
    Waiting,
    /// Judged a pass; its token is on the tree's cluster note.
    Passed,
    /// Judged not a pass; the gate runs where it always has.
    Failed,
}

impl GateState {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            GateState::Waiting => "waiting",
            GateState::Passed => "passed",
            GateState::Failed => "failed",
        }
    }

    fn parse(s: &str) -> Option<GateState> {
        match s {
            "waiting" => Some(GateState::Waiting),
            "passed" => Some(GateState::Passed),
            "failed" => Some(GateState::Failed),
            _ => None,
        }
    }
}

/// Where a Workflow stands, as the last collect saw it. The cases are the
/// phases Argo reports, plus the one before the cluster has been asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordPhase {
    /// Written before `create`; no Workflow has been seen yet.
    Submitting,
    Pending,
    Running,
    Succeeded,
    Failed,
    Error,
    /// A phase this amont does not know, or none yet.
    Unknown,
    /// The cluster no longer shows the Workflow: its TTL passed, or it was
    /// deleted.
    Gone,
}

impl RecordPhase {
    /// The spelling in the file: Argo's own for its phases.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            RecordPhase::Submitting => "Submitting",
            RecordPhase::Pending => "Pending",
            RecordPhase::Running => "Running",
            RecordPhase::Succeeded => "Succeeded",
            RecordPhase::Failed => "Failed",
            RecordPhase::Error => "Error",
            RecordPhase::Unknown => "Unknown",
            RecordPhase::Gone => "Gone",
        }
    }

    pub(crate) fn parse(s: &str) -> RecordPhase {
        match s {
            "Submitting" => RecordPhase::Submitting,
            "Pending" => RecordPhase::Pending,
            "Running" => RecordPhase::Running,
            "Succeeded" => RecordPhase::Succeeded,
            "Failed" => RecordPhase::Failed,
            "Error" => RecordPhase::Error,
            "Gone" => RecordPhase::Gone,
            _ => RecordPhase::Unknown,
        }
    }

    /// Argo will run nothing more in it.
    pub(crate) fn finished(self) -> bool {
        match self {
            RecordPhase::Succeeded
            | RecordPhase::Failed
            | RecordPhase::Error
            | RecordPhase::Gone => true,
            RecordPhase::Submitting
            | RecordPhase::Pending
            | RecordPhase::Running
            | RecordPhase::Unknown => false,
        }
    }

    /// For a person reading `rehearse --status`.
    pub(crate) fn plain(self) -> &'static str {
        match self {
            RecordPhase::Submitting => "being submitted",
            RecordPhase::Pending => "queued",
            RecordPhase::Running => "running",
            RecordPhase::Succeeded => "finished",
            RecordPhase::Failed => "failed",
            RecordPhase::Error => "ended in error",
            RecordPhase::Unknown => "not reported yet",
            RecordPhase::Gone => "gone (TTL passed or deleted)",
        }
    }
}

/// A failed read or write of the pending file, rendered only where it is
/// logged or said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingError {
    pub(crate) op: &'static str,
    pub(crate) path: PathBuf,
    pub(crate) cause: String,
}

impl PendingError {
    fn new(op: &'static str, path: &Path, cause: impl ToString) -> PendingError {
        PendingError {
            op,
            path: path.to_path_buf(),
            cause: cause.to_string(),
        }
    }
}

impl std::fmt::Display for PendingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}: {}", self.op, self.path.display(), self.cause)
    }
}

/// One gate, as it was submitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Gate {
    pub(super) id: String,
    /// Its DAG task in the Workflow (`g0`, `g1`, …).
    pub(super) task: String,
    pub(super) argv: Vec<String>,
    /// The file scope the gate was selected under, as text. Informational:
    /// the scope decided that the gate was submitted, not what it proves.
    pub(super) scope: String,
    pub(super) prepare: Option<String>,
    /// `sha256:<hex>` of the image every container of the pod must run.
    pub(super) digest: String,
    /// The identity contract version the token was worked out under.
    pub(super) contract: String,
    /// The cluster token a pass is recorded under.
    pub(super) token: String,
    pub(super) state: GateState,
}

/// Where the code came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Source {
    /// `refs/snapshots/<run label>` on the snapshot remote; `None` when the
    /// commit was already on the mirror and nothing was pushed.
    pub(super) snapshot_ref: Option<String>,
    /// The commit `fetch` reads: the snapshot's, or HEAD on the mirror.
    pub(super) oid: String,
}

/// One submitted (or about to be submitted) Workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Pending {
    pub(super) name: String,
    /// The Workflow's UID. `None` while the intent is all there is: written
    /// before `create`, or after a `create` whose answer was lost.
    pub(super) uid: Option<String>,
    /// The `amont.run` label of the Workflow and its pods.
    pub(super) label: String,
    pub(super) namespace: String,
    pub(super) kubeconfig: PathBuf,
    pub(super) tree: String,
    pub(super) source: Source,
    /// Epoch seconds.
    pub(super) submitted: u64,
    /// The Workflow phase last seen, for `--status`.
    pub(super) phase: RecordPhase,
    pub(super) gates: Vec<Gate>,
}

impl Pending {
    pub(super) fn render(&self) -> String {
        use crate::json::{array, int_field, object, opt_string_field, string_array_field};
        use crate::json::{bool_field, string_field};
        let gates: Vec<String> = self
            .gates
            .iter()
            .map(|g| {
                object(&[
                    string_field("id", &g.id),
                    string_field("task", &g.task),
                    string_array_field("argv", &g.argv),
                    string_field("scope", &g.scope),
                    opt_string_field("prepare", g.prepare.as_deref()),
                    string_field("digest", &g.digest),
                    string_field("contract", &g.contract),
                    string_field("token", &g.token),
                    string_field("state", g.state.as_str()),
                ])
            })
            .collect();
        let source = object(&[
            opt_string_field("snapshotRef", self.source.snapshot_ref.as_deref()),
            string_field("oid", &self.source.oid),
            bool_field("pushed", self.source.snapshot_ref.is_some()),
        ]);
        object(&[
            string_field("name", &self.name),
            opt_string_field("uid", self.uid.as_deref()),
            string_field("label", &self.label),
            string_field("namespace", &self.namespace),
            string_field("kubeconfig", &self.kubeconfig.to_string_lossy()),
            string_field("tree", &self.tree),
            format!("\"source\":{source}"),
            int_field(
                "submitted",
                i64::try_from(self.submitted).unwrap_or(i64::MAX),
            ),
            string_field("phase", self.phase.as_str()),
            format!("\"gates\":{}", array(&gates)),
        ])
    }

    fn parse(v: &Value) -> Option<Pending> {
        let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let gates = v
            .get("gates")?
            .items()
            .iter()
            .map(parse_gate)
            .collect::<Option<Vec<Gate>>>()?;
        let source = v.get("source")?;
        let submitted = match v.get("submitted")? {
            Value::Num(n) => n.parse().ok()?,
            _ => return None,
        };
        Some(Pending {
            name: text("name")?,
            uid: text("uid"),
            label: text("label")?,
            namespace: text("namespace")?,
            kubeconfig: PathBuf::from(text("kubeconfig")?),
            tree: text("tree")?,
            source: Source {
                snapshot_ref: source
                    .get("snapshotRef")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                oid: source.get("oid")?.as_str()?.to_string(),
            },
            submitted,
            phase: RecordPhase::parse(&text("phase")?),
            gates,
        })
    }

    /// The gates still waiting for a verdict.
    pub(super) fn waiting(&self) -> impl Iterator<Item = &Gate> {
        self.gates.iter().filter(|g| g.state == GateState::Waiting)
    }
}

fn parse_gate(v: &Value) -> Option<Gate> {
    let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Some(Gate {
        id: text("id")?,
        task: text("task")?,
        argv: v
            .get("argv")?
            .items()
            .iter()
            .map(|a| a.as_str().map(str::to_string))
            .collect::<Option<Vec<String>>>()?,
        scope: text("scope")?,
        prepare: text("prepare"),
        digest: text("digest")?,
        contract: text("contract")?,
        token: text("token")?,
        state: GateState::parse(v.get("state")?.as_str()?)?,
    })
}

fn render_file(records: &[Pending]) -> String {
    let body: Vec<String> = records.iter().map(Pending::render).collect();
    format!(
        "{{\"format\":{FORMAT},\"records\":{}}}\n",
        crate::json::array(&body)
    )
}

fn parse_file(text: &str) -> Result<Vec<Pending>, String> {
    let doc = crate::json_read::parse(text).ok_or("not JSON")?;
    match doc.get("format") {
        Some(Value::Num(n)) if n == &FORMAT.to_string() => {}
        _ => return Err("a layout this amont did not write".into()),
    }
    doc.get("records")
        .ok_or("no records")?
        .items()
        .iter()
        .map(|r| Pending::parse(r).ok_or_else(|| "a record this amont cannot read".to_string()))
        .collect()
}

/// A held lock on the pending file's lock file. Released on drop, and by the
/// kernel if the process dies.
pub(super) struct Guard {
    _file: File,
}

#[cfg(unix)]
extern "C" {
    #[link_name = "flock"]
    fn libc_flock(fd: i32, op: i32) -> i32;
}

#[cfg(unix)]
const LOCK_EX: i32 = 2;
#[cfg(unix)]
const LOCK_NB: i32 = 4;

/// An exclusive lock on `path` without waiting; `None` when another process
/// holds it or it cannot be made.
#[cfg(unix)]
fn try_lock_file(path: &Path) -> Option<File> {
    use std::os::unix::io::AsRawFd;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .ok()?;
    // SAFETY: a valid fd we own; flock touches no memory.
    (unsafe { libc_flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0).then_some(file)
}

/// No `flock` here: no lock is ever held, so no pending file is ever written
/// and the detached lane stays off.
#[cfg(not(unix))]
fn try_lock_file(_path: &Path) -> Option<File> {
    None
}

/// The pending file and its lock.
pub(super) struct Store {
    dir: PathBuf,
}

impl Store {
    /// The store of the repository this process stands in.
    pub(super) fn here() -> Option<Store> {
        let common =
            crate::git::stdout(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
        Some(Store {
            dir: Path::new(&common).join("amont"),
        })
    }

    #[cfg(test)]
    pub(super) fn in_dir(dir: &Path) -> Store {
        Store {
            dir: dir.to_path_buf(),
        }
    }

    fn file(&self) -> PathBuf {
        self.dir.join("cluster-pending")
    }

    /// Is there anything pending? One `stat`.
    pub(super) fn exists(&self) -> bool {
        self.file().exists()
    }

    /// The lock, without waiting.
    pub(super) fn try_lock(&self) -> Option<Guard> {
        std::fs::create_dir_all(&self.dir).ok()?;
        try_lock_file(&self.dir.join("cluster-pending.lock")).map(|_file| Guard { _file })
    }

    /// The lock, waiting at most `wait` for it.
    pub(super) fn lock_within(&self, wait: Duration) -> Option<Guard> {
        let until = Instant::now() + wait;
        loop {
            if let Some(held) = self.try_lock() {
                return Some(held);
            }
            if Instant::now() >= until {
                return None;
            }
            std::thread::sleep(LOCK_POLL);
        }
    }

    /// Every record. A missing file is no record; a file this amont cannot
    /// read is an error, so that nothing overwrites it.
    pub(super) fn read(&self) -> Result<Vec<Pending>, PendingError> {
        let path = self.file();
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                parse_file(&text).map_err(|why| PendingError::new("cannot read", &path, why))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(PendingError::new("cannot read", &path, e)),
        }
    }

    /// Replace the records, durably: a temporary file in the same directory,
    /// synced, renamed over the file, and the directory synced. With no
    /// record left the file is removed. A failure leaves no temporary file.
    pub(super) fn write(&self, _held: &Guard, records: &[Pending]) -> Result<(), PendingError> {
        let path = self.file();
        if records.is_empty() {
            return match std::fs::remove_file(&path) {
                Ok(()) => sync_dir(&self.dir),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(PendingError::new("cannot remove", &path, e)),
            };
        }
        let staging = self
            .dir
            .join(format!("cluster-pending.tmp-{}", std::process::id()));
        let staged = File::create(&staging).and_then(|mut file| {
            file.write_all(render_file(records).as_bytes())?;
            file.sync_all()
        });
        let moved = staged.and_then(|()| std::fs::rename(&staging, &path));
        if let Err(e) = moved {
            // The staging file is ours alone; if it cannot go either, the
            // next write of this process truncates it.
            let removed = std::fs::remove_file(&staging);
            drop(removed);
            return Err(PendingError::new("cannot write", &path, e));
        }
        sync_dir(&self.dir)
    }
}

/// Make a rename or a removal in `dir` durable. Not meaningful on Windows,
/// where a directory cannot be opened for syncing: the rename stands alone.
fn sync_dir(dir: &Path) -> Result<(), PendingError> {
    #[cfg(unix)]
    {
        File::open(dir)
            .and_then(|d| d.sync_all())
            .map_err(|e| PendingError::new("cannot sync", dir, e))
    }
    #[cfg(not(unix))]
    {
        let _ = dir; // no directory sync off unix; the rename stands alone
        Ok(())
    }
}

#[cfg(test)]
pub(in crate::cluster::detached) mod tests {
    use super::*;

    pub(in crate::cluster::detached) fn sample(name: &str) -> Pending {
        Pending {
            name: name.into(),
            uid: Some("uid-1".into()),
            label: "7-00000000000000aa-0".into(),
            namespace: "amont-gates".into(),
            kubeconfig: PathBuf::from("/k \"quoted\".yaml"),
            tree: "t".repeat(40),
            source: Source {
                snapshot_ref: Some("refs/snapshots/7-00000000000000aa-0".into()),
                oid: "c".repeat(40),
            },
            submitted: 1_700_000_000,
            phase: RecordPhase::Running,
            gates: vec![Gate {
                id: "pre-push-lint".into(),
                task: "g0".into(),
                argv: vec!["scripts/gate.sh".into(), "--a b".into()],
                scope: "Scope { .. }".into(),
                prepare: Some("make \"deps\"".into()),
                digest: format!("sha256:{}", "a".repeat(64)),
                contract: "amont-cluster-v1".into(),
                token: "pre-push-lint@abc".into(),
                state: GateState::Waiting,
            }],
        }
    }

    #[test]
    fn a_record_survives_the_round_trip() {
        let mut second = sample("wf-2");
        second.uid = None;
        second.source.snapshot_ref = None;
        second.gates[0].prepare = None;
        second.gates[0].state = GateState::Passed;
        let records = vec![sample("wf-1"), second];
        assert_eq!(parse_file(&render_file(&records)), Ok(records));
    }

    #[test]
    fn a_foreign_file_is_an_error_not_an_empty_list() {
        assert!(parse_file("").is_err());
        assert!(parse_file("{\"format\":2,\"records\":[]}").is_err());
        assert!(parse_file("{\"format\":1,\"records\":[{\"name\":\"x\"}]}").is_err());
    }

    #[test]
    fn writing_replaces_the_file_and_no_record_removes_it() {
        let dir = std::env::temp_dir().join(format!("amont-pending-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let store = Store::in_dir(&dir);
        assert_eq!(store.read(), Ok(Vec::new()));
        let Some(held) = store.try_lock() else {
            // No flock on this platform: the lane is off and has nothing to test.
            return;
        };
        store.write(&held, &[sample("wf-1")]).expect("write");
        assert!(store.exists());
        assert_eq!(store.read(), Ok(vec![sample("wf-1")]));
        store.write(&held, &[]).expect("remove");
        assert!(!store.exists());
        drop(held);
        std::fs::remove_dir_all(&dir).expect("clean");
    }

    #[cfg(unix)]
    #[test]
    fn a_second_writer_does_not_get_the_lock() {
        let dir = std::env::temp_dir().join(format!("amont-pending-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let store = Store::in_dir(&dir);
        let first = store.try_lock().expect("the first writer locks");
        assert!(store.try_lock().is_none(), "busy at once, never waiting");
        assert!(store.lock_within(Duration::from_millis(120)).is_none());
        drop(first);
        assert!(store.try_lock().is_some());
        std::fs::remove_dir_all(&dir).expect("clean");
    }
}
