//! greengate 4.0's first run in a repository greengate 3.x used, through
//! the real binary: refused while 3.x's parked changes are there, then the
//! state and keys moved once, `doctor` clean, and a `migrate --reverse`
//! that the next command does not undo (plan "greengate 4.0 renames its
//! data names", Phase 2).

use crate::common::Repo;

fn config(repo: &Repo, key: &str) -> String {
    String::from_utf8_lossy(&repo.git(&["config", "--get-all", key]).stdout).into_owned()
}

#[test]
fn a_3x_repository_is_moved_once_and_a_reverse_holds() {
    let repo = Repo::new();
    std::fs::write(
        repo.path(".git/amont-bypasses"),
        "amont-bypass-v1\n1 abc x\n",
    )
    .unwrap();
    repo.git(&["config", "amont.timeout", "700"]);
    std::fs::create_dir_all(repo.path(".git/amont-held")).unwrap();

    // 3.x's parked changes: refused, nothing moved, the way out named.
    let refused = repo.run(&["list"]);
    assert_eq!(refused.code, 1, "{}", refused.stderr);
    assert!(refused.stderr.contains("amont-held"), "{}", refused.stderr);
    assert!(refused.stderr.contains("restore"), "{}", refused.stderr);
    assert!(repo.path(".git/amont-bypasses").is_file());
    assert_eq!(config(&repo, "amont.timeout"), "700\n");

    // `restore` is not held back by the migration, and finds the old store.
    let restored = repo.run(&["restore"]);
    assert_eq!(restored.code, 0, "{}{}", restored.stdout, restored.stderr);
    assert!(
        !repo.path(".git/amont-held").exists(),
        "restore took the 3.x store"
    );

    // The first command moves everything, once.
    let listed = repo.run(&["list"]);
    assert_eq!(listed.code, 0, "{}", listed.stderr);
    assert_eq!(
        std::fs::read_to_string(repo.path(".git/greengate-bypasses")).unwrap(),
        "greengate-bypass-v1\n1 abc x\n"
    );
    assert!(!repo.path(".git/amont-bypasses").exists());
    assert_eq!(config(&repo, "greengate.timeout"), "700\n");
    assert_eq!(config(&repo, "amont.timeout"), "");
    assert!(repo.path(".git/greengate-migrated").is_file());

    let doctor = repo.run(&["doctor"]);
    assert!(
        !doctor.stdout.contains("state:") && !doctor.stdout.contains("local config:"),
        "{}",
        doctor.stdout
    );

    // Back to 3.x's names, and the next 4.0 command leaves them.
    let reversed = repo.run(&["migrate", "--reverse"]);
    assert_eq!(reversed.code, 0, "{}", reversed.stderr);
    assert_eq!(
        std::fs::read_to_string(repo.path(".git/amont-bypasses")).unwrap(),
        "amont-bypass-v1\n1 abc x\n"
    );
    assert_eq!(config(&repo, "amont.timeout"), "700\n");
    let again = repo.run(&["list"]);
    assert_eq!(again.code, 0, "{}", again.stderr);
    assert!(
        repo.path(".git/amont-bypasses").is_file(),
        "a reverse is not undone"
    );

    // Until `migrate` is asked for by name.
    let forward = repo.run(&["migrate"]);
    assert_eq!(forward.code, 0, "{}", forward.stderr);
    assert!(forward.stderr.contains("moved"), "{}", forward.stderr);
    assert!(repo.path(".git/greengate-bypasses").is_file());
}

#[test]
fn a_dry_run_moves_nothing() {
    let repo = Repo::new();
    std::fs::write(repo.path(".git/amont-bypasses"), "amont-bypass-v1\n").unwrap();
    let out = repo.run(&["land", "--dry-run"]);
    assert!(
        repo.path(".git/amont-bypasses").is_file(),
        "a dry run promised to change nothing: {}",
        out.stderr
    );
    assert!(!repo.path(".git/greengate-migrated").exists());
}

/// `repo`'s binary with a config home and global git config of its own,
/// so a test of the global half sees no other test's marker.
fn own_home(repo: &Repo, args: &[&str]) -> std::process::Output {
    let mut cmd = crate::common::amont_cmd();
    cmd.args(args)
        .current_dir(&repo.dir)
        .env("XDG_CONFIG_HOME", repo.path("own-config"))
        .env("GIT_CONFIG_GLOBAL", repo.path("own-gitconfig"))
        .stdin(std::process::Stdio::null());
    cmd.output().expect("run greengate")
}

fn global(repo: &Repo, key: &str) -> String {
    let out = std::process::Command::new("git")
        .args(["config", "--file"])
        .arg(repo.path("own-gitconfig"))
        .args(["--get-all", key])
        .output()
        .expect("git config --file");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_global_reverse_holds_until_migrate_global() {
    let repo = Repo::new();
    std::fs::write(repo.path("own-gitconfig"), "[amont]\n\thostSlots = 3\n").unwrap();
    let first = own_home(&repo, &["list"]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(global(&repo, "greengate.hostSlots"), "3\n");

    let back = own_home(&repo, &["migrate", "--reverse", "--global"]);
    assert!(
        back.status.success(),
        "{}",
        String::from_utf8_lossy(&back.stderr)
    );
    assert_eq!(global(&repo, "amont.hostSlots"), "3\n");
    let again = own_home(&repo, &["list"]);
    assert!(again.status.success());
    assert_eq!(
        global(&repo, "amont.hostSlots"),
        "3\n",
        "a global reverse is not undone"
    );

    let forward = own_home(&repo, &["migrate", "--global"]);
    assert!(
        forward.status.success(),
        "{}",
        String::from_utf8_lossy(&forward.stderr)
    );
    assert_eq!(global(&repo, "greengate.hostSlots"), "3\n");
}

#[test]
fn doctor_names_a_3x_variable_left_set() {
    let repo = Repo::new();
    let out = crate::common::amont_cmd()
        .arg("doctor")
        .current_dir(&repo.dir)
        .env("AMONT_TIMEOUT", "5")
        .env("GIT_CONFIG_GLOBAL", repo.path("fake-gitconfig"))
        .output()
        .expect("run doctor");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("AMONT_TIMEOUT") && stdout.contains("GREENGATE_TIMEOUT"),
        "{stdout}"
    );
}

#[test]
fn asking_for_help_moves_nothing() {
    let repo = Repo::new();
    std::fs::write(repo.path(".git/amont-bypasses"), "amont-bypass-v1\n").unwrap();
    for args in [["list", "--help"], ["list", "--bogus"]] {
        repo.run(&args);
        assert!(
            repo.path(".git/amont-bypasses").is_file(),
            "{args:?} moved the 3.x state"
        );
    }
    assert!(!repo.path(".git/greengate-migrated").exists());
}

#[test]
fn a_refusal_keeps_its_lines_and_the_rm_line_pastes() {
    let repo = Repo::new();
    std::fs::create_dir_all(repo.path(".git/amont")).unwrap();
    let pending = repo.path(".git/amont/cluster-pending");
    std::fs::write(&pending, "not json").unwrap();
    let out = repo.run(&["list"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(!out.stderr.contains("\\x0a"), "{}", out.stderr);
    // The binary names the path as the filesystem resolves it.
    let rm = format!(
        "  rm {}",
        std::fs::canonicalize(&pending).unwrap().display()
    );
    assert!(
        out.stderr.lines().any(|line| line == rm),
        "no line is exactly {rm:?}:\n{}",
        out.stderr
    );
}
