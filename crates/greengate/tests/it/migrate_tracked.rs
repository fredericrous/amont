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
    let r = Repo::new();
    r.stage("gate.sh", "printf x >> gate.log\n");
    r.stage(
        OLD_MANIFEST,
        "pre-commit  marks  *.txt  block  sh gate.sh\n",
    );
    r.stage(
        "AGENTS.md",
        &format!("# P\n\n{OLD_START}\nold block\n{OLD_END}\n"),
    );
    r.stage(
        ".github/attest-inputs",
        &format!("# names {OLD_MANIFEST}\ncargo-test Cargo.toml {OLD_MANIFEST} src\n"),
    );
    r.stage(".github/workflows/ci.yml", "env:\n  AMONT_TIMEOUT: 60\n");
    r.commit("chore: base");
    let trusted = r.run(&["trust"]);
    assert_eq!(trusted.code, 0, "{}", trusted.stderr);
    r
}

fn gate_runs(r: &Repo) -> usize {
    std::fs::read_to_string(r.path("gate.log"))
        .map(|log| log.len())
        .unwrap_or(0)
}

fn pre_commit(r: &Repo) -> crate::common::HookRun {
    r.hook_at(&r.dir, "pre-commit", &[])
}

#[test]
fn the_committed_names_are_renamed_staged_and_still_trusted() {
    let r = swept_3x();

    // Unswept, the old manifest is still read and its check runs.
    r.stage("a.txt", "a\n");
    let before = pre_commit(&r);
    assert_eq!(before.code, 0, "{}{}", before.stdout, before.stderr);
    assert_eq!(gate_runs(&r), 1, "the unswept manifest's check ran");
    r.commit("feat: a");

    let migrated = r.run(&["migrate", "--tracked"]);
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
    let status = r.git(&["diff", "--cached", "--name-status", "-M"]);
    let status = String::from_utf8_lossy(&status.stdout);
    assert!(
        status.contains("R100\tamont.conf\tgreengate.conf"),
        "{status}"
    );
    assert!(status.contains("M\tAGENTS.md"), "{status}");
    assert!(status.contains("M\t.github/attest-inputs"), "{status}");
    let agents = std::fs::read_to_string(r.path("AGENTS.md")).unwrap();
    assert!(
        agents.contains("<!-- greengate:start -->\nold block\n<!-- greengate:end -->"),
        "{agents}"
    );
    let inputs = std::fs::read_to_string(r.path(".github/attest-inputs")).unwrap();
    assert_eq!(
        inputs,
        format!("# names {OLD_MANIFEST}\ncargo-test Cargo.toml greengate.conf src\n")
    );
    assert!(
        std::fs::read_to_string(r.path(".github/workflows/ci.yml"))
            .unwrap()
            .contains("AMONT_TIMEOUT"),
        "a listed leftover is never edited"
    );

    // The rename commit passes the gate, and the check still runs: trust
    // is the content's blob id, which a rename keeps.
    r.stage("b.txt", "b\n");
    let after = pre_commit(&r);
    assert_eq!(after.code, 0, "{}{}", after.stdout, after.stderr);
    assert_eq!(
        gate_runs(&r),
        2,
        "the renamed manifest's check ran, trusted"
    );
    r.commit("chore: greengate names");

    // A second run has nothing to do.
    let again = r.run(&["migrate", "--tracked"]);
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
    let r = Repo::new();
    r.stage(OLD_MANIFEST, "pre-commit  a  *  warn  true\n");
    r.stage("greengate.conf", "pre-commit  b  *  warn  true\n");
    r.commit("chore: base");
    let refused = r.run(&["migrate", "--tracked"]);
    assert_eq!(refused.code, 1, "{}", refused.stderr);
    assert!(refused.stderr.contains("both"), "{}", refused.stderr);
    assert!(r.path(OLD_MANIFEST).is_file() && r.path("greengate.conf").is_file());
    let status = r.git(&["status", "--porcelain"]);
    assert!(status.stdout.is_empty(), "nothing staged or changed");
}
