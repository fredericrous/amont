//! `greengate migrate --tracked` through the real binary: a repository
//! greengate 3.x set up (`amont.conf` trusted, the old AGENTS.md markers,
//! `.github/attest-inputs` naming the manifest) gets every committed name
//! renamed and staged, and its declared check still runs afterwards, at
//! commit time, without a new `trust` (plan "greengate 4.0 renames its data
//! names", Phase 3).

use crate::common::Repo;

const OLD_MANIFEST: &str = "amont.conf";
const OLD_START: &str = "<!-- amont:start -->";
const OLD_END: &str = "<!-- amont:end -->";

/// A repository as greengate 3.x left it, its manifest trusted.
fn swept_3x() -> Repo {
    let repo = Repo::new();
    repo.stage("gate.sh", "printf x >> gate.log\n");
    repo.stage(
        OLD_MANIFEST,
        "pre-commit  marks  *.txt  block  sh gate.sh\n",
    );
    repo.stage(
        "AGENTS.md",
        &format!("# P\n\n{OLD_START}\nold block\n{OLD_END}\n"),
    );
    repo.stage(
        ".github/attest-inputs",
        &format!("# names {OLD_MANIFEST}\ncargo-test Cargo.toml {OLD_MANIFEST} src\n"),
    );
    repo.stage(".github/workflows/ci.yml", "env:\n  AMONT_TIMEOUT: 60\n");
    repo.commit("chore: base");
    let trusted = repo.run(&["trust"]);
    assert_eq!(trusted.code, 0, "{}", trusted.stderr);
    repo
}

fn gate_runs(repo: &Repo) -> usize {
    std::fs::read_to_string(repo.path("gate.log"))
        .map(|log| log.len())
        .unwrap_or(0)
}

fn pre_commit(repo: &Repo) -> crate::common::HookRun {
    repo.hook_at(&repo.dir, "pre-commit", &[])
}

#[test]
fn the_committed_names_are_renamed_staged_and_still_trusted() {
    let repo = swept_3x();

    // Unswept, the old manifest is still read and its check runs.
    repo.stage("a.txt", "a\n");
    let before = pre_commit(&repo);
    assert_eq!(before.code, 0, "{}{}", before.stdout, before.stderr);
    assert_eq!(gate_runs(&repo), 1, "the unswept manifest's check ran");
    repo.commit("feat: a");

    let migrated = repo.run(&["migrate", "--tracked"]);
    assert_eq!(migrated.code, 0, "{}", migrated.stderr);
    assert!(
        migrated
            .stderr
            .contains("renamed amont.conf to greengate.conf"),
        "{}",
        migrated.stderr
    );
    assert!(migrated.stderr.contains("AGENTS.md"), "{}", migrated.stderr);
    assert!(
        migrated.stderr.contains("ci.yml:2:  AMONT_TIMEOUT"),
        "a tracked AMONT_ setting is listed for reading: {}",
        migrated.stderr
    );

    // Staged as a rename, content unchanged; markers and inputs respelled.
    let status = repo.git(&["diff", "--cached", "--name-status", "-M"]);
    let status = String::from_utf8_lossy(&status.stdout);
    assert!(
        status.contains("R100\tamont.conf\tgreengate.conf"),
        "{status}"
    );
    assert!(status.contains("M\tAGENTS.md"), "{status}");
    assert!(status.contains("M\t.github/attest-inputs"), "{status}");
    let agents = std::fs::read_to_string(repo.path("AGENTS.md")).unwrap();
    assert!(
        agents.contains("<!-- greengate:start -->\nold block\n<!-- greengate:end -->"),
        "{agents}"
    );
    let inputs = std::fs::read_to_string(repo.path(".github/attest-inputs")).unwrap();
    assert_eq!(
        inputs,
        format!("# names {OLD_MANIFEST}\ncargo-test Cargo.toml greengate.conf src\n")
    );
    assert!(
        std::fs::read_to_string(repo.path(".github/workflows/ci.yml"))
            .unwrap()
            .contains("AMONT_TIMEOUT"),
        "a listed leftover is never edited"
    );

    // The rename commit passes the gate, and the check still runs: trust
    // is the content's blob id, which a rename keeps.
    repo.stage("b.txt", "b\n");
    let after = pre_commit(&repo);
    assert_eq!(after.code, 0, "{}{}", after.stdout, after.stderr);
    assert_eq!(
        gate_runs(&repo),
        2,
        "the renamed manifest's check ran, trusted"
    );
    repo.commit("chore: greengate names");

    // A second run has nothing to do.
    let again = repo.run(&["migrate", "--tracked"]);
    assert_eq!(again.code, 0, "{}", again.stderr);
    assert!(
        again.stderr.contains("nothing committed"),
        "{}",
        again.stderr
    );
}

/// Both spellings at once is a hand edit: refused, nothing changed.
#[test]
fn both_manifests_are_refused_and_nothing_moves() {
    let repo = Repo::new();
    repo.stage(OLD_MANIFEST, "pre-commit  a  *  warn  true\n");
    repo.stage("greengate.conf", "pre-commit  b  *  warn  true\n");
    repo.commit("chore: base");
    let refused = repo.run(&["migrate", "--tracked"]);
    assert_eq!(refused.code, 1, "{}", refused.stderr);
    assert!(refused.stderr.contains("both"), "{}", refused.stderr);
    assert!(
        refused.stderr.contains("(`git rm` it)"),
        "{}",
        refused.stderr
    );
    assert!(
        !refused.stderr.contains("git status"),
        "refused before anything moved, so no staging hint: {}",
        refused.stderr
    );
    assert!(repo.path(OLD_MANIFEST).is_file() && repo.path("greengate.conf").is_file());
    let status = repo.git(&["status", "--porcelain"]);
    assert!(status.stdout.is_empty(), "nothing staged or changed");
}

/// `--tracked` with a flag of the local move is a usage error: exit 2, no
/// migration hint, nothing changed.
#[test]
fn tracked_stands_alone() {
    let repo = Repo::new();
    repo.stage(OLD_MANIFEST, "pre-commit  a  *  warn  true\n");
    repo.commit("chore: base");
    let refused = repo.run(&["migrate", "--tracked", "--reverse"]);
    assert_eq!(refused.code, 2, "{}", refused.stderr);
    assert!(
        refused.stderr.contains("stands alone"),
        "{}",
        refused.stderr
    );
    assert!(!refused.stderr.contains("doctor"), "{}", refused.stderr);
    assert!(repo.path(OLD_MANIFEST).is_file());
}
