//! What the trust prompt shows is what gets trusted.
//!
//! `greengate trust` prints a repository's declarations so a person can read
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
    // The 3.x manifest name: 4.0 still reads it in a repository the sweep has not
    // renamed, until the census removes that fallback.
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

/// `greengate list` shows the same declarations, and is read more often.
#[test]
fn the_check_listing_does_not_pass_escapes_through_either() {
    let r = Repo::new();
    r.stage("x.txt", "seed\n");
    r.commit("chore: seed");
    // The 3.x manifest name: 4.0 still reads it in a repository the sweep has not
    // renamed, until the census removes that fallback.
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

/// `trust` talks to a person, on stderr; `trust --show` answers with data, on
/// stdout. 4.0 moved the accepting path's listing and verdict off stdout, so
/// a script reading `trust --show` gets the listing alone, and one reading
/// `trust` gets nothing to parse by mistake.
#[test]
fn trust_talks_on_stderr_and_trust_show_answers_on_stdout() {
    let r = Repo::new();
    r.stage("x.txt", "seed\n");
    r.commit("chore: seed");
    r.stage("greengate.conf", "pre-commit  first  *  block  /bin/true\n");
    let streams = |args: &[&str]| {
        let out = crate::common::amont_cmd()
            .args(args)
            .current_dir(&r.dir)
            .env("NO_COLOR", "1")
            .output()
            .expect("run greengate");
        assert!(out.status.success(), "{args:?}: {out:?}");
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };

    let (stdout, stderr) = streams(&["trust"]);
    assert_eq!(stdout, "", "accepting prints nothing on stdout");
    assert!(stderr.contains("greengate.conf declares:"), "{stderr}");
    assert!(stderr.contains("first"), "{stderr}");
    assert!(stderr.contains("trusted ("), "{stderr}");

    let (stdout, stderr) = streams(&["trust"]);
    assert_eq!(stdout, "", "nothing to do prints nothing on stdout");
    assert!(stderr.contains("already trusted"), "{stderr}");

    let (stdout, _) = streams(&["trust", "--show"]);
    assert!(stdout.starts_with("greengate.conf\n"), "{stdout}");
    assert!(stdout.contains("first"), "{stdout}");
    assert!(stdout.contains("trusted here"), "{stdout}");
}
