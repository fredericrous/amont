//! A throwaway git repo, for driving hooks from Rust.
//!
//! Phase 4 of docs/rust-migration.md. The zsh suites were the migration's
//! harness and stayed untouched on purpose while the hooks moved; now that the
//! hooks are Rust, the suites are the LAST thing requiring zsh — which is why
//! the Windows job can only run a smoke instead of the real tests.
//!
//! Each `Repo` is an isolated temp repository, cleaned up on drop, so tests run
//! in parallel. The old runner created one repo per SUITE and ran cases
//! sequentially inside it, which is why several cases depended on state left by
//! earlier ones (and why one of them could not fail — see pull-rebase).

#![allow(dead_code)] // each integration test binary uses a different subset

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

pub struct Repo {
    pub dir: PathBuf,
}

/// A counter, so parallel tests never collide on a directory name.
fn unique() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    format!(
        "amont-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

impl Repo {
    /// A fresh repo with an identity and NO hooks installed.
    ///
    /// `--template=` (empty) matters: without it `git init` copies the
    /// machine's own hooks in, and a test would exercise those instead of the
    /// binary under test. That bit the Windows smoke before it was noticed.
    ///
    /// `--initial-branch=main` at INIT time, not `git config init.defaultBranch`
    /// afterward — that config key only governs FUTURE `git init`/`git clone`
    /// calls, so setting it after this repo already exists changes nothing.
    /// Whatever the runner's own git build defaults to otherwise (still
    /// `master` on plenty of them) then decides the branch name instead, and a
    /// test that pushes the literal branch `main` silently pushes a ref that
    /// does not exist — discovered when the CI job with no global
    /// `init.defaultBranch` step hit exactly that.
    pub fn new() -> Self {
        let dir = std::env::temp_dir().join(unique());
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp repo");
        let r = Repo { dir };
        r.git(&["init", "-q", "--template=", "--initial-branch=main", "."]);
        r.git(&["config", "user.email", "test@example.com"]);
        r.git(&["config", "user.name", "test"]);
        // Keep the tests independent of the developer's global config.
        r.git(&["config", "commit.gpgsign", "false"]);
        // Git for Windows converts line endings on checkout by default. A test
        // that writes "a\n", stages it and then compares the file byte for byte
        // would be asserting git's newline policy rather than the behaviour
        // under test — and would pass on two platforms and fail on the third.
        r.git(&["config", "core.autocrlf", "false"]);
        // `amont.quiet` defaults to `auto`, and every command this harness runs
        // goes through `Command::output()` — so stderr is NEVER a terminal and
        // `auto` would resolve to quiet for the whole suite. That is not a
        // harmless difference: roughly twenty assertions here are NEGATIVE
        // (`!says(...)`, `silent()`), and they would all start passing
        // vacuously, because the line they guard against could no longer be
        // printed at all. The scope contracts — a commit touching no Dockerfile
        // says nothing about hadolint, kube-linter is silent without a config —
        // would stop being tested while staying green.
        //
        // So the suite pins the verbose form and tests that opt out say so:
        // `quiet_replaces_the_success_lines_with_their_count` sets `auto` and
        // `always` itself. Pinning also stops every other test asserting a
        // default by accident.
        r.git(&["config", "amont.quiet", "never"]);
        r
    }

    pub fn git(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new("git");
        cmd.args(args).current_dir(&self.dir).stdin(Stdio::null());
        Self::strip_git_env_impl(&mut cmd);
        cmd.output().expect("run git")
    }

    /// Write a file (creating parents) and stage it.
    pub fn stage(&self, path: &str, content: &str) {
        let full = self.dir.join(path);
        if let Some(p) = full.parent() {
            std::fs::create_dir_all(p).expect("create parent");
        }
        std::fs::write(&full, content).expect("write file");
        // `--`: a fixture staging a file named e.g. `-weird.json` (to prove a
        // hook itself handles one) would otherwise fail to stage at all —
        // `git add -weird.json` is `unknown switch 'w'` to git's OWN parser.
        self.git(&["add", "--", path]);
    }

    pub fn write(&self, path: &str, content: &str) {
        let full = self.dir.join(path);
        if let Some(p) = full.parent() {
            std::fs::create_dir_all(p).expect("create parent");
        }
        std::fs::write(&full, content).expect("write file");
    }

    pub fn commit(&self, msg: &str) {
        self.git(&["commit", "-q", "--no-verify", "-m", msg]);
    }

    /// Remove git's exported environment.
    ///
    /// These tests are themselves run by `pre-push-cargo-test`, and git gives a
    /// hook GIT_DIR/GIT_INDEX_FILE/GIT_WORK_TREE pointing at the REAL repo.
    /// Those beat `current_dir`, so without this a fixture's `git commit`
    /// commits to git-templates itself. It did exactly that once.
    pub fn strip_git_env_impl(cmd: &mut Command) {
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("GIT_") {
                cmd.env_remove(&k);
            }
        }
    }

    /// Run a hook through the binary, as the shim would.
    pub fn hook(&self, name: &str, args: &[&str]) -> HookRun {
        self.hook_at(&self.dir, name, args)
    }

    /// Pin the host-level knobs a fixture must not inherit from the machine
    /// (ADR-0009). These tests run under amont's own `pre-push-cargo-test`,
    /// on a workstation that is often loaded: without
    /// `AMONT_IDLE_LOAD_SCALE=1` every kill-time bound in `timing.rs`
    /// stretches with the load average, and without `AMONT_HOST_SLOT=held`
    /// a fixture's heavy check would queue on the real host slots behind the
    /// very suite that is running it. The slot and load fixtures use
    /// [`Repo::hook_watched_unpinned`], which sets neither.
    pub fn pin_host_env(cmd: &mut Command) {
        cmd.env("AMONT_IDLE_LOAD_SCALE", "1");
        cmd.env("AMONT_HOST_SLOT", "held");
    }

    /// Run a SUBCOMMAND — `amont list`, `amont setup` — from this repo.
    ///
    /// No `--hooks-dir`: that flag is hook mode, and a subcommand takes its own
    /// arguments verbatim.
    ///
    /// `GIT_CONFIG_GLOBAL` is set AFTER `strip_git_env_impl`, which removes
    /// every `GIT_*` variable including this one. Order matters: a test that
    /// exercises `--global` config would otherwise write the DEVELOPER's
    /// `~/.gitconfig`, and these tests are themselves run by
    /// `pre-push-cargo-test`. The file lives inside the repo's temp directory,
    /// so it is removed on drop with everything else.
    pub fn run(&self, args: &[&str]) -> HookRun {
        let mut cmd = Command::new(bin());
        cmd.args(args).current_dir(&self.dir).stdin(Stdio::null());
        Self::strip_git_env_impl(&mut cmd);
        Self::pin_host_env(&mut cmd);
        cmd.env("GIT_CONFIG_GLOBAL", self.dir.join("fake-gitconfig"));
        let out = cmd.output().expect("run amont");
        HookRun {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    /// Run a hook as the shim would from a LINKED WORKTREE: `--hooks-dir` is
    /// still this (main) repo's `.git/hooks` — real worktrees share the main
    /// checkout's hooks, they do not get their own — but the process runs
    /// with `cwd` set to wherever the commit is actually happening.
    pub fn hook_at(&self, cwd: &Path, name: &str, args: &[&str]) -> HookRun {
        let mut cmd = Command::new(bin());
        cmd.arg("--hooks-dir")
            .arg(self.dir.join(".git/hooks"))
            .arg(name)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null());
        Self::strip_git_env_impl(&mut cmd);
        Self::pin_host_env(&mut cmd);
        let out = cmd.output().expect("run amont");
        HookRun {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    /// Run a hook under a WATCHDOG that does not trust amont to return.
    ///
    /// For the fixtures that leave descendants behind on purpose (busy
    /// workers, orphans): amont kills only its direct child, and a surviving
    /// worker holding a pipe would make [`Repo::hook`]'s `output()` wait on
    /// it for as long as it lives. So amont runs in a process group of its
    /// own, its output goes to FILES (a worker holding one cannot block
    /// anything), and a separate clock SIGKILLs the whole group and fails the
    /// test at `limit`. The group is killed again afterwards either way, so
    /// nothing a fixture started outlives the test.
    ///
    /// Returns the run and how long it took.
    #[cfg(unix)]
    pub fn hook_watched(
        &self,
        name: &str,
        env: &[(&str, &std::ffi::OsStr)],
        limit: std::time::Duration,
    ) -> (HookRun, std::time::Duration) {
        self.hook_watched_in(&self.dir, name, env, limit, true)
    }

    /// [`Repo::hook_watched`] WITHOUT the host pins of [`Repo::pin_host_env`]:
    /// for the fixtures that are about host slots and the load-scaled
    /// budget, which set those knobs themselves through `env`.
    #[cfg(unix)]
    pub fn hook_watched_unpinned(
        &self,
        name: &str,
        env: &[(&str, &std::ffi::OsStr)],
        limit: std::time::Duration,
    ) -> (HookRun, std::time::Duration) {
        self.hook_watched_in(&self.dir, name, env, limit, false)
    }

    #[cfg(unix)]
    fn hook_watched_in(
        &self,
        cwd: &Path,
        name: &str,
        env: &[(&str, &std::ffi::OsStr)],
        limit: std::time::Duration,
        pinned: bool,
    ) -> (HookRun, std::time::Duration) {
        use std::os::unix::process::CommandExt;
        // Outside the repo: a pre-commit hold may park untracked files.
        let tmp = std::env::temp_dir();
        let out_path = tmp.join(format!("{}.out", unique()));
        let err_path = tmp.join(format!("{}.err", unique()));
        let mut cmd = Command::new(bin());
        cmd.arg("--hooks-dir")
            .arg(self.dir.join(".git/hooks"))
            .arg(name)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(std::fs::File::create(&out_path).expect("stdout file"))
            .stderr(std::fs::File::create(&err_path).expect("stderr file"))
            .process_group(0);
        Self::strip_git_env_impl(&mut cmd);
        if pinned {
            Self::pin_host_env(&mut cmd);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        let started = std::time::Instant::now();
        let mut child = cmd.spawn().expect("spawn amont");
        let group = child.id();
        let kill_group = || {
            let _ = Command::new("kill")
                .args(["-KILL", "--", &format!("-{group}")])
                .stderr(Stdio::null())
                .status();
        };
        let status = loop {
            if let Some(s) = child.try_wait().expect("wait amont") {
                break Some(s);
            }
            if started.elapsed() >= limit {
                break None;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        let took = started.elapsed();
        kill_group();
        let read = |p: &Path| std::fs::read_to_string(p).unwrap_or_default();
        let run = HookRun {
            code: status.and_then(|s| s.code()).unwrap_or(-1),
            stdout: read(&out_path),
            stderr: read(&err_path),
        };
        let _ = child.wait();
        let _ = std::fs::remove_file(&out_path);
        let _ = std::fs::remove_file(&err_path);
        assert!(
            status.is_some(),
            "the watchdog fired: amont had not returned after {limit:?}:\n{}",
            run.output()
        );
        (run, took)
    }

    /// A linked worktree of this repo, on its own branch. Its `.git` is a
    /// FILE pointing at a private admin directory under this repo's
    /// `.git/worktrees` — distinct from the common `.git` the hooks and
    /// their stash live in, which is exactly the distinction
    /// `amont-held` must get right.
    pub fn worktree(&self, name: &str) -> PathBuf {
        let path = self.dir.join(name);
        self.git(&[
            "worktree",
            "add",
            "-q",
            path.to_str().expect("utf8 path"),
            "-b",
            name,
        ]);
        path
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.dir.join(rel)
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub struct HookRun {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl HookRun {
    pub fn passed(&self) -> bool {
        self.code == 0
    }
    /// Everything the hook printed — several hooks report on stderr.
    pub fn output(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
    pub fn says(&self, needle: &str) -> bool {
        self.output().contains(needle)
    }
    /// No output at all: several hooks must be SILENT when out of scope, and
    /// "exit 0" alone does not distinguish that from "ran and approved".
    pub fn silent(&self) -> bool {
        self.output().trim().is_empty()
    }
}

#[cfg(unix)]
fn make_executable(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755));
}

/// Windows has no executable bit; the dispatcher runs scripts through their
/// shebang interpreter there, so nothing is needed.
#[cfg(not(unix))]
fn make_executable(_p: &Path) {}

/// The binary under test. Cargo builds it before integration tests and points
/// at it via CARGO_BIN_EXE_*.
fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_amont")
}

/// True when a tool the case genuinely needs is absent — and it SAYS SO.
///
/// Rust has no native skip: an early `return` reports as a pass, which is the
/// exact trap the zsh suites already guarded against by printing
/// "unavailable — skipping". The same phrase is used here so CI's
/// skip-reporter sees both harnesses, and a suite that quietly did not run
/// cannot masquerade as one that passed.
pub fn missing(tool: &str) -> bool {
    let found = std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path).any(|d| {
                d.join(tool).is_file()
                    || (cfg!(windows)
                        && [".exe", ".cmd", ".bat"]
                            .iter()
                            .any(|e| d.join(format!("{tool}{e}")).is_file()))
            })
        })
        .unwrap_or(false);
    if !found {
        println!("  ! {tool} unavailable — skipping");
    }
    !found
}

/// Absolute path to the repo's own templates/hooks, for tests that need a shim.
pub fn template_hook(name: &str) -> PathBuf {
    // ../../ — templates live at the WORKSPACE root, not in this crate.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../templates/hooks")
        .join(name)
}

/// Run a command that executes a binary some test COPIED into place,
/// retrying `ETXTBSY`. On Linux, another thread's fork-to-exec window can
/// briefly hold a write descriptor on the file (descriptors are
/// process-wide, and `fork` duplicates all of them), and executing it in
/// that window fails with "Text file busy". The window is microseconds;
/// the flake it produced on the ubuntu runner was real twice in one day.
/// Never retries any other error — a missing or broken binary is an answer.
pub fn output_retrying_etxtbsy(cmd: &mut std::process::Command) -> std::io::Result<Output> {
    let mut delay = std::time::Duration::from_millis(10);
    for tries_left in [4u8, 3, 2, 1, 0] {
        match cmd.output() {
            Err(e) if tries_left > 0 && e.raw_os_error() == Some(26) => {
                std::thread::sleep(delay);
                delay *= 2;
            }
            other => return other,
        }
    }
    unreachable!("the zero-tries arm returns")
}
