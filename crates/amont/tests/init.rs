//! `amont init` — the verb a package manager calls.
//!
//! `"prepare": "amont init"` in a `package.json` means the hooks arrive with a
//! `npm install`, so these run it the way npm would: unattended, with no
//! terminal to answer a prompt, in whatever directory the install happened in.
//!
//! Every case here is one where `install` would have done the wrong thing —
//! written outside the repository, hung on `/dev/tty`, or failed an install
//! that had no business failing.

use std::path::{Path, PathBuf};
use std::process::Command;

struct Sandbox(PathBuf);

impl Sandbox {
    fn new(name: &str) -> Self {
        let d = std::env::temp_dir().join(format!("amont-init-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("mkdir");
        Sandbox(d)
    }
    fn path(&self, rel: &str) -> PathBuf {
        rel.split('/').fold(self.0.clone(), |p, c| p.join(c))
    }
    /// `init`, with HOME and XDG pointed inside the sandbox so a regression that
    /// reached for either is visible as a written file rather than as damage to
    /// the developer's real configuration.
    ///
    /// **stdin is closed.** npm's `prepare` inherits a terminal, and the whole
    /// reason `init` exists rather than `install` is that `install` opens
    /// `/dev/tty` and blocks. A test that left stdin attached would pass while
    /// the real thing hung.
    fn init(&self, cwd: &Path) -> (i32, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_amont"))
            .arg("init")
            .current_dir(cwd)
            .env("HOME", &self.0)
            .env("USERPROFILE", &self.0)
            .env("XDG_CONFIG_HOME", self.path(".config"))
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run amont init");
        (
            out.status.code().unwrap_or(-1),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(dir: &Path, args: &[&str]) {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git");
}

fn init_repo(dir: &Path) {
    std::fs::create_dir_all(dir).expect("mkdir");
    git(dir, &["init", "-q", "--template=", "."]);
    git(dir, &["config", "user.email", "t@t.test"]);
    git(dir, &["config", "user.name", "t"]);
}

const DISPATCHERS: [&str; 4] = ["commit-msg", "pre-commit", "pre-push", "prepare-commit-msg"];

#[test]
fn init_writes_the_four_shims_and_bakes_the_running_binary() {
    let s = Sandbox::new("basic");
    let repo = s.path("repo");
    init_repo(&repo);

    let (code, out) = s.init(&repo);
    assert_eq!(code, 0, "{out}");
    for name in DISPATCHERS {
        assert!(
            repo.join(".git/hooks").join(name).is_file(),
            "{name} missing:\n{out}"
        );
    }

    let hook = std::fs::read_to_string(repo.join(".git/hooks/pre-commit")).expect("read");
    assert!(
        !hook.contains("__AMONT_BIN__"),
        "shim was not baked:\n{hook}"
    );
    assert!(
        hook.contains(env!("CARGO_BIN_EXE_amont")),
        "baked something other than the running binary:\n{:?}",
        hook.lines().find(|l| l.starts_with("BAKED="))
    );
}

/// The three things `install` does that a `prepare` script must not.
///
/// Each was a reason `install` could not simply be pointed at by npm:
/// `install_binary` copies into `~/.local/bin`, `populate_template_dir` writes
/// the XDG template directory — which would arrange for every FUTURE clone on
/// the machine to get hooks — and `offer_trust` prompts.
///
/// HOME and XDG are inside the sandbox, so if any of them ran, the evidence is
/// a file sitting right here.
#[test]
fn init_touches_nothing_outside_the_repository() {
    let s = Sandbox::new("scoped");
    let repo = s.path("repo");
    init_repo(&repo);

    let (code, out) = s.init(&repo);
    assert_eq!(code, 0, "{out}");
    assert!(
        !s.path(".local/bin").exists(),
        "init copied a binary into ~/.local/bin:\n{out}"
    );
    assert!(
        !s.path(".config/git/git-templates").exists(),
        "init populated the git template dir — every future clone would get hooks:\n{out}"
    );
}

/// npm runs `prepare` where there is no repository: installing from a tarball,
/// inside a Docker build, in CI against an exported tree. Failing there would
/// make the package uninstallable in all three, so this is the one case that
/// exits 0 in silence.
#[test]
fn init_outside_a_repository_is_a_silent_success() {
    let s = Sandbox::new("norepo");
    let plain = s.path("plain");
    std::fs::create_dir_all(&plain).expect("mkdir");

    let (code, out) = s.init(&plain);
    assert_eq!(code, 0, "init must not fail where there is no .git:\n{out}");
    assert!(
        out.trim().is_empty(),
        "init should say nothing outside a repository, said:\n{out}"
    );
}

/// The silence above is git's VERDICT ("not a git repository"), never git's
/// absence. A repository git refuses to answer about — dubious ownership in a
/// container bind mount, an unreadable `.git/config` — used to take the same
/// silent exit 0, so `npm install` logged success, the team believed hooks
/// were wired, and commits from that environment ran no checks. The doc
/// promises init "stays LOUD about everything else"; this is the everything
/// else.
#[cfg(unix)]
#[test]
fn init_is_loud_when_git_refuses_to_answer() {
    use std::os::unix::fs::PermissionsExt;
    let s = Sandbox::new("gitrefuses");
    let repo = s.path("repo");
    init_repo(&repo);
    // An unreadable `.git/config` makes every git command fail with something
    // other than "not a git repository" — the same failure class as dubious
    // ownership, which needs a second uid to stage. Skip under root, where
    // chmod 000 stops nothing.
    let config = repo.join(".git/config");
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    if std::fs::read(&config).is_ok() {
        return; // running as root; the fixture cannot fail
    }

    let (code, out) = s.init(&repo);
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o644)).expect("chmod back");
    assert_ne!(
        code, 0,
        "a repository git cannot answer about must fail init, not silently skip it:\n{out}"
    );
    assert!(
        out.contains("git would not say"),
        "the failure must say git is the blocker:\n{out}"
    );
    assert!(
        !repo.join(".git/hooks/pre-commit").exists(),
        "no shim may be written on a guess"
    );
}

/// `npm install` runs `prepare` every time, so this is the common path, not an
/// edge case. It must also RE-BAKE rather than skip: a version bump moves the
/// binary, and a shim still pointing at the old path is a repository whose
/// hooks stop resolving.
#[test]
fn init_is_idempotent_and_rebakes() {
    let s = Sandbox::new("twice");
    let repo = s.path("repo");
    init_repo(&repo);

    assert_eq!(s.init(&repo).0, 0);
    let first = std::fs::read_to_string(repo.join(".git/hooks/pre-commit")).expect("read");

    // A shim baked at a path that no longer exists — what a version bump leaves
    // behind. The second run must replace it, not decide there is nothing to do.
    std::fs::write(
        repo.join(".git/hooks/pre-commit"),
        first.replace(env!("CARGO_BIN_EXE_amont"), "/nonexistent/old/amont"),
    )
    .expect("write");

    let (code, out) = s.init(&repo);
    assert_eq!(code, 0, "{out}");
    let second = std::fs::read_to_string(repo.join(".git/hooks/pre-commit")).expect("read");
    assert_eq!(first, second, "a stale baked path was left in place");
}

/// Hooks are shared across worktrees. A `prepare` run from a linked worktree —
/// ordinary, since that is where the work happens — must bake into the common
/// directory git actually dispatches from, not the worktree's private gitdir.
#[test]
fn init_from_a_linked_worktree_bakes_into_the_shared_dir() {
    let s = Sandbox::new("worktree");
    let repo = s.path("repo");
    init_repo(&repo);
    git(&repo, &["commit", "--allow-empty", "-qm", "seed"]);
    let wt = s.path("repo-wt");
    let out = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["worktree", "add", "-q", "-b", "feature"])
        .arg(&wt)
        .output()
        .expect("git worktree add");
    assert!(out.status.success(), "worktree add failed");

    let (code, out) = s.init(&wt);
    assert_eq!(code, 0, "{out}");
    assert!(
        repo.join(".git/hooks/pre-commit").is_file(),
        "did not bake into the shared hooks dir:\n{out}"
    );
    assert!(
        !repo.join(".git/worktrees/feature/hooks").exists(),
        "baked into the worktree-private gitdir, where git never looks"
    );
}

/// The husky collision, from the `prepare` side.
///
/// This is the state a repository is in DURING a migration: husky still owns
/// `core.hooksPath`, and `package.json` already says `amont init`. Writing
/// either directory would be wrong — `.git/hooks` is not dispatched from, and
/// `.husky/_` is regenerated by husky's own prepare — so it fails loudly, which
/// is what makes the half-finished migration visible instead of silent.
#[test]
fn init_refuses_a_repository_husky_still_owns() {
    let s = Sandbox::new("husky");
    let repo = s.path("repo");
    init_repo(&repo);
    std::fs::create_dir_all(repo.join(".husky/_")).expect("mkdir");
    git(&repo, &["config", "core.hooksPath", ".husky/_"]);

    let (code, out) = s.init(&repo);
    assert_ne!(code, 0, "init wrote into a husky repository:\n{out}");
    assert!(out.contains("husky"), "must name the tool:\n{out}");
    assert!(
        out.contains("git config --unset core.hooksPath"),
        "must name the remedy:\n{out}"
    );
    assert!(
        !repo.join(".git/hooks/pre-commit").exists(),
        "wrote a shim git would never dispatch"
    );
    assert!(
        !repo.join(".husky/_/pre-commit").exists(),
        "wrote into the directory husky regenerates"
    );
}

/// A hook somebody wrote themselves is theirs. `init` has no `--force` and must
/// not behave as though it did: it runs unattended, so there is nobody who
/// could have decided that file was replaceable.
#[test]
fn init_refuses_to_overwrite_a_hook_it_did_not_write() {
    let s = Sandbox::new("foreign");
    let repo = s.path("repo");
    init_repo(&repo);
    let hooks = repo.join(".git/hooks");
    std::fs::create_dir_all(&hooks).expect("mkdir");
    let mine = "#!/bin/sh\n# mine, thanks\nexit 0\n";
    std::fs::write(hooks.join("pre-commit"), mine).expect("write");

    let (code, out) = s.init(&repo);
    assert_ne!(code, 0, "init overwrote a foreign hook:\n{out}");
    assert_eq!(
        std::fs::read_to_string(hooks.join("pre-commit")).expect("read"),
        mine,
        "somebody's own pre-commit was replaced"
    );
    // Fail closed for the whole repository: three shims plus one refusal is the
    // state where half the checks run and nothing says so.
    for name in ["commit-msg", "pre-push", "prepare-commit-msg"] {
        assert!(
            !hooks.join(name).exists(),
            "{name} was written despite the refusal — a half-installed repo"
        );
    }
}

/// The hooks `init` writes must actually run. Everything above asserts on files;
/// this one commits.
#[test]
fn the_hooks_init_writes_run_a_real_commit() {
    let s = Sandbox::new("real-commit");
    let repo = s.path("repo");
    init_repo(&repo);
    let (code, out) = s.init(&repo);
    assert_eq!(code, 0, "{out}");

    std::fs::write(repo.join("a.txt"), "hello\n").expect("write");
    git(&repo, &["add", "a.txt"]);
    let out = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["commit", "-m", "feat: add a file"])
        .env("HOME", &s.0)
        .output()
        .expect("git commit");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "the commit was rejected:\n{text}");
    // A hook that ran says so. A silent pass is indistinguishable from a shim
    // git never invoked, which is the failure this whole change is about.
    assert!(
        text.contains('✓') || text.to_lowercase().contains("passed"),
        "no sign a check ran:\n{text}"
    );
}
