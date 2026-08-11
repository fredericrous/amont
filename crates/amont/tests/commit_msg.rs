//! commit-msg, ported from its zsh suite.

mod common;
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

#[test]
fn rejects_a_subject_over_72_characters() {
    let r = Repo::new();
    let long = format!("feat: {}", "x".repeat(80));
    assert!(!rewrite(&r, &long).0);
}

#[test]
fn accepts_a_short_conventional_subject() {
    let r = Repo::new();
    assert!(rewrite(&r, "feat: a short one\n").0);
}

#[test]
fn rejects_a_description_over_50_characters() {
    let r = Repo::new();
    let msg = format!("feat: {}", "d".repeat(60));
    assert!(!rewrite(&r, &msg).0);
}

/// The whole subject may run to 72 even when the scope is long, as long as the
/// description after the colon stays under 50.
#[test]
fn a_long_scope_is_fine_while_the_description_is_short() {
    let r = Repo::new();
    assert!(rewrite(&r, "feat(a-very-long-scope-name-here): short desc\n").0);
}

#[test]
fn rejects_a_message_with_no_type_prefix() {
    let r = Repo::new();
    assert!(!rewrite(&r, "just a message\n").0);
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

/// The pass-through is for git's EXACT shapes, not for humans who happen to
/// start a sentence with one of the words.
#[test]
fn human_subjects_near_the_git_shapes_are_still_judged() {
    let r = Repo::new();
    for msg in [
        "Merges: cleanup\n",       // not "Merge "
        "Reverted the parser\n",   // not "Revert \""
        "fixup the parser\n",      // not "fixup! "
        "Mergesort implemented\n", // not "Merge "
    ] {
        assert!(!rewrite(&r, msg).0, "let a human subject through: {msg:?}");
    }
}

#[test]
fn rejects_a_prefix_with_no_description() {
    let r = Repo::new();
    assert!(!rewrite(&r, "feat:\n").0);
}

/// The shipped default decorates nothing.
///
/// A gitmoji in every commit subject is the most divisive opinion this project
/// ever held, and it used to be unavoidable — `commit-msg` takes no
/// `hook.skip` and no severity override, so short of `--no-verify` the emoji
/// could be complied with or uninstalled. It is now a placement somebody
/// chooses.
#[test]
fn nothing_is_decorated_unless_a_placement_is_chosen() {
    let r = Repo::new();
    let (ok, out) = rewrite(&r, "feat: add a thing\n");
    assert!(ok);
    assert_eq!(
        out.lines().next(),
        Some("feat: add a thing"),
        "got: {out:?}"
    );
}

#[test]
fn each_placement_puts_the_emoji_where_it_says() {
    for (placement, expected) in [
        ("none", "feat: add a thing"),
        ("prefix", "✨  feat: add a thing"),
        ("suffix", "feat: add a thing ✨"),
        ("replace", "✨  add a thing"),
    ] {
        let r = Repo::new();
        r.git(&["config", "amont.commit.gitmoji", placement]);
        let (ok, out) = rewrite(&r, "feat: add a thing\n");
        assert!(ok, "{placement} rejected the message: {out:?}");
        assert_eq!(out.lines().next(), Some(expected), "{placement}: {out:?}");
    }
}

/// PROPERTY: re-running the hook over its own output changes nothing.
///
/// This is what `--amend`, a rebase reword and a `--no-verify` retry all do.
/// `suffix` used to grow one emoji per run, and `replace` REJECTED its own
/// output — it demands a type prefix, and it had just replaced the type with an
/// emoji.
#[test]
fn re_running_over_a_stored_message_is_a_no_op() {
    for placement in ["none", "prefix", "suffix", "replace"] {
        let r = Repo::new();
        r.git(&["config", "amont.commit.gitmoji", placement]);
        let (ok, once) = rewrite(&r, "feat(api)!: add a thing\n\nbody\n");
        assert!(ok, "{placement} rejected the message: {once:?}");
        let (ok, twice) = rewrite(&r, &once);
        assert!(ok, "{placement} rejected its own output: {once:?}");
        assert_eq!(once, twice, "{placement} is not idempotent");
    }
}

/// REGRESSION: the gap between subject and body must not grow on a re-run.
///
/// This shipped. `splitn` hands the body back with the separator newline still
/// on it, and the format string wrote another — so every `--amend` and every
/// rebase reword added one more blank line, for as long as somebody kept
/// editing the commit. It survived because the idempotence test covered
/// `group_footer` in isolation rather than the whole rewrite.
#[test]
fn re_running_does_not_grow_the_gap_after_the_subject() {
    let r = Repo::new();
    let (ok, once) = rewrite(&r, "feat: subject\n\nbody\n");
    assert!(ok);
    assert_eq!(
        once.lines().take_while(|l| *l != "body").count(),
        2,
        "expected subject + one blank line: {once:?}"
    );
    let (_, twice) = rewrite(&r, &once);
    let (_, thrice) = rewrite(&r, &twice);
    assert_eq!(once, twice, "the second pass changed the message");
    assert_eq!(twice, thrice, "the third pass changed the message");
}

/// A subject at exactly the limit must survive its own decoration, or the
/// first amend of a maximal subject is rejected for length the hook added.
#[test]
fn decoration_never_counts_against_the_subject_limit() {
    for placement in ["none", "prefix", "suffix", "replace"] {
        let r = Repo::new();
        r.git(&["config", "amont.commit.gitmoji", placement]);
        let typed = format!("feat: {}\n", "x".repeat(50));
        let (ok, once) = rewrite(&r, &typed);
        assert!(ok, "{placement} rejected a 56-character subject: {once:?}");
        assert!(
            rewrite(&r, &once).0,
            "{placement} rejected the subject it had just written: {once:?}"
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

#[test]
fn accepts_a_subject_that_already_carries_an_emoji() {
    let r = Repo::new();
    assert!(rewrite(&r, "♻️  refactor: already prefixed\n").0);
}

#[test]
fn accepts_scopes_including_hyphenated_ones() {
    let r = Repo::new();
    assert!(rewrite(&r, "fix(parser): trim\n").0);
    assert!(rewrite(&r, "fix(my-scope): trim\n").0);
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

#[test]
fn groups_footers_after_one_blank_line() {
    let r = Repo::new();
    let (ok, out) = rewrite(&r, "feat: x\n\nbody\n\nCo-Authored-By: a <a@x>\n");
    assert!(ok);
    assert!(out.ends_with("Co-Authored-By: a <a@x>\n"), "got: {out:?}");
    assert!(out.contains("body\n\nCo-Authored-By"));
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

/// Git's own boolean and integer spellings work, because git does the parsing.
#[test]
fn a_placement_is_matched_however_it_is_spelled() {
    for spelling in ["prefix", "PREFIX", "Prefix"] {
        let r = Repo::new();
        r.git(&["config", "amont.commit.gitmoji", spelling]);
        let (ok, out) = rewrite(&r, "feat: add a thing\n");
        assert!(ok);
        assert_eq!(
            out.lines().next(),
            Some("✨  feat: add a thing"),
            "{spelling}: {out:?}"
        );
    }
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

/// Every type in the vocabulary must be accepted — this is what caught the
/// commit-type/branch-prefix divergence being real rather than theoretical.
#[test]
fn every_declared_commit_type_is_accepted() {
    let r = Repo::new();
    for t in [
        "build", "chore", "docs", "feat", "fix", "perf", "refactor", "revert", "style", "test",
        "add", "remove",
    ] {
        assert!(rewrite(&r, &format!("{t}: a description\n")).0, "{t}");
    }
}
