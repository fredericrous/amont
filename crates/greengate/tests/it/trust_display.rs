//! What the trust prompt shows is what gets trusted.
//!
//! `amont trust` prints a repository's declarations so a person can read
//! them before accepting. The names and commands in that listing are chosen by
//! whoever wrote the repository, and they were printed verbatim — so a
//! declaration whose name began with `\x1b[8m` (conceal) HID THE NEXT ONE.
//! The reader saw two declarations, pressed y, and got three.
//!
//! Nothing about the trust model was bypassed: the fingerprint still covered
//! every byte, the gate still ran. The rendering lied about what was being
//! accepted, which reaches the same place by a shorter route — and unlike a
//! race, it is not even timing-dependent.

use crate::common;
use common::Repo;

/// Run a subcommand with colour off, so any escape byte in the output can only
/// have come from the repository rather than from our own painting.
fn run(r: &Repo, args: &[&str]) -> String {
    let out = crate::common::amont_cmd()
        .args(args)
        .current_dir(&r.dir)
        .env("NO_COLOR", "1")
        .output()
        .expect("run amont");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
}

/// Three declarations, the second dressed to swallow the third.
fn concealing_manifest() -> String {
    format!(
        "pre-commit  first   *  block  /bin/true\n\
         pre-commit  {}hidden  *  block  /bin/true\n\
         pre-commit  third   *  block  /bin/true\n",
        "\u{1b}[8m"
    )
}

/// The listing must not be able to hide part of itself.
#[test]
fn a_declaration_cannot_conceal_the_next_one_from_the_trust_prompt() {
    let r = Repo::new();
    r.stage("x.txt", "seed\n");
    r.commit("chore: seed");
    r.stage("amont.conf", &concealing_manifest());

    let shown = run(&r, &["trust", "--show"]);

    assert!(
        !shown.contains('\u{1b}'),
        "an escape byte from the manifest reached the terminal: {shown:?}"
    );
    // And the declaration it was trying to swallow is still visible.
    for name in ["first", "hidden", "third"] {
        assert!(
            shown.contains(name),
            "declaration {name:?} is missing from the listing:\n{shown}"
        );
    }
}

/// `amont list` shows the same declarations, and is read more often.
#[test]
fn the_check_listing_does_not_pass_escapes_through_either() {
    let r = Repo::new();
    r.stage("x.txt", "seed\n");
    r.commit("chore: seed");
    r.stage("amont.conf", &concealing_manifest());

    let shown = run(&r, &["list"]);
    assert!(
        !shown.contains('\u{1b}'),
        "an escape byte from the manifest reached the terminal: {shown:?}"
    );
}

/// A malformed line is repo-controlled too: the parse error quotes the text it
/// could not understand, so the error path needs the same guard as the happy
/// one.
#[test]
fn a_parse_error_does_not_carry_the_manifests_escapes() {
    let r = Repo::new();
    r.stage("x.txt", "seed\n");
    r.commit("chore: seed");
    r.stage(
        "amont.conf",
        &format!("pre-commit  bad  *  {}nonsense  /bin/true\n", "\u{1b}[8m"),
    );

    let shown = run(&r, &["trust", "--show"]);
    assert!(
        !shown.contains('\u{1b}'),
        "an escape byte reached the terminal through an error: {shown:?}"
    );
}
