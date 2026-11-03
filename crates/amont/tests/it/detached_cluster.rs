//! A rehearsal's gates on a detached Argo Workflow (ADR-0017), against the
//! fake `kubectl` of the cluster gates, which here also answers for Workflows:
//! `create -f` of a Workflow (scripted per attempt in `create_wf`: `reject`,
//! `never`, `lost`, `hang`), `get workflow <name>`, `patch workflow`, and the
//! one batched `get workflows,pods -l …`, answered from `wf_list.json`, which
//! each test writes from the Workflow the fake was handed and the pod of the
//! first live run (`fixtures/argo/pod-succeeded.json`), edited per case.
//!
//! The fake Forgejo is two local bare repositories under the fake's
//! directory: the snapshot remote and the mirror. No network.

use crate::cluster_gates::{names, out, Cluster, IMAGE_HEX};
use crate::common;

use std::path::PathBuf;
use std::process::Command;

/// The pod of the first live run, compact so a replacement does not depend on
/// its indentation.
const POD: &str = include_str!("fixtures/argo/pod-succeeded.json");
/// The digest that run's image was pinned to.
const LIVE_DIGEST: &str = "fa7a661059493955d9590559a3ff62bc956fedd83f41eea739e73d1996797f7c";
const LIVE_TREE: &str = "13b5ee2d54389399fa2738f084a1b653b5844028";
const LIVE_COMMIT: &str = "7e04624939f8c93f774d570276af8643c61f0320";
/// `main`'s termination in that pod: exit 0, 04:40:34.
const MAIN_DONE: &str =
    "\"exitCode\":0,\"finishedAt\":\"2026-11-03T04:40:34Z\",\"reason\":\"Completed\"";

/// Two gates the cluster may take, and one it may not.
const MIXED: &str = "set executor cluster\n\
    pre-push  lint   *.txt  block  platform=any scripts/gate.sh\n\
    pre-push  types  *.txt  block  platform=any scripts/a.sh\n\
    pre-push  local  *.txt  block  scripts/b.sh\n";
/// Only gates the cluster may take: a rehearsal of it spawns no worker.
const ANY: &str = "set executor cluster\n\
    pre-push  lint   *.txt  block  platform=any scripts/gate.sh\n\
    pre-push  types  *.txt  block  platform=any scripts/a.sh\n";
const ONE: &str = "set executor cluster\n\
    pre-push  lint   *.txt  block  platform=any scripts/gate.sh\n";

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

/// A Workflow the fake was handed.
struct Wf {
    name: String,
    label: String,
    json: String,
}

impl Wf {
    fn parse(json: String) -> Wf {
        let after = |marker: &str| -> String {
            let at = json.find(marker).expect(marker) + marker.len();
            json[at..].split('"').next().unwrap().to_string()
        };
        Wf {
            name: after("\"metadata\":{\"name\":\""),
            label: after("\"labels\":{\"app.kubernetes.io/managed-by\":\"amont\",\"amont.run\":\""),
            json,
        }
    }

    fn uid(&self) -> String {
        format!("uid-{}", self.name)
    }

    fn tree(&self) -> String {
        self.env("EXPECTED_TREE")
    }

    fn env(&self, name: &str) -> String {
        let marker = format!("\"name\":\"{name}\",\"value\":\"");
        let at = self.json.find(&marker).expect(&marker) + marker.len();
        self.json[at..].split('"').next().unwrap().to_string()
    }

    /// The inner text of the `command` array of the template of `task`.
    fn command(&self, task: &str) -> String {
        let at = self
            .json
            .find(&format!("\"name\":\"gate-{task}\""))
            .expect("template");
        let rest = &self.json[at..];
        let from = rest.find("\"command\":[").expect("command") + "\"command\":[".len();
        rest[from..].split(']').next().unwrap().to_string()
    }

    /// The Workflow as the cluster reports it: this manifest with a UID and a
    /// status of `phase`, one Pod node per `(task, phase)`.
    fn object(&self, phase: &str, nodes: &[(&str, &str)]) -> String {
        let with_uid = self.json.replacen(
            "\"metadata\":{\"name\"",
            &format!("\"metadata\":{{\"uid\":\"{}\",\"name\"", self.uid()),
            1,
        );
        let nodes: Vec<String> = nodes
            .iter()
            .map(|(task, p)| {
                format!(
                    "\"node-{task}\":{{\"id\":\"node-{task}\",\"name\":\"{}.{task}\",\"type\":\"Pod\",\
                     \"templateName\":\"gate-{task}\",\"phase\":\"{p}\"}}",
                    self.name
                )
            })
            .collect();
        let body = &with_uid[..with_uid.len() - 1];
        format!(
            "{body},\"status\":{{\"phase\":\"{phase}\",\"nodes\":{{{}}}}}}}",
            nodes.join(",")
        )
    }

    /// The pod of `task`: the first live run's, made the pod of this Workflow's
    /// gate, then `edit`ed.
    fn pod(&self, task: &str, edit: impl Fn(String) -> String) -> String {
        let pod = compact(POD)
            .replace(
                "\"workflows.argoproj.io/node-name\":\"amont-handrun-pqtbl\"",
                &format!(
                    "\"workflows.argoproj.io/node-name\":\"{}.{task}\"",
                    self.name
                ),
            )
            .replace(
                "\"workflows.argoproj.io/node-id\":\"amont-handrun-pqtbl\"",
                &format!("\"workflows.argoproj.io/node-id\":\"node-{task}\""),
            )
            .replace(
                "\"name\":\"amont-handrun-pqtbl\",\"uid\":\"b8eb52b7-8ede-4c1f-bd94-9ada529eec97\"",
                &format!("\"name\":\"{}\",\"uid\":\"{}\"", self.name, self.uid()),
            )
            .replace("amont-handrun-pqtbl", &format!("{}-{task}", self.name))
            .replace("handrun-2", &self.label)
            .replace(LIVE_TREE, &self.tree())
            .replace(LIVE_COMMIT, &self.env("GATE_REF"))
            .replace(LIVE_DIGEST, IMAGE_HEX)
            .replace(
                "\"python3\",\"scripts/check-module-cycles.py\"",
                &self.command(task),
            );
        edit(pod)
    }
}

fn list(items: &[String]) -> String {
    format!("{{\"kind\":\"List\",\"items\":[{}]}}", items.join(","))
}

/// `main` exits `code` for `reason`.
fn main_exits(code: i32, reason: &str) -> impl Fn(String) -> String + '_ {
    move |pod| {
        pod.replacen(
            MAIN_DONE,
            &format!(
                "\"exitCode\":{code},\"finishedAt\":\"2026-11-03T04:40:34Z\",\"reason\":\"{reason}\""
            ),
            1,
        )
    }
}

/// A pod with a `prepare` init container that exited `code`; with a non-zero
/// code `main` never started.
fn with_prepare(command: &'static str, code: i32) -> impl Fn(String) -> String {
    move |pod| {
        let pod = pod
            .replacen(
                "\"initContainers\":[",
                &format!(
                    "\"initContainers\":[{{\"args\":[\"sh\",\"-c\",\"{command}\"],\"image\":\"reg/gate:1@sha256:{IMAGE_HEX}\",\"name\":\"prepare\"}},"
                ),
                1,
            )
            .replacen(
                "\"initContainerStatuses\":[",
                &format!(
                    "\"initContainerStatuses\":[{{\"name\":\"prepare\",\"state\":{{\"terminated\":{{\"exitCode\":{code}}}}}}},"
                ),
                1,
            );
        if code == 0 {
            pod
        } else {
            pod.replace(
                "\"terminated\":{\"containerID\":\"containerd://72683744e4b32662fcdee44e7d5fd846899a8b994145dfc21a07ee4d385f3989\"",
                "\"waiting\":{\"reason\":\"PodInitializing\"},\"was\":{\"containerID\":\"containerd://72683744e4b32662fcdee44e7d5fd846899a8b994145dfc21a07ee4d385f3989\"",
            )
        }
    }
}

/// The fake cluster, the fake Forgejo and a repository that attests, with
/// `feat/x` one commit ahead of `origin/main`.
struct Lane {
    cluster: Cluster,
    /// The commit `feat/x` branched from.
    base: String,
    tip: String,
    tree: String,
}

impl Lane {
    fn new(conf: &str) -> Option<Lane> {
        if common::missing("ssh-keygen") {
            // A skip on a laptop; on CI a silent skip would hide the whole
            // module, so CI fails instead.
            assert!(
                std::env::var_os("CI").is_none(),
                "ssh-keygen is missing on CI: the detached tests would be skipped silently"
            );
            return None;
        }
        let cluster = Cluster::attesting(conf);
        let forgejo = cluster.kube.join("forgejo");
        for repo in ["remote.git", "remote-mirror.git"] {
            let path = forgejo.join(repo);
            std::fs::create_dir_all(&path).unwrap();
            assert!(cluster
                .r
                .git(&["init", "-q", "--bare", &path.display().to_string()])
                .status
                .success());
        }
        let base = out(&cluster.r, &["rev-parse", "HEAD"]);
        cluster
            .r
            .git(&["update-ref", "refs/remotes/origin/main", &base]);
        let (tip, tree) = cluster.commit_on(&base, "feat/x", "a.txt");
        let lane = Lane {
            cluster,
            base,
            tip,
            tree,
        };
        lane.configure();
        Some(lane)
    }

    fn forgejo(&self) -> PathBuf {
        self.cluster.kube.join("forgejo")
    }

    fn configure(&self) {
        let forgejo = self.forgejo().display().to_string();
        let fetch = "http://forgejo-http.forgejo.svc.cluster.local:3000/amont-snapshots";
        self.cluster.config(
            "amont.cluster.snapshotRemote",
            &format!("{forgejo}/{{repo}}.git"),
        );
        self.cluster.config(
            "amont.cluster.snapshotFetch",
            &format!("{fetch}/{{repo}}.git"),
        );
        self.cluster.config(
            "amont.cluster.mirrorRemote",
            &format!("{forgejo}/{{repo}}-mirror.git"),
        );
        self.cluster.config(
            "amont.cluster.mirror",
            &format!("{fetch}/{{repo}}-mirror.git"),
        );
    }

    fn rehearse(&self, args: &[&str]) -> (i32, String) {
        let mut all = vec!["rehearse"];
        all.extend_from_slice(args);
        self.cluster.amont(&all, &[], None)
    }

    fn pending(&self) -> String {
        std::fs::read_to_string(self.cluster.r.dir.join(".git/amont/cluster-pending"))
            .unwrap_or_default()
    }

    fn workflows(&self) -> Vec<Wf> {
        let mut found: Vec<Wf> = std::fs::read_dir(self.cluster.kube.join("created-wf"))
            .map(|d| {
                d.map(|e| Wf::parse(std::fs::read_to_string(e.unwrap().path()).unwrap()))
                    .collect()
            })
            .unwrap_or_default();
        found.sort_by(|a, b| a.name.cmp(&b.name));
        found
    }

    fn workflow(&self) -> Wf {
        let mut all = self.workflows();
        assert_eq!(all.len(), 1, "one Workflow was created");
        all.remove(0)
    }

    /// Submit with `rehearse --cluster` and return the Workflow created.
    fn submitted(&self) -> Wf {
        let (code, text) = self.rehearse(&["--cluster"]);
        assert_eq!(code, 0, "{text}");
        assert!(text.contains("submitted amont-rh-"), "{text}");
        self.workflow()
    }

    /// What the cluster answers to the next batched call.
    fn answer(&self, items: &[String]) {
        self.cluster.answer("wf_list.json", &list(items));
    }

    fn tokens(&self) -> String {
        self.cluster
            .cluster_note(&self.tree)
            .lines()
            .next()
            .unwrap_or_default()
            .to_string()
    }

    fn collect(&self) -> (i32, String) {
        self.cluster.amont(&["cluster", "collect"], &[], None)
    }

    /// How many Workflows `kubectl create` was asked for (the Job lane's
    /// creates are not counted).
    fn workflow_creates(&self) -> usize {
        self.cluster.read("create_wf_n").trim().parse().unwrap_or(0)
    }

    fn calls(&self, verb: &str) -> usize {
        self.cluster
            .kubectl_calls()
            .lines()
            .filter(|l| l.contains(&format!(" {verb} ")))
            .count()
    }

    /// Forget the calls the fake counted, and nothing else.
    fn forget_calls(&self) {
        for file in ["calls", "n"] {
            let _ = std::fs::remove_file(self.cluster.kube.join(file)); // absent is the point: the fake may never have been called
        }
    }

    fn push(&self) -> (i32, String) {
        self.cluster.pre_push(&self.base, &self.tip)
    }
}

/// The snapshot goes in, is read back, one Workflow names every `platform=any`
/// gate and not the local one, a pending line is written, and the command
/// returns before any pod runs.
#[test]
fn rehearse_cluster_pushes_the_snapshot_reads_it_back_and_creates_one_workflow() {
    let Some(l) = Lane::new(MIXED) else { return };
    let started = std::time::Instant::now();
    let (code, text) = l.rehearse(&["--wait", "--cluster"]);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("submitted amont-rh-") && text.contains("pre-push-lint pre-push-types"),
        "one line, with the gates: {text}"
    );
    assert!(text.contains("the next commit or push collects"), "{text}");
    assert_eq!(l.workflow_creates(), 1, "{}", l.cluster.kubectl_calls());
    let wf = l.workflow();
    assert!(wf.name.len() <= 63, "{}", wf.name);
    assert!(wf
        .json
        .contains("\"podPriorityClassName\":\"gate-detached\""));
    // the snapshot: on the remote, under the run label, and what the pod reads
    let snapshot_ref = format!("refs/snapshots/{}", wf.label);
    let on_remote = out(
        &l.cluster.r,
        &[
            "--git-dir",
            &l.forgejo().join("remote.git").display().to_string(),
            "rev-parse",
            &snapshot_ref,
        ],
    );
    assert_eq!(
        on_remote,
        wf.env("GATE_REF"),
        "the pod fetches the snapshot"
    );
    assert_eq!(wf.env("EXPECTED_TREE"), l.tree);
    assert!(
        wf.env("GATE_URL").ends_with("/amont-snapshots/remote.git"),
        "{}",
        wf.env("GATE_URL")
    );
    // the pending line, written before any pod could run
    let pending = l.pending();
    assert!(
        pending.contains(&wf.name)
            && pending.contains("pre-push-lint")
            && pending.contains("pre-push-types"),
        "{pending}"
    );
    assert!(!pending.contains("pre-push-local"), "{pending}");
    // only the local gate ran here, and no Job: the others went to the cluster
    assert_eq!(l.cluster.local_runs(), 1, "{text}");
    assert!(
        l.cluster.created_jobs().is_empty(),
        "no Job for a detached gate"
    );
    assert!(started.elapsed().as_secs() < 60);
}

/// Without `--wait` and with only cluster gates, the command returns after the
/// submission and no worker is left behind.
#[test]
fn rehearse_cluster_returns_once_the_workflow_is_created() {
    let Some(l) = Lane::new(ANY) else { return };
    let wf = l.submitted();
    assert_eq!(l.workflow_creates(), 1);
    assert_eq!(
        l.calls("get"),
        0,
        "no pod was looked at: {}",
        l.cluster.kubectl_calls()
    );
    assert_eq!(l.cluster.local_runs(), 0);
    assert!(l.pending().contains(&wf.name));
    // nothing but the Workflow's own run label identifies it
    assert_eq!(wf.name, format!("amont-rh-{}", wf.label));
}

/// The snapshot commit has HEAD as its parent and HEAD's tree, and is dated
/// now: the prune ages a ref by its committer date.
#[test]
fn the_snapshot_commit_has_head_as_parent_and_a_fresh_committer_date() {
    let Some(l) = Lane::new(ONE) else { return };
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let tip = l.tip.clone();
    let wf = l.submitted();
    let git_dir = l.forgejo().join("remote.git").display().to_string();
    let show = |fmt: &str| {
        out(
            &l.cluster.r,
            &[
                "--git-dir",
                &git_dir,
                "log",
                "-1",
                &format!("--format={fmt}"),
                &format!("refs/snapshots/{}", wf.label),
            ],
        )
    };
    assert_eq!(show("%P"), tip, "HEAD is the parent");
    assert_eq!(show("%T"), l.tree);
    let committed: u64 = show("%ct").parse().unwrap();
    assert!(
        committed >= before && committed <= before + 120,
        "{committed} vs {before}"
    );
}

/// A push accepted but not read back (Forgejo has lost accepted pushes)
/// submits nothing; the rehearsal runs as today.
#[test]
fn a_snapshot_that_is_not_read_back_submits_nothing() {
    let Some(l) = Lane::new(ONE) else { return };
    let hook = l.forgejo().join("remote.git/hooks/post-receive");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    common::fixture_exec(
        &hook,
        "#!/bin/sh\nwhile read old new ref; do git update-ref -d \"$ref\"; done\n",
    );
    let (code, text) = l.rehearse(&["--wait", "--cluster"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("not read back"), "{text}");
    assert_eq!(
        l.workflow_creates(),
        0,
        "no Workflow: {}",
        l.cluster.kubectl_calls()
    );
    assert!(l.workflows().is_empty());
    assert_eq!(
        l.pending(),
        "",
        "no record for a run that was not submitted"
    );
    assert!(
        l.cluster.cluster_log().contains("pre-push-lint detached:"),
        "{}",
        l.cluster.cluster_log()
    );
    // the gate ran as it does without the lane: on the Job lane
    assert_eq!(l.cluster.created_jobs().len(), 1, "{text}");
}

/// HEAD is already on the mirror: nothing is pushed, and the pod fetches from
/// the mirror at HEAD.
#[test]
fn a_commit_already_on_the_mirror_pushes_nothing_and_fetches_from_the_mirror() {
    let Some(l) = Lane::new(ONE) else { return };
    let mirror = l.forgejo().join("remote-mirror.git").display().to_string();
    let pushed = l
        .cluster
        .r
        .git(&["push", "-q", "--no-verify", &mirror, "HEAD:refs/heads/main"]);
    assert!(pushed.status.success());
    let wf = l.submitted();
    assert!(
        wf.env("GATE_URL")
            .ends_with("/amont-snapshots/remote-mirror.git"),
        "{}",
        wf.env("GATE_URL")
    );
    assert_eq!(wf.env("GATE_REF"), l.tip);
    let snapshots = out(
        &l.cluster.r,
        &[
            "--git-dir",
            &l.forgejo().join("remote.git").display().to_string(),
            "for-each-ref",
            "refs/snapshots",
        ],
    );
    assert_eq!(snapshots, "", "nothing was pushed");
    assert!(!l.pending().contains("refs/snapshots"), "{}", l.pending());
}

/// A mirror that does not advertise HEAD is not used: it is a snapshot.
#[test]
fn a_mirror_without_the_commit_is_not_used() {
    let Some(l) = Lane::new(ONE) else { return };
    let mirror = l.forgejo().join("remote-mirror.git").display().to_string();
    l.cluster.r.git(&[
        "push",
        "-q",
        "--no-verify",
        &mirror,
        &format!("{}:refs/heads/main", l.base),
    ]);
    let wf = l.submitted();
    assert!(
        wf.env("GATE_URL").ends_with("/remote.git"),
        "{}",
        wf.env("GATE_URL")
    );
    assert_ne!(wf.env("GATE_REF"), l.tip, "a snapshot commit, not HEAD");
}

/// The pass is collected: the token is the Job lane's, byte for byte (carried
/// item 8), and a later push skips the gate and signs the linux block.
#[test]
fn collect_writes_the_job_lanes_token_and_the_push_skips_and_signs() {
    let Some(l) = Lane::new(ONE) else { return };
    // the Job lane first: the token it earns for this tree and image
    let (code, text) = l.rehearse(&["--wait"]);
    assert_eq!(code, 0, "{text}");
    let job_token = l.tokens();
    assert!(job_token.contains("pre-push-lint@"), "{job_token}");
    assert!(l
        .cluster
        .r
        .git(&["update-ref", "-d", "refs/notes/amont-gate-cluster"])
        .status
        .success());
    assert_eq!(l.tokens(), "");

    let wf = l.submitted();
    l.answer(&[
        wf.object("Succeeded", &[("g0", "Succeeded")]),
        wf.pod("g0", |p| p),
    ]);
    let (code, text) = l.collect();
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("collected the cluster pass of pre-push-lint"),
        "{text}"
    );
    assert_eq!(
        l.tokens(),
        job_token,
        "the Workflow lane writes the Job lane's token"
    );
    let note = l.cluster.cluster_note(&l.tree);
    assert!(
        note.contains("platform=x86_64-linux") && note.contains(&wf.uid()),
        "{note}"
    );
    assert_eq!(l.pending(), "", "a finished Workflow leaves the file");

    // the push: skipped on the cluster token, signed on x86_64-linux
    l.cluster.reset_records();
    let (code, text) = l.push();
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("not repeating it here"), "{text}");
    assert_eq!(l.cluster.local_runs(), 0, "{text}");
    assert!(!l.cluster.kubectl_calls().contains(" create "), "{text}");
    assert_eq!(
        names(&l.cluster.remote_blocks(&l.tree), "pre-push-lint"),
        ["x86_64-linux"],
        "{text}"
    );
}

/// Collecting twice writes one token.
#[test]
fn collecting_twice_writes_one_token() {
    let Some(l) = Lane::new(ONE) else { return };
    let wf = l.submitted();
    l.answer(&[
        wf.object("Succeeded", &[("g0", "Succeeded")]),
        wf.pod("g0", |p| p),
    ]);
    let (_, first) = l.collect();
    assert!(first.contains("collected 1 cluster pass"), "{first}");
    let note = l.cluster.cluster_note(&l.tree);
    let (_, second) = l.collect();
    assert!(second.contains("collected 0 cluster passes"), "{second}");
    assert_eq!(l.cluster.cluster_note(&l.tree), note);
    assert_eq!(
        note.matches("provenance pre-push-lint@").count(),
        1,
        "{note}"
    );
    assert_eq!(
        note.lines()
            .next()
            .unwrap()
            .matches("pre-push-lint@")
            .count(),
        1,
        "{note}"
    );
}

/// Every way a gate is not a pass writes no token and one `cluster.log` line.
#[test]
fn every_non_pass_collects_nothing_and_logs_one_line() {
    type Edit = Box<dyn Fn(String) -> String>;
    // (what is wrong, the Workflow's phase, the node's phase, the pod edit)
    let cases: Vec<(&str, &str, &str, Option<Edit>)> = vec![
        (
            "exit 1",
            "Failed",
            "Failed",
            Some(Box::new(main_exits(1, "Error"))),
        ),
        (
            "OOMKilled",
            "Failed",
            "Failed",
            Some(Box::new(main_exits(137, "OOMKilled"))),
        ),
        (
            "Workflow Error",
            "Error",
            "Error",
            Some(Box::new(main_exits(1, "Error"))),
        ),
        (
            "node Succeeded, main exit 1",
            "Succeeded",
            "Succeeded",
            Some(Box::new(main_exits(1, "Error"))),
        ),
        ("no pod", "Failed", "Failed", None),
    ];
    for (what, phase, node, edit) in cases {
        let Some(l) = Lane::new(ONE) else { return };
        let wf = l.submitted();
        let mut items = vec![wf.object(phase, &[("g0", node)])];
        if let Some(edit) = edit {
            items.push(wf.pod("g0", edit));
        }
        l.answer(&items);
        let (code, text) = l.collect();
        assert_eq!(code, 0, "{what}: {text}");
        assert_eq!(l.tokens(), "", "{what}: no token");
        assert_eq!(
            l.cluster.cluster_log().lines().count(),
            1,
            "{what}: {}",
            l.cluster.cluster_log()
        );
        assert!(
            l.cluster.cluster_log().contains("pre-push-lint detached"),
            "{what}: {}",
            l.cluster.cluster_log()
        );
        assert_eq!(
            l.pending(),
            "",
            "{what}: the finished Workflow leaves the file"
        );
        // and the push runs the gate: nothing vouches for it
        l.cluster.reset_records();
        let (code, text) = l.push();
        assert_eq!(code, 0, "{what}: {text}");
        assert!(!text.contains("not repeating it here"), "{what}: {text}");
        assert_eq!(
            l.cluster.created_jobs().len(),
            1,
            "{what}: the gate ran: {text}"
        );
    }
}

/// A tree mismatch reported by `fetch`, and a failed `prepare`.
#[test]
fn a_tree_mismatch_or_a_failed_prepare_is_no_pass() {
    // a tree mismatch
    let Some(l) = Lane::new(ONE) else { return };
    let wf = l.submitted();
    let tree = wf.tree();
    l.answer(&[
        wf.object("Succeeded", &[("g0", "Succeeded")]),
        wf.pod("g0", |p| {
            p.replace(
                &format!("\"message\":\"tree {tree}\\n\""),
                &format!("\"message\":\"tree {}\\n\"", "0".repeat(40)),
            )
        }),
    ]);
    l.collect();
    assert_eq!(l.tokens(), "");
    assert_eq!(
        l.cluster.cluster_log().lines().count(),
        1,
        "{}",
        l.cluster.cluster_log()
    );
    assert!(
        l.cluster.cluster_log().contains("fetch reported"),
        "{}",
        l.cluster.cluster_log()
    );

    // a failed prepare
    let conf = format!("{ONE}set clusterPrepare make deps\n");
    let Some(l) = Lane::new(&conf) else { return };
    let wf = l.submitted();
    assert!(wf.json.contains("\"name\":\"prepare\""), "{}", wf.json);
    l.answer(&[
        wf.object("Failed", &[("g0", "Failed")]),
        wf.pod("g0", with_prepare("make deps", 2)),
    ]);
    l.collect();
    assert_eq!(l.tokens(), "");
    assert_eq!(
        l.cluster.cluster_log().lines().count(),
        1,
        "{}",
        l.cluster.cluster_log()
    );
    assert!(
        l.cluster.cluster_log().contains("prepare"),
        "{}",
        l.cluster.cluster_log()
    );
}

/// With `clusterPrepare`, a pass needs `prepare` to have exited 0.
#[test]
fn a_pass_with_a_prepare_step_needs_prepare_to_have_passed() {
    let conf = format!("{ONE}set clusterPrepare make deps\n");
    let Some(l) = Lane::new(&conf) else { return };
    let wf = l.submitted();
    l.answer(&[
        wf.object("Succeeded", &[("g0", "Succeeded")]),
        wf.pod("g0", with_prepare("make deps", 0)),
    ]);
    l.collect();
    assert!(l.tokens().contains("pre-push-lint@"), "{}", l.tokens());
}

/// Pods gone after the TTL: the Workflow is no longer there, the record goes,
/// one line.
#[test]
fn a_workflow_gone_after_its_ttl_leaves_the_file_with_one_line() {
    let Some(l) = Lane::new(ONE) else { return };
    let wf = l.submitted();
    // adopt it first (a first collect sees it running), then it vanishes
    l.answer(&[wf.object("Running", &[("g0", "Running")])]);
    l.collect();
    assert!(l.pending().contains(&wf.uid()), "{}", l.pending());
    l.answer(&[]);
    l.collect();
    assert_eq!(l.pending(), "");
    assert_eq!(l.tokens(), "");
    assert_eq!(
        l.cluster.cluster_log().lines().count(),
        1,
        "{}",
        l.cluster.cluster_log()
    );
}

/// A running Workflow stays pending, and `--status` shows its phase.
#[test]
fn status_shows_the_phase_and_stop_terminates_the_workflow() {
    let Some(l) = Lane::new(ONE) else { return };
    let wf = l.submitted();
    l.answer(&[wf.object("Running", &[("g0", "Running")])]);
    let (code, text) = l.rehearse(&["--status"]);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains(&format!("cluster run {}: running", wf.name))
            && text.contains("waiting: pre-push-lint"),
        "{text}"
    );
    let (code, text) = l.rehearse(&["--stop"]);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains(&format!("terminating cluster run {}", wf.name)),
        "{text}"
    );
    let patches = l.cluster.read("patches");
    assert!(
        patches.contains(&format!("patch workflow {} --type merge", wf.name))
            && patches.contains("\"shutdown\":\"Terminate\""),
        "{patches}"
    );
}

/// With nothing pending, a hook, `stamps` and `cluster collect` ask the
/// cluster nothing.
#[test]
fn collect_with_nothing_pending_spawns_no_kubectl() {
    let Some(l) = Lane::new(ANY) else { return };
    let (code, text) = l.collect();
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("collected 0 cluster passes; 0 Workflows still pending"),
        "{text}"
    );
    let (code, text) = l.cluster.amont(&["stamps", "--json"], &[], None);
    assert_eq!(code, 0, "{text}");
    let (code, text) = l.cluster.pre_commit(&[]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(l.cluster.kubectl_calls(), "", "kubectl was never started");
}

/// Three pending records, one question.
#[test]
fn collect_makes_one_batched_call_for_three_pending_records() {
    let Some(l) = Lane::new(ONE) else { return };
    let mut made = Vec::new();
    for file in ["b.txt", "c.txt", "d.txt"] {
        let wf = l.submitted_again(file);
        made.push(wf);
    }
    l.forget_calls();
    l.answer(&[]);
    let (code, text) = l.collect();
    assert_eq!(code, 0, "{text}");
    let batched: Vec<String> = l
        .cluster
        .kubectl_calls()
        .lines()
        .filter(|c| c.contains("workflows,pods"))
        .map(str::to_string)
        .collect();
    assert_eq!(batched.len(), 1, "{}", l.cluster.kubectl_calls());
    assert_eq!(
        l.cluster.kubectl_calls().lines().count(),
        1,
        "no per-Workflow call"
    );
    assert!(
        batched[0].contains("--request-timeout=1s"),
        "{}",
        batched[0]
    );
    assert!(batched[0].contains("amont.run in ("), "{}", batched[0]);
    for wf in &made {
        assert!(
            batched[0].contains(&wf.label),
            "{} in {}",
            wf.label,
            batched[0]
        );
    }
}

impl Lane {
    /// Commit `file` on top and submit that tree too, keeping the earlier
    /// Workflows pending.
    fn submitted_again(&self, file: &str) -> Wf {
        let known: Vec<String> = self.workflows().into_iter().map(|w| w.name).collect();
        self.cluster.r.stage(file, "hello\n");
        self.cluster.r.commit(&format!("feat: {file}"));
        let (code, text) = self.rehearse(&["--cluster"]);
        assert_eq!(code, 0, "{text}");
        self.workflows()
            .into_iter()
            .find(|w| !known.contains(&w.name))
            .expect("a new Workflow")
    }
}

/// A lock somebody else holds: collect returns at once and asks nothing.
#[test]
fn a_busy_lock_returns_at_once_without_asking_the_cluster() {
    let Some(l) = Lane::new(ONE) else { return };
    let wf = l.submitted();
    l.answer(&[
        wf.object("Succeeded", &[("g0", "Succeeded")]),
        wf.pod("g0", |p| p),
    ]);
    l.forget_calls();
    let lock = l.cluster.r.dir.join(".git/amont/cluster-pending.lock");
    let mut holder = Command::new("perl")
        .args([
            "-MFcntl=:flock",
            "-e",
            "$|=1; open(F, '>', $ARGV[0]) or die; flock(F, LOCK_EX) or die; print \"held\\n\"; sleep 20;",
        ])
        .arg(&lock)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("perl");
    let mut line = String::new();
    std::io::BufRead::read_line(
        &mut std::io::BufReader::new(holder.stdout.take().unwrap()),
        &mut line,
    )
    .unwrap();
    assert_eq!(line.trim(), "held");
    let started = std::time::Instant::now();
    let (code, text) = l.collect();
    let waited = started.elapsed();
    holder.kill().unwrap();
    holder.wait().unwrap();
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("another collect is running"), "{text}");
    assert!(!text.contains("0 cluster passes"), "{text}");
    assert!(waited.as_secs() < 5, "{waited:?}");
    assert_eq!(l.calls("get"), 0, "{}", l.cluster.kubectl_calls());
    assert_eq!(l.tokens(), "");
}

/// A closed lid: amont dies right after the Workflow is accepted, before its
/// UID is recorded. A later pre-push collects and skips the gate, and signs.
#[test]
fn a_closed_lid_loses_nothing() {
    let Some(l) = Lane::new(ONE) else { return };
    let (code, text) = l.cluster.amont(
        &["rehearse", "--cluster"],
        &[("AMONT_TEST_CRASH", "after-create")],
        None,
    );
    assert_eq!(code, 137, "killed after the create: {text}");
    let wf = l.workflow();
    let pending = l.pending();
    assert!(
        pending.contains(&wf.name) && pending.contains("\"uid\":null"),
        "intent, no UID: {pending}"
    );
    // time passes; the cluster finishes the run
    l.answer(&[
        wf.object("Succeeded", &[("g0", "Succeeded")]),
        wf.pod("g0", |p| p),
    ]);
    l.cluster.reset_records();
    l.answer(&[
        wf.object("Succeeded", &[("g0", "Succeeded")]),
        wf.pod("g0", |p| p),
    ]);
    let (code, text) = l.push();
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("collected the cluster pass of pre-push-lint"),
        "{text}"
    );
    assert!(text.contains("not repeating it here"), "{text}");
    assert_eq!(l.cluster.local_runs(), 0, "{text}");
    assert_eq!(
        l.calls("create"),
        0,
        "never created twice: {}",
        l.cluster.kubectl_calls()
    );
    assert_eq!(
        names(&l.cluster.remote_blocks(&l.tree), "pre-push-lint"),
        ["x86_64-linux"],
        "{text}"
    );
}

/// amont dies after the intent is written and before `create`: nothing exists
/// on the cluster; a rerun does not submit the same gates twice.
#[test]
fn a_crash_before_create_leaves_an_intent_and_creates_nothing_twice() {
    let Some(l) = Lane::new(ONE) else { return };
    let (code, text) = l.cluster.amont(
        &["rehearse", "--cluster"],
        &[("AMONT_TEST_CRASH", "before-create")],
        None,
    );
    assert_eq!(code, 137, "{text}");
    assert_eq!(l.workflow_creates(), 0);
    assert!(l.pending().contains("\"uid\":null"), "{}", l.pending());
    // the intent is young: collect keeps it and asks once
    l.answer(&[]);
    let (_, text) = l.collect();
    assert!(text.contains("1 Workflow still pending"), "{text}");
    // a rerun finds the intent and does not create anything
    let (code, text) = l.rehearse(&["--cluster"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(l.workflow_creates(), 0, "{}", l.cluster.kubectl_calls());
}

/// The apiserver accepted the Workflow and the answer was lost: `get` by the
/// deterministic name finds it, validates it and adopts its UID. One create.
#[test]
fn a_create_whose_answer_was_lost_is_adopted_by_name_not_repeated() {
    let Some(l) = Lane::new(ONE) else { return };
    l.cluster.answer("create_wf", "lost\n");
    let wf = l.submitted();
    assert_eq!(l.workflow_creates(), 1, "{}", l.cluster.kubectl_calls());
    assert_eq!(
        l.calls("get"),
        1,
        "get by name: {}",
        l.cluster.kubectl_calls()
    );
    assert!(l
        .cluster
        .kubectl_calls()
        .contains(&format!("get workflow {}", wf.name)));
    assert!(
        l.pending().contains(&format!("\"uid\":\"{}\"", wf.uid())),
        "{}",
        l.pending()
    );
}

/// The apiserver answered with an error: nothing was created, the intent goes,
/// the rehearsal runs as before.
#[test]
fn a_create_refused_by_the_apiserver_submits_nothing() {
    let Some(l) = Lane::new(ONE) else { return };
    l.cluster.answer("create_wf", "reject\n");
    let (code, text) = l.rehearse(&["--wait", "--cluster"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(l.workflow_creates(), 1);
    assert_eq!(l.pending(), "");
    assert_eq!(
        l.cluster.created_jobs().len(),
        1,
        "the gate ran on the Job lane: {text}"
    );
}

/// An unconfirmed create (the answer lost and `get` finds nothing) keeps its
/// intent, so a request that lands late is adopted and never repeated; the
/// gates run in the meantime.
#[test]
fn an_unconfirmed_create_keeps_the_intent_and_the_gates_run() {
    let Some(l) = Lane::new(ONE) else { return };
    l.cluster.answer("create_wf", "never\n");
    let (code, text) = l.rehearse(&["--wait", "--cluster"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(l.workflow_creates(), 1);
    assert!(l.pending().contains("\"uid\":null"), "{}", l.pending());
    assert_eq!(l.cluster.created_jobs().len(), 1, "{text}");
}

/// The token is the one frozen at submission: change the image between
/// submission and collection and the push does not accept the pass.
#[test]
fn a_changed_image_between_submission_and_collect_is_not_accepted_at_push() {
    let Some(l) = Lane::new(ONE) else { return };
    let wf = l.submitted();
    let other = format!("reg/gate:2@sha256:{}", "2".repeat(64));
    l.cluster.config("amont.cluster.image", &other);
    l.answer(&[
        wf.object("Succeeded", &[("g0", "Succeeded")]),
        wf.pod("g0", |p| p),
    ]);
    let (code, text) = l.collect();
    assert_eq!(code, 0, "{text}");
    assert!(
        l.tokens().contains("pre-push-lint@"),
        "the frozen token is written: {}",
        l.tokens()
    );
    l.cluster.reset_records();
    let (code, text) = l.push();
    assert_eq!(code, 0, "{text}");
    assert!(
        !text.contains("not repeating it here"),
        "the new image has another identity, so the gate runs: {text}"
    );
    assert_eq!(l.cluster.created_jobs().len(), 1, "the gate ran: {text}");
}

/// The same for a changed prepare command.
#[test]
fn a_changed_prepare_between_submission_and_collect_is_not_accepted_at_push() {
    let conf = format!("{ONE}set clusterPrepare make deps\n");
    let Some(l) = Lane::new(&conf) else { return };
    let wf = l.submitted();
    // the repository now says another prepare, trusted
    l.cluster.r.write(
        "amont.conf",
        &format!("{ONE}set clusterPrepare make other\n"),
    );
    let (code, text) = l.cluster.amont(&["trust"], &[], None);
    assert_eq!(code, 0, "{text}");
    l.answer(&[
        wf.object("Succeeded", &[("g0", "Succeeded")]),
        wf.pod("g0", with_prepare("make deps", 0)),
    ]);
    let (code, text) = l.collect();
    assert_eq!(code, 0, "{text}");
    assert!(
        l.tokens().contains("pre-push-lint@"),
        "the frozen token is written: {}",
        l.tokens()
    );
    l.cluster.reset_records();
    let (code, text) = l.push();
    assert_eq!(code, 0, "{text}");
    assert!(!text.contains("not repeating it here"), "{text}");
    assert_eq!(l.cluster.created_jobs().len(), 1, "the gate ran: {text}");
}

/// A pod that is not the one submitted (another command) earns nothing even
/// if it passed.
#[test]
fn a_pod_running_another_command_is_not_the_submission() {
    let Some(l) = Lane::new(ONE) else { return };
    let wf = l.submitted();
    l.answer(&[
        wf.object("Succeeded", &[("g0", "Succeeded")]),
        wf.pod("g0", |p| p.replace("scripts/gate.sh", "scripts/other.sh")),
    ]);
    l.collect();
    assert_eq!(l.tokens(), "");
    assert!(
        l.cluster.cluster_log().contains("not the one submitted"),
        "{}",
        l.cluster.cluster_log()
    );
}

/// None of the new keys set: no detached lane, and a rehearsal is what it was.
#[test]
fn with_no_detached_keys_a_rehearsal_is_unchanged() {
    let Some(l) = Lane::new(ONE) else { return };
    for key in ["snapshotRemote", "snapshotFetch", "mirrorRemote", "mirror"] {
        assert!(l
            .cluster
            .r
            .git(&["config", "--unset", &format!("amont.cluster.{key}")])
            .status
            .success());
    }
    let (code, text) = l.rehearse(&["--wait"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(l.calls("create"), 1, "the Job lane, as today");
    assert_eq!(l.cluster.created_jobs().len(), 1);
    assert!(l.workflows().is_empty());
    assert_eq!(l.pending(), "");
    assert!(!text.contains("submitted"), "{text}");
    // asking for it without the keys changes nothing but one warning
    l.cluster.reset_records();
    assert!(l
        .cluster
        .r
        .git(&["update-ref", "-d", "refs/notes/amont-gate-cluster"])
        .status
        .success());
    let (code, text) = l.rehearse(&["--wait", "--cluster"]);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("amont.cluster.snapshotRemote is not set"),
        "{text}"
    );
    assert!(l.workflows().is_empty(), "{text}");
    assert_eq!(
        l.cluster.created_jobs().len(),
        1,
        "the Job lane ran it: {text}"
    );
}

/// `amont.rehearse.cluster true` alone sends the post-commit rehearsal's gates
/// to the cluster, with no flag.
#[test]
fn the_machine_key_alone_detaches_the_rehearsal() {
    let Some(l) = Lane::new(MIXED) else { return };
    l.cluster.config("amont.rehearse.cluster", "true");
    let (code, text) = l.rehearse(&["--wait"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(
        l.workflow().json.matches("\"name\":\"gate-g").count(),
        2,
        "{text}"
    );
    assert!(
        l.cluster.created_jobs().is_empty(),
        "no Job for a detached gate: {text}"
    );
    assert_eq!(
        l.cluster.local_runs(),
        1,
        "only the local gate ran here: {text}"
    );
}

/// `amont stamps --json` collects first, and its stdout stays one JSON object.
#[test]
fn stamps_collects_and_keeps_its_json_clean() {
    let Some(l) = Lane::new(ONE) else { return };
    let wf = l.submitted();
    l.answer(&[
        wf.object("Succeeded", &[("g0", "Succeeded")]),
        wf.pod("g0", |p| p),
    ]);
    let output = l
        .cluster
        .command(&["stamps", "--json", "lint"], &[])
        .output()
        .expect("amont stamps");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains("collected the cluster pass of pre-push-lint"),
        "{stderr}"
    );
    assert!(
        stdout.trim_start().starts_with('{') && stdout.trim_end().ends_with('}'),
        "{stdout}"
    );
    assert!(
        stdout.contains("\"lint\"") || stdout.contains("pre-push-lint"),
        "{stdout}"
    );
    assert!(l.tokens().contains("pre-push-lint@"), "{}", l.tokens());
}

/// The pass was judged, the Workflow is finished, and git refuses the token:
/// the record stays, and the next collect writes the token.
#[test]
fn a_token_git_refuses_to_write_keeps_the_record_for_the_next_collect() {
    let Some(l) = Lane::new(ONE) else { return };
    let wf = l.submitted();
    l.answer(&[
        wf.object("Succeeded", &[("g0", "Succeeded")]),
        wf.pod("g0", |p| p),
    ]);
    let lock = l
        .cluster
        .r
        .dir
        .join(".git/refs/notes/amont-gate-cluster.lock");
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    std::fs::write(&lock, "").unwrap();
    let (code, text) = l.collect();
    assert_eq!(code, 0, "{text}");
    assert_eq!(l.tokens(), "", "git refused: {text}");
    assert!(
        l.pending().contains(&wf.name),
        "the record stays: {}",
        l.pending()
    );
    assert!(
        l.cluster.cluster_log().contains("not recorded"),
        "{}",
        l.cluster.cluster_log()
    );
    std::fs::remove_file(&lock).unwrap();
    let (code, text) = l.collect();
    assert_eq!(code, 0, "{text}");
    assert!(l.tokens().contains("pre-push-lint@"), "{}", l.tokens());
    assert_eq!(l.pending(), "");
}

/// A cluster nobody can read does not keep a record past its TTL.
#[test]
fn an_unreadable_cluster_still_lets_old_records_expire() {
    let Some(l) = Lane::new(ONE) else { return };
    let wf = l.submitted();
    l.cluster.answer("wf_down", "");
    // young: kept, and the collect says the cluster was not reached
    let (code, text) = l.collect();
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("cluster not reached"), "{text}");
    assert!(!text.contains("0 cluster passes"), "{text}");
    assert!(l.pending().contains(&wf.name));
    // old: dropped with one line, even though the cluster is still down
    let pending_file = l.cluster.r.dir.join(".git/amont/cluster-pending");
    let aged = l
        .pending()
        .replacen("\"submitted\":", "\"submitted\":1,\"was\":", 1);
    std::fs::write(&pending_file, aged).unwrap();
    let before = l.cluster.cluster_log().lines().count();
    l.collect();
    assert_eq!(l.pending(), "");
    let log = l.cluster.cluster_log();
    assert_eq!(
        log.lines().count(),
        before + 2,
        "the read failure and the drop: {log}"
    );
    assert!(
        log.contains("older than the Workflow TTL; dropped"),
        "{log}"
    );
}

/// A command or its arguments changed in amont.conf after submitting: the
/// pass is collected under the frozen token and the push runs the gate.
#[test]
fn a_changed_command_or_args_between_submission_and_collect_is_not_accepted_at_push() {
    for changed in [
        "pre-push  lint   *.txt  block  platform=any scripts/gate.sh --strict\n",
        "pre-push  lint   *.txt  block  platform=any scripts/a.sh\n",
    ] {
        let Some(l) = Lane::new(ONE) else { return };
        let wf = l.submitted();
        l.cluster
            .r
            .write("amont.conf", &format!("set executor cluster\n{changed}"));
        let (code, text) = l.cluster.amont(&["trust"], &[], None);
        assert_eq!(code, 0, "{text}");
        l.answer(&[
            wf.object("Succeeded", &[("g0", "Succeeded")]),
            wf.pod("g0", |p| p),
        ]);
        let (code, text) = l.collect();
        assert_eq!(code, 0, "{text}");
        assert!(
            l.tokens().contains("pre-push-lint@"),
            "frozen token: {}",
            l.tokens()
        );
        l.cluster.reset_records();
        let (code, text) = l.push();
        assert_eq!(code, 0, "{changed}: {text}");
        assert!(!text.contains("not repeating it here"), "{changed}: {text}");
        assert_eq!(
            l.cluster.created_jobs().len(),
            1,
            "{changed}: the gate ran: {text}"
        );
    }
}

/// `rehearse --status` shows the runs a collect has just finished, including
/// the ones that did not pass.
#[test]
fn status_shows_a_finished_run_and_what_it_did_not_pass() {
    let Some(l) = Lane::new(ONE) else { return };
    let wf = l.submitted();
    l.answer(&[
        wf.object("Failed", &[("g0", "Failed")]),
        wf.pod("g0", main_exits(1, "Error")),
    ]);
    let (code, text) = l.rehearse(&["--status"]);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains(&format!(
            "cluster run {} failed: did not pass: pre-push-lint",
            wf.name
        )) && text.contains("see cluster.log; the gate runs at the push"),
        "{text}"
    );
}

/// The talk is stderr; stdout is for asked-for data.
#[test]
fn submission_talks_on_stderr_and_stdout_stays_empty() {
    let Some(l) = Lane::new(ONE) else { return };
    let output = l
        .cluster
        .command(&["rehearse", "--cluster"], &[])
        .output()
        .expect("amont rehearse");
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    assert!(String::from_utf8_lossy(&output.stderr).contains("submitted amont-rh-"));
}

/// `--stop` exits 1 when a patch failed, with the command to do it by hand.
#[test]
fn stop_fails_loudly_when_a_workflow_cannot_be_terminated() {
    let Some(l) = Lane::new(ONE) else { return };
    let wf = l.submitted();
    l.cluster.answer("patch_fail", "");
    let output = l
        .cluster
        .command(&["rehearse", "--stop"], &[])
        .output()
        .expect("amont rehearse");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("could not terminate cluster run {}", wf.name))
            && stderr.contains("kubectl --kubeconfig '")
            && stderr.contains(" -n amont-gates patch workflow"),
        "{stderr}"
    );
}

/// A runtime failure of `rehearse --cluster` (here: no upstream to rehearse
/// against) is exit 1, not a usage error; `cluster collect --help` is exit 0.
#[test]
fn exit_codes_follow_the_cli_contract() {
    let Some(l) = Lane::new(ONE) else { return };
    let (code, text) = l
        .cluster
        .amont(&["cluster", "collect", "--help"], &[], None);
    assert_eq!(code, 0, "{text}");
    let (code, text) = l.cluster.amont(&["cluster", "bogus"], &[], None);
    assert_eq!(code, 2, "{text}");
    l.cluster.r.git(&["checkout", "-q", "--detach"]);
    l.cluster
        .r
        .git(&["update-ref", "-d", "refs/remotes/origin/main"]);
    let (code, text) = l.rehearse(&["--cluster"]);
    assert_eq!(code, 1, "{text}");
}
