//! The onboarding journey: how amont arrives in a repository, what it says
//! is in effect there, and what taking it out leaves behind.
//!
//! Three ways in, three flows that run side by side, each on a machine of
//! its own: a temp directory standing in for HOME, USERPROFILE and
//! XDG_CONFIG_HOME, so a bug can never reach the developer's real
//! configuration and whatever a step wrote outside the repository is a file
//! sitting right there to assert on. stdin is closed, as it is for npm's
//! `prepare`, so nothing can stop on a prompt.
//!
//! 1. `amont init`, the verb a package manager calls: outside a repository
//!    it is silent; in one it bakes the six shims and touches nothing else;
//!    they run a real commit; it re-bakes a stale shim; from a linked
//!    worktree it bakes the shared directory.
//! 2. `amont install` and `amont uninstall`, a person's own machine: the
//!    binary, the template directory and the repository's shims, twice; a
//!    real commit; AGENTS.md offered and never written unasked, then
//!    written by `amont agents-md` and not offered again; uninstall takes
//!    back our shims, trust and bookkeeping and nothing of the person's,
//!    takes back the template directory and names the standing grant, keeps
//!    the binary unless asked; installing outside a repository, an
//!    unreadable hook named, and an install from a linked worktree.
//! 3. `amont enroll` (unix), the machine-wide grant: a bad flag writes
//!    nothing; `init.templateDir` is pointed and re-pointing is a no-op; a
//!    plain `git init` arrives with working hooks; `--conventions declared`
//!    and a clone carrying an `amont.conf`, as `amont list --json` reports
//!    them; somebody else's template directory is refused; and how a shim
//!    that was never baked finds (and refuses to find) the binary.
//!
//! Trust and what a manifest's declarations do are the declared journey's.

use crate::common::{output_retrying_etxtbsy, Repo};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const DISPATCHERS: [&str; 6] = [
    "commit-msg",
    "post-commit",
    "post-rewrite",
    "pre-commit",
    "pre-push",
    "prepare-commit-msg",
];

/// The binary under test, as the shims must name it.
const ME: &str = env!("CARGO_BIN_EXE_amont");

/// A person's machine: a temp directory that is their HOME.
struct Machine {
    root: PathBuf,
    /// `GIT_CONFIG_GLOBAL`, pinned to a file inside the machine as the belt
    /// to HOME's suspenders (enroll writes the global config on purpose).
    global: Option<PathBuf>,
}

impl Machine {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "amont-onboarding-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        Machine { root, global: None }
    }

    /// A machine whose global git config is one named file.
    #[cfg(unix)]
    fn pinned(tag: &str) -> Self {
        let mut m = Machine::new(tag);
        m.global = Some(m.path("gitconfig"));
        m
    }

    /// Joined component by component, NOT `join("a/b")`: on Windows `join`
    /// keeps a forward slash it was handed, and two spellings of one path
    /// then compare unequal for a reason that is not the product's.
    fn path(&self, rel: &str) -> PathBuf {
        rel.split('/').fold(self.root.clone(), |p, c| p.join(c))
    }

    /// `program` from `cwd` on this machine: no `GIT_*` from the process
    /// running the suite, HOME and XDG here, stdin closed.
    fn cmd(&self, program: impl AsRef<OsStr>, cwd: &Path) -> Command {
        let mut c = Command::new(program);
        c.current_dir(cwd).stdin(Stdio::null());
        Repo::strip_git_env_impl(&mut c);
        Repo::pin_host_env(&mut c);
        c.env("HOME", &self.root)
            .env("USERPROFILE", &self.root)
            .env("XDG_CONFIG_HOME", self.path(".config"));
        if let Some(g) = &self.global {
            c.env("GIT_CONFIG_GLOBAL", g);
        }
        c
    }

    /// Exit code, then stdout and stderr together.
    fn said(mut cmd: Command) -> (i32, String) {
        let out = output_retrying_etxtbsy(&mut cmd).expect("run");
        (
            out.status.code().unwrap_or(-1),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }

    fn amont(&self, cwd: &Path, args: &[&str]) -> (i32, String) {
        let mut c = self.cmd(ME, cwd);
        c.args(args);
        Self::said(c)
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> (i32, String) {
        let mut c = self.cmd("git", cwd);
        c.args(args);
        Self::said(c)
    }

    /// A repository with no hooks and an identity, the way a clone made
    /// before amont looks.
    fn repo(&self, name: &str) -> PathBuf {
        let dir = self.path(name);
        std::fs::create_dir_all(&dir).expect("mkdir");
        for args in [
            &["init", "-q", "--template=", "."][..],
            &["config", "user.email", "t@t.test"],
            &["config", "user.name", "t"],
        ] {
            let (code, out) = self.git(&dir, args);
            assert_eq!(code, 0, "git {args:?}:\n{out}");
        }
        dir
    }

    /// A linked worktree of `repo` on a new branch `feature`.
    fn worktree(&self, repo: &Path, name: &str) -> PathBuf {
        let wt = self.path(name);
        let (code, out) = self.git(
            repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feature",
                wt.to_str().expect("utf8"),
            ],
        );
        assert_eq!(code, 0, "git worktree add:\n{out}");
        wt
    }

    /// One key of the global git config, as git reads it here.
    fn global(&self, key: &str) -> Option<String> {
        let (code, out) = self.git(&self.root, &["config", "--global", "--get", key]);
        (code == 0).then(|| out.trim().to_string())
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn write(p: &Path, body: &str) {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).expect("mkdir");
    }
    std::fs::write(p, body).unwrap_or_else(|e| panic!("write {}: {e}", p.display()));
}

/// A real `git commit` of one new file, through whatever hooks are there.
fn commit_a_file(m: &Machine, repo: &Path, msg: &str) -> (i32, String) {
    write(&repo.join("a.txt"), "hello\n");
    let (code, out) = m.git(repo, &["add", "a.txt"]);
    assert_eq!(code, 0, "git add:\n{out}");
    m.git(repo, &["commit", "-m", msg])
}

// ---- 1. amont init -----------------------------------------------------------

/// `"prepare": "amont init"` in a `package.json` means the hooks arrive
/// with `npm install`, so this runs it the way npm would: unattended, no
/// terminal, in whatever directory the install happened in. The refusals
/// (a husky redirect, a foreign hook, git refusing to answer, a push
/// snapshot) keep their own fixtures in `init.rs`.
#[test]
fn the_init_journey() {
    let m = Machine::new("init");

    // npm runs `prepare` where there is no repository: from a tarball, in a
    // Docker build. Failing there would make the package uninstallable.
    let plain = m.path("plain");
    std::fs::create_dir_all(&plain).expect("mkdir");
    let (code, out) = m.amont(&plain, &["init"]);
    step_eq!(
        "outside a repository",
        code,
        0,
        "init must not fail where there is no .git:\n{out}"
    );
    step!(
        "outside a repository",
        out.trim().is_empty(),
        "init should say nothing outside a repository, said:\n{out}"
    );

    // The six shims, baked with the running binary.
    let repo = m.repo("repo");
    let hooks = repo.join(".git").join("hooks");
    let (code, out) = m.amont(&repo, &["init"]);
    step_eq!("init", code, 0, "{out}");
    for name in DISPATCHERS {
        step!("init", hooks.join(name).is_file(), "{name} missing:\n{out}");
    }
    let first = read(&hooks.join("pre-commit"));
    step!(
        "init",
        !first.contains("__AMONT_BIN__"),
        "shim was not baked:\n{first}"
    );
    step!(
        "init",
        first.contains(ME),
        "baked something other than the running binary:\n{:?}",
        first.lines().find(|l| l.starts_with("BAKED="))
    );

    // What `install` does and a `prepare` script must not: a binary in
    // ~/.local/bin, or a template dir that hooks every FUTURE clone.
    step!(
        "nothing outside",
        !m.path(".local/bin").exists(),
        "init copied a binary into ~/.local/bin:\n{out}"
    );
    step!(
        "nothing outside",
        !m.path(".config/git/git-templates").exists(),
        "init populated the git template dir — every future clone would get hooks:\n{out}"
    );

    // The shims run. A hook that ran says so: a silent pass is the same as
    // a shim git never invoked.
    let (code, out) = commit_a_file(&m, &repo, "feat: add a file");
    step_eq!("commit", code, 0, "the commit was rejected:\n{out}");
    step!(
        "commit",
        out.contains('✓') || out.to_lowercase().contains("passed"),
        "no sign a check ran:\n{out}"
    );

    // A version bump moves the binary: `prepare` runs again and must
    // re-bake a shim pointing at the old path, not skip it.
    write(
        &hooks.join("pre-commit"),
        &first.replace(ME, "/nonexistent/old/amont"),
    );
    let (code, out) = m.amont(&repo, &["init"]);
    step_eq!("re-bake", code, 0, "{out}");
    step_eq!(
        "re-bake",
        read(&hooks.join("pre-commit")),
        first,
        "a stale baked path was left in place"
    );

    // Hooks are shared across worktrees: `prepare` run in a linked one bakes
    // the common directory git dispatches from, never the worktree's own.
    // Stale first, so only this run can make it current.
    write(
        &hooks.join("pre-commit"),
        &first.replace(ME, "/nonexistent/old/amont"),
    );
    let wt = m.worktree(&repo, "repo-wt");
    let (code, out) = m.amont(&wt, &["init"]);
    step_eq!("worktree", code, 0, "{out}");
    step_eq!(
        "worktree",
        read(&hooks.join("pre-commit")),
        first,
        "did not bake into the shared hooks dir:\n{out}"
    );
    step!(
        "worktree",
        !repo.join(".git/worktrees/feature/hooks").exists(),
        "baked into the worktree-private gitdir, where git never looks"
    );
}

// ---- 2. amont install and uninstall --------------------------------------------

/// The binary the installer placed, whichever platform this is.
fn installed_bin(m: &Machine) -> Option<PathBuf> {
    ["amont", "amont.exe"]
        .into_iter()
        .map(|n| m.path(".local/bin").join(n))
        .find(|p| p.is_file())
}

/// A person's own machine, one repository. The refusals (redirects,
/// symlinks, unwritable dirs, foreign hooks and `--force`) and where the
/// binary is taken from (PATH, `AMONT_BIN_DIR`, a build dir) keep their own
/// fixtures in `install.rs`.
#[test]
fn the_install_journey() {
    let m = Machine::new("install");
    let repo = m.repo("repo");
    let hooks = repo.join(".git").join("hooks");
    let templates = m.path(".config/git/git-templates/templates/hooks");

    // The binary, the template dir, the repository's six shims: baked, not
    // merely copied, since the placeholder is what a GUI client cannot
    // resolve. No terminal, so the AGENTS.md question goes unanswered and
    // nothing is written.
    let (code, out) = m.amont(&repo, &["install"]);
    step_eq!("install", code, 0, "{out}");
    let bin = installed_bin(&m)
        .unwrap_or_else(|| panic!("step `install`: no binary in ~/.local/bin:\n{out}"));
    for name in DISPATCHERS {
        step!(
            "install",
            templates.join(name).is_file(),
            "template {name} missing:\n{out}"
        );
        step!(
            "install",
            hooks.join(name).is_file(),
            "repo hook {name} missing:\n{out}"
        );
    }
    let first = read(&hooks.join("pre-commit"));
    step!(
        "install",
        !first.contains("__AMONT_BIN__"),
        "shim was not baked"
    );
    step!(
        "install",
        first.contains(bin.to_str().expect("utf8")),
        "shim names {:?}, not the installed {}",
        first.lines().find(|l| l.contains("amont")),
        bin.display()
    );
    step!(
        "install",
        !repo.join("AGENTS.md").exists(),
        "an unanswered prompt must not write anything"
    );

    // Twice is how a new build is picked up: a no-op, and still no AGENTS.md.
    let (code, out) = m.amont(&repo, &["install"]);
    step_eq!("again", code, 0, "a second install failed:\n{out}");
    step_eq!(
        "again",
        read(&hooks.join("pre-commit")),
        first,
        "a second install changed the shim"
    );
    step!("again", !repo.join("AGENTS.md").exists());

    let (code, out) = commit_a_file(&m, &repo, "feat: a commit through installed hooks");
    step_eq!("commit", code, 0, "commit failed:\n{out}");
    step!(
        "commit",
        !out.contains("amont binary not found"),
        "the installed shim could not resolve the binary:\n{out}"
    );

    // AGENTS.md: absent, written on request, then up to date — and an
    // install no longer offers what is already there.
    let (code, out) = m.amont(&repo, &["agents-md", "--check"]);
    step_eq!("agents-md", code, 0, "{out}");
    step!("agents-md", out.contains("not present"), "{out}");
    let (code, out) = m.amont(&repo, &["agents-md"]);
    step_eq!("agents-md", code, 0, "{out}");
    let agents = read(&repo.join("AGENTS.md"));
    step!(
        "agents-md",
        agents.contains("<!-- amont:start -->"),
        "{agents}"
    );
    step!(
        "agents-md",
        agents.contains("amont list --json"),
        "{agents}"
    );
    let (code, out) = m.amont(&repo, &["agents-md", "--check"]);
    step_eq!("agents-md", code, 0, "{out}");
    step!("agents-md", out.contains("up to date"), "{out}");
    let (code, out) = m.amont(&repo, &["install"]);
    step_eq!("agents-md", code, 0, "{out}");
    step!(
        "agents-md",
        !out.contains("AGENTS.md can point"),
        "an already up-to-date block must not be offered again:\n{out}"
    );

    // Uninstall takes what is ours and leaves what is the person's: their
    // own hook (`post-checkout`: `post-commit` is ours now), their policy,
    // their manifest, and work an interrupted run parked. Trust is consent
    // to run what a file names, and goes; so does bookkeeping about hooks
    // that are leaving. The binary is shared with every other repository.
    let theirs = hooks.join("post-checkout");
    write(&theirs, "#!/bin/sh\necho theirs\n");
    let (code, out) = m.git(&repo, &["config", "hook.skip", "pre-commit-clippy"]);
    step_eq!("uninstall", code, 0, "{out}");
    write(&repo.join("amont.conf"), "skip yamllint\n");
    let (code, out) = m.amont(&repo, &["trust"]);
    step_eq!("uninstall", code, 0, "amont trust:\n{out}");
    let trusted = || {
        let (_, out) = m.git(&repo, &["config", "--local", "--get", "amont.trusted"]);
        out.trim().to_string()
    };
    step!("uninstall", !trusted().is_empty(), "fixture: trust granted");
    let git_dir = repo.join(".git");
    write(&git_dir.join("amont-held/index"), "amont-held-v1\0");
    write(&git_dir.join("amont-bypasses"), "amont-bypasses-v1\n");
    write(&git_dir.join("amont-skew"), "amont-skew-v1\n1.0.0\n");

    let (code, out) = m.amont(&repo, &["uninstall"]);
    step_eq!("uninstall", code, 0, "{out}");
    for name in DISPATCHERS {
        step!(
            "uninstall",
            !hooks.join(name).exists(),
            "{name} survived uninstall:\n{out}"
        );
    }
    step!(
        "uninstall",
        theirs.exists(),
        "a hook we did not write was deleted"
    );
    let (_, skips) = m.git(&repo, &["config", "--get-all", "hook.skip"]);
    step!(
        "uninstall",
        skips.contains("pre-commit-clippy"),
        "uninstall discarded the user's own policy"
    );
    step!(
        "uninstall",
        bin.exists(),
        "uninstall took the shared binary unasked"
    );
    step!(
        "uninstall",
        trusted().is_empty(),
        "trust survived uninstall — a reinstall would re-honour it:\n{out}"
    );
    step!(
        "uninstall",
        out.contains("forgot") && out.contains("trust"),
        "what was forgotten must be said out loud:\n{out}"
    );
    step!(
        "uninstall",
        git_dir.join("amont-held/index").exists(),
        "uninstall deleted PARKED WORK — the one thing it must never touch:\n{out}"
    );
    step!("uninstall", repo.join("amont.conf").exists(), "{out}");
    step!(
        "uninstall",
        !git_dir.join("amont-bypasses").exists(),
        "the bypass ledger outlived the hooks it describes:\n{out}"
    );
    step!("uninstall", !git_dir.join("amont-skew").exists(), "{out}");

    // Twice is how somebody makes sure.
    let (code, out) = m.amont(&repo, &["uninstall"]);
    step_eq!(
        "uninstall again",
        code,
        0,
        "second uninstall failed:\n{out}"
    );

    // With `init.templateDir` set, the template dir is what installs hooks
    // into every new clone: uninstall takes it back and names the grant,
    // which it leaves for the person to revoke.
    let (code, out) = m.amont(&repo, &["install"]);
    step_eq!("standing grant", code, 0, "{out}");
    for name in DISPATCHERS {
        step!(
            "standing grant",
            templates.join(name).is_file(),
            "install wrote no template shims"
        );
    }
    let grant = m.path(".config/git/git-templates");
    let (code, out) = m.git(
        &repo,
        &[
            "config",
            "--global",
            "init.templateDir",
            grant.to_str().expect("utf8"),
        ],
    );
    step_eq!("standing grant", code, 0, "{out}");
    let (code, out) = m.amont(&repo, &["uninstall"]);
    step_eq!("standing grant", code, 0, "{out}");
    for name in DISPATCHERS {
        step!(
            "standing grant",
            !templates.join(name).exists(),
            "{name} survived in the template dir — the next clone reinstalls it:\n{out}"
        );
    }
    step!(
        "standing grant",
        out.contains("init.templateDir"),
        "uninstall must name the setting that keeps installing hooks:\n{out}"
    );
    step!(
        "standing grant",
        out.contains("git config --global --unset init.templateDir"),
        "and give the command that revokes it:\n{out}"
    );

    // `--binary` is opt-in because other repositories use it.
    let (code, out) = m.amont(&repo, &["uninstall", "--binary"]);
    step_eq!("--binary", code, 0, "{out}");
    step!("--binary", !bin.exists(), "--binary did not remove it");

    // The person revokes the grant as told, and installs again from a plain
    // directory: the binary and the templates, and plainly no repo hooks.
    let (code, out) = m.git(
        &repo,
        &["config", "--global", "--unset", "init.templateDir"],
    );
    step_eq!("outside a repository", code, 0, "{out}");
    let plain = m.path("plain");
    std::fs::create_dir_all(&plain).expect("mkdir");
    let (code, out) = m.amont(&plain, &["install"]);
    step_eq!("outside a repository", code, 0, "{out}");
    step!(
        "outside a repository",
        installed_bin(&m).is_some(),
        "no binary installed:\n{out}"
    );
    step!(
        "outside a repository",
        out.contains("not inside a git repository"),
        "should say why no repo hooks were written:\n{out}"
    );

    // A foreign hook is "left alone and named", even one that is not text.
    let (code, out) = m.amont(&repo, &["install"]);
    step_eq!("unreadable hook", code, 0, "{out}");
    let elf = hooks.join("commit-msg");
    std::fs::write(&elf, [0x7f, b'E', b'L', b'F', 0xff, 0xfe]).expect("write");
    let (code, out) = m.amont(&repo, &["uninstall"]);
    step_eq!("unreadable hook", code, 0, "{out}");
    step!(
        "unreadable hook",
        out.contains("commit-msg") && out.contains("not valid UTF-8"),
        "an unreadable hook was passed over in silence:\n{out}"
    );
    step!(
        "unreadable hook",
        elf.exists(),
        "an unreadable hook was deleted"
    );
    std::fs::remove_file(&elf).expect("the person removes it");

    // Installed from a linked worktree: the shims land where git looks,
    // the common directory, never the worktree's private gitdir.
    let wt = m.worktree(&repo, "repo-wt");
    let (code, out) = m.amont(&wt, &["install"]);
    step_eq!("worktree", code, 0, "{out}");
    for name in DISPATCHERS {
        step!(
            "worktree",
            hooks.join(name).is_file(),
            "{name} missing from the common hooks dir:\n{out}"
        );
    }
    step!(
        "worktree",
        !repo.join(".git/worktrees/feature/hooks").exists(),
        "baked into the worktree-private gitdir instead of the shared one"
    );
}

// ---- 3. amont enroll -------------------------------------------------------------

/// `amont list --json` from `dir`, parsed.
#[cfg(unix)]
fn list_json(m: &Machine, dir: &Path) -> serde_json::Value {
    let mut c = m.cmd(ME, dir);
    c.args(["list", "--json"]);
    let out = c.output().expect("run amont list");
    let text = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("list --json: {e}\n{text}"))
}

/// The machine-wide grant, run once per machine so every FUTURE `git clone`
/// and `git init` arrives with the shims. Unix-gated as its suite was: the
/// commits go through the copied shims, which need `sh` and the execute bit.
#[cfg(unix)]
#[test]
fn the_enroll_journey() {
    let m = Machine::pinned("enroll");

    // A bad flag value writes NOTHING: an enroll that half-runs and then
    // rejects its own argument has still changed global state.
    let (code, out) = m.amont(&m.root, &["enroll", "--conventions", "sometimes"]);
    step!("bad value", code != 0, "{out}");
    step_eq!(
        "bad value",
        m.global("init.templateDir"),
        None,
        "nothing was granted"
    );
    step_eq!("bad value", m.global("amont.conventions"), None);

    // The grant reaches forward, the output says so, and existing clones get
    // a remedy.
    let (code, out) = m.amont(&m.root, &["enroll"]);
    step_eq!("enroll", code, 0, "{out}");
    let configured = m.global("init.templateDir").expect("templateDir set");
    step!(
        "enroll",
        Path::new(&configured)
            .join("hooks")
            .join("pre-commit")
            .is_file(),
        "the template dir it points at holds the shims: {configured}"
    );
    step!("enroll", out.contains("future"), "{out}");
    step!(
        "enroll",
        out.contains("amont init"),
        "existing clones get a remedy: {out}"
    );

    let (code, out) = m.amont(&m.root, &["enroll"]);
    step_eq!("again", code, 0, "{out}");
    step!("again", out.contains("already points here"), "{out}");

    // A plain `git init` arrives with working hooks: binary resolved, checks
    // run, a clean commit through.
    let fresh = m.path("fresh");
    std::fs::create_dir_all(&fresh).expect("mkdir");
    for args in [
        &["init", "-q", "--initial-branch=main"][..],
        &["config", "user.email", "t@t"],
        &["config", "user.name", "t"],
    ] {
        let (code, out) = m.git(&fresh, args);
        step_eq!("git init", code, 0, "git {args:?}:\n{out}");
    }
    step!(
        "git init",
        fresh.join(".git/hooks/pre-commit").is_file(),
        "the grant delivered the shims"
    );
    let (code, out) = commit_a_file(&m, &fresh, "feat: through the shim");
    step_eq!(
        "git init",
        code,
        0,
        "commit through the enrolled shim: {out}"
    );

    // A machine that clones other people's projects: house rules only where
    // a repository commits an amont.conf. `fresh` has none.
    let (code, out) = m.amont(&m.root, &["enroll", "--conventions", "declared"]);
    step_eq!("declared", code, 0, "{out}");
    step_eq!(
        "declared",
        m.global("amont.conventions").as_deref(),
        Some("declared")
    );
    step_eq!(
        "declared",
        list_json(&m, &fresh)["conventions_apply"],
        false,
        "a repository with no manifest holds the house rules back"
    );

    // Somebody else's project, carrying an amont.conf, cloned: the hooks
    // arrive with the clone, and `list --json` says what is in effect —
    // the house rules apply, and the manifest's check waits for trust.
    let upstream = m.repo("upstream");
    write(
        &upstream.join("amont.conf"),
        "pre-commit  suite  *.txt  block  true\n",
    );
    for args in [
        &["add", "amont.conf"][..],
        &["commit", "-qm", "chore: seed"],
    ] {
        let (code, out) = m.git(&upstream, args);
        step_eq!("clone", code, 0, "git {args:?}:\n{out}");
    }
    let clone = m.path("clone");
    let (code, out) = m.git(
        &m.root,
        &[
            "clone",
            "-q",
            upstream.to_str().expect("utf8"),
            clone.to_str().expect("utf8"),
        ],
    );
    step_eq!("clone", code, 0, "{out}");
    step!(
        "clone",
        clone.join(".git/hooks/pre-commit").is_file(),
        "the clone arrived without the shims"
    );
    let listed = list_json(&m, &clone);
    step_eq!("clone", listed["format"], "amont-list-v1");
    step_eq!(
        "clone",
        listed["conventions_apply"],
        true,
        "a committed manifest declares the repository"
    );
    let suite = listed["checks"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|c| c["id"] == "pre-commit-suite")
        .unwrap_or_else(|| panic!("step `clone`: the declared check is not listed:\n{listed}"));
    step_eq!("clone", suite["source"], "declared");
    step_eq!("clone", suite["status"], "unusable", "{suite}");
    step!(
        "clone",
        suite["reason"]
            .as_str()
            .is_some_and(|r| r.contains("untrusted")),
        "{suite}"
    );

    // The person undoes the grant as enroll said to; another tool then owns
    // `init.templateDir`, and that grant is not ours to overwrite.
    let (code, out) = m.git(
        &m.root,
        &["config", "--global", "--unset", "init.templateDir"],
    );
    step_eq!("foreign template dir", code, 0, "{out}");
    let other = m.path("their-templates");
    std::fs::create_dir_all(&other).expect("mkdir");
    let (code, out) = m.git(
        &m.root,
        &[
            "config",
            "--global",
            "init.templateDir",
            other.to_str().expect("utf8"),
        ],
    );
    step_eq!("foreign template dir", code, 0, "{out}");
    let (code, out) = m.amont(&m.root, &["enroll"]);
    step!("foreign template dir", code != 0, "{out}");
    step!("foreign template dir", out.contains("already set"), "{out}");
    step_eq!(
        "foreign template dir",
        m.global("init.templateDir").as_deref(),
        other.to_str(),
        "their grant survives"
    );

    unbaked_shims(&m);
}

/// The system utilities the shim itself needs (`dirname`, `basename`), and
/// nothing of ours.
#[cfg(unix)]
const MINIMAL_PATH: &str = "/usr/bin:/bin";

/// How a shim that was never baked finds the binary, driven through a real
/// `sh` — the template dir pointed at a checkout keeps the placeholder on
/// purpose. git runs a hook from the WORKING TREE, so a relative path in
/// the resolution chain is a question asked of the repository being
/// committed to, and a repository must never be able to answer it.
#[cfg(unix)]
fn unbaked_shims(m: &Machine) {
    use std::os::unix::fs::PermissionsExt;

    let template = read(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../templates/hooks/pre-commit")
            .canonicalize()
            .expect("template path"),
    );
    let r = Repo::new();
    let hooks = r.dir.join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).expect("hooks dir");
    let hook = hooks.join("pre-commit");
    let chmod = |p: &Path| {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).expect("chmod")
    };
    let install_hook = |text: &str| {
        std::fs::write(&hook, text).expect("write hook");
        chmod(&hook);
    };
    // HOME empty, so `~/.local/bin` finds nothing, and PATH of system
    // utilities only: anything that runs here ran because the shim chose it.
    // `/bin/sh` by absolute path, so PATH governs only what the shim
    // resolves.
    let home = m.path("empty-home");
    std::fs::create_dir_all(&home).expect("mkdir");
    let run = |path: &str| {
        Command::new("/bin/sh")
            .arg(&hook)
            .current_dir(&r.dir)
            .env_remove("GIT_HOOKS_BIN")
            .env("HOME", &home)
            .env("PATH", path)
            .output()
            .expect("run hook")
    };
    // A file the hook must never run: it touches `sentinel` if it does.
    let payload = |at: &Path, sentinel: &Path| {
        write(
            at,
            &format!("#!/bin/sh\ntouch {}\nexit 0\n", sentinel.display()),
        );
        chmod(at);
    };
    // An amont on the minimal PATH would resolve the shim's last candidate
    // and make the "not found" steps vacuous rather than wrong.
    let vacuous = MINIMAL_PATH
        .split(':')
        .any(|d| Path::new(d).join("amont").exists());
    if vacuous {
        println!("  ! amont on {MINIMAL_PATH} — skipping the not-found steps");
    }
    let token = r.dir.join("__AMONT_BIN__");

    // The clone plants an executable named for the placeholder token.
    install_hook(&template);
    let sentinel = m.path("ran-planted");
    payload(&token, &sentinel);
    if !vacuous {
        let out = run(MINIMAL_PATH);
        step!(
            "planted file",
            !sentinel.exists(),
            "the repository's own file was executed by the hook"
        );
        step!(
            "planted file",
            !out.status.success(),
            "an unresolvable binary must fail loudly, not pass"
        );
        let err = String::from_utf8_lossy(&out.stderr);
        step!(
            "planted file",
            err.contains("amont binary not found"),
            "expected the loud failure, got: {err}"
        );
    }

    // A trailing empty PATH component means the current directory: the old
    // shim ran the repository's file there and exited 0.
    let out = run(&format!("{MINIMAL_PATH}:"));
    step!(
        "PATH holds the cwd",
        !sentinel.exists(),
        "the repository's own file was executed on commit"
    );
    step!(
        "PATH holds the cwd",
        !out.status.success(),
        "and the hook must not report success either"
    );
    std::fs::remove_file(&token).expect("remove the planted file");

    // A directory by that name is the same trick: `[ -x dir ]` is true.
    std::fs::create_dir_all(&token).expect("plant dir");
    if !vacuous {
        let out = run(MINIMAL_PATH);
        step!(
            "planted directory",
            !out.status.success(),
            "a directory named for the token must not be treated as the binary"
        );
        step!(
            "planted directory",
            String::from_utf8_lossy(&out.stderr).contains("amont binary not found"),
            "expected the loud failure"
        );
    }
    std::fs::remove_dir(&token).expect("remove the planted dir");

    // The guard does not cost the feature: a baked absolute path runs, and
    // receives `--hooks-dir` and the hook name.
    let record = m.path("argv");
    let fake = m.path("bin/amont");
    write(
        &fake,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\nexit 0\n",
            record.display()
        ),
    );
    chmod(&fake);
    install_hook(&template.replace("__AMONT_BIN__", &fake.display().to_string()));
    let out = run(MINIMAL_PATH);
    step!(
        "baked absolute",
        out.status.success(),
        "the baked binary should have run: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let argv = std::fs::read_to_string(&record).expect("the baked binary did not run");
    step!(
        "baked absolute",
        argv.contains("--hooks-dir"),
        "argv was {argv:?}"
    );
    step!(
        "baked absolute",
        argv.contains("pre-commit"),
        "argv was {argv:?}"
    );

    // A relative baked path is refused by the shim itself, even with
    // something executable at exactly that path in the worktree.
    if !vacuous {
        let sentinel = m.path("ran-relative");
        payload(&r.dir.join("tools").join("amont"), &sentinel);
        install_hook(&template.replace("__AMONT_BIN__", "tools/amont"));
        let out = run(MINIMAL_PATH);
        step!(
            "relative baked",
            !sentinel.exists(),
            "a relative baked path was resolved"
        );
        step!(
            "relative baked",
            !out.status.success(),
            "must fail loudly instead"
        );
    }
}
