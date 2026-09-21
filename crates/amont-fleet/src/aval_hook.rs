//! Whether each decision-bearing repository's `aval` session hook is current.
//!
//! `aval hook install` writes two tracked files — `.claude/hooks/aval-heads.sh`
//! and a `SessionStart` entry in `.claude/settings.json` — so that an agent
//! session opens with the repository's decision heads and adopted rules in
//! front of it. The script's bytes are its version: every aval release that
//! changes it leaves every repository behind until somebody re-runs the
//! command there, which is the same silent drift the `AGENTS.md` pointer had
//! before `--agents-md` existed, spread across every repository that keeps an
//! `.adr.yaml`.
//!
//! This module never renders the hook. The template is aval's, its notion of
//! "current" is aval's, and a copy here would be a second way to be stale —
//! so both questions are put to the `aval` binary and only its exit code is
//! read (`0` current, `1` out of date), which is the contract its `--check`
//! documents. A repository with no `.adr.yaml` is not asked at all: there is
//! no corpus for the hook to print, and `aval hook install --check` would
//! report it as stale.
//!
//! What git would do with the files is reported alongside, from `git
//! check-ignore`, because the failure that motivated tracking the hook was a
//! repository whose `.gitignore` excluded `.claude/` wholesale: the command
//! succeeded, the hook worked for the one person who ran it, and shipped to
//! nobody. That is a `Warning`, never a write — editing another repository's
//! `.gitignore` is a judgment (`.claude/*` plus negations, in every case seen
//! so far, but with per-repository exceptions) and not this tool's to make.

use std::path::Path;
use std::process::Command;

use serde::Serialize;

/// The registry aval reads. Its presence is what makes a repository a corpus.
pub const REGISTRY: &str = ".adr.yaml";
/// The two files `aval hook install` writes, spelled as aval spells them.
pub const SCRIPT_PATH: &str = ".claude/hooks/aval-heads.sh";
pub const SETTINGS_PATH: &str = ".claude/settings.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AvalHookState {
    /// No `.adr.yaml` here. Not applicable, and nothing was spawned to find out.
    NoCorpus,
    /// A corpus, and no `aval` on `PATH` to judge its hook with. Absence of
    /// evidence: reported, never read as "current".
    NoAval,
    /// `aval hook install --check` exited 0.
    Current,
    /// `aval hook install --check` exited 1: at least one of the two files
    /// would be written.
    Stale,
    /// aval answered with something other than 0 or 1 — a usage error from a
    /// version that spells the subcommand differently, a corpus it will not
    /// load. Carried verbatim rather than mapped onto a state it did not say.
    Unknown { why: String },
}

/// One repository's answer, plus what git would do with the files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AvalHook {
    pub state: AvalHookState,
    /// The hook's paths that `git check-ignore` matched. Non-empty means a
    /// current hook still ships to nobody. Always empty for `NoCorpus`.
    pub ignored: Vec<String>,
}

impl Default for AvalHook {
    fn default() -> Self {
        AvalHook {
            state: AvalHookState::NoCorpus,
            ignored: Vec::new(),
        }
    }
}

/// Ask aval about `repo`. `aval` is the binary to run — `"aval"` in
/// production, a fake under test — and nothing is spawned for a repository
/// without a registry.
pub fn state(repo: &Path, aval: &str) -> AvalHook {
    if !repo.join(REGISTRY).is_file() {
        return AvalHook::default();
    }
    let state = match Command::new(aval)
        .args(["hook", "install", "--check"])
        .current_dir(repo)
        .output()
    {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => AvalHookState::NoAval,
        Err(e) => AvalHookState::Unknown { why: e.to_string() },
        Ok(out) => match out.status.code() {
            Some(0) => AvalHookState::Current,
            Some(1) => AvalHookState::Stale,
            code => AvalHookState::Unknown {
                why: format!(
                    "aval hook install --check exited {}: {}",
                    code.map_or("by signal".to_string(), |c| c.to_string()),
                    first_line(&out.stderr).unwrap_or_else(
                        || first_line(&out.stdout).unwrap_or_else(|| "no output".to_string())
                    )
                ),
            },
        },
    };
    AvalHook {
        state,
        ignored: ignored(repo),
    }
}

/// Run `aval hook install` in `repo`. Returns how many files aval reports
/// having written, from the `wrote` lines of its own report.
pub fn install(repo: &Path, aval: &str) -> Result<usize, String> {
    let out = Command::new(aval)
        .args(["hook", "install"])
        .current_dir(repo)
        .output()
        .map_err(|e| format!("{aval}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "aval hook install exited {}: {}",
            out.status
                .code()
                .map_or("by signal".to_string(), |c| c.to_string()),
            first_line(&out.stderr)
                .or_else(|| first_line(&out.stdout))
                .unwrap_or_else(|| "no output".to_string())
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.trim_start().starts_with("wrote"))
        .count())
}

/// The hook's paths git would ignore in `repo`, best effort. Exit 1 from
/// `check-ignore` is "nothing matched", the ordinary case; no git at all is
/// not this module's to report.
fn ignored(repo: &Path) -> Vec<String> {
    let Ok(out) = Command::new("git")
        .args(["check-ignore", "--", SCRIPT_PATH, SETTINGS_PATH])
        .current_dir(repo)
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

fn first_line(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fleet-aval-hook-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A stand-in `aval` whose `--check` exits `check_rc` and whose `install`
    /// prints `wrote` lines. Records what it was asked in `<dir>/calls`.
    /// A `/bin/sh` script, so unix-only — the same `cfg` every executable
    /// fixture in this crate carries; the two tests below that need no fake
    /// binary run everywhere.
    #[cfg(unix)]
    fn fake_aval(d: &Path, check_rc: i32) -> String {
        use std::os::unix::fs::PermissionsExt;
        let p = d.join("aval");
        std::fs::write(
            &p,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{calls}\"\n\
                 case \"$*\" in\n\
                   'hook install --check') exit {check_rc} ;;\n\
                   'hook install') printf '  wrote  {script}  (regenerated)\\n  wrote  {settings}  (merged)\\n'; exit 0 ;;\n\
                   *) echo 'usage' >&2; exit 2 ;;\n\
                 esac\n",
                calls = d.join("calls").display(),
                script = SCRIPT_PATH,
                settings = SETTINGS_PATH,
            ),
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p.display().to_string()
    }

    #[cfg(unix)]
    fn calls(d: &Path) -> String {
        std::fs::read_to_string(d.join("calls")).unwrap_or_default()
    }

    #[cfg(unix)]
    fn git_repo(d: &Path) {
        assert!(Command::new("git")
            .args(["init", "-q"])
            .current_dir(d)
            .status()
            .unwrap()
            .success());
    }

    #[cfg(unix)]
    #[test]
    fn a_repo_without_a_registry_is_not_asked() {
        let d = dir("no-corpus");
        let aval = fake_aval(&d, 1);
        let repo = d.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        assert_eq!(state(&repo, &aval), AvalHook::default());
        assert_eq!(calls(&d), "", "no spawn for a repository with no corpus");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn avals_exit_code_is_the_state() {
        for (rc, want) in [(0, AvalHookState::Current), (1, AvalHookState::Stale)] {
            let d = dir(&format!("rc{rc}"));
            let aval = fake_aval(&d, rc);
            let repo = d.join("repo");
            std::fs::create_dir_all(&repo).unwrap();
            std::fs::write(repo.join(REGISTRY), "dir: docs/adr\n").unwrap();
            let got = state(&repo, &aval);
            assert_eq!(got.state, want);
            assert!(calls(&d).contains("hook install --check"));
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_exit_code_aval_did_not_promise_is_unknown_not_a_state() {
        let d = dir("rc2");
        let aval = fake_aval(&d, 2);
        let repo = d.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join(REGISTRY), "dir: docs/adr\n").unwrap();
        let got = state(&repo, &aval);
        assert!(
            matches!(got.state, AvalHookState::Unknown { ref why } if why.contains("exited 2")),
            "{got:?}"
        );
        assert_ne!(
            got.state,
            AvalHookState::Stale,
            "unknown must never plan an install"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_missing_binary_is_reported_not_read_as_current() {
        let d = dir("no-aval");
        let repo = d.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join(REGISTRY), "dir: docs/adr\n").unwrap();
        let got = state(&repo, &d.join("definitely-not-aval").display().to_string());
        assert_eq!(got.state, AvalHookState::NoAval);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn a_gitignored_hook_is_named() {
        let d = dir("ignored");
        let aval = fake_aval(&d, 0);
        let repo = d.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_repo(&repo);
        std::fs::write(repo.join(REGISTRY), "dir: docs/adr\n").unwrap();
        std::fs::write(repo.join(".gitignore"), ".claude/\n").unwrap();
        let got = state(&repo, &aval);
        assert_eq!(got.state, AvalHookState::Current);
        assert_eq!(
            got.ignored,
            vec![SCRIPT_PATH.to_string(), SETTINGS_PATH.to_string()]
        );

        // The negation shape every wired repository uses clears it.
        std::fs::write(
            repo.join(".gitignore"),
            ".claude/*\n!.claude/settings.json\n!.claude/hooks/\n.claude/hooks/*\n!.claude/hooks/aval-heads.sh\n",
        )
        .unwrap();
        assert!(state(&repo, &aval).ignored.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn install_counts_what_aval_says_it_wrote() {
        let d = dir("install");
        let aval = fake_aval(&d, 1);
        let repo = d.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        assert_eq!(install(&repo, &aval), Ok(2));
        assert!(calls(&d).lines().any(|l| l == "hook install"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_failed_install_carries_avals_words() {
        let d = dir("install-fail");
        let repo = d.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let err = install(&repo, &d.join("nope").display().to_string()).unwrap_err();
        assert!(err.contains("nope"), "{err}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
