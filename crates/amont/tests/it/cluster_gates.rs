//! A `platform=any` gate on the cluster (ADR-0016), against a fake `kubectl`.
//!
//! The fake is a shell script on `PATH`, scripted per case through files in
//! its directory (`FAKE_KUBE_DIR`): how `create` answers each attempt, whether
//! `fetch` reaches Running, how the gate container and the Job end. It keys
//! everything by slot, so two gates of one hook each get their own Job, pod
//! and verdict. It records every call's argv, every call's stdin, each Job
//! manifest it was handed and the bundle `exec -i` streamed, so a test can say
//! what amont asked of the cluster without a cluster. No network.

use crate::common;
use common::Repo;

use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;

const IMAGE_HEX: &str = "1111111111111111111111111111111111111111111111111111111111111111";

/// The fake `kubectl`. It refuses a call without `--kubeconfig` (exit 99).
const FAKE_KUBECTL: &str = r#"#!/bin/sh
D="$FAKE_KUBE_DIR"
n=$(cat "$D/n" 2>/dev/null || echo 0); n=$((n+1)); echo "$n" > "$D/n"
printf '%s\n' "$*" >> "$D/calls"
[ "$1" = "--kubeconfig" ] || { echo "no --kubeconfig" >&2; exit 99; }
shift 2; shift; shift 2
val() { cat "$D/$1" 2>/dev/null || printf '%s' "$2"; }
case "$1" in
  exec)
    k=${5#pod-}
    cat > "$D/bundle-$k"; cp "$D/bundle-$k" "$D/bundle"
    printf '%s\n' "$*" > "$D/exec-args"; touch "$D/received-$k"; exit 0 ;;
esac
cat > "$D/stdin-$n"
case "$1" in
  create)
    a=$(cat "$D/create_n" 2>/dev/null || echo 0); a=$((a+1)); echo "$a" > "$D/create_n"
    mode=$(sed -n "${a}p" "$D/create" 2>/dev/null)
    case "$mode" in
      409) echo 'Error from server (AlreadyExists): jobs.batch "x" already exists' >&2; exit 1 ;;
      403) echo 'Error from server (Forbidden): jobs.batch "x" is forbidden: exceeded quota: gates' >&2; exit 1 ;;
      down) echo 'Unable to connect to the server: dial tcp: i/o timeout' >&2; exit 1 ;;
    esac
    k=$(grep -o '"name":"amont-gate-slot-[0-9]*"' "$3" | head -1 | sed 's/.*-//; s/"//')
    mkdir "$D/slot-$k" 2>/dev/null || { echo 'Error from server (AlreadyExists): jobs.batch already exists' >&2; exit 1; }
    cp "$3" "$D/job-$k.json"; cp "$3" "$D/job.json"
    mkdir -p "$D/created"; cp "$3" "$(mktemp "$D/created/job.XXXXXX")"
    printf 'uid-%s' "$k"; exit 0 ;;
  get)
    if [ "$2" = "job" ]; then
      k=${3##*-}
      gate=$(grep -o '"amont.dev/gate":"[^"]*"' "$D/job-$k.json" | sed 's/.*:"//; s/"$//')
      meta=$(printf '"metadata":{"uid":"uid-%s","annotations":{"amont.dev/gate":"%s"}}' "$k" "$gate")
      job=$(val job Complete)
      case "$job" in
        Complete) printf '{%s,"status":{"conditions":[{"type":"Complete","status":"True"}]}}' "$meta" ;;
        *) printf '{%s,"status":{"conditions":[{"type":"Failed","status":"True","reason":"%s"}]}}' "$meta" "${job#Failed:}" ;;
      esac
      exit 0
    fi
    k=${4##*uid-}
    if [ ! -f "$D/received-$k" ]; then
      st=$(val start running)
      if [ "$st" = running ]; then s='{"running":{"startedAt":"t"}}'; else s='{"waiting":{"reason":"ContainerCreating"}}'; fi
      printf '{"items":[{"metadata":{"name":"pod-%s"},"status":{"phase":"Pending","initContainerStatuses":[{"name":"fetch","state":%s}]}}]}' "$k" "$s"
      exit 0
    fi
    tree=$(grep -o 'EXPECTED_TREE","value":"[0-9a-f]*' "$D/job-$k.json" | sed 's/.*"//')
    msg=$(val fetch_msg "tree $tree")
    cond=''
    [ "$(val disrupted 0)" = 1 ] && cond='{"type":"DisruptionTarget","status":"True"}'
    prep=''
    if grep -q '"name":"prepare"' "$D/job-$k.json"; then
      prep=$(printf ',{"name":"prepare","state":{"terminated":{"exitCode":%s}}}' "$(val prepare_exit 0)")
    fi
    if [ "$(val prepare_exit 0)" != 0 ]; then
      g='{"waiting":{"reason":"PodInitializing"}}'
    else
      g=$(printf '{"terminated":{"exitCode":%s,"reason":"%s"}}' "$(val gate_exit 0)" "$(val gate_reason Completed)")
    fi
    printf '{"items":[{"metadata":{"name":"pod-%s"},"status":{"phase":"Succeeded","conditions":[%s],"initContainerStatuses":[{"name":"fetch","state":{"terminated":{"exitCode":0,"message":"%s"}}}%s],"containerStatuses":[{"name":"gate","imageID":"reg/gate@sha256:observed","state":%s}]}}]}' "$k" "$cond" "$msg" "$prep" "$g"
    exit 0 ;;
  logs) echo "output of the gate in the pod"; exit 0 ;;
  delete)
    printf '%s\n' "$*" >> "$D/deletes"
    k=${3##*-}; rmdir "$D/slot-$k" 2>/dev/null; rm -f "$D/received-$k"; exit 0 ;;
esac
echo "fake kubectl: unexpected $*" >&2; exit 98
"#;

/// The local gate: logs each run to `$GATE_LOG`, exits `$LOCAL_EXIT`.
const LOCAL_GATE: &str = "#!/bin/sh\necho ran >> \"$GATE_LOG\"\nexit \"${LOCAL_EXIT:-0}\"\n";

/// The fake's scripted answers, kept by [`Cluster::reset_records`].
const ANSWERS: &[&str] = &[
    "bin",
    "kubeconfig.yaml",
    "gate.log",
    "create",
    "start",
    "gate_exit",
    "gate_reason",
    "disrupted",
    "job",
    "fetch_msg",
    "prepare_exit",
];

struct Cluster {
    r: Repo,
    /// The fake's directory: its bin, its scripted answers, its records.
    kube: PathBuf,
}

impl Cluster {
    /// A repository whose committed `amont.conf` is `conf`, trusted and
    /// installed, with the machine keys pointing at the fake.
    fn trusted(conf: &str) -> Cluster {
        let c = Cluster::untrusted(conf);
        for verb in ["trust", "init"] {
            let (code, out) = c.amont(&[verb], &[], None);
            assert_eq!(code, 0, "amont {verb}: {out}");
        }
        c
    }

    /// The same repository, its manifest never trusted.
    fn untrusted(conf: &str) -> Cluster {
        let r = Repo::new();
        let kube = r.dir.join(".fake-kube");
        std::fs::create_dir_all(kube.join("bin")).unwrap();
        common::fixture_exec(&kube.join("bin/kubectl"), FAKE_KUBECTL);
        std::fs::write(kube.join("kubeconfig.yaml"), "apiVersion: v1\n").unwrap();
        r.stage(".gitignore", ".fake-kube/\n");
        std::fs::create_dir_all(r.dir.join("scripts")).unwrap();
        for script in ["gate.sh", "a.sh", "b.sh"] {
            common::fixture_exec(&r.dir.join("scripts").join(script), LOCAL_GATE);
            r.git(&["add", &format!("scripts/{script}")]);
        }
        r.stage("amont.conf", conf);
        r.commit("chore: base");
        let c = Cluster { r, kube };
        c.config(
            "amont.cluster.kubeconfig",
            &c.kube.join("kubeconfig.yaml").to_string_lossy(),
        );
        c.config(
            "amont.cluster.image",
            &format!("reg/gate:1@sha256:{IMAGE_HEX}"),
        );
        c.config("amont.timeout", "600");
        c.config("amont.cluster.startWait", "1");
        c
    }

    fn config(&self, key: &str, value: &str) {
        assert!(self.r.git(&["config", key, value]).status.success());
    }

    /// Script one answer of the fake.
    fn answer(&self, file: &str, content: &str) {
        std::fs::write(self.kube.join(file), content).unwrap();
    }

    fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.kube.join(file)).unwrap_or_default()
    }

    /// Forget what the fake recorded, keeping its scripted answers.
    fn reset_records(&self) {
        for entry in std::fs::read_dir(&self.kube).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            if ANSWERS.contains(&name.as_str()) {
                continue;
            }
            let path = entry.path();
            let removed = if path.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            removed.expect("a record of the fake is ours to remove");
        }
    }

    /// The Job manifests the fake was handed, in no particular order.
    fn created_jobs(&self) -> Vec<String> {
        std::fs::read_dir(self.kube.join("created"))
            .map(|d| {
                d.map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn local_runs(&self) -> usize {
        self.read("gate.log").lines().count()
    }

    fn cluster_log(&self) -> String {
        std::fs::read_to_string(self.r.dir.join(".git/amont/cluster.log")).unwrap_or_default()
    }

    fn kubectl_calls(&self) -> String {
        self.read("calls")
    }

    /// `amont <args>`, with the fake first on PATH, the machine's global git
    /// config and `KUBECONFIG` out of reach, and `stdin` piped in if given.
    fn amont(&self, args: &[&str], env: &[(&str, &str)], stdin: Option<&str>) -> (i32, String) {
        let mut cmd = common::amont_cmd();
        let path = format!(
            "{}:{}",
            self.kube.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        cmd.args(args)
            .current_dir(&self.r.dir)
            .env("PATH", path)
            .env("FAKE_KUBE_DIR", &self.kube)
            .env("GATE_LOG", self.kube.join("gate.log"))
            .env("GIT_CONFIG_GLOBAL", self.r.dir.join(".fake-kube/gitconfig"))
            .env_remove("KUBECONFIG")
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("spawn amont");
        if let Some(text) = stdin {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(text.as_bytes())
                .unwrap();
        }
        let out = child.wait_with_output().expect("wait");
        (
            out.status.code().unwrap_or(-1),
            plain(&format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )),
        )
    }

    fn hook(&self, name: &str, env: &[(&str, &str)], stdin: Option<&str>) -> (i32, String) {
        let hooks = self.r.dir.join(".git/hooks");
        let hooks = hooks.to_string_lossy().into_owned();
        self.amont(&["--hooks-dir", &hooks, name], env, stdin)
    }

    fn pre_commit(&self, env: &[(&str, &str)]) -> (i32, String) {
        self.hook("pre-commit", env, None)
    }

    fn staged_tree(&self) -> String {
        out(&self.r, &["write-tree"])
    }

    fn cluster_note(&self, key: &str) -> String {
        out(
            &self.r,
            &["notes", "--ref", "amont-gate-cluster", "show", key],
        )
    }

    fn local_note(&self, key: &str) -> String {
        out(&self.r, &["notes", "--ref", "amont-gate", "show", key])
    }

    /// The skip tokens of the local note at `key`: its first line, past the
    /// format word. Evidence `run` lines are not tokens.
    fn local_tokens(&self, key: &str) -> String {
        let note = self.local_note(key);
        let first = note.lines().next().unwrap_or("");
        first
            .strip_prefix("amont-gate-v1")
            .unwrap_or(first)
            .trim()
            .to_string()
    }
}

/// Captured output without colour codes, so a word that `highlight` wraps in
/// them still reads as the word.
fn plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for n in chars.by_ref() {
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn out(r: &Repo, args: &[&str]) -> String {
    String::from_utf8_lossy(&r.git(args).stdout)
        .trim()
        .to_string()
}

const GATE_AT_COMMIT: &str =
    "set executor cluster\npre-commit  lint  *.txt  block  platform=any scripts/gate.sh\n";

/// A trusted repository with the commit-time gate and `a.txt` staged.
fn staged(conf: &str) -> Cluster {
    let c = Cluster::trusted(conf);
    c.r.stage("a.txt", "hello\n");
    c
}

/// A pass on the cluster: the gate does not run here, one line says where it
/// runs, a cluster token lands on the staged tree with its provenance, and
/// nothing lands in amont-gate — not at pre-commit, not when the same tree
/// is judged again (reuse), not at post-commit.
#[test]
fn a_cluster_pass_writes_a_cluster_token_and_no_local_one() {
    let c = staged(GATE_AT_COMMIT);
    let (code, text) = c.pre_commit(&[]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(
        c.local_runs(),
        0,
        "the gate ran on the cluster, not here: {text}"
    );
    assert_eq!(text.matches("running on the cluster").count(), 1, "{text}");
    assert!(
        !text.contains("amont-gate-slot-"),
        "the Job name goes to the record: {text}"
    );
    let tree = c.staged_tree();
    let note = c.cluster_note(&tree);
    assert!(note.starts_with("amont-gate-v1 pre-commit-lint@"), "{note}");
    assert!(
        note.contains("provenance pre-commit-lint@")
            && note.contains("platform=x86_64-linux")
            && note.contains("image=reg/gate@sha256:observed")
            && note.contains("job=uid-0")
            && note.contains("name=amont-gate-slot-0")
            && note.contains("bundle="),
        "{note}"
    );
    assert!(c.cluster_log().is_empty(), "{}", c.cluster_log());
    assert!(c.read("deletes").contains("--cascade=foreground --wait"));

    // The same staged tree again: reused, no Job.
    c.reset_records();
    let (code, text) = c.pre_commit(&[]);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("passed on the cluster on this exact tree earlier"),
        "{text}"
    );
    assert!(
        !c.kubectl_calls().contains("create"),
        "{}",
        c.kubectl_calls()
    );

    assert!(c
        .r
        .git(&["commit", "-q", "--no-verify", "-m", "feat: a"])
        .status
        .success());
    let (code, text) = c.hook("post-commit", &[], None);
    assert_eq!(code, 0, "{text}");
    assert_eq!(c.local_tokens(&tree), "", "no local token on the tree");
    assert_eq!(c.local_tokens("HEAD"), "", "no local token on the commit");
    assert_eq!(c.local_runs(), 0);
}

/// What the terminal says when the cluster does not prove a gate.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Said {
    /// Nothing: this machine has no cluster.
    Nothing,
    /// One warning that names the fix.
    TheFix(&'static str),
    /// The gate failed there and runs here for the verdict.
    FailedThere,
    /// The cluster is unavailable.
    Unavailable,
}

/// One way the cluster does not prove a pass.
struct Mode {
    what: &'static str,
    /// What its cluster.log line says.
    why: &'static str,
    said: Said,
    answers: &'static [(&'static str, &'static str)],
    /// Extra amont.conf lines (a prepare command).
    conf: &'static str,
    /// A git config key to set or unset, beside the default ones.
    config: Option<(&'static str, Option<&'static str>)>,
    /// Stage a file larger than the bundle cap.
    big: bool,
}

const fn mode(
    what: &'static str,
    why: &'static str,
    said: Said,
    answers: &'static [(&'static str, &'static str)],
) -> Mode {
    Mode {
        what,
        why,
        said,
        answers,
        conf: "",
        config: None,
        big: false,
    }
}

const FAILED: (&str, &str) = ("job", "Failed:BackoffLimitExceeded");

const MODES: &[Mode] = &[
    mode(
        "exit 1",
        "failed on the cluster (exit 1)",
        Said::FailedThere,
        &[("gate_exit", "1"), ("gate_reason", "Error"), FAILED],
    ),
    mode(
        "exit 2",
        "failed on the cluster (exit 2)",
        Said::FailedThere,
        &[("gate_exit", "2"), ("gate_reason", "Error"), FAILED],
    ),
    mode(
        "OOMKilled",
        "exit 137, OOMKilled",
        Said::FailedThere,
        &[("gate_exit", "137"), ("gate_reason", "OOMKilled"), FAILED],
    ),
    mode(
        "DisruptionTarget",
        "DisruptionTarget",
        Said::Unavailable,
        &[
            ("gate_exit", "137"),
            ("disrupted", "1"),
            ("job", "Failed:PodFailurePolicy"),
        ],
    ),
    mode(
        "DeadlineExceeded",
        "DeadlineExceeded",
        Said::Unavailable,
        &[("job", "Failed:DeadlineExceeded")],
    ),
    mode(
        "Pending past the start deadline",
        "not Running within 1 s",
        Said::Unavailable,
        &[("start", "pending")],
    ),
    Mode {
        conf: "set clusterPrepare make deps\n",
        ..mode(
            "failed prepare",
            "prepare exit 1",
            Said::Unavailable,
            &[("prepare_exit", "1")],
        )
    },
    mode(
        "tree mismatch",
        "fetch reported",
        Said::Unavailable,
        &[("fetch_msg", "tree 0000000000000000000000000000000000000000")],
    ),
    mode(
        "all slots 409",
        "all slots taken",
        Said::Unavailable,
        &[("create", "409\n409\n")],
    ),
    mode(
        "403 quota",
        "the quota refused every slot",
        Said::Unavailable,
        &[("create", "403\n403\n")],
    ),
    mode(
        "apiserver down",
        "Unable to connect",
        Said::Unavailable,
        &[("create", "down\n")],
    ),
    Mode {
        config: Some(("amont.cluster.kubeconfig", None)),
        ..mode(
            "missing kubeconfig",
            "kubeconfig is not set",
            Said::Nothing,
            &[],
        )
    },
    Mode {
        config: Some(("amont.cluster.image", None)),
        ..mode("missing image", "image is not set", Said::Nothing, &[])
    },
    Mode {
        config: Some(("amont.cluster.image", Some("reg/gate:1"))),
        ..mode(
            "image without @sha256",
            "not pinned",
            Said::TheFix("git config --global amont.cluster.image"),
            &[],
        )
    },
    Mode {
        config: Some(("amont.cluster.kubeconfig", Some("/nonexistent/kubeconfig"))),
        ..mode(
            "kubeconfig file missing",
            "does not exist",
            Said::TheFix("git config --global amont.cluster.kubeconfig"),
            &[],
        )
    },
    Mode {
        config: Some(("amont.cluster.maxBundleMiB", Some("1"))),
        big: true,
        ..mode(
            "bundle over the cap",
            "MiB, over amont.cluster.maxBundleMiB (1)",
            Said::Unavailable,
            &[],
        )
    },
];

/// Incompressible bytes, so a bundle cannot shrink them under the cap.
fn noise(len: usize) -> String {
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            char::from(b'!' + (x % 90) as u8)
        })
        .collect()
}

fn assert_said(mode: &Mode, text: &str) {
    let what = mode.what;
    match mode.said {
        Said::Nothing => assert!(
            !text.contains("runs here") && !text.contains("cluster"),
            "{what}: a machine with no cluster says nothing: {text}"
        ),
        Said::TheFix(fix) => {
            assert!(text.contains(fix), "{what}: names the fix: {text}");
            assert!(
                text.contains("git config amont.executor local"),
                "{what}: {text}"
            );
        }
        Said::FailedThere => assert!(
            text.contains("lint failed on the cluster (")
                && text.contains("running it here for the verdict"),
            "{what}: {text}"
        ),
        Said::Unavailable => assert!(
            text.contains("lint runs here: the cluster is unavailable ("),
            "{what}: {text}"
        ),
    }
}

/// Every way the cluster does not prove a pass re-runs the gate here,
/// unchanged; the local outcome is the verdict; one cluster.log line says
/// why; no cluster token is written; the terminal says it the way the case
/// deserves.
#[test]
fn every_non_pass_reruns_the_gate_locally() {
    for mode in MODES {
        let c = staged(&format!("{}{GATE_AT_COMMIT}", mode.conf));
        for (file, content) in mode.answers {
            c.answer(file, content);
        }
        match mode.config {
            Some((key, Some(v))) => c.config(key, v),
            Some((key, None)) => {
                assert!(c.r.git(&["config", "--unset", key]).status.success());
            }
            None => {}
        }
        if mode.big {
            c.r.stage("big.txt", &noise(2 * 1024 * 1024 + 512 * 1024));
        }
        let (code, text) = c.pre_commit(&[]);
        let what = mode.what;
        assert_eq!(code, 0, "{what}: the local pass is the verdict: {text}");
        assert_eq!(c.local_runs(), 1, "{what}: ran here once: {text}");
        let log = c.cluster_log();
        assert_eq!(log.lines().count(), 1, "{what}: {log}");
        let fields: Vec<&str> = log.splitn(3, ' ').collect();
        assert!(
            fields[0].parse::<u64>().is_ok(),
            "{what}: epoch first: {log}"
        );
        assert_eq!(fields[1], "pre-commit-lint", "{what}: {log}");
        assert!(fields[2].contains(mode.why), "{what}: {log}");
        assert_eq!(c.cluster_note(&c.staged_tree()), "", "{what}");
        assert_said(mode, &text);
        if c.read("job.json").contains("amont-gate-slot-") {
            assert!(
                c.read("deletes").contains("delete job"),
                "{what}: Job removed"
            );
        }
    }
}

/// The local outcome is the verdict in the other direction too: the cluster
/// failed, the local re-run fails, and the commit is blocked.
#[test]
fn a_local_failure_after_a_cluster_failure_blocks() {
    let c = staged(GATE_AT_COMMIT);
    c.answer("gate_exit", "1");
    c.answer("job", "Failed:BackoffLimitExceeded");
    let (code, text) = c.pre_commit(&[("LOCAL_EXIT", "1")]);
    assert_ne!(code, 0, "{text}");
    assert_eq!(c.local_runs(), 1);
    assert_eq!(c.cluster_log().lines().count(), 1);
}

/// A pass git would not record is no pass: the gate runs here.
#[test]
fn a_cluster_pass_with_no_record_falls_back() {
    let c = staged(GATE_AT_COMMIT);
    let lock = c.r.dir.join(".git/refs/notes/amont-gate-cluster.lock");
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    std::fs::write(&lock, "").unwrap();
    let (code, text) = c.pre_commit(&[]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(c.local_runs(), 1, "{text}");
    assert!(
        c.cluster_log()
            .contains("git refused to write the cluster stamp"),
        "{}",
        c.cluster_log()
    );
}

/// Two `platform=any` gates of one commit run concurrently, each in its own
/// Job with its own command, and each earns its own token.
#[test]
fn concurrent_gates_each_get_their_own_job() {
    let c = staged(
        "set executor cluster\n\
         pre-commit  a  *.txt  block  platform=any scripts/a.sh\n\
         pre-commit  b  *.txt  block  platform=any scripts/b.sh\n",
    );
    let (code, text) = c.pre_commit(&[]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(c.local_runs(), 0, "both ran on the cluster: {text}");
    let jobs = c.created_jobs();
    assert_eq!(jobs.len(), 2, "{jobs:?}");
    for (gate, script) in [
        ("pre-commit-a", "scripts/a.sh"),
        ("pre-commit-b", "scripts/b.sh"),
    ] {
        let job = jobs
            .iter()
            .find(|j| j.contains(&format!("\"amont.dev/gate\":\"{gate}\"")))
            .unwrap_or_else(|| panic!("no Job for {gate}: {jobs:?}"));
        assert!(
            job.contains(&format!("\"args\":[\"{script}\"]")),
            "{gate} runs {script}: {job}"
        );
    }
    let note = c.cluster_note(&c.staged_tree());
    let tokens = note.lines().next().unwrap_or("");
    assert!(
        tokens.contains("pre-commit-a@") && tokens.contains("pre-commit-b@"),
        "{note}"
    );
}

/// The Job amont hands `kubectl create` carries the homelab contract, and
/// the bundle goes in through `exec -i` with its exact size.
#[test]
fn the_job_spec_carries_every_required_field() {
    let c = staged(&format!("set clusterPrepare make deps\n{GATE_AT_COMMIT}"));
    let (code, text) = c.pre_commit(&[]);
    assert_eq!(code, 0, "{text}");
    let job = c.read("job.json");
    for needle in [
        "\"backoffLimit\":0",
        "\"restartPolicy\":\"Never\"",
        "\"activeDeadlineSeconds\":660",
        "\"ttlSecondsAfterFinished\":300",
        "\"nodeSelector\":{\"kubernetes.io/hostname\":\"talos-wk-3-gpu\"}",
        "\"priorityClassName\":\"gate-batch\"",
        "\"automountServiceAccountToken\":false",
        "\"imagePullSecrets\":[{\"name\":\"forgejo-registry-pull\"}]",
        "\"args\":[\"scripts/gate.sh\"]",
        "\"args\":[\"sh\",\"-c\",\"make deps\"]",
        "\"args\":[\"/usr/local/bin/fetch\"]",
        "\"limits\":{\"memory\":\"4Gi\",\"cpu\":\"2\"}",
        "\"name\":\"amont-gate-slot-0\"",
        "\"claimName\":\"gate-cache-0\"",
    ] {
        assert!(job.contains(needle), "{needle} missing: {job}");
    }
    assert!(!job.contains("\"command\""), "args, never command: {job}");
    let repo_dir = c.r.dir.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        job.contains(&format!(
            "{{\"name\":\"REPO_DIR\",\"value\":\"{repo_dir}\"}}"
        )),
        "{job}"
    );
    assert!(
        job.contains(&format!(
            "\"mountPath\":\"/cache\",\"subPath\":\"{repo_dir}\""
        )),
        "{job}"
    );
    assert!(
        job.contains(&format!("\"value\":\"{}\"", c.staged_tree())),
        "{job}"
    );
    let bytes = std::fs::metadata(c.kube.join("bundle")).unwrap().len();
    assert!(bytes > 0);
    assert_eq!(
        c.read("exec-args").trim(),
        format!("exec -i -c fetch pod-0 -- env EXPECTED_BYTES={bytes} fetch-receive")
    );
    let calls = c.kubectl_calls();
    assert!(
        calls.lines().all(|l| l.starts_with("--kubeconfig ")),
        "every call names the kubeconfig: {calls}"
    );
    assert!(
        calls.contains("get pods -l controller-uid=uid-0"),
        "{calls}"
    );
}

/// A gate without `platform=any` never goes to the cluster, whatever the
/// executor says.
#[test]
fn a_gate_without_the_marker_never_goes_remote() {
    let c = staged("set executor cluster\npre-commit  lint  *.txt  block  scripts/gate.sh\n");
    let (code, text) = c.pre_commit(&[]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(c.local_runs(), 1);
    assert_eq!(c.kubectl_calls(), "", "kubectl was never called");
    assert_eq!(c.cluster_log(), "");
}

/// Without `set executor cluster`, a marked gate runs here and says nothing
/// about the cluster.
#[test]
fn the_executor_is_local_unless_the_repository_says_cluster() {
    let c = staged("pre-commit  lint  *.txt  block  platform=any scripts/gate.sh\n");
    let (code, text) = c.pre_commit(&[]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(c.local_runs(), 1);
    assert_eq!(c.kubectl_calls(), "");
    assert_eq!(c.cluster_log(), "");
}

/// An untrusted manifest's marker and settings are ignored: nothing reaches
/// kubectl.
#[test]
fn an_untrusted_marker_is_ignored() {
    let c = Cluster::untrusted(GATE_AT_COMMIT);
    c.r.stage("a.txt", "hello\n");
    let (_code, text) = c.pre_commit(&[]);
    assert_eq!(c.kubectl_calls(), "", "kubectl was never called: {text}");
    assert_eq!(c.cluster_log(), "");
}

/// `platform=any` with `fix` or with `files` is a line that cannot run, and
/// says which conflict.
#[test]
fn platform_any_with_fix_or_files_fails_to_parse() {
    for (line, says) in [
        (
            "pre-commit  lint  *.txt  block  platform=any fix scripts/gate.sh\n",
            "`platform=any` cannot be combined with `fix`",
        ),
        (
            "pre-commit  lint  *.txt  block  files platform=any scripts/gate.sh\n",
            "`platform=any` cannot be combined with `files`",
        ),
    ] {
        let c = staged(line);
        let (_code, text) = c.pre_commit(&[]);
        assert!(text.contains(says), "{line}: {text}");
        assert_eq!(c.local_runs(), 0, "{line}");
        assert_eq!(c.kubectl_calls(), "", "{line}");
    }
}

/// At pre-push the cluster judges the pushed tip's tree, every kubectl call
/// but the bundle's has an empty stdin, and the hook's own ref lines reach
/// the push gate intact.
#[test]
fn pre_push_stdin_is_intact_after_a_cluster_gate() {
    let c = Cluster::trusted(
        "set executor cluster\npre-push  lint  *.txt  block  platform=any scripts/gate.sh\n",
    );
    let base = out(&c.r, &["rev-parse", "HEAD"]);
    c.r.stage("a.txt", "hello\n");
    c.r.commit("feat: a");
    let tip = out(&c.r, &["rev-parse", "HEAD"]);
    let line = format!("refs/heads/feat/x {tip} refs/heads/feat/x {base}\n");
    let (code, text) = c.hook("pre-push", &[], Some(&line));
    assert_eq!(code, 0, "{text}");
    assert_eq!(c.local_runs(), 0, "{text}");
    let tree = out(&c.r, &["rev-parse", "HEAD^{tree}"]);
    assert!(c
        .read("job.json")
        .contains(&format!("\"value\":\"{tree}\"")));
    let kubectl_calls: usize = c.read("n").trim().parse().unwrap();
    for i in 1..=kubectl_calls {
        let path = c.kube.join(format!("stdin-{i}"));
        if path.exists() {
            assert_eq!(std::fs::read(&path).unwrap(), b"", "call {i} saw stdin");
        }
    }
    let bundle = std::fs::read(c.kube.join("bundle")).unwrap();
    assert!(
        bundle.starts_with(b"# v2 git bundle"),
        "the bundle was the exec's stdin"
    );
    assert!(c.cluster_note(&tree).contains("pre-push-lint@"));
    assert_eq!(
        c.local_tokens(&tip),
        "",
        "no local push stamp for a cluster pass"
    );
    assert_eq!(c.local_tokens(&tree), "", "{}", c.local_note(&tree));

    // The same push again: the cluster token vouches for the tip.
    c.reset_records();
    let (code, text) = c.hook("pre-push", &[], Some(&line));
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("passed on this exact tree earlier"), "{text}");
    assert!(!c.kubectl_calls().contains("create"));
}

/// A cluster token is reused with the gate's program absent from this
/// machine, and a changed `amont.cluster.image` invalidates it.
#[test]
fn a_changed_image_invalidates_the_cluster_token() {
    let c = staged(
        "set executor cluster\npre-commit  lint  *.txt  block  platform=any gate-only-in-the-image\n",
    );
    let (code, text) = c.pre_commit(&[]);
    assert_eq!(code, 0, "{text}");
    assert!(c.kubectl_calls().contains(" create "));

    c.reset_records();
    let (code, text) = c.pre_commit(&[]);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("on this exact tree earlier"),
        "reused: {text}"
    );
    assert_eq!(c.kubectl_calls(), "", "no kubectl call for a reuse");

    c.reset_records();
    let other = "2".repeat(64);
    c.config("amont.cluster.image", &format!("reg/gate:2@sha256:{other}"));
    let (code, text) = c.pre_commit(&[]);
    assert_eq!(code, 0, "{text}");
    assert!(
        c.kubectl_calls().contains(" create "),
        "a new image runs the gate again: {text}"
    );
    let note = c.cluster_note(&c.staged_tree());
    assert_eq!(
        note.lines().next().unwrap().split(' ').count(),
        3,
        "two tokens: {note}"
    );
}

/// `amont stamps` reads a cluster token as `stamped` for a trusted
/// `platform=any` gate, and as `absent` once the manifest is not trusted.
#[test]
fn stamps_reads_a_cluster_token_only_from_a_trusted_manifest() {
    let c = staged(GATE_AT_COMMIT);
    let (code, text) = c.pre_commit(&[]);
    assert_eq!(code, 0, "{text}");
    assert!(c
        .r
        .git(&["commit", "-q", "--no-verify", "-m", "feat: a"])
        .status
        .success());
    let (code, text) = c.amont(&["stamps", "--json", "lint"], &[], None);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("\"pre-commit-lint\":\"stamped\""), "{text}");

    assert!(c
        .r
        .git(&["config", "--unset-all", "amont.trusted"])
        .status
        .success());
    let (code, text) = c.amont(&["stamps", "--json", "lint"], &[], None);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("\"pre-commit-lint\":\"absent\""), "{text}");
}
