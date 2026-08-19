//! `amont enroll` — the standing grant, exercised in a sandbox so a bug can
//! never reach the developer's real global config.
//!
//! Unix-gated like the installer suite: the end-to-end case drives a real
//! `git init` + commit through the copied shims, which needs `sh` and the
//! execute bit.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Sandbox(PathBuf);

fn unique() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    format!(
        "amont-enroll-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

impl Sandbox {
    fn new() -> Sandbox {
        let dir = std::env::temp_dir().join(unique());
        std::fs::create_dir_all(&dir).expect("mkdir");
        Sandbox(dir)
    }
    fn path(&self, rel: &str) -> PathBuf {
        rel.split('/').fold(self.0.clone(), |p, c| p.join(c))
    }
    /// Every command in this suite carries the same insulation: HOME and XDG
    /// inside the sandbox, and GIT_CONFIG_GLOBAL pinned to a sandbox file as
    /// the belt to that suspenders.
    fn cmd(&self, program: &str, cwd: &Path) -> Command {
        let mut c = Command::new(program);
        c.current_dir(cwd)
            .env("HOME", &self.0)
            .env("USERPROFILE", &self.0)
            .env("XDG_CONFIG_HOME", self.path(".config"))
            .env("GIT_CONFIG_GLOBAL", self.path("gitconfig"))
            .stdin(Stdio::null());
        c
    }
    fn enroll(&self, args: &[&str]) -> (i32, String) {
        let out = self
            .cmd(env!("CARGO_BIN_EXE_amont"), &self.0)
            .arg("enroll")
            .args(args)
            .output()
            .expect("run amont enroll");
        (
            out.status.code().unwrap_or(-1),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }
    fn global(&self, key: &str) -> Option<String> {
        let out = self
            .cmd("git", &self.0)
            .args(["config", "--global", "--get", key])
            .output()
            .expect("git config");
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn enroll_grants_and_is_idempotent() {
    let s = Sandbox::new();
    let (code, out) = s.enroll(&[]);
    assert_eq!(code, 0, "{out}");

    let configured = s.global("init.templateDir").expect("templateDir set");
    assert!(
        Path::new(&configured)
            .join("hooks")
            .join("pre-commit")
            .is_file(),
        "the template dir it points at holds the shims: {configured}"
    );
    // The grant reaches forward, and the output says so.
    assert!(out.contains("future"), "{out}");
    assert!(
        out.contains("amont init"),
        "existing clones get a remedy: {out}"
    );

    // Second run: same answer, nothing re-claimed.
    let (code, out) = s.enroll(&[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("already points here"), "{out}");
}

#[test]
fn enroll_can_scope_the_conventions() {
    let s = Sandbox::new();
    let (code, out) = s.enroll(&["--conventions", "declared"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(s.global("amont.conventions").as_deref(), Some("declared"));
}

/// A bad flag value writes NOTHING — an enroll that half-runs and then
/// rejects its own argument has still changed global state.
#[test]
fn a_bad_conventions_value_writes_nothing() {
    let s = Sandbox::new();
    let (code, out) = s.enroll(&["--conventions", "sometimes"]);
    assert_ne!(code, 0, "{out}");
    assert_eq!(s.global("init.templateDir"), None, "nothing was granted");
    assert_eq!(s.global("amont.conventions"), None);
}

/// somebody else's template dir is not ours to overwrite.
#[test]
fn a_foreign_template_dir_is_refused_not_overwritten() {
    let s = Sandbox::new();
    let theirs = s.path("their-templates");
    std::fs::create_dir_all(&theirs).unwrap();
    let ok = s
        .cmd("git", &s.0)
        .args([
            "config",
            "--global",
            "init.templateDir",
            theirs.to_str().unwrap(),
        ])
        .status()
        .expect("git config")
        .success();
    assert!(ok);

    let (code, out) = s.enroll(&[]);
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("already set"), "{out}");
    assert_eq!(
        s.global("init.templateDir").as_deref(),
        theirs.to_str(),
        "their grant survives"
    );
}

/// End to end: after enroll, a plain `git init` arrives with working hooks —
/// the clone-time self-install the npm route had and nothing else did.
#[test]
fn a_fresh_repo_after_enroll_has_working_hooks() {
    let s = Sandbox::new();
    let (code, out) = s.enroll(&[]);
    assert_eq!(code, 0, "{out}");

    let repo = s.path("fresh");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        vec!["init", "-q", "--initial-branch=main"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
    ] {
        assert!(s.cmd("git", &repo).args(&args).status().unwrap().success());
    }
    assert!(
        repo.join(".git/hooks/pre-commit").is_file(),
        "the grant delivered the shims"
    );

    // A clean commit passes through the shim: binary resolved, checks run.
    std::fs::write(repo.join("a.txt"), "hello\n").unwrap();
    assert!(s
        .cmd("git", &repo)
        .args(["add", "a.txt"])
        .status()
        .unwrap()
        .success());
    let out = s
        .cmd("git", &repo)
        .args(["commit", "-m", "feat: through the shim"])
        .output()
        .expect("commit");
    assert!(
        out.status.success(),
        "commit through the enrolled shim: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
