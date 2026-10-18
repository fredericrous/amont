//! Committed repo policy — `severity` and `skip` lines in `amont.conf`.
//!
//! The parser and the folds are unit-tested next to themselves, and
//! `flows/declared.rs` walks what a committed line does to a verdict, how
//! trust gates it, and local config beating it. What stays here are the
//! rungs of the specificity ladder (default < system < global < POLICY <
//! local < worktree < command) a repository's own config cannot reach: a
//! sandboxed global, command scope, a git too old for `--show-scope`, and a
//! comma-joined `hook.skip` with no manifest at all.

use crate::common;
use common::Repo;
use std::process::{Command, Stdio};

/// Write the manifest AND trust it — same shape as external.rs, because
/// every test about what policy DOES has to grant consent first.
fn manifest(r: &Repo, body: &str) {
    r.stage("amont.conf", body);
    trust(r);
}

fn trust(r: &Repo) {
    let out = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("trust")
        .current_dir(&r.dir)
        .output()
        .expect("amont trust");
    assert!(out.status.success(), "could not trust the manifest");
}

/// A staged file lint-json-yaml must object to.
fn stage_bad_json(r: &Repo) {
    r.stage("bad.json", "{ not json\n");
}

/// Run the pre-commit stage with extra environment — the ladder tests need
/// GIT_CONFIG_GLOBAL (a sandboxed global) and GIT_CONFIG_* (command scope).
fn pre_commit_env(r: &Repo, env: &[(&str, &str)]) -> (i32, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_amont"));
    cmd.arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-commit")
        .current_dir(&r.dir)
        .stdin(Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run pre-commit");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// Policy beats GLOBAL config — the team's committed decision outranks a
/// personal default, without touching the local escape hatch.
#[test]
fn policy_beats_global_config() {
    let r = Repo::new();
    manifest(&r, "severity lint-json-yaml warn\n");
    let global = r.path("sandbox-gitconfig");
    std::fs::write(&global, "[amont \"severity\"]\n\tlint-json-yaml = block\n").unwrap();
    stage_bad_json(&r);
    let (code, out) = pre_commit_env(&r, &[("GIT_CONFIG_GLOBAL", global.to_str().unwrap())]);
    assert_eq!(code, 0, "policy warn beats global block: {out}");
}

/// Command scope (`git -c` / GIT_CONFIG_*) beats policy — most specific of
/// all.
#[test]
fn command_scope_beats_policy() {
    let r = Repo::new();
    manifest(&r, "severity lint-json-yaml warn\n");
    stage_bad_json(&r);
    let (code, out) = pre_commit_env(
        &r,
        &[
            ("GIT_CONFIG_COUNT", "1"),
            ("GIT_CONFIG_KEY_0", "amont.severity.lint-json-yaml"),
            ("GIT_CONFIG_VALUE_0", "block"),
        ],
    );
    assert_ne!(code, 0, "command-scope block must win: {out}");
}

/// One `hook.skip` value may name several checks, joined by commas, exactly as
/// several `-c hook.skip=` would: both are skipped and both are announced.
#[test]
fn a_comma_joined_skip_skips_every_name_in_it() {
    let r = Repo::new();
    r.git(&["config", "--add", "hook.skip", "ban-terms, lint-json-yaml"]);
    stage_bad_json(&r);
    r.stage("f.ts", "debugger;\n");
    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.stdout);
    assert!(
        run.says("pre-commit-ban-terms") && run.says("pre-commit-lint-json-yaml"),
        "{}",
        run.stdout
    );
}

/// Old git degrades fail-safe: when `--show-scope` is unsupported, ALL git
/// config beats policy — a global block wins where it normally would not.
#[cfg(unix)]
#[test]
fn degraded_git_lets_all_config_beat_policy() {
    use std::os::unix::fs::PermissionsExt;
    let r = Repo::new();
    manifest(&r, "severity lint-json-yaml warn\n");
    stage_bad_json(&r);
    // A git shim that refuses --show-scope like a pre-2.26 git (exit 129)
    // and forwards everything else to the real git.
    let real = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|d| d.join("git"))
        .find(|p| p.is_file())
        .expect("git on PATH");
    let dir = r.path(".git/toolshims");
    std::fs::create_dir_all(&dir).unwrap();
    let shim = dir.join("git");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do [ \"$a\" = --show-scope ] && exit 129; done\nexec {} \"$@\"\n",
            real.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    let global = r.path("sandbox-gitconfig");
    std::fs::write(&global, "[amont \"severity\"]\n\tlint-json-yaml = block\n").unwrap();
    let (code, out) = pre_commit_env(
        &r,
        &[
            ("GIT_CONFIG_GLOBAL", global.to_str().unwrap()),
            (
                "PATH",
                &format!("{}:{}", dir.display(), std::env::var("PATH").unwrap()),
            ),
        ],
    );
    assert_ne!(code, 0, "degraded mode: global config beats policy: {out}");
}
