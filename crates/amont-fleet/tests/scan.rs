//! Scanner behaviour, driven through the real binary.
//!
//! The cases that matter are the ones where the old shell sweep was silent: a
//! root that is wrong, a depth that is too shallow, a directory that cannot be
//! read. Each has to be distinguishable from "the fleet is clean", which is the
//! single failure this tool exists to prevent.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_amont-fleet")
}

struct Tree(PathBuf);

impl Tree {
    fn new(name: &str) -> Self {
        let d = std::env::temp_dir().join(format!("fleet-scan-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("mkdir");
        Tree(d)
    }
    /// A repo whose hooks dispatch to the binary, i.e. one of ours.
    fn managed_repo(&self, rel: &str) -> &Self {
        let hooks = self.0.join(rel).join(".git/hooks");
        std::fs::create_dir_all(&hooks).expect("mkdir");
        std::fs::write(
            hooks.join("pre-commit"),
            "#!/bin/sh\n# git-templates hook shim.\nexec \"$BIN\" --hooks-dir \"$(dirname \"$0\")\" pre-commit \"$@\"\n",
        )
        .expect("write");
        self
    }
    /// A git repo we do not manage — no shim dispatching to the binary.
    fn foreign_repo(&self, rel: &str) -> &Self {
        let hooks = self.0.join(rel).join(".git/hooks");
        std::fs::create_dir_all(&hooks).expect("mkdir");
        std::fs::write(hooks.join("pre-commit"), "#!/bin/sh\necho mine\n").expect("write");
        self
    }
    /// A REAL repository (`managed_repo`/`foreign_repo` only fake the
    /// directory shape) whose `core.hooksPath` points somewhere other than
    /// `.git/hooks`, with a managed shim living at that redirected location
    /// and nothing at the default one.
    fn custom_hooks_path_repo(&self, rel: &str, hooks_rel: &str) -> &Self {
        let repo = self.0.join(rel);
        std::fs::create_dir_all(&repo).expect("mkdir");
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .expect("git");
        };
        git(&["init", "-q", "--template=", "."]);
        git(&["config", "core.hooksPath", hooks_rel]);
        let hooks = repo.join(hooks_rel);
        std::fs::create_dir_all(&hooks).expect("mkdir");
        std::fs::write(
            hooks.join("pre-commit"),
            "#!/bin/sh\n# git-templates hook shim.\nexec \"$BIN\" --hooks-dir \"$(dirname \"$0\")\" pre-commit \"$@\"\n",
        )
        .expect("write");
        self
    }
    /// `core.hooksPath` set via an INCLUDED file rather than directly in
    /// `.git/config` — the case a local-text-only shortcut would miss, since
    /// the literal string `hooksPath` never appears in `.git/config` itself.
    fn hooks_path_via_include_repo(&self, rel: &str, hooks_rel: &str) -> &Self {
        let repo = self.0.join(rel);
        std::fs::create_dir_all(&repo).expect("mkdir");
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .expect("git");
        };
        git(&["init", "-q", "--template=", "."]);
        std::fs::write(
            repo.join("shared.gitconfig"),
            format!("[core]\n\thooksPath = {hooks_rel}\n"),
        )
        .expect("write shared config");
        git(&["config", "--add", "include.path", "../shared.gitconfig"]);
        let hooks = repo.join(hooks_rel);
        std::fs::create_dir_all(&hooks).expect("mkdir");
        std::fs::write(
            hooks.join("pre-commit"),
            "#!/bin/sh\n# git-templates hook shim.\nexec \"$BIN\" --hooks-dir \"$(dirname \"$0\")\" pre-commit \"$@\"\n",
        )
        .expect("write");
        self
    }
    /// A real repository with one commit, ready to be a superproject, a
    /// submodule source or a worktree host.
    fn real_repo(&self, rel: &str) -> PathBuf {
        let repo = self.0.join(rel);
        std::fs::create_dir_all(&repo).expect("mkdir");
        git(&repo, &["init", "-q", "--template=", "."]);
        git(&repo, &["config", "user.email", "t@t"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("seed.txt"), "x\n").expect("write");
        git(&repo, &["add", "seed.txt"]);
        git(&repo, &["commit", "-q", "--no-verify", "-m", "chore: seed"]);
        repo
    }
    fn dir(&self, rel: &str) -> &Self {
        std::fs::create_dir_all(self.0.join(rel)).expect("mkdir");
        self
    }
    /// A committed `amont.conf` at a repo root.
    fn manifest(&self, rel: &str, body: &str) -> &Self {
        std::fs::write(self.0.join(rel).join("amont.conf"), body).expect("write");
        self
    }
    /// A committed `AGENTS.md` at a repo root, verbatim.
    fn agents_md(&self, rel: &str, body: &str) -> &Self {
        std::fs::write(self.0.join(rel).join("AGENTS.md"), body).expect("write");
        self
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        // Cloning a submodule from a local path needs this since git 2.38's
        // CVE-2022-39253 fix; without it `submodule add` fails and the fixture
        // silently builds a repository with no submodule in it.
        .args(["-c", "protocol.file.allow=always"])
        // And an EMPTY template, because `submodule add` clones and a clone
        // honours `init.templateDir`. Whoever runs this suite very likely has
        // that pointing at THIS project's hook templates — so the submodule
        // arrived pre-installed and the fixture asserted "unmanaged" against a
        // repository that was, in fact, managed. A fixture must not depend on
        // the machine's git config, least of all on this project's own.
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

/// The shim body, baked at `binary`, exactly as the installer writes it.
fn shim_for(binary: &str) -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../templates/hooks/pre-commit"
    ))
    .expect("template")
    .replace("__AMONT_BIN__", binary)
}

struct Run {
    code: i32,
    stdout: String,
}

fn run(args: &[&str]) -> Run {
    let out = Command::new(bin()).args(args).output().expect("run");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
    }
}

fn json(args: &[&str]) -> serde_json::Value {
    let mut a = args.to_vec();
    a.push("--json");
    serde_json::from_str(&run(&a).stdout).expect("valid json")
}

/// The live status line is drawn for a person watching a terminal, and this
/// asserts the other half of that sentence: when nothing is watching — which
/// is every piped, redirected and CI invocation, including this test's own —
/// the run is silent on stderr and clean on stdout.
///
/// Worth a test rather than a reading of the code because the failure is not
/// cosmetic. The line redraws itself with CR and CSI K; emitted into a pipe
/// those are bytes in the capture, at up to twelve frames a second for the
/// whole walk. A log would fill with them, and anything parsing stdout beside
/// them would be parsing a moving target.
#[test]
fn nothing_is_drawn_when_nothing_is_watching() {
    let t = Tree::new("quiet");
    t.managed_repo("a").managed_repo("b/c");
    let out = Command::new(bin())
        .args(["--root", t.path().to_str().unwrap()])
        .output()
        .expect("run");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.is_empty(), "stderr must stay empty: {stderr:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains('\u{1b}') && !stdout.contains('\r'),
        "the report carries no redraw sequences: {stdout:?}"
    );
}

#[test]
fn counts_managed_and_unmanaged_separately() {
    let t = Tree::new("mixed");
    t.managed_repo("a").managed_repo("b/c").foreign_repo("d");
    let v = json(&["--root", t.path().to_str().unwrap()]);
    assert_eq!(v["git_dirs_found"], 3);
    assert_eq!(v["managed_seen"], 2);
    assert_eq!(v["unmanaged_seen"], 1);
    assert_eq!(v["repos"].as_array().unwrap().len(), 3);
}

/// THE case. A wrong root must be loud in three independent ways: a non-zero
/// exit, the words "SCAN FAILURE", and counters that show nothing was examined.
/// Any one of them alone could be missed.
#[test]
fn a_wrong_root_is_a_failure_not_an_empty_fleet() {
    let t = Tree::new("empty");
    t.dir("nothing/here");
    let r = run(&["--root", t.path().to_str().unwrap()]);
    assert_eq!(r.code, 1, "an empty scan must not exit 0");
    assert!(
        r.stdout.contains("SCAN FAILURE"),
        "must say so in words: {}",
        r.stdout
    );
    assert!(
        r.stdout.contains("--root") && r.stdout.contains("--depth"),
        "must name the two things worth checking: {}",
        r.stdout
    );
    let v = json(&["--root", t.path().to_str().unwrap()]);
    assert_eq!(v["git_dirs_found"], 0);
    assert!(v["dirs_visited"].as_u64().unwrap() > 0, "it did look");
}

/// A depth too shallow to reach the repos is the same failure wearing a
/// different hat, and it is the one that produced `0 copies / 0 distinct`.
#[test]
fn a_too_shallow_depth_finds_nothing_and_says_so() {
    let t = Tree::new("deep");
    t.managed_repo("one/two/three/four");
    let deep = json(&["--root", t.path().to_str().unwrap(), "--depth", "6"]);
    assert_eq!(deep["git_dirs_found"], 1, "reachable at depth 6");

    let r = run(&["--root", t.path().to_str().unwrap(), "--depth", "1"]);
    assert_eq!(r.code, 1);
    assert!(r.stdout.contains("SCAN FAILURE"), "{}", r.stdout);
    assert!(
        r.stdout.contains("currently: 1"),
        "must show the depth it used: {}",
        r.stdout
    );
}

#[test]
fn excluded_subtrees_are_skipped_and_counted() {
    let t = Tree::new("excluded");
    t.managed_repo("app");
    t.managed_repo("app/node_modules/pkg");
    let v = json(&["--root", t.path().to_str().unwrap()]);
    assert_eq!(v["git_dirs_found"], 1, "the vendored repo must not count");
    assert!(
        v["excluded_dirs"].as_u64().unwrap() >= 1,
        "and the skip must be recorded, not silent"
    );
}

/// Repos are reported relative to the root, because that is what a human
/// recognises, and sorted so output is stable between runs.
#[test]
fn paths_are_root_relative_and_sorted() {
    let t = Tree::new("sorted");
    t.managed_repo("zeta")
        .managed_repo("alpha")
        .managed_repo("mid");
    let v = json(&["--root", t.path().to_str().unwrap()]);
    let paths: Vec<&str> = v["repos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();
    // Single components, so no separator appears — safe to compare literally.
    assert_eq!(paths, vec!["alpha", "mid", "zeta"]);
}

#[test]
fn a_root_that_does_not_exist_is_rejected_before_scanning() {
    let r = run(&["--root", "/definitely/not/here"]);
    assert_eq!(r.code, 2, "usage error, distinct from an empty scan");
}

/// The JSON is the contract the TUI renders over, so its shape is pinned.
#[test]
fn json_carries_every_counter() {
    let t = Tree::new("shape");
    t.managed_repo("a");
    let v = json(&["--root", t.path().to_str().unwrap()]);
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
        assert!(!v[k].is_null(), "missing {k} from the scan contract");
    }
    // Per-repo, the two fields a reader needs to tell "we did not act on this"
    // from "there was nothing to do".
    assert_eq!(v["repos"][0]["hooks_dir"]["where"], "in");
    assert!(
        v["repos"][0].get("shares_hooks_with").is_some(),
        "shares_hooks_with must be present (null), not absent: {}",
        v["repos"][0]
    );
}

/// A repo four directories down must be found at the default depth.
///
/// Not hypothetical: `Perso/jellyfish/tuna/Coinbase-OAuth2` sat at that depth
/// and was invisible to every sweep, because `propagate.sh` and the hand-run
/// verification both used `find -maxdepth 6` while its hook files are eight
/// path components deep. It is still running pre-migration zsh hooks. The whole
/// fleet was reported consistent while one repo had never been touched.
#[test]
fn a_deeply_nested_repo_is_found_at_default_depth() {
    let t = Tree::new("nested");
    t.managed_repo("Perso/group/project/sub");
    let v = json(&["--root", t.path().to_str().unwrap()]);
    assert_eq!(v["git_dirs_found"], 1, "default depth must reach it");
    // Built through PathBuf: paths serialise with the platform's separator, so
    // a hardcoded "a/b" passes on Unix and fails on Windows for no real reason.
    let expected = PathBuf::from("Perso")
        .join("group")
        .join("project")
        .join("sub");
    assert_eq!(v["repos"][0]["path"], expected.to_string_lossy().as_ref());
}

/// Files nothing dispatches any more: they look installed and never run.
#[test]
fn stale_and_foreign_hook_files_are_reported_separately() {
    let t = Tree::new("leftovers");
    t.managed_repo("a");
    let hooks = t.path().join("a/.git/hooks");
    // Ours, but no longer shipped (a retired per-check shim).
    std::fs::write(
        hooks.join("pre-commit-ruff"),
        "#!/bin/sh\n# git-templates hook shim.\nexec x --hooks-dir y pre-commit-ruff\n",
    )
    .unwrap();
    // Somebody's own sub-hook: not ours, and now dispatched by nothing.
    std::fs::write(hooks.join("pre-push-mine.sh"), "#!/bin/sh\necho hi\n").unwrap();
    std::fs::write(hooks.join("package.json"), "{\"//\":\"Forces Node\"}").unwrap();

    let v = json(&["--root", t.path().to_str().unwrap()]);
    let r = &v["repos"][0];
    assert_eq!(r["stale_ours"][0], "pre-commit-ruff");
    assert_eq!(r["foreign_subs"][0], "pre-push-mine.sh");
    assert_eq!(r["hook_pkgjson"], true);
}

/// "Ours" is answered by the shim marker, not by grepping for `--hooks-dir` —
/// a hand-written hook that happens to mention that flag (in a comment, or
/// forwarding it to another tool) is not ours, and must not be classified as
/// a stale shim of ours: that classification feeds `fix --apply`'s removal
/// list directly.
#[test]
fn a_hook_merely_mentioning_hooks_dir_is_not_classified_as_ours() {
    let t = Tree::new("mentions-flag");
    t.managed_repo("a");
    let hooks = t.path().join("a/.git/hooks");
    std::fs::write(
        hooks.join("pre-commit-mine"),
        "#!/bin/sh\n# forwards --hooks-dir to some other tool of mine\nexec my-own-tool \"$@\"\n",
    )
    .unwrap();

    let v = json(&["--root", t.path().to_str().unwrap()]);
    let r = &v["repos"][0];
    assert_eq!(
        r["stale_ours"].as_array().map(|a| a.len()).unwrap_or(0),
        0,
        "a hook that only MENTIONS --hooks-dir was classified as ours: {r}"
    );
    assert_eq!(r["foreign_subs"][0], "pre-commit-mine");
}

/// A shim installed by `make install` must classify as OK, not drifted — the
/// failure that would make the tool cry wolf on all 96 repos.
#[test]
fn a_baked_shim_is_ok_and_a_hand_edited_one_is_not() {
    let t = Tree::new("baked");
    let hooks = t.path().join("r/.git/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let template = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../templates/hooks/pre-commit"
    ))
    .unwrap();
    for n in [
        "commit-msg",
        "post-commit",
        "pre-commit",
        "pre-push",
        "prepare-commit-msg",
    ] {
        std::fs::write(
            hooks.join(n),
            template.replace("__AMONT_BIN__", "/opt/amont"),
        )
        .unwrap();
    }
    let v = json(&["--root", t.path().to_str().unwrap(), "--depth", "3"]);
    let r = &v["repos"][0];
    assert!(
        r["shims"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["state"] == "ok"),
        "baked shims must not read as drift: {:?}",
        r["shims"]
    );
    assert_eq!(
        r["baked"]["bake"], "stale",
        "and /opt is not where we install"
    );

    std::fs::write(hooks.join("pre-push"), "#!/bin/sh\necho tampered\n").unwrap();
    let v = json(&["--root", t.path().to_str().unwrap(), "--depth", "3"]);
    let states: Vec<&str> = v["repos"][0]["shims"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["state"].as_str().unwrap())
        .collect();
    assert!(
        states.contains(&"drifted"),
        "real drift must show: {states:?}"
    );
}

/// Declared checks were invisible to the fleet view: a repository could run a
/// command on every commit that no column mentioned. The scanner reads the same
/// file the dispatcher does, with the same parser.
#[test]
fn a_declared_check_reaches_the_scan() {
    let t = Tree::new("declared");
    t.managed_repo("a").manifest(
        "a",
        "pre-commit  shellcheck  *.sh  block  scripts/lint-shell.sh\n",
    );
    let v = json(&["--root", t.path().to_str().unwrap()]);
    let d = &v["repos"][0]["declared"][0];
    assert_eq!(d["name"], "shellcheck");
    assert_eq!(d["stage"], "pre-commit");
    // A tagged sum, so the usable fields exist only when the line is usable.
    assert_eq!(d["state"], "usable");
    assert_eq!(d["severity"], "block");
    assert_eq!(d["command"], "scripts/lint-shell.sh");
    assert_eq!(d["exts"][0], ".sh");
}

/// The case worth scanning ninety-six repositories for: a line committed
/// months ago that has never once run.
#[test]
fn an_unusable_declaration_is_reported_as_such() {
    let t = Tree::new("declared-broken");
    t.managed_repo("a")
        .manifest("a", "pre-commit  shellcheck  *.sh  LOUD  make lint\n");
    let v = json(&["--root", t.path().to_str().unwrap()]);
    let d = &v["repos"][0]["declared"][0];
    assert_eq!(d["state"], "unusable", "{d}");
    let why = d["why"].as_str().expect("must say why");
    assert!(why.contains("severity"), "{why}");
    assert!(why.contains("line 1"), "{why}");
    // And it carries no command, because the type has nowhere to put one.
    assert!(d["command"].is_null(), "{d}");
}

/// Ninety-six repositories have no manifest, and that must cost nothing and
/// produce no rows.
#[test]
fn a_repo_without_a_manifest_declares_nothing() {
    let t = Tree::new("declared-none");
    t.managed_repo("a");
    let v = json(&["--root", t.path().to_str().unwrap()]);
    assert_eq!(
        v["repos"][0]["declared"].as_array().map(Vec::len),
        Some(0),
        "{}",
        v["repos"][0]
    );
}

/// A repo can point `core.hooksPath` anywhere; the scanner must look there,
/// not at `.git/hooks`. Otherwise a redirected repo reads as unmanaged (the
/// shim IS there, just not where this tool assumed) and `fix --apply` would
/// go on to WRITE at a path git never executes.
#[test]
fn core_hooks_path_is_honoured_not_assumed() {
    let t = Tree::new("hookspath");
    t.custom_hooks_path_repo("redirected", "tooling/hooks");
    let v = json(&["--root", t.path().to_str().unwrap()]);
    let repo = &v["repos"][0];
    assert_eq!(repo["managed"], true, "{repo}");
    // The default location was never created — a managed reading here would
    // mean the scan fell back to `.git/hooks` and got lucky, not that it
    // resolved `core.hooksPath`.
    assert!(!t.path().join("redirected/.git/hooks").exists());
}

/// The husky case, at scan level. `.husky/_` is inside the working tree, so
/// every containment test the scanner had said "in" — and it reported eleven
/// repositories as managed-but-drifted when what was true is that amont was not
/// running in any of them.
///
/// A hook manager that REGENERATES its directory is the distinguishing fact, and
/// `redirect_is_hostile` reads it off the destination. So this must report
/// `redirected`, `hostile`, and NOT managed.
#[test]
fn a_husky_hooks_path_is_reported_as_hostile_not_managed() {
    let t = Tree::new("hookspath-husky");
    t.custom_hooks_path_repo("redirected", ".husky/_");
    let v = json(&["--root", t.path().to_str().unwrap()]);
    let repo = &v["repos"][0];
    assert_eq!(repo["hooks_dir"]["where"], "redirected", "{repo}");
    assert_eq!(repo["hooks_dir"]["hostile"], true, "{repo}");
    assert_eq!(
        repo["managed"], false,
        "a repository whose dispatch husky owns is not managed by us: {repo}"
    );
}

/// The other half of the same predicate, and the reason it is not simply
/// "refuse every `core.hooksPath`".
///
/// A repository that deliberately keeps its hooks in `tooling/hooks` has chosen
/// a location, not handed dispatch to a tool that will overwrite it. Nothing
/// regenerates that directory, so there is nothing to protect it from — it stays
/// benign, stays managed, and `fix` still writes there.
#[test]
fn a_deliberate_in_repo_hooks_path_stays_benign() {
    let t = Tree::new("hookspath-benign");
    t.custom_hooks_path_repo("redirected", "tooling/hooks");
    let v = json(&["--root", t.path().to_str().unwrap()]);
    let repo = &v["repos"][0];
    assert_eq!(repo["hooks_dir"]["where"], "redirected", "{repo}");
    assert_eq!(repo["hooks_dir"]["hostile"], false, "{repo}");
    assert_eq!(repo["managed"], true, "{repo}");
}

/// `core.hooksPath` reaching a repo through an `include`, not a literal key
/// in its own `.git/config` — a local-file-only shortcut would see nothing
/// to act on here and fall back to `.git/hooks`, wrongly.
#[test]
fn core_hooks_path_via_an_include_is_still_honoured() {
    let t = Tree::new("hookspath-include");
    t.hooks_path_via_include_repo("redirected", "tooling/hooks");
    let v = json(&["--root", t.path().to_str().unwrap()]);
    let repo = &v["repos"][0];
    assert_eq!(repo["managed"], true, "{repo}");
    assert!(!t.path().join("redirected/.git/hooks").exists());
}

/// THE invisibility bug. A submodule's `.git` is a FILE holding `gitdir: …`,
/// and the walk tested `path.is_dir()` before it looked at the name — so every
/// submodule on the machine was not scanned, not installed into, and NOT
/// COUNTED AS UNCOVERED. The dashboard reported a clean fleet while commits
/// inside a submodule ran no checks at all, which is the same failure the
/// scanner's module doc opens with, one file type along.
///
/// And it must be installable, at the resolved location: a submodule's hooks
/// live in the SUPERPROJECT's `.git/modules/<name>/hooks`, so an install that
/// wrote to `<super>/sub/.git/hooks` would create a directory git never reads
/// and report success.
#[test]
fn a_submodule_is_found_counted_and_installed_at_its_real_hooks_dir() {
    let t = Tree::new("submodule");
    let sup = t.real_repo("sup");
    let src = t.real_repo("src");
    git(
        &sup,
        &["submodule", "add", "-q", src.to_str().unwrap(), "sub"],
    );
    git(&sup, &["commit", "-q", "--no-verify", "-m", "chore: sub"]);
    assert!(
        sup.join("sub/.git").is_file(),
        "fixture: a submodule's .git must be a FILE, not a directory"
    );

    let v = json(&["--root", t.path().to_str().unwrap()]);
    let paths: Vec<&str> = v["repos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();
    let sub_rel = PathBuf::from("sup").join("sub");
    assert!(
        paths.contains(&sub_rel.to_string_lossy().as_ref()),
        "the submodule was invisible to the scan: {paths:?}"
    );
    assert_eq!(
        v["git_dirs_found"], 3,
        "sup, src and the submodule — a `.git` file counts: {v}"
    );
    assert_eq!(
        v["unmanaged_seen"], 3,
        "and an invisible repo cannot be counted as uncovered"
    );

    // Installing must land where git will actually read it.
    let binary = t.path().join("fake-amont");
    std::fs::write(&binary, "#!/bin/sh\nexit 0\n").unwrap();
    let out = Command::new(bin())
        .args(["install", "--root"])
        .arg(t.path())
        .arg("--binary")
        .arg(&binary)
        .output()
        .expect("install");
    assert!(out.status.success(), "{out:?}");
    let real = sup.join(".git/modules/sub/hooks/pre-commit");
    assert!(
        real.is_file(),
        "the submodule's shim must land in .git/modules/sub/hooks, not beside its .git file:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !sup.join("sub/.git/hooks").exists(),
        "and nothing may be created next to the .git FILE"
    );
}

/// A linked worktree reaches the SAME hooks directory as its main repo. Once
/// `.git` files are recognised, both show up in `repos` — so exactly one of them
/// must own the hooks, or `fix --apply` writes four files twice and reports
/// eight, which is the shape of the `192 removals across 96 repos` number this
/// crate's module doc opens with.
#[test]
fn a_linked_worktree_shares_its_hooks_rather_than_doubling_them() {
    let t = Tree::new("worktree");
    let main = t.real_repo("main");
    git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            t.path().join("wt").to_str().unwrap(),
            "-b",
            "side",
        ],
    );

    let v = json(&["--root", t.path().to_str().unwrap()]);
    let repos = v["repos"].as_array().unwrap();
    assert_eq!(repos.len(), 2, "both are repositories: {v}");
    let shared: Vec<&serde_json::Value> = repos
        .iter()
        .filter(|r| !r["shares_hooks_with"].is_null())
        .collect();
    assert_eq!(
        shared.len(),
        1,
        "exactly one of the pair must defer to the other: {v}"
    );
    assert_eq!(
        shared[0]["shares_hooks_with"], "main",
        "and it must name the repository that owns the hooks: {}",
        shared[0]
    );

    // The plan for the deferring one is empty — not refused, because nothing is
    // wrong with it.
    let binary = t.path().join("fake-amont");
    std::fs::write(&binary, "#!/bin/sh\nexit 0\n").unwrap();
    let plans = json(&[
        "fix",
        "--root",
        t.path().to_str().unwrap(),
        "--binary",
        binary.to_str().unwrap(),
    ]);
    let deferring = plans
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["repo"] == shared[0]["path"])
        .expect("planned");
    assert!(
        deferring["write"].as_array().unwrap().is_empty()
            && deferring["remove"].as_array().unwrap().is_empty()
            && deferring["refuse"].as_array().unwrap().is_empty(),
        "a repo covered by its sibling plans nothing at all: {deferring}"
    );
}

/// `core.hooksPath` may be an ABSOLUTE path anywhere on the disk, and
/// `Intent::Activate` used to `create_dir_all` whatever came back and write four
/// 0o755 files into it. So a scanned repository could name any directory it
/// liked and a fleet-wide install would create and populate it.
///
/// The assertion that matters is the last one: after a full `fix --apply`, the
/// directory the repository named STILL DOES NOT EXIST.
#[test]
fn a_hooks_path_outside_the_repo_is_reported_and_never_created() {
    let t = Tree::new("hookspath-outside");
    let repo = t.real_repo("redirected");
    let elsewhere = t.path().join(format!("elsewhere-{}", std::process::id()));
    git(
        &repo,
        &["config", "core.hooksPath", elsewhere.to_str().unwrap()],
    );

    let v = json(&["--root", t.path().to_str().unwrap()]);
    let r = &v["repos"][0];
    assert_eq!(r["hooks_dir"]["where"], "outside", "{r}");
    assert_eq!(v["hooks_outside_seen"], 1, "and it must be counted: {v}");
    assert_eq!(
        r["managed"], false,
        "a repo we will not look inside is never managed: {r}"
    );

    let binary = t.path().join("fake-amont");
    std::fs::write(&binary, "#!/bin/sh\nexit 0\n").unwrap();
    let out = Command::new(bin())
        .args(["fix", "--apply", "--root"])
        .arg(t.path())
        .arg("--binary")
        .arg(&binary)
        .output()
        .expect("apply");
    assert!(
        !elsewhere.exists(),
        "fix --apply created a directory a scanned repository named:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );

    // Install is the mode that used to create it, so it is checked separately.
    let out = Command::new(bin())
        .args(["install", "--root"])
        .arg(t.path())
        .arg("--binary")
        .arg(&binary)
        .output()
        .expect("install");
    assert!(
        !elsewhere.exists(),
        "install created a directory a scanned repository named:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// A dispatcher that is a SYMLINK read as a perfectly healthy shim, because
/// `read_to_string` follows links and reported the target's bytes. Nothing in
/// the dashboard or the plan could see that a write there lands somewhere else —
/// which is the verified incident: four tracked files rewritten, reported as
/// "4 written".
#[cfg(unix)]
#[test]
fn a_symlinked_dispatcher_is_reported_as_a_symlink_not_as_ok() {
    let t = Tree::new("symlinked-dispatcher");
    let hooks = t.path().join("r/.git/hooks");
    let shared = t.path().join("r/devhooks");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::write(shared.join("pre-commit"), shim_for("/opt/amont")).unwrap();
    std::os::unix::fs::symlink(shared.join("pre-commit"), hooks.join("pre-commit")).unwrap();

    let v = json(&["--root", t.path().to_str().unwrap()]);
    let r = &v["repos"][0];
    let states: Vec<&str> = r["shims"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["state"].as_str().unwrap())
        .collect();
    assert!(
        states.contains(&"symlink"),
        "a linked dispatcher must not read as a healthy shim: {states:?}"
    );
    // The deliberate consequence, stated: a repo whose dispatchers are links no
    // longer counts as managed, so `fix` reports it rather than writing through
    // the link.
    assert_eq!(r["managed"], false, "{r}");
}

/// A compiled hook — somebody's Go binary at `.git/hooks/pre-commit`, a
/// perfectly ordinary thing to have — is not valid UTF-8. `read_to_string`
/// returned `Err`, and every predicate in this crate answered that with
/// `unwrap_or(false)`: not ours for `is_managed`, and `Missing` for the shim
/// state, which is the value that makes `fix` decide to WRITE one.
#[test]
fn a_non_utf8_hook_is_never_ours_and_never_missing() {
    let t = Tree::new("binary-hook");
    let hooks = t.path().join("r/.git/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::write(
        hooks.join("pre-commit"),
        [0x7f, b'E', b'L', b'F', 0x02, 0x01, 0xff, 0xfe],
    )
    .unwrap();
    std::fs::write(
        hooks.join("pre-commit-compiled"),
        [0x7f, b'E', b'L', b'F', 0x02, 0x01, 0xff, 0xfe],
    )
    .unwrap();

    let v = json(&["--root", t.path().to_str().unwrap()]);
    let r = &v["repos"][0];
    assert_eq!(r["managed"], false, "a binary hook is not one of ours: {r}");
    // Index 2: shims sit in DISPATCHERS order, and `pre-commit` follows
    // `commit-msg` and `post-commit`.
    assert_eq!(
        r["shims"][2]["state"], "unreadable",
        "and it is not MISSING, which is what makes fix write: {r}"
    );
    assert_eq!(
        r["stale_ours"].as_array().map(Vec::len),
        Some(0),
        "a binary must never reach the removal list: {r}"
    );
}

/// A repo with no `AGENTS.md` at all is `missing`, not a problem to alarm on —
/// the pointer is opt-in, same posture as `amont.conf`.
#[test]
fn a_repo_without_agents_md_scans_as_missing() {
    let t = Tree::new("agents-md-missing");
    t.managed_repo("a");
    let v = json(&["--root", t.path().to_str().unwrap()]);
    assert_eq!(v["repos"][0]["agents_md"], "missing");
}

/// The exact block `amont agents-md` would write scans as up to date, so
/// a fleet-wide rollout knows which repos are already done.
#[test]
fn a_repo_with_the_generated_block_scans_as_up_to_date() {
    let t = Tree::new("agents-md-current");
    t.managed_repo("a")
        .agents_md("a", &amont_runtime::agents_md::generate_block());
    let v = json(&["--root", t.path().to_str().unwrap()]);
    assert_eq!(v["repos"][0]["agents_md"], "up_to_date");
}

/// A block that has drifted from what `agents-md` would generate today — the
/// case a fleet rollout exists to repair.
#[test]
fn a_repo_with_a_stale_block_scans_as_drifted() {
    let t = Tree::new("agents-md-drifted");
    t.managed_repo("a").agents_md(
        "a",
        &format!(
            "{}\nstale content from an older version\n{}\n",
            amont_runtime::agents_md::START,
            amont_runtime::agents_md::END
        ),
    );
    let v = json(&["--root", t.path().to_str().unwrap()]);
    assert_eq!(v["repos"][0]["agents_md"], "drifted");
}
