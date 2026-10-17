//! commit-msg, ported from its zsh suite.

use crate::common;
use common::Repo;

/// Run the hook over a message file and return what it left behind.
fn rewrite(r: &Repo, msg: &str) -> (bool, String) {
    r.write("MSG", msg);
    let run = r.hook("commit-msg", &["MSG"]);
    (
        run.passed(),
        std::fs::read_to_string(r.path("MSG")).unwrap_or_default(),
    )
}

/// What the shipped limits and vocabulary accept, judged by the hook as a
/// commit would. The parsing behind each verdict is unit-tested beside it.
#[test]
fn the_default_rules_judge_each_subject() {
    let r = Repo::new();
    let accepted = [
        "feat: a short one\n".to_owned(),
        // the subject may run to 72 while the description stays under 50
        "feat(a-very-long-scope-name-here): short desc\n".to_owned(),
        "fix(parser): trim\n".to_owned(),
        "fix(my-scope): trim\n".to_owned(),
        "♻️  refactor: already prefixed\n".to_owned(),
    ]
    .into_iter()
    // every type in the vocabulary: this caught the commit-type and
    // branch-prefix lists diverging
    .chain(
        [
            "build", "chore", "docs", "feat", "fix", "perf", "refactor", "revert", "style", "test",
            "add", "remove",
        ]
        .map(|t| format!("{t}: a description\n")),
    );
    for msg in accepted {
        assert!(rewrite(&r, &msg).0, "rejected: {msg:?}");
    }
    for msg in [
        format!("feat: {}", "x".repeat(80)), // subject over 72
        format!("feat: {}", "d".repeat(60)), // description over 50
        "just a message\n".to_owned(),
        "feat:\n".to_owned(),
        // git's pass-through shapes are exact: a human subject starting with
        // one of the words is still judged
        "Merges: cleanup\n".to_owned(),
        "Reverted the parser\n".to_owned(),
        "fixup the parser\n".to_owned(),
        "Mergesort implemented\n".to_owned(),
    ] {
        assert!(!rewrite(&r, &msg).0, "accepted: {msg:?}");
    }
}

/// Messages git itself writes pass through unjudged — and UNTOUCHED. `git
/// merge` invokes this hook (githooks(5)); blocking its own message blocks
/// merging, and the workaround people reach for is `--no-verify`, which
/// turns off the checks that DO apply.
#[test]
fn git_generated_messages_pass_through_untouched() {
    let r = Repo::new();
    for msg in [
        "Merge branch 'feat/x' into main\n",
        "Merge remote-tracking branch 'origin/main'\n",
        "Merge pull request #12 from fork/feat\n",
        "Revert \"feat: the thing\"\n\nThis reverts commit abc123.\n",
        "Reapply \"feat: the thing\"\n",
        "fixup! feat: the thing\n",
        "squash! feat: the thing\n",
        "amend! feat: the thing\n",
    ] {
        let (passed, after) = rewrite(&r, msg);
        assert!(passed, "blocked git's own message: {msg:?}");
        assert_eq!(after, msg, "rewrote git's own message: {msg:?}");
    }
}

/// Each placement puts the emoji where it says, however git's value is
/// spelled, and the shipped default decorates nothing.
///
/// A gitmoji in every commit subject is the most divisive opinion this project
/// ever held, and it used to be unavoidable — `commit-msg` takes no
/// `hook.skip` and no severity override, so short of `--no-verify` the emoji
/// could be complied with or uninstalled. It is now a placement somebody
/// chooses.
#[test]
fn each_placement_puts_the_emoji_where_it_says() {
    let r = Repo::new();
    for (placement, expected) in [
        (None, "feat: add a thing"),
        (Some("none"), "feat: add a thing"),
        (Some("prefix"), "✨  feat: add a thing"),
        (Some("PREFIX"), "✨  feat: add a thing"),
        (Some("Prefix"), "✨  feat: add a thing"),
        (Some("suffix"), "feat: add a thing ✨"),
        (Some("replace"), "✨  add a thing"),
    ] {
        if let Some(p) = placement {
            r.git(&["config", "amont.commit.gitmoji", p]);
        }
        let (ok, out) = rewrite(&r, "feat: add a thing\n");
        assert!(ok, "{placement:?} rejected the message: {out:?}");
        assert_eq!(out.lines().next(), Some(expected), "{placement:?}: {out:?}");
    }
}

/// PROPERTY: re-running the hook over its own output changes nothing, under
/// every placement.
///
/// This is what `--amend`, a rebase reword and a `--no-verify` retry all do.
/// Three bugs shipped here: `suffix` grew one emoji per run; `replace`
/// REJECTED its own output (it demands a type prefix, and it had just
/// replaced the type with an emoji); and the gap after the subject grew a
/// blank line per run, because `splitn` handed the body back with its
/// separator and the format string wrote another. A subject at exactly the
/// description limit must also survive its own decoration, or the first
/// amend of a maximal subject is rejected for length the hook added.
#[test]
fn re_running_over_a_stored_message_is_a_no_op() {
    let r = Repo::new();
    let maximal = format!("feat: {}\n", "x".repeat(50));
    for placement in ["none", "prefix", "suffix", "replace"] {
        r.git(&["config", "amont.commit.gitmoji", placement]);
        for typed in ["feat(api)!: add a thing\n\nbody\n", maximal.as_str()] {
            let (ok, once) = rewrite(&r, typed);
            assert!(ok, "{placement} rejected {typed:?}: {once:?}");
            let (ok, twice) = rewrite(&r, &once);
            assert!(ok, "{placement} rejected its own output: {once:?}");
            let (_, thrice) = rewrite(&r, &twice);
            assert_eq!(once, twice, "{placement}: the second pass changed it");
            assert_eq!(twice, thrice, "{placement}: the third pass changed it");
        }
        let (_, once) = rewrite(&r, "feat: subject\n\nbody\n");
        assert_eq!(
            once.lines().take_while(|l| *l != "body").count(),
            2,
            "{placement}: expected subject + one blank line: {once:?}"
        );
    }
}

/// An emoji the author chose is theirs — we neither recover a type from it nor
/// strip it back off.
#[test]
fn an_emoji_the_author_chose_is_left_alone() {
    let r = Repo::new();
    r.git(&["config", "amont.commit.gitmoji", "suffix"]);
    let (ok, out) = rewrite(&r, "feat: ship it 🚀\n");
    assert!(ok);
    assert_eq!(out.lines().next(), Some("feat: ship it 🚀 ✨"), "{out:?}");
}

/// A body quoting a conventional commit — a revert citing what it undid — must
/// not be promoted into the subject. The JS once used /ms flags and rewrote the
/// commit with that body line as the new summary.
#[test]
fn a_body_quoting_a_conventional_commit_is_not_promoted() {
    let r = Repo::new();
    let (ok, out) = rewrite(
        &r,
        "revert: undo it\n\nIn abc1234 (\"fix(scope): thing\")\n",
    );
    assert!(ok);
    assert!(out.lines().next().unwrap().contains("revert: undo it"));
}

/// A subject that merely LOOKS like a trailer must not eat the trailer block.
///
/// `fix: pre-commit: stop hanging` contains the fragment `pre-commit: s`, which
/// the old anywhere-in-the-line footer rule accepted. `group_footer` then
/// scanned every line from the bottom, never found a non-footer, split at index
/// 0 and emitted the subject INSIDE the footer group behind a blank line. git
/// strips that leading blank, the subject ends up glued to the trailer with no
/// blank line between them, and `%(trailers)` reports NOTHING for a commit that
/// visibly carries a `Co-Authored-By`.
///
/// Only an end-to-end commit can assert that last part: `%(trailers)` is git's
/// own parse of the stored message, not something the hook can be asked for.
#[test]
fn a_colon_in_the_subject_does_not_destroy_the_trailers() {
    let r = Repo::new();
    let (ok, out) = rewrite(
        &r,
        "fix: pre-commit: stop hanging\n\nCo-Authored-By: a <a@x>\n",
    );
    assert!(ok, "hook rejected the message: {out:?}");

    r.stage("f.txt", "x");
    let msg = r.path("MSG");
    r.git(&[
        "commit",
        "-q",
        "--no-verify",
        "-F",
        msg.to_str().expect("utf8 path"),
    ]);

    let logged = r.git(&["log", "-1", "--pretty=%(trailers:key=Co-Authored-By)"]);
    let trailers = String::from_utf8_lossy(&logged.stdout).into_owned();
    assert!(
        trailers.contains("Co-Authored-By: a <a@x>"),
        "git saw no trailers ({trailers:?}) in the rewritten message {out:?}"
    );
}

/// The limits are what the repository says they are.
#[test]
fn the_configured_limits_are_what_is_enforced() {
    let r = Repo::new();
    let sixty = format!("feat: {}\n", "d".repeat(60));
    assert!(!rewrite(&r, &sixty).0, "60 > the default budget of 50");

    r.git(&["config", "amont.commit.descriptionMax", "60"]);
    assert!(rewrite(&r, &sixty).0, "a raised budget was not honoured");

    // And the boundary is exact, on both sides of it.
    let sixty_one = format!("feat: {}\n", "d".repeat(61));
    assert!(!rewrite(&r, &sixty_one).0, "61 > the configured 60");

    let r2 = Repo::new();
    r2.git(&["config", "amont.commit.subjectMax", "20"]);
    assert!(!rewrite(&r2, "feat: a description that is plainly too long\n").0);
    assert!(rewrite(&r2, "feat: short\n").0);
}

/// `0` leaves the body exactly as written — what keeps a pasted stack trace or
/// a fenced code block intact.
#[test]
fn a_zero_wrap_column_leaves_the_body_alone() {
    let long = "x ".repeat(80);
    let msg = format!("feat: x\n\n{long}\n");

    let r = Repo::new();
    let (ok, wrapped) = rewrite(&r, &msg);
    assert!(ok);
    assert!(
        wrapped.lines().any(|l| l.chars().count() <= 72),
        "the default should have wrapped: {wrapped:?}"
    );

    let r2 = Repo::new();
    r2.git(&["config", "amont.commit.bodyWrap", "0"]);
    let (ok, intact) = rewrite(&r2, &msg);
    assert!(ok);
    assert!(
        intact.contains(long.trim_end()),
        "the body was wrapped anyway: {intact:?}"
    );
}

/// A value git cannot parse falls back to the shipped default AND says so.
/// Silence here is the failure this whole config surface is arranged against:
/// a limit you believe you raised and did not.
#[test]
fn a_bad_config_value_is_never_silent() {
    let r = Repo::new();
    r.git(&["config", "amont.commit.descriptionMax", "wide"]);
    r.write("MSG", "feat: a short one\n");
    let run = r.hook("commit-msg", &["MSG"]);
    assert!(run.passed(), "the default should still apply");
    assert!(
        run.says("amont.commit.descriptionMax"),
        "the key was not named: {:?}",
        run.output()
    );

    // The default is genuinely what applied, not some other number.
    let sixty = format!("feat: {}\n", "d".repeat(60));
    assert!(!rewrite(&r, &sixty).0);
}

/// Out of range is a mistake, reported the same way — a limit of 0 would block
/// every commit forever from a config file.
#[test]
fn a_limit_outside_its_range_is_reported_and_ignored() {
    let r = Repo::new();
    r.git(&["config", "amont.commit.subjectMax", "0"]);
    r.write("MSG", "feat: a short one\n");
    let run = r.hook("commit-msg", &["MSG"]);
    assert!(run.passed(), "a zero limit must not block everything");
    assert!(run.says("amont.commit.subjectMax"), "{:?}", run.output());
}

/// A word that is not a placement names what was wanted, rather than sending
/// the reader to the documentation for a list we already hold.
#[test]
fn an_unknown_placement_lists_the_ones_that_exist() {
    let r = Repo::new();
    r.git(&["config", "amont.commit.gitmoji", "sideways"]);
    r.write("MSG", "feat: a short one\n");
    let run = r.hook("commit-msg", &["MSG"]);
    assert!(run.passed());
    let said = run.output();
    for word in ["none", "prefix", "suffix", "replace"] {
        assert!(said.contains(word), "{word} not offered: {said:?}");
    }
}
