//! Committed repo policy — `severity` and `skip` lines in `amont.conf`.
//!
//! The parser and the folds are unit-tested next to themselves; these cover
//! what a unit test cannot: that a committed line actually changes a
//! verdict, that trust actually gates it, and that the specificity ladder
//! (default < system < global < POLICY < local < worktree < command) holds
//! through real git config in a real repository.

mod common;
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

/// The headline: a committed severity line downgrades a built-in, for
/// everyone who trusted the manifest, with no git config anywhere.
#[test]
fn a_trusted_severity_line_downgrades_a_builtin() {
    let r = Repo::new();
    manifest(&r, "severity lint-json-yaml warn\n");
    stage_bad_json(&r);
    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "warn must not block: {}", run.stdout);
    assert!(
        run.says("set to warn"),
        "the downgrade is said out loud: {}",
        run.stdout
    );
}

/// Untrusted policy is inert — and SAYS so, because policy that silently
/// does not apply is a silent behaviour change.
#[test]
fn untrusted_policy_is_inert_and_announced() {
    let r = Repo::new();
    r.stage("amont.conf", "severity lint-json-yaml warn\n");
    stage_bad_json(&r);
    let run = r.hook("pre-commit", &[]);
    assert!(
        !run.passed(),
        "policy must not apply untrusted: {}",
        run.stdout
    );
    assert!(
        run.says("policy not applied"),
        "withheld policy is announced: {}",
        run.stdout
    );
}

/// Local git config beats policy — the developer owns their machine.
#[test]
fn local_config_beats_policy() {
    let r = Repo::new();
    manifest(&r, "severity lint-json-yaml warn\n");
    r.git(&["config", "amont.severity.lint-json-yaml", "block"]);
    stage_bad_json(&r);
    let run = r.hook("pre-commit", &[]);
    assert!(!run.passed(), "local block must win: {}", run.stdout);
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

/// Specificity decides BETWEEN keys, whatever the source: a policy full id
/// beats a local trigger key. The source ladder only owns same-key fights.
#[test]
fn a_policy_full_id_beats_a_local_trigger_key() {
    let r = Repo::new();
    manifest(&r, "severity pre-commit-lint-json-yaml warn\n");
    r.git(&["config", "amont.severity.pre-commit", "block"]);
    stage_bad_json(&r);
    let run = r.hook("pre-commit", &[]);
    assert!(
        run.passed(),
        "full-id policy outranks a trigger key: {}",
        run.stdout
    );
}

/// A policy skip is announced as the TEAM's decision, on its own line.
#[test]
fn a_policy_skip_is_announced_separately() {
    let r = Repo::new();
    manifest(&r, "skip lint-json-yaml\n");
    stage_bad_json(&r);
    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "skipped means skipped: {}", run.stdout);
    // `amont.conf` is highlighted, so the two fragments sit either side of
    // an escape sequence.
    assert!(
        run.says("skipped by") && run.says("amont.conf"),
        "the team's skip is named as such: {}",
        run.stdout
    );
}

/// Machine and policy skips coexist, each announced under its own source.
#[test]
fn machine_and_policy_skips_are_two_lines() {
    let r = Repo::new();
    manifest(&r, "skip lint-json-yaml\n");
    r.git(&["config", "--add", "hook.skip", "ban-terms"]);
    stage_bad_json(&r);
    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.stdout);
    assert!(
        run.says("hook.skip") && run.says("pre-commit-ban-terms"),
        "{}",
        run.stdout
    );
    assert!(
        run.says("amont.conf") && run.says("pre-commit-lint-json-yaml"),
        "{}",
        run.stdout
    );
}

/// A typo'd target is a positioned note, and nothing else changes.
#[test]
fn an_unmatched_target_is_a_note_with_a_position() {
    let r = Repo::new();
    manifest(&r, "severity clipy warn\n");
    r.stage("a.txt", "x\n");
    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.stdout);
    assert!(
        run.says("amont.conf:1") && run.says("names no check here"),
        "the typo is pointed at: {}",
        run.stdout
    );
}

/// `amont run <check>` resolves severity through a different path than the
/// dispatcher; the two must not disagree about policy.
#[test]
fn direct_run_honors_policy_like_the_dispatcher() {
    let r = Repo::new();
    manifest(&r, "severity lint-json-yaml warn\n");
    stage_bad_json(&r);
    let out = Command::new(env!("CARGO_BIN_EXE_amont"))
        .args(["run", "pre-commit-lint-json-yaml"])
        .current_dir(&r.dir)
        .stdin(Stdio::null())
        .output()
        .expect("amont run");
    assert!(
        out.status.success(),
        "direct run must apply the same downgrade: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `amont list --json` reports the provenance, additively.
#[test]
fn list_reports_policy_provenance() {
    let r = Repo::new();
    manifest(&r, "severity lint-json-yaml warn\nskip yamllint\n");
    let out = Command::new(env!("CARGO_BIN_EXE_amont"))
        .args(["list", "--json"])
        .current_dir(&r.dir)
        .stdin(Stdio::null())
        .output()
        .expect("amont list");
    let json = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(json.contains("\"severity_source\":\"policy\""), "{json}");
    assert!(json.contains("\"severity_overridden\":true"), "{json}");
    assert!(json.contains("skipped via amont.conf"), "{json}");
}

/// The consent prompt shows the policy being granted — `trust --show`
/// renders it as its own block, never as a broken check.
#[test]
fn trust_show_lists_the_policy() {
    let r = Repo::new();
    manifest(&r, "severity lint-json-yaml warn\ntool jq 1.7\n");
    let out = Command::new(env!("CARGO_BIN_EXE_amont"))
        .args(["trust", "--show"])
        .current_dir(&r.dir)
        .output()
        .expect("trust --show");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("sets policy for built-in checks"), "{text}");
    assert!(text.contains("severity  lint-json-yaml  warn"), "{text}");
    assert!(!text.contains("! tool pin"), "a pin is not broken: {text}");
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

/// `set` ships a threshold: a 2 MB file warns under a committed 1 MB line.
#[test]
fn a_set_line_ships_a_threshold() {
    let r = Repo::new();
    manifest(&r, "set largeFileWarn 1\n");
    let big = vec![b'x'; 2 * 1024 * 1024];
    std::fs::write(r.path("blob.bin"), &big).unwrap();
    r.git(&["add", "blob.bin"]);
    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "warn tier does not block: {}", run.stdout);
    assert!(run.says("blob.bin"), "the file is named: {}", run.stdout);
}

/// Local config still beats a committed threshold.
#[test]
fn local_config_beats_a_set_line() {
    let r = Repo::new();
    manifest(&r, "set largeFileWarn 1\n");
    r.git(&["config", "amont.largeFileWarn", "1000"]);
    let big = vec![b'x'; 2 * 1024 * 1024];
    std::fs::write(r.path("blob.bin"), &big).unwrap();
    r.git(&["add", "blob.bin"]);
    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.stdout);
    assert!(
        !run.says("blob.bin"),
        "the local 1000 MB threshold wins: {}",
        run.stdout
    );
}

/// Commit style travels: a committed subjectMax reaches commit-msg — a
/// SEPARATE process, which is what the install-at-every-load invariant buys.
#[test]
fn a_set_line_reaches_commit_msg() {
    let r = Repo::new();
    manifest(&r, "set commit.subjectMax 20\n");
    let msg = r.path("msg.txt");
    std::fs::write(&msg, "feat: much too long for twenty\n").unwrap();
    let run = r.hook("commit-msg", &[msg.to_str().unwrap()]);
    assert!(!run.passed(), "20-char budget enforced: {}", run.stdout);

    std::fs::write(&msg, "feat: short\n").unwrap();
    let run = r.hook("commit-msg", &[msg.to_str().unwrap()]);
    assert!(run.passed(), "{}", run.stdout);
}

/// Untrusted `set` lines are inert like every other policy line.
#[test]
fn an_untrusted_set_line_is_inert() {
    let r = Repo::new();
    r.stage("amont.conf", "set commit.subjectMax 20\n");
    let msg = r.path("msg.txt");
    std::fs::write(&msg, "feat: much too long for twenty\n").unwrap();
    let run = r.hook("commit-msg", &[msg.to_str().unwrap()]);
    assert!(
        run.passed(),
        "untrusted policy must not bind: {}",
        run.stdout
    );
}

/// A key policy may not reach is a loud, positioned gap — `fix` above all,
/// because a committed file must not change what trusted commands may DO.
#[test]
fn an_unsettable_key_is_a_positioned_gap() {
    let r = Repo::new();
    manifest(&r, "set fix true\n");
    r.stage("a.txt", "x\n");
    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.stdout);
    assert!(
        run.says("not a policy-settable key"),
        "the refusal is named: {}",
        run.stdout
    );
}

/// The display tells the truth: a policy-supplied value shows its origin.
#[test]
fn list_shows_the_policy_scope_for_settings() {
    let r = Repo::new();
    manifest(&r, "set commit.subjectMax 50\n");
    let out = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("list")
        .current_dir(&r.dir)
        .stdin(Stdio::null())
        .output()
        .expect("amont list");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(text.contains("(amont.conf)"), "{text}");
    assert!(text.contains("50"), "{text}");
}
