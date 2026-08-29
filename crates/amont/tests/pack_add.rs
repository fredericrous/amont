//! `amont add` — vendoring a pack's declarations into `amont.conf`.
//!
//! The property every test here circles is that **adding is not trusting**. A
//! pack arrives as text; the manifest's content fingerprint changes; and every
//! declared check — the new ones and any that were already trusted — is inert
//! until a human runs `amont trust`. If that ever stops being true, a remote
//! source has been handed the one decision the trust gate exists to keep local.
//!
//! No test here touches the network: a pack source is just a git repository,
//! so a local path is a complete fixture.

mod common;

use common::Repo;

/// A git repository carrying `amont.pack`, usable as a source.
fn pack_repo(body: &str) -> Repo {
    let r = Repo::new();
    r.stage("amont.pack", body);
    r.commit("feat: pack");
    r
}

fn source(pack: &Repo) -> String {
    pack.path("")
        .to_string_lossy()
        .trim_end_matches('/')
        .to_string()
}

fn manifest(repo: &Repo) -> String {
    std::fs::read_to_string(repo.path("amont.conf")).unwrap_or_default()
}

/// A consumer with one hand-written declaration, already trusted.
fn consumer() -> Repo {
    let r = Repo::new();
    r.stage("amont.conf", "pre-commit  mine  *  block  true\n");
    r.commit("feat: seed");
    assert!(r.run(&["trust"]).passed(), "precondition: trusted");
    r
}

#[test]
fn a_pack_lands_inside_markers_with_its_commit_id() {
    let pack = pack_repo("pre-commit  hadolint  Dockerfile  block  hadolint\n");
    let repo = consumer();

    let run = repo.run(&["add", &source(&pack)]);
    assert!(run.passed(), "{}", run.output());

    let text = manifest(&repo);
    assert!(
        text.contains("pre-commit  mine"),
        "the existing line survives"
    );
    assert!(text.contains("pre-commit  hadolint  Dockerfile  block  hadolint"));
    assert!(text.contains("# amont:pack:start"), "{text}");
    assert!(text.contains("# amont:pack:end"), "{text}");

    // The recorded id is the pack's commit, not the revision that named it.
    let id = String::from_utf8_lossy(&pack.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    assert!(text.contains(&id), "the commit id is recorded: {text}");
}

/// THE property. Adding text is not consenting to run it — and the append
/// invalidates the whole file, so a check that WAS trusted goes inert too.
#[test]
fn adding_does_not_trust_and_leaves_everything_inert() {
    let pack = pack_repo("pre-commit  hadolint  Dockerfile  block  hadolint\n");
    let repo = consumer();
    repo.run(&["add", &source(&pack)]);

    let run = repo.run(&["run"]);
    assert!(
        run.says("mine") && run.says("hadolint"),
        "both the old and the new check must be held: {}",
        run.output()
    );
    assert!(
        run.says("trust"),
        "the way out has to be named: {}",
        run.output()
    );

    // …and trusting releases them.
    assert!(repo.run(&["trust"]).passed());
    let after = repo.run(&["run"]);
    assert!(
        !after.says("untrusted") && !after.says("changed since it was trusted"),
        "after trust nothing is held: {}",
        after.output()
    );
}

/// A pack's own rows are not privileged: editing one by hand revokes consent
/// exactly as editing a hand-written line does. Without this the block would
/// be a hole in the gate rather than a record of it.
#[test]
fn editing_a_vendored_row_revokes_trust() {
    let pack = pack_repo("pre-commit  hadolint  Dockerfile  block  hadolint\n");
    let repo = consumer();
    repo.run(&["add", &source(&pack)]);
    repo.run(&["trust"]);

    let edited = manifest(&repo).replace("block  hadolint", "block  rm -rf /");
    std::fs::write(repo.path("amont.conf"), edited).unwrap();

    let run = repo.run(&["run"]);
    assert!(
        run.says("changed since it was trusted"),
        "a hand-edited pack row must revoke consent: {}",
        run.output()
    );
}

#[test]
fn re_adding_replaces_the_block_rather_than_appending() {
    let pack = pack_repo("pre-commit  hadolint  Dockerfile  block  hadolint\n");
    let repo = consumer();
    repo.run(&["add", &source(&pack)]);

    pack.stage(
        "amont.pack",
        "pre-commit  hadolint  Dockerfile  warn  hadolint --strict\n",
    );
    pack.commit("feat: v2");
    let run = repo.run(&["add", &source(&pack)]);
    assert!(run.passed(), "{}", run.output());

    let text = manifest(&repo);
    assert_eq!(
        text.matches("amont:pack:start").count(),
        1,
        "one block, not two — a duplicate id would break the manifest: {text}"
    );
    assert!(text.contains("hadolint --strict"), "{text}");
    assert!(
        !text.contains("block  hadolint\n"),
        "the old row is gone: {text}"
    );
    assert!(text.contains("pre-commit  mine"), "unrelated lines survive");
}

/// Policy is about the repository INSTALLING the pack. A third party reaching
/// `skip` could silence the secrets scan on somebody else's machine.
#[test]
fn a_pack_carrying_policy_is_refused_whole() {
    for body in [
        "pre-commit  ok  *  block  true\nskip  secrets\n",
        "pre-commit  ok  *  block  true\nseverity  secrets  warn\n",
        "pre-commit  ok  *  block  true\ntool  hadolint  2.12\n",
        "pre-commit  ok  *  block  true\nnot a declaration at all\n",
    ] {
        let pack = pack_repo(body);
        let repo = consumer();
        let before = manifest(&repo);

        let run = repo.run(&["add", &source(&pack)]);
        assert!(!run.passed(), "should refuse {body:?}: {}", run.output());
        assert_eq!(
            manifest(&repo),
            before,
            "refused whole — amont.conf must be untouched for {body:?}"
        );
    }
}

#[test]
fn dry_run_writes_nothing_but_shows_everything() {
    let pack = pack_repo("pre-commit  hadolint  Dockerfile  block  hadolint\n");
    let repo = consumer();
    let before = manifest(&repo);

    let run = repo.run(&["add", &source(&pack), "--dry-run"]);
    assert!(run.passed(), "{}", run.output());
    assert!(run.says("hadolint"), "the rows are shown: {}", run.output());
    assert_eq!(manifest(&repo), before, "--dry-run must not write");
}

#[test]
fn an_unresolvable_revision_fails_before_writing() {
    let pack = pack_repo("pre-commit  hadolint  Dockerfile  block  hadolint\n");
    let repo = consumer();
    let before = manifest(&repo);

    let run = repo.run(&["add", &format!("{}@no-such-tag", source(&pack))]);
    assert!(!run.passed(), "{}", run.output());
    assert_eq!(manifest(&repo), before);
}

#[test]
fn a_source_without_a_pack_is_reported() {
    let empty = Repo::new();
    empty.stage("readme.md", "not a pack\n");
    empty.commit("feat: seed");
    let repo = consumer();

    let run = repo.run(&["add", &source(&empty)]);
    assert!(!run.passed(), "{}", run.output());
    assert!(run.says("amont.pack"), "{}", run.output());
}

#[test]
fn usage_errors_exit_two() {
    let repo = consumer();
    assert_eq!(repo.run(&["add"]).code, 2, "no source named");
    assert_eq!(repo.run(&["add", "x", "--nope"]).code, 2, "unknown flag");
}

/// Once vendored and trusted, a pack row is an ORDINARY declaration: it runs
/// from the manifest with nothing fetched. The marker line is a comment the
/// parser skips, so the source URL never reaches any code path.
#[test]
fn a_vendored_row_is_just_a_declaration() {
    let pack = pack_repo("pre-commit  says-hi  *  block  echo vendored-check-ran\n");
    let repo = consumer();
    repo.run(&["add", &source(&pack)]);
    repo.run(&["trust"]);

    repo.stage("a.txt", "x\n");
    let run = repo.run(&["run"]);
    assert!(
        run.says("vendored-check-ran"),
        "the vendored command runs like any other: {}",
        run.output()
    );
}
