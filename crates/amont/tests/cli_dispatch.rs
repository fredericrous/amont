//! What the binary does with its argument list, driven through the real binary.
//!
//! The parse is unit-tested next to itself in `src/main.rs`, and that test
//! proves the ARGV maps to the right `Invocation`. This file proves the other
//! half, which a unit test structurally cannot: that dispatching on a hook
//! argument does not actually run the installer. The failure being guarded
//! against is not a wrong enum value — it is `git push` copying a binary into
//! `~/.local/bin`, populating a template directory, and baking shims, because
//! somebody's remote is called `install`.
//!
//! So every assertion here is about the FILESYSTEM and the exit code, with HOME
//! and XDG_CONFIG_HOME inside the sandbox: if a subcommand ran, it left
//! evidence, and the evidence is what is checked.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Same shape as `tests/install.rs`'s sandbox, and for the same reason: a bug
/// in the code under test must not be able to reach the developer's real
/// configuration. Duplicated rather than shared because a `tests/common` module
/// that two files pull different halves of is how helpers grow parameters
/// nobody uses.
struct Sandbox(PathBuf);

impl Sandbox {
    fn new(name: &str) -> Self {
        let d = std::env::temp_dir().join(format!("gh-cli-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("mkdir");
        Sandbox(d)
    }
    /// Joined component by component, NOT `join("a/b")` — see tests/install.rs.
    fn path(&self, rel: &str) -> PathBuf {
        rel.split('/').fold(self.0.clone(), |p, c| p.join(c))
    }
    fn run(&self, cwd: &Path, args: &[&str]) -> (i32, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_amont"))
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.0)
            .env("USERPROFILE", &self.0)
            .env("XDG_CONFIG_HOME", self.path(".config"))
            .output()
            .expect("run amont");
        (
            out.status.code().unwrap_or(-1),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }
    /// Nothing a subcommand would have left behind.
    fn assert_nothing_was_installed(&self, repo: &Path, context: &str) {
        for n in ["amont", "amont.exe"] {
            assert!(
                !self.path(".local/bin").join(n).exists(),
                "{context}: the INSTALLER RAN — a binary appeared in ~/.local/bin"
            );
        }
        let tpl = self.path(".config/git/git-templates/templates/hooks");
        for name in DISPATCHERS {
            assert!(
                !tpl.join(name).exists(),
                "{context}: a template shim was written"
            );
            assert!(
                !repo.join(".git/hooks").join(name).exists(),
                "{context}: a repo shim was baked"
            );
        }
        assert!(
            !repo.join("AGENTS.md").exists(),
            "{context}: agents-md ran and wrote AGENTS.md"
        );
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const DISPATCHERS: [&str; 5] = [
    "commit-msg",
    "post-commit",
    "pre-commit",
    "pre-push",
    "prepare-commit-msg",
];

/// Every verb the dispatch table holds. Spelled out here on purpose: this file
/// is the outside view, and it should fail if the binary quietly grows a verb
/// that is not covered.
const VERBS: [&str; 7] = [
    "list",
    "install",
    "uninstall",
    "run",
    "trust",
    "restore",
    "agents-md",
];

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

/// The verified bug: `amont --hooks-dir <d> pre-push <remote> <url>` is what
/// the pre-push shim execs, and dispatch was reading `<remote>`. A remote named
/// `install` ran the FULL INSTALLER mid-push; a remote named `run`, `list`,
/// `trust`, `restore` or `uninstall` made the hook a no-op that exited 0 — a
/// push with zero checks, reported as a pass.
///
/// Asserted on the filesystem rather than on the exit code, because "exited 0"
/// is precisely what the broken version did.
#[test]
fn a_remote_named_like_a_subcommand_still_runs_the_hook() {
    for verb in VERBS {
        let s = Sandbox::new(&format!("remote-{}", verb.replace('-', "")));
        let repo = s.path("repo");
        init_repo(&repo);
        let hooks = repo.join(".git/hooks");
        std::fs::create_dir_all(&hooks).expect("mkdir");

        let (_, out) = s.run(
            &repo,
            &[
                "--hooks-dir",
                hooks.to_str().expect("utf8"),
                "pre-push",
                verb,
                "https://example.test/r.git",
            ],
        );
        s.assert_nothing_was_installed(&repo, &format!("remote named {verb:?}, output:\n{out}"));
    }
}

/// The same for the first argument of a `commit-msg` hook, which git hands a
/// PATH. A repository whose commit-message template lives at a file called
/// `install` is not a thing anybody plans, but it is a thing `git commit`
/// would have run the installer over.
#[test]
fn a_hook_argument_that_is_a_file_named_install_reaches_the_hook_verbatim() {
    let s = Sandbox::new("literal-install");
    let repo = s.path("repo");
    init_repo(&repo);
    let hooks = repo.join(".git/hooks");
    std::fs::create_dir_all(&hooks).expect("mkdir");

    // A commit message file literally named `install`, with a subject the
    // commit-msg check will reject — so we can tell the hook actually READ this
    // file rather than dispatching on its name.
    let msg = repo.join("install");
    std::fs::write(
        &msg,
        "not a conventional subject at all, and far too long to pass\n",
    )
    .expect("write");

    let (code, out) = s.run(
        &repo,
        &[
            "--hooks-dir",
            hooks.to_str().expect("utf8"),
            "commit-msg",
            msg.to_str().expect("utf8"),
        ],
    );
    s.assert_nothing_was_installed(&repo, &format!("output:\n{out}"));
    assert_eq!(
        code, 1,
        "the commit-msg check did not read the file it was handed:\n{out}"
    );
    assert!(
        std::fs::read_to_string(&msg)
            .expect("read")
            .starts_with("not a conventional"),
        "the hook argument was treated as something other than a path"
    );
}

/// A subcommand's own arguments are passed through verbatim, including ones
/// spelled like other subcommands.
#[test]
fn a_subcommand_argument_spelled_like_a_verb_is_just_a_value() {
    let s = Sandbox::new("verb-value");
    let repo = s.path("repo");
    init_repo(&repo);

    // `--path install` must write AGENTS.md content to a file called `install`,
    // not run the installer.
    let (code, out) = s.run(&repo, &["agents-md", "--path", "install"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        repo.join("install").is_file(),
        "--path was not honoured:\n{out}"
    );
    for n in ["amont", "amont.exe"] {
        assert!(
            !s.path(".local/bin").join(n).exists(),
            "THE INSTALLER RAN from an argument value:\n{out}"
        );
    }
}

/// Asking a program what it does must not be answered with "you used it
/// wrong". `--help` exited 2 with a single line naming `--hooks-dir` and none
/// of the seven verbs.
#[test]
fn help_exits_zero_and_describes_the_program() {
    let s = Sandbox::new("help");
    let plain = s.path("plain");
    std::fs::create_dir_all(&plain).expect("mkdir");

    for flag in ["--help", "-h"] {
        let (code, out) = s.run(&plain, &[flag]);
        assert_eq!(code, 0, "{flag} exited {code}:\n{out}");
        for verb in VERBS {
            assert!(out.contains(verb), "{flag} never mentions {verb}:\n{out}");
        }
        assert!(out.contains("--hooks-dir"), "{out}");
        assert!(out.contains("--stage"), "{out}");
    }
}

/// No arguments is a usage error, and a usage error says why AND shows the
/// same block `--help` shows.
#[test]
fn no_arguments_is_a_usage_error_that_still_explains_itself() {
    let s = Sandbox::new("noargs");
    let plain = s.path("plain");
    std::fs::create_dir_all(&plain).expect("mkdir");

    let (code, out) = s.run(&plain, &[]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("agents-md"), "{out}");
    assert!(out.contains("--hooks-dir"), "{out}");
}
