//! greengate 4.0's first run in a repository greengate 3.x used, through
//! the real binary: refused while 3.x's parked changes are there, then the
//! state and keys moved once, `doctor` clean, and a `migrate --reverse`
//! that the next command does not undo (plan "greengate 4.0 renames its
//! data names", Phase 2).

use crate::common::Repo;

fn config(r: &Repo, key: &str) -> String {
    String::from_utf8_lossy(&r.git(&["config", "--get-all", key]).stdout).into_owned()
}

#[test]
fn a_3x_repository_is_moved_once_and_a_reverse_holds() {
    let r = Repo::new();
    std::fs::write(r.path(".git/amont-bypasses"), "amont-bypass-v1\n1 abc x\n").unwrap();
    r.git(&["config", "amont.timeout", "700"]);
    std::fs::create_dir_all(r.path(".git/amont-held")).unwrap();

    // 3.x's parked changes: refused, nothing moved, the way out named.
    let refused = r.run(&["list"]);
    assert_eq!(refused.code, 1, "{}", refused.stderr);
    assert!(refused.stderr.contains("amont-held"), "{}", refused.stderr);
    assert!(refused.stderr.contains("restore"), "{}", refused.stderr);
    assert!(r.path(".git/amont-bypasses").is_file());
    assert_eq!(config(&r, "amont.timeout"), "700\n");

    // `restore` is not held back by the migration, and finds the old store.
    let restored = r.run(&["restore"]);
    assert_eq!(restored.code, 0, "{}{}", restored.stdout, restored.stderr);
    assert!(
        !r.path(".git/amont-held").exists(),
        "restore took the 3.x store"
    );

    // The first command moves everything, once.
    let listed = r.run(&["list"]);
    assert_eq!(listed.code, 0, "{}", listed.stderr);
    assert_eq!(
        std::fs::read_to_string(r.path(".git/greengate-bypasses")).unwrap(),
        "greengate-bypass-v1\n1 abc x\n"
    );
    assert!(!r.path(".git/amont-bypasses").exists());
    assert_eq!(config(&r, "greengate.timeout"), "700\n");
    assert_eq!(config(&r, "amont.timeout"), "");
    assert!(r.path(".git/greengate-migrated").is_file());

    let doctor = r.run(&["doctor"]);
    assert!(
        !doctor.stdout.contains("state:") && !doctor.stdout.contains("local config:"),
        "{}",
        doctor.stdout
    );

    // Back to 3.x's names, and the next 4.0 command leaves them.
    let reversed = r.run(&["migrate", "--reverse"]);
    assert_eq!(reversed.code, 0, "{}", reversed.stderr);
    assert_eq!(
        std::fs::read_to_string(r.path(".git/amont-bypasses")).unwrap(),
        "amont-bypass-v1\n1 abc x\n"
    );
    assert_eq!(config(&r, "amont.timeout"), "700\n");
    let again = r.run(&["list"]);
    assert_eq!(again.code, 0, "{}", again.stderr);
    assert!(
        r.path(".git/amont-bypasses").is_file(),
        "a reverse is not undone"
    );

    // Until `migrate` is asked for by name.
    let forward = r.run(&["migrate"]);
    assert_eq!(forward.code, 0, "{}", forward.stderr);
    assert!(forward.stderr.contains("moved"), "{}", forward.stderr);
    assert!(r.path(".git/greengate-bypasses").is_file());
}

#[test]
fn a_dry_run_moves_nothing() {
    let r = Repo::new();
    std::fs::write(r.path(".git/amont-bypasses"), "amont-bypass-v1\n").unwrap();
    let out = r.run(&["land", "--dry-run"]);
    assert!(
        r.path(".git/amont-bypasses").is_file(),
        "a dry run promised to change nothing: {}",
        out.stderr
    );
    assert!(!r.path(".git/greengate-migrated").exists());
}
