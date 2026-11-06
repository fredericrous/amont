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

pub(crate) const IMAGE_HEX: &str =
    "1111111111111111111111111111111111111111111111111111111111111111";

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
    if grep -q '"kind":"Workflow"' "$3"; then
      wf=$(grep -o '"name":"amont-rh-[^"]*"' "$3" | head -1 | sed 's/.*:"//; s/"$//')
      a=$(cat "$D/create_wf_n" 2>/dev/null || echo 0); a=$((a+1)); echo "$a" > "$D/create_wf_n"
      mode=$(sed -n "${a}p" "$D/create_wf" 2>/dev/null)
      case "$mode" in
        reject) echo 'Error from server (Forbidden): workflows.argoproj.io is forbidden: cannot create' >&2; exit 1 ;;
        never) echo 'error: unexpected EOF' >&2; exit 1 ;;
      esac
      cp "$3" "$D/wf-$wf.json"; mkdir -p "$D/created-wf"; cp "$3" "$D/created-wf/$wf.json"
      case "$mode" in
        lost) echo 'error: unexpected EOF' >&2; exit 1 ;;
        hang) touch "$D/create-started"; sleep 30; [ -d "$D" ] || exit 1 ;;
      esac
      printf 'uid-%s' "$wf"; exit 0
    fi
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
    if [ "$mode" = hang ]; then touch "$D/create-started"; sleep 30; [ -d "$D" ] || exit 1; fi
    [ "$mode" = nouid ] || printf 'uid-%s' "$k"; exit 0 ;;
  get)
    touch "$D/get-started"
    if [ -f "$D/get_sleep" ]; then sleep "$(cat "$D/get_sleep")"; [ -d "$D" ] || exit 1; fi
    if [ "$2" = "workflows,pods" ]; then
      if [ -f "$D/wf_down" ]; then echo 'Unable to connect to the server: dial tcp: i/o timeout' >&2; exit 1; fi
      if [ -f "$D/wf_list.json" ]; then cat "$D/wf_list.json"; else printf '{"kind":"List","items":[]}'; fi
      exit 0
    fi
    if [ "$2" = "workflow" ]; then
      if [ -f "$D/wf-$3.json" ]; then
        sed 's/"metadata":{"name"/"metadata":{"uid":"uid-'"$3"'","name"/' "$D/wf-$3.json"; exit 0
      fi
      echo "Error from server (NotFound): workflows.argoproj.io \"$3\" not found" >&2; exit 1
    fi
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
  patch)
    printf '%s\n' "$*" >> "$D/patches"
    [ -f "$D/patch_fail" ] && { echo 'Error from server (Forbidden): workflows.argoproj.io is forbidden: cannot patch' >&2; exit 1; }
    [ -f "$D/wf-$3.json" ] || { echo "Error from server (NotFound): workflows.argoproj.io \"$3\" not found" >&2; exit 1; }
    exit 0 ;;
  logs) echo "output of the gate in the pod"; exit 0 ;;
  delete)
    printf '%s\n' "$*" >> "$D/deletes"
    if [ -f "$D/delete_sleep" ]; then sleep "$(cat "$D/delete_sleep")"; fi
    [ -d "$D" ] || exit 1
    touch "$D/delete-done"
    if [ "$3" = "-l" ]; then
      label=${4#amont.run=}
      for f in "$D"/job-*.json; do
        [ -f "$f" ] && grep -q "\"amont.run\":\"$label\"" "$f" || continue
        k=${f##*job-}; k=${k%.json}; rmdir "$D/slot-$k" 2>/dev/null; rm -f "$D/received-$k"
      done
      exit 0
    fi
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
    "delete_sleep",
    "get_sleep",
    "create_wf",
    "wf_list.json",
    "wf_down",
    "patch_fail",
];

pub(crate) struct Cluster {
    pub(crate) r: Repo,
    /// The fake's directory: its bin, its scripted answers, its records.
    pub(crate) kube: PathBuf,
}

impl Cluster {
    /// A repository whose committed `amont.conf` is `conf`, trusted and
    /// installed, with the machine keys pointing at the fake.
    pub(crate) fn trusted(conf: &str) -> Cluster {
        let c = Cluster::untrusted(conf);
        for verb in ["trust", "init"] {
            let (code, out) = c.amont(&[verb], &[], None);
            assert_eq!(code, 0, "amont {verb}: {out}");
        }
        c
    }

    /// The same repository, its manifest never trusted.
    pub(crate) fn untrusted(conf: &str) -> Cluster {
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

    pub(crate) fn config(&self, key: &str, value: &str) {
        assert!(self.r.git(&["config", key, value]).status.success());
    }

    /// Script one answer of the fake.
    pub(crate) fn answer(&self, file: &str, content: &str) {
        std::fs::write(self.kube.join(file), content).unwrap();
    }

    pub(crate) fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.kube.join(file)).unwrap_or_default()
    }

    /// Forget what the fake recorded, keeping its scripted answers.
    pub(crate) fn reset_records(&self) {
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
    pub(crate) fn created_jobs(&self) -> Vec<String> {
        std::fs::read_dir(self.kube.join("created"))
            .map(|d| {
                d.map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn local_runs(&self) -> usize {
        self.read("gate.log").lines().count()
    }

    pub(crate) fn cluster_log(&self) -> String {
        std::fs::read_to_string(self.r.dir.join(".git/amont/cluster.log")).unwrap_or_default()
    }

    pub(crate) fn kubectl_calls(&self) -> String {
        self.read("calls")
    }

    /// `amont <args>` as a command: the fake first on PATH, the machine's
    /// global git config and `KUBECONFIG` out of reach.
    pub(crate) fn command(&self, args: &[&str], env: &[(&str, &str)]) -> std::process::Command {
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
            .env_remove("KUBECONFIG");
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd
    }

    /// `amont <args>`, and `stdin` piped in if given.
    pub(crate) fn amont(
        &self,
        args: &[&str],
        env: &[(&str, &str)],
        stdin: Option<&str>,
    ) -> (i32, String) {
        let mut cmd = self.command(args, env);
        cmd.stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
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

    pub(crate) fn hook(
        &self,
        name: &str,
        env: &[(&str, &str)],
        stdin: Option<&str>,
    ) -> (i32, String) {
        let hooks = self.r.dir.join(".git/hooks");
        let hooks = hooks.to_string_lossy().into_owned();
        self.amont(&["--hooks-dir", &hooks, name], env, stdin)
    }

    pub(crate) fn pre_commit(&self, env: &[(&str, &str)]) -> (i32, String) {
        self.hook("pre-commit", env, None)
    }

    pub(crate) fn staged_tree(&self) -> String {
        out(&self.r, &["write-tree"])
    }

    pub(crate) fn cluster_note(&self, key: &str) -> String {
        out(
            &self.r,
            &["notes", "--ref", "amont-gate-cluster", "show", key],
        )
    }

    pub(crate) fn local_note(&self, key: &str) -> String {
        out(&self.r, &["notes", "--ref", "amont-gate", "show", key])
    }

    /// The skip tokens of the local note at `key`: its first line, past the
    /// format word. Evidence `run` lines are not tokens.
    pub(crate) fn local_tokens(&self, key: &str) -> String {
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
pub(crate) fn plain(s: &str) -> String {
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

pub(crate) fn out(r: &Repo, args: &[&str]) -> String {
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
    /// The apiserver did not answer.
    Unreachable,
    /// The bundle is over the cap.
    BundleCap,
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
        Said::Unreachable,
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
            "MiB, over the 1 MiB cap",
            Said::BundleCap,
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
        Said::Unreachable => assert!(
            text.contains("the cluster did not answer (")
                && text.contains("lint and this hook's other cluster gates run here"),
            "{what}: {text}"
        ),
        Said::BundleCap => assert!(
            text.contains("lint runs here: its bundle is ")
                && text.contains(
                    "MiB cap — raise it with `git config amont.cluster.maxBundleMiB <MiB>`"
                )
                && !text.contains("unavailable"),
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
    // The fake's call log and Job manifests in the message: this has failed
    // on CI only ("a runs here: amont-gate-slot-0 is not the Job this run
    // created"), and never in 30 local runs; the next failure says why.
    assert_eq!(
        c.local_runs(),
        0,
        "both ran on the cluster: {text}\n--- kubectl calls:\n{}\n--- jobs created:\n{:#?}",
        c.read("calls"),
        c.created_jobs()
    );
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
    // ADR-0018: this test covers the inline path, where the gate runs in the push.
    c.r.git(&["config", "amont.pushGates", "inline"]);
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

/// `git_attest::format::MAX_BLOCKS`: past it the verifier ignores blocks.
const MAX_BLOCKS: usize = 32;

const ATTEST_CONF: &str = "set executor cluster\n\
    pre-push  lint  *.txt  block  platform=any scripts/gate.sh\n\
    pre-push  local  *.txt  block  scripts/a.sh\n";

impl Cluster {
    /// A trusted repository that attests: a bare `origin`, a signing key and
    /// its allowed signers.
    pub(crate) fn attesting(conf: &str) -> Cluster {
        let c = Cluster::trusted(conf);
        let remote = c.remote();
        assert!(c
            .r
            .git(&["init", "-q", "--bare", &remote.display().to_string()])
            .status
            .success());
        c.r.git(&["remote", "add", "origin", &remote.display().to_string()]);
        let key = c.r.path(".git/attest-key");
        let made = std::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "t@example.org", "-f"])
            .arg(&key)
            .status()
            .expect("ssh-keygen")
            .success();
        assert!(made, "ssh-keygen");
        let public = std::fs::read_to_string(c.r.path(".git/attest-key.pub")).unwrap();
        c.r.write(
            ".github/allowed_signers",
            &format!(
                "t@example.org namespaces=\"amont-attest,amont-provenance\" {}\n",
                public.trim()
            ),
        );
        c.config("amont.attest", "true");
        c.config("amont.attestKey", &key.display().to_string());
        c
    }

    pub(crate) fn remote(&self) -> PathBuf {
        self.r.dir.join("remote.git")
    }

    /// The pre-push hook for the ref lines `pushes` (`(ref, tip, base)`), to
    /// `origin`.
    pub(crate) fn pre_push_refs(&self, pushes: &[(&str, &str, &str)]) -> (i32, String) {
        let lines: String = pushes
            .iter()
            .map(|(name, tip, base)| format!("refs/heads/{name} {tip} refs/heads/{name} {base}\n"))
            .collect();
        let hooks = self.r.dir.join(".git/hooks").to_string_lossy().into_owned();
        let url = self.remote().to_string_lossy().into_owned();
        self.amont(
            &["--hooks-dir", &hooks, "pre-push", "origin", &url],
            &[],
            Some(&lines),
        )
    }

    pub(crate) fn pre_push(&self, base: &str, tip: &str) -> (i32, String) {
        self.pre_push_refs(&[("feat/x", tip, base)])
    }

    /// The signed note on `key` in the remote, empty when there is none.
    pub(crate) fn remote_note(&self, key: &str) -> String {
        let shown = std::process::Command::new("git")
            .arg("--git-dir")
            .arg(self.remote())
            .args(["notes", "--ref", "amont-attest", "show", key])
            .output()
            .expect("git notes show");
        String::from_utf8_lossy(&shown.stdout).into_owned()
    }

    /// `(platform, gates)` of each block of the remote's note on `key`.
    pub(crate) fn remote_blocks(&self, key: &str) -> Vec<(String, Vec<String>)> {
        let note = self.remote_note(key);
        note.split("amont-attest-v2\n")
            .skip(1)
            .map(|block| {
                let field = |name: &str| {
                    block
                        .lines()
                        .find_map(|l| l.strip_prefix(name).and_then(|r| r.strip_prefix(' ')))
                        .unwrap_or("")
                        .to_string()
                };
                (
                    field("platform"),
                    field("gates")
                        .split_whitespace()
                        .map(str::to_string)
                        .collect(),
                )
            })
            .collect()
    }

    /// Commit `file` on top of `base` on a branch of its own, and return the
    /// commit and its tree.
    pub(crate) fn commit_on(&self, base: &str, branch: &str, file: &str) -> (String, String) {
        self.r
            .git(&["checkout", "-q", "--no-track", "-b", branch, base]);
        self.r.stage(file, "hello\n");
        self.r.commit(&format!("feat: {file}"));
        (
            out(&self.r, &["rev-parse", "HEAD"]),
            out(&self.r, &["rev-parse", "HEAD^{tree}"]),
        )
    }
}

pub(crate) fn names(blocks: &[(String, Vec<String>)], gate: &str) -> Vec<String> {
    blocks
        .iter()
        .filter(|(_, gates)| gates.iter().any(|g| g == gate))
        .map(|(platform, _)| platform.clone())
        .collect()
}

/// A cluster pass is signed on `x86_64-linux` only; the laptop's own block
/// names the local gate and never the cluster's.
#[test]
fn a_cluster_pass_is_attested_on_linux_and_not_in_the_laptop_block() {
    if common::missing("ssh-keygen") {
        return;
    }
    let c = Cluster::attesting(ATTEST_CONF);
    // ADR-0018: this test covers the inline path, where the gate runs in the push.
    c.r.git(&["config", "amont.pushGates", "inline"]);
    let base = out(&c.r, &["rev-parse", "HEAD"]);
    let (tip, tree) = c.commit_on(&base, "feat/x", "a.txt");
    let (code, text) = c.pre_push(&base, &tip);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("on x86_64-linux"), "{text}");
    for key in [&tree, &tip] {
        let blocks = c.remote_blocks(key);
        assert_eq!(
            names(&blocks, "pre-push-lint"),
            ["x86_64-linux"],
            "{blocks:?}"
        );
        assert_eq!(
            names(&blocks, "pre-push-local"),
            [amont_runtime::attest::platform()],
            "{blocks:?}"
        );
        assert!(
            blocks
                .iter()
                .all(|(_, g)| !(g.contains(&"pre-push-lint".to_string())
                    && g.contains(&"pre-push-local".to_string()))),
            "the laptop block does not name the cluster gate: {blocks:?}"
        );
    }
}

/// The cluster judged only one tip's tree: the other tip's note carries no
/// linux block naming the gate.
#[test]
fn a_linux_block_is_signed_only_for_the_tree_the_cluster_judged() {
    if common::missing("ssh-keygen") {
        return;
    }
    let c = Cluster::attesting(ATTEST_CONF);
    // ADR-0018: this test covers the inline path, where the gate runs in the push.
    c.r.git(&["config", "amont.pushGates", "inline"]);
    let base = out(&c.r, &["rev-parse", "HEAD"]);
    let (judged, judged_tree) = c.commit_on(&base, "feat/a", "a.txt");
    // `*.md` is outside the gate's `*.txt` scope: the cluster is never asked.
    let (unjudged, unjudged_tree) = c.commit_on(&base, "feat/b", "b.md");
    let (code, text) = c.pre_push_refs(&[("feat/a", &judged, &base), ("feat/b", &unjudged, &base)]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(c.created_jobs().len(), 1, "one tree judged: {text}");
    assert_eq!(
        names(&c.remote_blocks(&judged_tree), "pre-push-lint"),
        ["x86_64-linux"]
    );
    let other = c.remote_blocks(&unjudged_tree);
    assert!(
        names(&other, "pre-push-lint").is_empty(),
        "no linux pass for a tree the cluster never judged: {other:?}"
    );
    assert!(!other.is_empty(), "the other tip is attested: {text}");
}

/// The payload is deterministic, so twenty pushes of one tree add no block,
/// and what remains verifies. A push that does change the payload (another
/// gate set, another amont version) now adds two blocks per key, one per
/// platform, so the note reaches `MAX_BLOCKS` in half the pushes it did.
#[test]
fn repeated_pushes_of_one_tree_do_not_grow_its_note() {
    if common::missing("ssh-keygen") {
        return;
    }
    let c = Cluster::attesting(ATTEST_CONF);
    // ADR-0018: this test covers the inline path, where the gate runs in the push.
    c.r.git(&["config", "amont.pushGates", "inline"]);
    let base = out(&c.r, &["rev-parse", "HEAD"]);
    let (tip, tree) = c.commit_on(&base, "feat/x", "a.txt");
    for round in 0..20 {
        let (code, text) = c.pre_push(&base, &tip);
        assert_eq!(code, 0, "round {round}: {text}");
    }
    let blocks = c.remote_blocks(&tree);
    assert!(blocks.len() <= MAX_BLOCKS, "{blocks:?}");
    assert_eq!(blocks.len(), 2, "one block per platform: {blocks:?}");
    let signers = c.r.path(".github/allowed_signers").display().to_string();
    c.r.git(&["fetch", "-q", "origin", "refs/notes/*:refs/notes/*"]);
    let (code, text) = c.amont(
        &[
            "attest",
            "covered",
            "--signers",
            &signers,
            "--platform",
            "any",
        ],
        &[],
        None,
    );
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("pre-push-lint") && text.contains("pre-push-local"),
        "the note still verifies: {text}"
    );
}

/// With no token of its own, a gate only the linux block covers reads
/// `attested` in `amont stamps`; CI's `attest covered` never passes `anywhere`,
/// so a platform that is not linux's finds it uncovered.
#[test]
fn stamps_reads_a_linux_only_attestation_and_attest_covered_does_not() {
    if common::missing("ssh-keygen") {
        return;
    }
    let c = Cluster::attesting(ATTEST_CONF);
    // ADR-0018: this test covers the inline path, where the gate runs in the push.
    c.r.git(&["config", "amont.pushGates", "inline"]);
    let base = out(&c.r, &["rev-parse", "HEAD"]);
    let (tip, _) = c.commit_on(&base, "feat/x", "a.txt");
    let (code, text) = c.pre_push(&base, &tip);
    assert_eq!(code, 0, "{text}");
    for notes in [
        "refs/notes/amont-gate-cluster",
        "refs/notes/amont-gate",
        "refs/notes/amont-gate-inputs",
    ] {
        c.r.git(&["update-ref", "-d", notes]);
    }
    let (code, text) = c.amont(&["stamps", "--json", "lint"], &[], None);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("\"pre-push-lint\":\"attested\""), "{text}");

    let signers = c.r.path(".github/allowed_signers").display().to_string();
    let (code, text) = c.amont(
        &[
            "attest",
            "covered",
            "--signers",
            &signers,
            "--platform",
            "aarch64-darwin",
        ],
        &[],
        None,
    );
    assert_eq!(code, 0, "{text}");
    assert!(!text.contains("pre-push-lint"), "{text}");
}

/// A rehearsal after a cluster pass: the push that follows does not repeat
/// the gate.
#[test]
fn a_push_after_a_rehearsal_does_not_repeat_a_cluster_gate() {
    let c = Cluster::trusted(
        "set executor cluster\npre-push  lint  *.txt  block  platform=any scripts/gate.sh\n",
    );
    let base = out(&c.r, &["rev-parse", "HEAD"]);
    c.r.git(&["update-ref", "refs/remotes/origin/main", &base]);
    let (tip, _) = c.commit_on(&base, "feat/x", "a.txt");
    let (code, text) = c.amont(&["rehearse", "--wait"], &[], None);
    assert_eq!(code, 0, "{text}");
    assert!(c.kubectl_calls().contains(" create "), "{text}");

    c.reset_records();
    let line = format!("refs/heads/feat/x {tip} refs/heads/feat/x {base}\n");
    let (code, text) = c.hook("pre-push", &[], Some(&line));
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("not repeating it here"), "{text}");
    assert!(!c.kubectl_calls().contains(" create "), "{text}");
    assert_eq!(c.local_runs(), 0, "{text}");
}

/// Both twins are `platform=any`; the commit twin ran on the cluster. The
/// push says it is gated at commit, runs nothing, and counts as a cluster
/// pass under its own id.
#[test]
fn a_push_twin_reuses_its_commit_twins_cluster_pass() {
    if common::missing("ssh-keygen") {
        return;
    }
    let c = Cluster::attesting(
        "set executor cluster\n\
         pre-commit  lint  *.txt  block  platform=any scripts/gate.sh\n\
         pre-push  lint  *.txt  block  platform=any scripts/gate.sh\n",
    );
    let base = out(&c.r, &["rev-parse", "HEAD"]);
    c.r.git(&["checkout", "-q", "--no-track", "-b", "feat/x"]);
    c.r.stage("a.txt", "hello\n");
    let (code, text) = c.pre_commit(&[]);
    assert_eq!(code, 0, "{text}");
    assert!(c
        .r
        .git(&["commit", "-q", "--no-verify", "-m", "feat: a"])
        .status
        .success());
    let tip = out(&c.r, &["rev-parse", "HEAD"]);
    let tree = out(&c.r, &["rev-parse", "HEAD^{tree}"]);

    c.reset_records();
    let (code, text) = c.pre_push(&base, &tip);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("gated at commit on the cluster instead — not repeating it here"),
        "{text}"
    );
    assert!(!c.kubectl_calls().contains(" create "), "{text}");
    assert_eq!(c.local_runs(), 0, "{text}");
    assert_eq!(
        names(&c.remote_blocks(&tree), "pre-push-lint"),
        ["x86_64-linux"],
        "{text}"
    );
}

/// A push twin that runs another command than its commit twin claims nothing
/// from the commit twin's cluster pass: the push gate runs.
#[test]
fn a_push_twin_with_another_command_runs_itself() {
    let c = Cluster::trusted(
        "set executor cluster\n\
         pre-commit  lint  *.txt  block  platform=any scripts/gate.sh\n\
         pre-push  lint  *.txt  block  platform=any scripts/gate.sh --full\n",
    );
    // ADR-0018: this test covers the inline path, where the gate runs in the push.
    c.r.git(&["config", "amont.pushGates", "inline"]);
    let base = out(&c.r, &["rev-parse", "HEAD"]);
    c.r.git(&["checkout", "-q", "--no-track", "-b", "feat/x"]);
    c.r.stage("a.txt", "hello\n");
    let (code, text) = c.pre_commit(&[]);
    assert_eq!(code, 0, "{text}");
    assert!(
        c.kubectl_calls().contains(" create "),
        "the commit twin's Job ran: {text}"
    );
    assert!(c
        .r
        .git(&["commit", "-q", "--no-verify", "-m", "feat: a"])
        .status
        .success());
    let tip = out(&c.r, &["rev-parse", "HEAD"]);

    c.reset_records();
    let (code, text) = c.pre_push(&base, &tip);
    assert_eq!(code, 0, "{text}");
    assert!(!text.contains("gated at commit on the cluster"), "{text}");
    assert!(
        c.kubectl_calls().contains(" create "),
        "the push gate ran on the cluster itself: {text}"
    );
    assert_eq!(c.local_runs(), 0, "{text}");
}

/// `kubectl create` succeeded but printed no UID: the Job exists, so it is
/// deleted by name, not left to hold its slot until the deadline.
#[test]
fn a_job_created_without_a_uid_is_deleted_by_name() {
    let c = staged(GATE_AT_COMMIT);
    c.answer("create", "nouid\n");
    let (code, text) = c.pre_commit(&[]);
    assert_eq!(code, 0, "the local pass is the verdict: {text}");
    assert_eq!(c.local_runs(), 1, "{text}");
    assert!(
        c.read("deletes")
            .contains("delete job amont-gate-slot-0 --wait=false"),
        "deleted by name, without waiting: {}",
        c.read("deletes")
    );
    assert!(
        c.cluster_log().contains("printed no Job UID"),
        "{}",
        c.cluster_log()
    );
}

/// The apiserver not answering is said on the terminal once per hook, however
/// many gates it affects; each gate still logs its own line.
#[test]
fn an_unreachable_cluster_is_said_once_per_hook() {
    let c = staged(
        "set executor cluster\n\
         pre-commit  a  *.txt  block  platform=any scripts/a.sh\n\
         pre-commit  b  *.txt  block  platform=any scripts/b.sh\n",
    );
    c.answer("create", "down\ndown\n");
    let (code, text) = c.pre_commit(&[]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(c.local_runs(), 2, "both ran here: {text}");
    assert_eq!(
        text.matches("the cluster did not answer").count(),
        1,
        "said once: {text}"
    );
    assert_eq!(c.cluster_log().lines().count(), 2, "{}", c.cluster_log());
}

/// `kubectl create` outlives its bound after making the Job: the answer is
/// lost, so the run's Jobs are deleted by their label, not left to hold the
/// slot until the deadline.
#[test]
fn a_create_that_times_out_deletes_the_runs_jobs_by_label() {
    let c = staged(GATE_AT_COMMIT);
    c.answer("create", "hang\n");
    let (code, text) = c.pre_commit(&[]);
    assert_eq!(code, 0, "the local pass is the verdict: {text}");
    assert_eq!(c.local_runs(), 1, "{text}");
    assert!(
        c.read("deletes").contains("delete job -l amont.run="),
        "{}",
        c.read("deletes")
    );
    assert!(
        c.read("deletes").contains("--wait=false"),
        "{}",
        c.read("deletes")
    );
    assert!(!c.kube.join("slot-0").exists(), "the slot is free again");
}

#[cfg(unix)]
mod interrupted {
    use super::*;
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::time::{Duration, Instant};

    /// How a signal reaches the hook.
    #[derive(Clone, Copy)]
    enum Reach {
        /// `kill <pid>`: the hook alone.
        Hook,
        /// Ctrl-C: the hook's whole process group, its `kubectl` included.
        Group,
    }

    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let until = Instant::now() + Duration::from_secs(30);
        while !done() {
            assert!(Instant::now() < until, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn send(signal: &str, pid: u32, reach: Reach) {
        let target = match reach {
            Reach::Hook => pid.to_string(),
            Reach::Group => format!("-{pid}"),
        };
        let sent = std::process::Command::new("kill")
            // `--` so a group target, `-<pid>`, is never read as an option.
            .args([format!("-{signal}"), "--".to_string(), target])
            .status()
            .unwrap();
        assert!(sent.success());
    }

    /// A running pre-commit hook whose cluster Job `fetch` never reaches
    /// Running, with its temp files under a directory of ours.
    struct Held {
        child: std::process::Child,
        tmp: PathBuf,
        output: PathBuf,
    }

    impl Held {
        /// Start the hook and wait, bounded, for the fake to write the marker
        /// `ready`: the hook is then inside a `kubectl` call the test scripted
        /// to stay alive (`get_sleep`, or a `create` that hangs).
        fn start(c: &Cluster, ready: &str) -> Held {
            c.config("amont.cluster.startWait", "60");
            c.answer("start", "pending");
            let tmp = c.kube.join("tmp");
            std::fs::create_dir_all(&tmp).unwrap();
            let output = c.kube.join("hook.out");
            let log = std::fs::File::create(&output).unwrap();
            let hooks = c.r.dir.join(".git/hooks").to_string_lossy().into_owned();
            let child = c
                .command(
                    &["--hooks-dir", &hooks, "pre-commit"],
                    &[("TMPDIR", &tmp.to_string_lossy())],
                )
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(log.try_clone().unwrap())
                .stderr(log)
                .spawn()
                .expect("spawn amont");
            let held = Held { child, tmp, output };
            wait_until(ready, || c.kube.join(ready).exists());
            assert!(held.scratch_dirs() > 0, "the scratch directory exists");
            held
        }

        fn scratch_dirs(&self) -> usize {
            std::fs::read_dir(&self.tmp)
                .unwrap()
                .filter(|e| {
                    e.as_ref()
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with("amont-cluster-")
                })
                .count()
        }

        fn output(&self) -> String {
            plain(&std::fs::read_to_string(&self.output).unwrap_or_default())
        }

        /// The hook's end: killed by `signal`, within `within`.
        fn ends_by(&mut self, signal: i32, within: Duration) {
            let until = Instant::now() + within;
            let status = loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    break status;
                }
                if Instant::now() >= until {
                    self.child.kill().unwrap();
                    panic!("the hook outlived the signal: {}", self.output());
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            assert!(!status.success(), "{}", self.output());
            assert_eq!(status.signal(), Some(signal), "{}", self.output());
        }
    }

    /// A signal while a `kubectl get` is alive and the Job is pending. Checks
    /// what is observable: the Job is deleted without waiting, the gate never
    /// runs here, the scratch directory is gone, and the hook dies of the
    /// signal. It does NOT prove the order of the `DYING` store against the
    /// fallback: with Ctrl-C the `get` dies of the same signal, but the
    /// watcher wins that race in practice, and the test stays green with the
    /// store removed.
    fn a_signal_cancels_the_job_and_never_runs_the_gate_here(
        name: &str,
        signal: i32,
        reach: Reach,
    ) {
        let c = staged(GATE_AT_COMMIT);
        c.answer("get_sleep", "8");
        let mut held = Held::start(&c, "get-started");
        send(name, held.child.id(), reach);
        held.ends_by(signal, Duration::from_secs(30));
        let deletes = c.read("deletes");
        assert!(
            deletes.contains("delete job amont-gate-slot-0 --wait=false"),
            "{deletes} / {}",
            held.output()
        );
        assert_eq!(c.local_runs(), 0, "{}", held.output());
        assert_eq!(held.scratch_dirs(), 0, "{}", held.output());
    }

    #[test]
    fn sigterm_cancels_the_cluster_job_and_never_runs_the_gate_here() {
        a_signal_cancels_the_job_and_never_runs_the_gate_here("TERM", 15, Reach::Hook);
    }

    #[test]
    fn ctrl_c_cancels_the_cluster_job_and_never_runs_the_gate_here() {
        a_signal_cancels_the_job_and_never_runs_the_gate_here("INT", 2, Reach::Group);
    }

    /// A signal while `kubectl create` is in flight, after it made the Job:
    /// the hook has no name for the Job yet, so the cancel deletes by the
    /// run's label.
    #[test]
    fn a_signal_during_create_deletes_the_runs_jobs_by_label() {
        let c = staged(GATE_AT_COMMIT);
        c.answer("create", "hang\n");
        let mut held = Held::start(&c, "create-started");
        assert!(c.kube.join("slot-0").exists(), "the Job was made");
        send("TERM", held.child.id(), Reach::Hook);
        held.ends_by(15, Duration::from_secs(30));
        let deletes = c.read("deletes");
        assert!(
            deletes.contains("delete job -l amont.run=") && deletes.contains("--wait=false"),
            "{deletes} / {}",
            held.output()
        );
        assert!(!c.kube.join("slot-0").exists(), "the slot is free again");
        assert_eq!(c.local_runs(), 0, "{}", held.output());
        assert_eq!(held.scratch_dirs(), 0, "{}", held.output());
    }

    /// A second signal while the Job is being cancelled ends amont at once and
    /// leaves the Job: it does not wait for the delete.
    #[test]
    fn a_second_signal_leaves_the_job_and_exits_at_once() {
        let c = staged(GATE_AT_COMMIT);
        c.answer("delete_sleep", "6");
        c.answer("get_sleep", "8");
        let mut held = Held::start(&c, "get-started");
        send("TERM", held.child.id(), Reach::Hook);
        wait_until("the cancel to start", || !c.read("deletes").is_empty());
        send("TERM", held.child.id(), Reach::Hook);
        held.ends_by(15, Duration::from_secs(4));
        assert_eq!(c.local_runs(), 0, "{}", held.output());
        assert!(
            held.output().contains("Ctrl-C again to leave it"),
            "{}",
            held.output()
        );
        // The fake's slow delete outlives the hook: let it finish before the
        // directory it writes to goes away.
        wait_until("the fake delete to finish", || {
            c.kube.join("delete-done").exists()
        });
    }
}
