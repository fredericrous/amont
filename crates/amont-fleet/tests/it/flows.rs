//! The fleet journey: what a person does with `amont-fleet`, one test per
//! part (decisions plan 2026-10-17-amont-tests-follow-user-flows, Phase 5).
//!
//! Each flow builds its scan root once and walks the journey, asserting what
//! each step leaves on the terminal and on disk: the tool's public
//! interface. Every assertion names its step, so a failure says where the
//! journey broke, not only that it did. The three parts run side by side,
//! each on a root of its own:
//!
//! 1. The survey (read-only): a root of ten repositories in different states,
//!    scanned as a person reads it (text) and as the dashboard reads it
//!    (`--json`): the counters, every repository's state, declared checks,
//!    the AGENTS.md pointer, the bypass ledger, a redirected hooks path, a
//!    vendored subtree that does not count.
//! 2. The repair: `fix` as a dry run, the plan (text and `--json`),
//!    `fix --apply`, and applying again.
//! 3. On and off: `install` turning hooks on where there were none and
//!    refusing somebody else's, then `uninstall` taking every shim of ours
//!    back out, with our bookkeeping and none of the person's things, and
//!    `uninstall` again.
//!
//! The cases with fixtures of their own (a wrong root, a submodule, a
//! worktree, symlinks, a git that will not answer, a read-only hooks
//! directory, the golden removal set) stay in the modules beside this one.

/// `assert!` for a flow: the message starts with the step's name.
///
/// ```ignore
/// step!("scan", out.contains("SCAN"), "{out}");
/// ```
macro_rules! step {
    ($step:literal, $cond:expr $(,)?) => {
        assert!($cond, "step `{}`: {}", $step, stringify!($cond))
    };
    ($step:literal, $cond:expr, $($arg:tt)+) => {
        assert!($cond, "step `{}`: {}", $step, format_args!($($arg)+))
    };
}

/// `assert_eq!` for a flow, named the same way.
macro_rules! step_eq {
    ($step:literal, $left:expr, $right:expr $(,)?) => {
        assert_eq!($left, $right, "step `{}`", $step)
    };
    ($step:literal, $left:expr, $right:expr, $($arg:tt)+) => {
        assert_eq!($left, $right, "step `{}`: {}", $step, format_args!($($arg)+))
    };
}

use crate::common::{bin, shim_for, DISPATCHERS};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A scan root in a temp directory of its own, removed on drop.
struct Root(PathBuf);

impl Root {
    fn new(name: &str) -> Self {
        let d = std::env::temp_dir().join(format!("fleet-flow-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("mkdir");
        Root(d)
    }

    /// Joined component by component, so a nested path is spelled with the
    /// platform's separator, as the scan reports it.
    fn path(&self, rel: &str) -> PathBuf {
        rel.split('/').fold(self.0.clone(), |p, c| p.join(c))
    }

    fn hooks(&self, rel: &str) -> PathBuf {
        let h = self.path(rel).join(".git").join("hooks");
        std::fs::create_dir_all(&h).expect("mkdir");
        h
    }

    fn write(&self, rel: &str, body: impl AsRef<[u8]>) {
        let p = self.path(rel);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).expect("mkdir");
        }
        std::fs::write(&p, body).unwrap_or_else(|e| panic!("write {}: {e}", p.display()));
    }

    fn read(&self, rel: &str) -> String {
        let p = self.path(rel);
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
    }

    fn exists(&self, rel: &str) -> bool {
        self.path(rel).exists()
    }

    /// `amont-fleet <args> --root <root>`: exit code, stdout, stderr.
    fn fleet(&self, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(bin())
            .args(args)
            .arg("--root")
            .arg(&self.0)
            .output()
            .expect("run amont-fleet");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// The same, read as the JSON it prints.
    fn json(&self, args: &[&str]) -> Value {
        let mut a = args.to_vec();
        a.push("--json");
        let (_, out, err) = self.fleet(&a);
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("{a:?}: not JSON ({e}):\n{out}{err}"))
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `git <args>` in `dir`, which must succeed. An empty template, so the
/// machine's `init.templateDir` cannot install hooks into a fixture.
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "init.templateDir="])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// One local config value, empty when unset.
fn config(dir: &Path, key: &str) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["config", "--local", "--get", key])
        .output()
        .expect("git");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A repository path as the scan reports it: root-relative, with the
/// platform's separator.
fn rel(path: &str) -> String {
    path.split('/')
        .fold(PathBuf::new(), |p, c| p.join(c))
        .to_string_lossy()
        .into_owned()
}

/// The scan's or plan's entry for one repository, by path.
fn entry<'a>(list: &'a Value, key: &str, path: &str) -> &'a Value {
    let want = rel(path);
    list.as_array()
        .expect("an array")
        .iter()
        .find(|r| r[key] == want.as_str())
        .unwrap_or_else(|| panic!("no {path} in {list}"))
}

/// The last path component of each entry, in order.
fn names(list: &Value) -> Vec<String> {
    list.as_array()
        .map(|a| {
            a.iter()
                .map(|v| {
                    let p = v
                        .as_str()
                        .or_else(|| v["path"].as_str())
                        .unwrap_or_default();
                    Path::new(p)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---- 1. the survey ---------------------------------------------------------

/// The pre-commit dispatcher the scan recognises as ours by its marker.
const MARKED: &str = "#!/bin/sh\n# git-templates hook shim.\nexec \"$BIN\" --hooks-dir \"$(dirname \"$0\")\" pre-commit \"$@\"\n";

#[test]
fn the_survey_journey() {
    let t = Root::new("survey");
    // Ten repositories, one per state worth reporting, and one that must
    // not count. The fake ones are only the directory shape; `ledger` and
    // `redirected` are real, because what they show is git's to answer.
    for repo in [
        "alpha",
        "beta/gamma",
        "broken",
        "leftovers",
        "mentions",
        "ledger",
        "app",
        "app/node_modules/pkg",
        "Perso/group/project/sub",
    ] {
        t.hooks(repo);
        t.write(&format!("{repo}/.git/hooks/pre-commit"), MARKED);
    }
    // A declared check, and the AGENTS.md block `amont agents-md` writes.
    t.write(
        "alpha/amont.conf",
        "pre-commit  lint-shell  *.sh  block  scripts/lint-shell.sh\n",
    );
    t.write(
        "alpha/AGENTS.md",
        amont_runtime::agents_md::generate_block(&amont_runtime::config::Settings::default()),
    );
    // A declaration that has never run, and a block from an older amont.
    t.write(
        "broken/amont.conf",
        "pre-commit  lint-shell  *.sh  LOUD  make lint\n",
    );
    t.write(
        "broken/AGENTS.md",
        format!(
            "{}\nstale content from an older version\n{}\n",
            amont_runtime::agents_md::START,
            amont_runtime::agents_md::END
        ),
    );
    // A retired shim of ours, somebody's own sub-hook, the node-era file.
    t.write(
        "leftovers/.git/hooks/pre-commit-ruff",
        "#!/bin/sh\n# git-templates hook shim.\nexec x --hooks-dir y pre-commit-ruff\n",
    );
    t.write(
        "leftovers/.git/hooks/pre-push-mine.sh",
        "#!/bin/sh\necho hi\n",
    );
    t.write(
        "leftovers/.git/hooks/package.json",
        "{\"//\":\"Forces Node\"}",
    );
    // A hook that MENTIONS --hooks-dir is not ours for saying so.
    t.write(
        "mentions/.git/hooks/pre-commit-mine",
        "#!/bin/sh\n# forwards --hooks-dir to some other tool of mine\nexec my-own-tool \"$@\"\n",
    );
    // Three `--no-verify` commits in the ledger the runtime keeps.
    git(&t.path("ledger"), &["init", "-q", "--template=", "."]);
    t.write(
        "ledger/.git/amont-bypasses",
        "amont-bypass-v1\n100 abcdef0 typecheck\n200 abcdef0 typecheck\n150 abcdef0 test\n",
    );
    // Somebody else's hooks.
    t.hooks("legacy");
    t.write("legacy/.git/hooks/pre-commit", "#!/bin/sh\necho mine\n");
    // Hooks kept in the tree on purpose, through `core.hooksPath`, and
    // nothing at `.git/hooks`.
    std::fs::create_dir_all(t.path("redirected")).expect("mkdir");
    git(&t.path("redirected"), &["init", "-q", "--template=", "."]);
    git(
        &t.path("redirected"),
        &["config", "core.hooksPath", "tooling/hooks"],
    );
    t.write("redirected/tooling/hooks/pre-commit", MARKED);

    // The report a person reads, from a pipe: nothing is watching, so
    // nothing is drawn.
    let (code, out, err) = t.fleet(&[]);
    step_eq!("scan", code, 0, "{out}{err}");
    step!("scan", err.is_empty(), "stderr must stay empty: {err:?}");
    step!(
        "scan",
        !out.contains('\u{1b}') && !out.contains('\r'),
        "the report carries no redraw sequences: {out:?}"
    );
    step!(
        "scan",
        out.contains("10 git repositories · 9 managed · 1 unmanaged"),
        "{out}"
    );
    step!("scan", out.contains("· 1 subtrees skipped ·"), "{out}");
    step!(
        "scan",
        out.contains("3 unverified commits across 1 repositories"),
        "{out}"
    );

    // The same scan as the dashboard reads it. Its shape is the contract.
    let v = t.json(&[]);
    for k in [
        "root",
        "depth",
        "git_dirs_found",
        "hook_dirs_seen",
        "managed_seen",
        "unmanaged_seen",
        "unreadable",
        "hooks_outside_seen",
        "excluded_dirs",
        "dirs_visited",
        "repos",
    ] {
        step!(
            "scan --json",
            !v[k].is_null(),
            "missing {k} from the scan contract"
        );
    }
    step_eq!("scan --json", v["git_dirs_found"], 10, "{v}");
    step_eq!("scan --json", v["managed_seen"], 9);
    step_eq!("scan --json", v["unmanaged_seen"], 1);
    // The vendored repository is skipped, and the skip recorded.
    step_eq!("scan --json", v["excluded_dirs"], 1);
    // `hooks_outside_seen` counts every hooks directory that is not the
    // repository's own `.git/hooks`: here, the redirect.
    step_eq!("scan --json", v["hooks_outside_seen"], 1);
    // Root-relative, sorted, the deepest one found at the default depth.
    let paths: Vec<&str> = v["repos"]
        .as_array()
        .expect("repos")
        .iter()
        .map(|r| r["path"].as_str().expect("path"))
        .collect();
    let want: Vec<String> = [
        "Perso/group/project/sub",
        "alpha",
        "app",
        "beta/gamma",
        "broken",
        "ledger",
        "leftovers",
        "legacy",
        "mentions",
        "redirected",
    ]
    .into_iter()
    .map(rel)
    .collect();
    step_eq!("scan --json", paths, want);
    // Absent fields would be "we did not look"; they are present and null.
    for r in v["repos"].as_array().expect("repos") {
        step!(
            "scan --json",
            r.get("shares_hooks_with") == Some(&Value::Null),
            "shares_hooks_with must be present (null), not absent: {r}"
        );
    }
    let repos = &v["repos"];

    let alpha = entry(repos, "path", "alpha");
    step_eq!("repo states", alpha["hooks_dir"]["where"], "in", "{alpha}");
    step_eq!("repo states", alpha["agents_md"], "up_to_date");
    let d = &alpha["declared"][0];
    step_eq!("repo states", d["name"], "lint-shell", "{d}");
    step_eq!("repo states", d["stage"], "pre-commit");
    step_eq!("repo states", d["state"], "usable");
    step_eq!("repo states", d["severity"], "block");
    step_eq!("repo states", d["command"], "scripts/lint-shell.sh");
    step_eq!("repo states", d["exts"][0], ".sh");

    let plain = entry(repos, "path", "beta/gamma");
    step_eq!(
        "repo states",
        plain["declared"].as_array().map(Vec::len),
        Some(0),
        "{plain}"
    );
    step_eq!("repo states", plain["agents_md"], "missing");

    let broken = entry(repos, "path", "broken");
    let d = &broken["declared"][0];
    step_eq!("repo states", d["state"], "unusable", "{d}");
    let why = d["why"].as_str().expect("must say why");
    step!("repo states", why.contains("severity"), "{why}");
    step!("repo states", why.contains("line 1"), "{why}");
    step!("repo states", d["command"].is_null(), "{d}");
    step_eq!("repo states", broken["agents_md"], "drifted");

    let left = entry(repos, "path", "leftovers");
    step_eq!(
        "repo states",
        left["stale_ours"][0],
        "pre-commit-ruff",
        "{left}"
    );
    step_eq!("repo states", left["foreign_subs"][0], "pre-push-mine.sh");
    step_eq!("repo states", left["hook_pkgjson"], true);

    let mentions = entry(repos, "path", "mentions");
    step_eq!(
        "repo states",
        mentions["stale_ours"]
            .as_array()
            .map(|a| a.len())
            .unwrap_or(0),
        0,
        "a hook that only MENTIONS --hooks-dir was classified as ours: {mentions}"
    );
    step_eq!(
        "repo states",
        mentions["foreign_subs"][0],
        "pre-commit-mine"
    );

    step_eq!(
        "repo states",
        entry(repos, "path", "legacy")["managed"],
        false
    );

    // The redirect is followed, not assumed: managed through `tooling/hooks`
    // with nothing at `.git/hooks`, and benign, since nothing regenerates a
    // directory the repository chose.
    let redirected = entry(repos, "path", "redirected");
    step_eq!("repo states", redirected["managed"], true, "{redirected}");
    step_eq!(
        "repo states",
        redirected["hooks_dir"]["where"],
        "redirected"
    );
    step_eq!("repo states", redirected["hooks_dir"]["hostile"], false);
    step!("repo states", !t.exists("redirected/.git/hooks"));

    // The ledger, read through the runtime's parser, and summed.
    let b = &entry(repos, "path", "ledger")["bypasses"];
    step_eq!("bypasses", b["total"], 3, "{b}");
    step_eq!("bypasses", b["last"], 200, "{b}");
    step_eq!("bypasses", b["by_script"][0]["script"], "typecheck");
    step_eq!("bypasses", b["by_script"][0]["count"], 2);
    step_eq!("bypasses", b["by_script"][0]["last"], 200);
    step_eq!("bypasses", v["bypassed_commits"], 3);
    step_eq!("bypasses", v["repos_with_bypasses"], 1);
    for r in repos.as_array().expect("repos") {
        if r["path"] != "ledger" {
            step_eq!("bypasses", r["bypasses"]["total"], 0, "{r}");
        }
    }
}

// ---- 2. the repair ---------------------------------------------------------

/// The binary the shims should name, and a shim baked at it. It is never
/// run.
fn fake_binary(t: &Root) -> (String, String) {
    let binary = t.path("fake-amont");
    std::fs::write(&binary, "#!/bin/sh\nexit 0\n").expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let binary = binary.to_str().expect("utf8").to_string();
    let baked = shim_for(&binary);
    (binary, baked)
}

/// Installed and correct: all six dispatchers, baked at `baked`.
fn healthy(t: &Root, repo: &str, baked: &str) {
    t.hooks(repo);
    for n in DISPATCHERS {
        t.write(&format!("{repo}/.git/hooks/{n}"), baked);
    }
}

/// Pre-migration hooks that dispatch to nothing of ours, and a sub-hook of
/// that era. Returns its `pre-commit`, to compare against afterwards.
fn legacy(t: &Root) -> String {
    t.hooks("legacy");
    t.write("legacy/.git/hooks/pre-commit", "#!/bin/zsh\necho legacy\n");
    t.write("legacy/.git/hooks/pre-commit-ban-terms.js", "// legacy\n");
    t.read("legacy/.git/hooks/pre-commit")
}

/// The fleet's own fix: `fix` as a dry run, the plan, `fix --apply`, and
/// applying again.
#[test]
fn the_repair_journey() {
    let t = Root::new("repair");
    let (bin_arg, baked) = fake_binary(&t);
    let bin_arg = bin_arg.as_str();
    // Installed and correct, carrying a retired shim of ours, missing a
    // dispatcher, somebody else's, and never installed.
    healthy(&t, "clean", &baked);
    healthy(&t, "stale", &baked);
    t.write(
        "stale/.git/hooks/pre-commit-ruff",
        "#!/bin/sh\n# git-templates hook shim.\nexec x --hooks-dir y pre-commit-ruff\n",
    );
    healthy(&t, "incomplete", &baked);
    std::fs::remove_file(t.path("incomplete/.git/hooks/pre-push")).expect("rm");
    let legacy_before = legacy(&t);
    t.hooks("fresh");

    // `fix` as typed: a dry run, and it says so.
    let (_, out, _) = t.fleet(&["fix"]);
    step!("fix", out.contains("DRY RUN"), "and must say so:\n{out}");
    step!(
        "fix",
        t.exists("stale/.git/hooks/pre-commit-ruff"),
        "dry run must not delete"
    );

    // Somebody else's repository is refused, whatever binary is in play.
    let plans = t.json(&["fix"]);
    let legacy = entry(&plans, "repo", "legacy");
    step_eq!(
        "fix --json",
        legacy["refuse"][0]["refusal"],
        "unmanaged",
        "{legacy}"
    );
    step!(
        "fix --json",
        legacy["write"].as_array().expect("write").is_empty()
    );

    // The plan, against the binary the shims should name.
    let plans = t.json(&["fix", "--binary", bin_arg]);
    let removals: Vec<String> = plans
        .as_array()
        .expect("plans")
        .iter()
        .flat_map(|p| {
            let repo = p["repo"].as_str().unwrap_or_default().to_string();
            names(&p["remove"])
                .into_iter()
                .map(move |n| format!("{repo}:{n}"))
        })
        .collect();
    step_eq!("plan", removals, vec!["stale:pre-commit-ruff"], "{plans}");
    let writes = |repo: &str| -> Vec<String> {
        let w = entry(&plans, "repo", repo)["write"].clone();
        let changing: Vec<Value> = w
            .as_array()
            .expect("write")
            .iter()
            .filter(|w| w["changes"] == true)
            .cloned()
            .collect();
        names(&Value::Array(changing))
    };
    step!("plan", writes("clean").is_empty(), "{plans}");
    step!("plan", writes("stale").is_empty(), "{plans}");
    step_eq!("plan", writes("incomplete"), vec!["pre-push"]);
    for repo in ["legacy", "fresh"] {
        step_eq!(
            "plan",
            entry(&plans, "repo", repo)["refuse"][0]["refusal"],
            "unmanaged",
            "fix repairs; it does not install: {repo}"
        );
    }
    let (code, out, _) = t.fleet(&["fix", "--binary", bin_arg]);
    step_eq!("plan", code, 0, "{out}");
    step!(
        "plan",
        out.contains("2 of 5 repositories would change · 2 refused · 1 already correct")
            && out.contains("1 removals · 1 writes"),
        "{out}"
    );
    step!(
        "plan",
        t.exists("stale/.git/hooks/pre-commit-ruff"),
        "{out}"
    );

    // `fix --apply`: the plan carried out, and nothing else touched.
    let (code, out, err) = t.fleet(&["fix", "--apply", "--json", "--binary", bin_arg]);
    step_eq!("apply", code, 0, "{out}{err}");
    let reports: Value = serde_json::from_str(&out).expect("json");
    let outcome = |repo: &str| entry(&reports, "repo", repo)["outcome"]["outcome"].clone();
    let applied: Vec<&str> = reports
        .as_array()
        .expect("reports")
        .iter()
        .filter(|r| r["outcome"]["outcome"] == "applied")
        .map(|r| r["repo"].as_str().unwrap_or_default())
        .collect();
    step_eq!("apply", applied, vec!["incomplete", "stale"], "{reports}");
    step_eq!("apply", outcome("clean"), "unchanged");
    step_eq!("apply", outcome("legacy"), "refused");
    step!(
        "apply",
        !t.exists("stale/.git/hooks/pre-commit-ruff"),
        "the stale file must be gone"
    );
    step_eq!("apply", t.read("incomplete/.git/hooks/pre-push"), baked);
    step_eq!(
        "apply",
        t.read("legacy/.git/hooks/pre-commit"),
        legacy_before,
        "an unmanaged repo must be byte-identical afterwards"
    );
    step!(
        "apply",
        t.exists("legacy/.git/hooks/pre-commit-ban-terms.js"),
        "including its legacy sub-hooks"
    );
    step!(
        "apply",
        t.path("fresh/.git/hooks")
            .read_dir()
            .map(|mut d| d.next().is_none())
            .unwrap_or(false),
        "fix --apply installed into a repository nobody installed into"
    );

    // Again: nothing left to do.
    let again = t.json(&["fix", "--apply", "--binary", bin_arg]);
    step!(
        "apply again",
        again
            .as_array()
            .expect("reports")
            .iter()
            .all(|r| r["outcome"]["outcome"] != "applied"),
        "applying twice must change nothing the second time: {again}"
    );
}

// ---- 3. on and off ---------------------------------------------------------

/// Hooks turned on across the root, then taken back out: `install` where
/// there were none, `uninstall` with our bookkeeping and none of the
/// person's things, and `uninstall` again.
#[test]
fn the_install_journey() {
    let t = Root::new("install");
    let (bin_arg, baked) = fake_binary(&t);
    let bin_arg = bin_arg.as_str();
    // Installed and correct, somebody else's, never installed, and a real
    // repository with one shim of ours, our bookkeeping and the person's
    // own things in it.
    healthy(&t, "clean", &baked);
    let legacy_before = legacy(&t);
    t.hooks("fresh");
    let kept = t.path("kept");
    std::fs::create_dir_all(&kept).expect("mkdir");
    // A plain `git init`, the machine's template and all, as the fixture
    // this journey carries always had.
    let init = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&kept)
        .output()
        .expect("git init");
    assert!(init.status.success(), "{init:?}");
    t.write("kept/.git/hooks/pre-commit", shim_for("/bin/fake-amont"));
    // Ours: a trust record and a ledger. Theirs: a skip policy and parked
    // work an interrupted run left behind.
    git(&kept, &["config", "amont.trusted", "deadbeef"]);
    git(&kept, &["config", "hook.skip", "pre-commit-clippy"]);
    t.write("kept/.git/amont-bypasses", "amont-bypasses-v1\n");
    t.write("kept/.git/amont-held/index", b"amont-held-v1\0");

    // `install` turns hooks on where there were none, and refuses to write
    // over somebody else's.
    let (code, out, _) = t.fleet(&["install", "--binary", bin_arg]);
    step_eq!("install", code, 0, "{out}");
    for repo in ["fresh", "kept"] {
        for n in DISPATCHERS {
            step_eq!(
                "install",
                t.read(&format!("{repo}/.git/hooks/{n}")),
                baked,
                "{repo}/{n}"
            );
        }
        step!("install", out.contains(&format!("{repo}  -0 +6")), "{out}");
    }
    step!(
        "install",
        out.contains("pre-commit was written by somebody else — activating would overwrite it"),
        "{out}"
    );
    step!(
        "install",
        out.contains("pre-commit-ban-terms.js  (a hook we did not write)"),
        "{out}"
    );
    step!(
        "install",
        out.contains("2 of 4 repositories changed · 1 refused · 1 already correct · 0 failed")
            && out.contains("0 removed · 12 written"),
        "{out}"
    );
    step_eq!(
        "install",
        t.read("legacy/.git/hooks/pre-commit"),
        legacy_before
    );
    let v = t.json(&[]);
    step_eq!("install", v["managed_seen"], 3, "{v}");
    step_eq!("install", v["unmanaged_seen"], 1, "{v}");

    // `uninstall`, as typed: every shim of ours out, our bookkeeping
    // forgotten, and nothing of the person's taken.
    let (code, out, _) = t.fleet(&["uninstall"]);
    step_eq!("uninstall", code, 0, "{out}");
    step!(
        "uninstall",
        out.contains("18 shims removed from 3 repositories"),
        "{out}"
    );
    for repo in ["clean", "fresh", "kept"] {
        for n in DISPATCHERS {
            step!(
                "uninstall",
                !t.exists(&format!("{repo}/.git/hooks/{n}")),
                "{repo}/{n} survived:\n{out}"
            );
        }
    }
    step!(
        "uninstall",
        config(&kept, "amont.trusted").is_empty(),
        "trust survived a fleet uninstall:\n{out}"
    );
    step!(
        "uninstall",
        !t.exists("kept/.git/amont-bypasses"),
        "the ledger outlived the hooks it describes:\n{out}"
    );
    step!(
        "uninstall",
        out.contains("forgot"),
        "sweeping 200 repositories must say so:\n{out}"
    );
    // Only `kept` had bookkeeping: a repository that lost shims and nothing
    // else gets no `forgot` line.
    step_eq!("uninstall", out.matches("    forgot ").count(), 1, "{out}");
    step_eq!(
        "uninstall",
        config(&kept, "hook.skip"),
        "pre-commit-clippy",
        "the user's own policy is not ours to drop:\n{out}"
    );
    step!(
        "uninstall",
        t.exists("kept/.git/amont-held/index"),
        "PARKED WORK was deleted — the one thing uninstall must never take:\n{out}"
    );
    step_eq!(
        "uninstall",
        t.read("legacy/.git/hooks/pre-commit"),
        legacy_before
    );
    step!(
        "uninstall",
        t.exists("legacy/.git/hooks/pre-commit-ban-terms.js")
    );

    // Twice is quiet: nothing of ours is left to name.
    let (code, out, _) = t.fleet(&["uninstall"]);
    step_eq!("uninstall again", code, 0, "{out}");
    step!(
        "uninstall again",
        out.contains("0 shims removed from 0 repositories")
            && !out.contains("left alone (not ours)"),
        "{out}"
    );
}
