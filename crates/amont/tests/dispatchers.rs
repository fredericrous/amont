//! The pre-commit and pre-push dispatchers.
//!
//! They are NOT the same shape and both shapes are load-bearing — pre-commit
//! runs its checks concurrently and reports EVERY failure; pre-push runs them
//! serially and stops at the FIRST. A shared "run all" helper is the obvious
//! way to lose that distinction, so both are pinned here.
//!
//! These cases used to plant synthetic `pre-commit-aaa` shell files and watch
//! them append to a log. That scaffolding went with the file-based sub-hooks,
//! and it was always testing the harness as much as the product: it proved a
//! glob ran three scripts, not that a commit is stopped for the right reason.
//! Each case now drives a REAL check to a real verdict. The concurrency itself
//! is unit-tested by rendezvous in `dispatch::tests`, next to the runner.

mod common;
use common::Repo;
use std::io::Write;
use std::process::{Command, Stdio};

/// Content that makes `pre-commit-merge-conflict` fail. Built rather than
/// written literally, or this file trips the very check it exercises —
/// `git grep --cached` scans the whole index, including this test.
fn conflicted() -> String {
    format!(
        "{}\nours\n{}\ntheirs\n{}\n",
        "<".repeat(7),
        "=".repeat(7),
        ">".repeat(7)
    )
}

/// Content that makes `pre-commit-ban-terms` fail.
const BANNED: &str = "fit('x', () => {});\n";

/// Feed `pre-push` a ref line on stdin, as git does.
fn pre_push(r: &Repo, remote_ref: &str) -> (i32, String) {
    let line = format!("refs/heads/local aaa {remote_ref} bbb\n");
    let mut child = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-push")
        .current_dir(&r.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(line.as_bytes())
        .expect("write");
    let out = child.wait_with_output().expect("wait");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn pre_commit_passes_a_clean_tree() {
    let r = Repo::new();
    r.stage("a.txt", "fine\n");
    assert!(r.hook("pre-commit", &[]).passed());
}

/// The reason pre-commit is concurrent rather than fail-fast: fixing lint one
/// error per commit attempt is the experience this avoids.
#[test]
fn pre_commit_reports_every_failure_not_just_the_first() {
    let r = Repo::new();
    r.stage("bad.txt", &conflicted());
    r.stage("bad.ts", BANNED);
    let run = r.hook("pre-commit", &[]);
    assert!(!run.passed());
    assert!(
        run.says("pre-commit-merge-conflict"),
        "missing merge-conflict: {}",
        run.stdout
    );
    assert!(
        run.says("pre-commit-ban-terms"),
        "missing ban-terms: {}",
        run.stdout
    );
}

/// A cherry-pick replays commits that already passed these checks, so
/// re-running the CONTENT checks turns a conflict resolution into a second
/// review. That was the reason for the original guard and it still holds.
///
/// What changed: the guard skipped the whole stage, which also silenced
/// `merge-conflict` — the one check a resolution commit most needs, because
/// leaving a marker in the commit that resolves the pick is the bug. Each check
/// now declares for itself, and this one keeps running. See
/// `tests/git_state.rs`.
#[test]
fn a_cherry_pick_pauses_the_content_checks_but_not_merge_conflict() {
    let r = Repo::new();
    r.stage("x.json", "{ BROKEN\n");
    assert!(!r.hook("pre-commit", &[]).passed(), "guard: fails normally");
    std::fs::write(r.path(".git/CHERRY_PICK_HEAD"), "deadbeef\n").expect("write");
    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "content checks must pause:\n{}", run.output());
    assert!(run.says("paused during a cherry-pick"), "{}", run.output());

    // …and the marker check is not among them.
    let r = Repo::new();
    r.stage("bad.txt", &conflicted());
    std::fs::write(r.path(".git/CHERRY_PICK_HEAD"), "deadbeef\n").expect("write");
    assert!(
        !r.hook("pre-commit", &[]).passed(),
        "a conflict marker must still block the resolution commit"
    );
}

/// A SHORT NAME names its check. This used to work by substring reach; it now
/// works because the short name is one of the three things that name a check,
/// which is the same convenience without the collateral.
#[test]
fn a_short_name_disables_its_check() {
    let r = Repo::new();
    r.stage("bad.txt", &conflicted());
    assert!(!r.hook("pre-commit", &[]).passed(), "guard");
    r.git(&["config", "--add", "hook.skip", "merge-conflict"]);
    assert!(r.hook("pre-commit", &[]).passed(), "skip should disable it");
}

/// And the full id names it too, so both spellings a reader might copy out of
/// the dashboard work.
#[test]
fn a_full_id_disables_its_check() {
    let r = Repo::new();
    r.stage("bad.txt", &conflicted());
    r.git(&["config", "--add", "hook.skip", "pre-commit-merge-conflict"]);
    assert!(r.hook("pre-commit", &[]).passed());
}

#[test]
fn pre_push_allows_an_ordinary_branch() {
    let r = Repo::new();
    r.stage("a.txt", "x\n");
    r.commit("feat: a");
    // The LOCAL branch has to satisfy branch-pattern too — a fresh repo is on
    // `main`, which that check rejects by design.
    r.git(&["checkout", "-q", "-b", "feat/x"]);
    let (code, out) = pre_push(&r, "refs/heads/feat/x");
    assert_eq!(code, 0, "{out}");
}

/// pre-push is fail-fast, and branch-protect is deliberately FIRST: the
/// cheapest and most decisive step. Nothing after it should have run.
#[test]
fn pre_push_stops_at_the_first_failure() {
    let r = Repo::new();
    r.stage("a.txt", "x\n");
    r.commit("feat: a");
    r.git(&["checkout", "-q", "-b", "feat/x"]);
    let (code, out) = pre_push(&r, "refs/heads/main");
    assert_ne!(code, 0);
    assert!(
        out.contains("forbidden"),
        "expected the protect message: {out}"
    );
    assert!(
        out.contains("pre-push-branch-protect"),
        "the failing check is named singularly: {out}"
    );
    assert!(
        !out.contains("upstream"),
        "pull-rebase ran after a failure — fail-fast is broken: {out}"
    );
}

/// An unknown hook name in hook mode is a shim from a NEWER template, not a
/// usage error — it absorbs at exit 0 and explains itself once. The loud
/// exit-2 this test used to pin is what printed `unknown hook` on every
/// commit of every machine whose binary lagged the template; the loudness
/// now lives on the interactive path (cli_dispatch pins both halves).
#[test]
fn an_unknown_hook_is_absorbed_as_version_skew() {
    let r = Repo::new();
    let run = r.hook("pre-commit-not-a-hook", &[]);
    assert_eq!(run.code, 0, "{}", run.output());
    assert!(run.says("newer than this binary"), "{}", run.output());
}

/// A skip is otherwise invisible at exactly the moment it matters: with
/// `hook.skip` set, a commit printed a wall of green ticks and no hint that a
/// check had been disabled. The developer sees a clean run and concludes they
/// are covered.
#[test]
fn skipped_checks_are_announced_by_name() {
    let r = Repo::new();
    r.stage("a.txt", "x\n");
    r.git(&["config", "--add", "hook.skip", "merge-conflict"]);
    let run = r.hook("pre-commit", &[]);
    assert!(run.passed());
    assert!(
        run.says("pre-commit-merge-conflict"),
        "the skipped check must be named: {}",
        run.stdout
    );
    assert!(run.says("skipped by"), "{}", run.stdout);
}

/// And silent otherwise — a line on every ordinary commit would be noise, and
/// noise is how a warning stops being read.
#[test]
fn no_skips_means_no_extra_output() {
    let r = Repo::new();
    r.stage("a.txt", "x\n");
    let run = r.hook("pre-commit", &[]);
    assert!(!run.says("skipped by"), "{}", run.stdout);
}

/// A one-letter skip used to disable all twenty checks, because matching was
/// `check.contains(skip)` and every name contains an `e`. The old doc comment
/// called that "a sharp edge, not a bug" and the dispatcher announced the
/// damage on every commit.
///
/// It now reaches nothing at all, so there is no damage to announce. A value
/// names a check only by its full id, its trigger, or its short name.
#[test]
fn a_one_letter_skip_now_reaches_nothing() {
    let r = Repo::new();
    r.stage("a.txt", "x\n");
    r.git(&["config", "--add", "hook.skip", "e"]);
    let run = r.hook("pre-commit", &[]);
    assert!(
        !run.says("skipped by"),
        "`e` should suppress nothing at all: {}",
        run.stdout
    );
    assert!(run.says("No merge conflict detected"), "{}", run.stdout);
}

/// A TRIGGER, though, still disables its whole stage — deliberately now, rather
/// than by accident of substring reach — and still says exactly what it cost.
#[test]
fn a_trigger_skip_disables_its_stage_and_says_so() {
    let r = Repo::new();
    r.stage("a.txt", "x\n");
    r.git(&["config", "--add", "hook.skip", "pre-commit"]);
    let run = r.hook("pre-commit", &[]);
    assert!(
        run.says("25 checks skipped"),
        "a trigger disables its stage: {}",
        run.stdout
    );
    // Named, not just counted: a number alone does not tell you what you lost.
    assert!(run.says("pre-commit-ban-terms"), "{}", run.stdout);
}

/// pre-push announces its own, separately — the two dispatchers have different
/// check lists and run at different moments.
#[test]
fn pre_push_announces_its_skips_too() {
    let r = Repo::new();
    r.stage("a.txt", "x\n");
    r.commit("feat: a");
    r.git(&["checkout", "-q", "-b", "feat/x"]);
    r.git(&["config", "--add", "hook.skip", "branch-protect"]);
    let (_, out) = pre_push(&r, "refs/heads/feat/x");
    assert!(
        out.contains("pre-push-branch-protect"),
        "expected the skip to be named at push time: {out}"
    );
}

/// `NO_COLOR` must actually work, not merely be documented.
///
/// The fleet spec asserted this property for several PRs while nothing read the
/// variable. Colour is now base ANSI so the terminal's own theme decides the
/// hue, and this checks the off switch exists — the glyphs carry the meaning
/// regardless, which is what makes turning colour off safe.
#[test]
fn no_color_suppresses_every_escape_sequence() {
    let r = Repo::new();
    r.stage("a.txt", "x\n");

    // `r.hook()` inherits this process's environment, and a caller running
    // with `NO_COLOR=1` set — or `TERM=dumb` — would make the "colour is on
    // by default" premise below false through no fault of the code under
    // test. Pin both explicitly rather than trusting whatever the shell that
    // happens to run `cargo test` left behind.
    let with_colour = std::process::Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-commit")
        .current_dir(&r.dir)
        .env_remove("NO_COLOR")
        .env("TERM", "xterm-256color")
        .output()
        .expect("run");
    let with_colour_stdout = String::from_utf8_lossy(&with_colour.stdout).into_owned();
    assert!(
        with_colour_stdout.contains('\u{1b}'),
        "colour is on by default: {with_colour_stdout:?}"
    );

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-commit")
        .current_dir(&r.dir)
        .env("NO_COLOR", "1")
        .output()
        .expect("run");
    let plain = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        !plain.contains('\u{1b}'),
        "NO_COLOR must leave no escape sequences: {plain:?}"
    );
    // And the screen stays readable without them.
    assert!(
        plain.contains('✓'),
        "the glyph carries the meaning: {plain}"
    );
}

/// A terminal that cannot render SGR gets none either.
#[test]
fn term_dumb_suppresses_colour() {
    let r = Repo::new();
    r.stage("a.txt", "x\n");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-commit")
        .current_dir(&r.dir)
        .env("TERM", "dumb")
        .output()
        .expect("run");
    assert!(!String::from_utf8_lossy(&out.stdout).contains('\u{1b}'));
}

/// `amont list` answers "would this run here, and if not why", which was
/// previously a code-reading exercise. It lives in the hook binary because that
/// is the one installed everywhere and the question is about the repo you are
/// standing in.
#[test]
fn list_reports_what_would_run_here() {
    let r = Repo::new();
    r.stage("Cargo.toml", "[package]\nname=\"t\"\n");
    r.stage("src/main.rs", "fn main() {}\n");
    r.commit("feat: a rust repo");

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("list")
        // Asserting on the LAYOUT, so ask for it without SGR rather than
        // matching around escape sequences.
        .env("NO_COLOR", "1")
        .current_dir(&r.dir)
        .output()
        .expect("run");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();

    // The trigger is a HEADING, and each row under it is the short name. It
    // used to be repeated on all twenty rows, which pushed the reason — the one
    // part that differs per row — eleven columns to the right.
    assert!(
        text.lines().any(|l| l == "pre-commit"),
        "the trigger heads its section: {text}"
    );
    let clippy = text.lines().find(|l| l.contains("clippy")).unwrap_or("");
    let ruff = text.lines().find(|l| l.contains("ruff")).unwrap_or("");
    assert!(
        !clippy.contains("pre-commit-"),
        "the heading already said the trigger: {clippy}"
    );
    // A rust repo: clippy runs, and is shown.
    assert!(clippy.contains('●'), "clippy should run here: {clippy}");

    // ruff is INERT, and the default view no longer spends a row on it. Half
    // this list is inert even in a repository amont serves well, and every one
    // of those rows names somebody else's language — read top to bottom that
    // said "this tool is for other people". It collapses to a count instead.
    assert!(
        ruff.is_empty(),
        "an inert check should not have its own row by default: {ruff}"
    );
    assert!(
        text.contains("active here.") && text.contains("inert"),
        "…but the count has to say they exist: {text}"
    );
    assert!(
        text.contains("--all"),
        "and name the way to see them: {text}"
    );

    // `--all` is that way, and it still answers "why is ruff not running".
    let all = r.run(&["list", "--all"]).output();
    let ruff = all.lines().find(|l| l.contains("ruff")).unwrap_or("");
    assert!(ruff.contains('○'), "ruff should be inert here: {ruff}");
    assert!(ruff.contains(".py"), "and say what it would need: {ruff}");
}

/// Collapsing the inert rows must never swallow something the reader has to
/// act on. `⊘` means somebody silenced a check and `✗` means a declaration is
/// broken — both are still one row each in the DEFAULT view, because a count
/// is the wrong shape for either.
#[test]
fn collapsing_inert_rows_never_hides_a_skip_or_a_broken_declaration() {
    let r = Repo::new();
    r.stage("main.rs", "fn main() {}\n");
    // A declaration the parser cannot read, and a silenced built-in.
    r.stage("amont.conf", "pre-commit  no-command-at-all\n");
    r.git(&["config", "--add", "hook.skip", "pre-commit-secrets"]);

    let text = r.run(&["list"]).output();
    assert!(
        text.contains('⊘'),
        "a skipped check keeps its row by default: {text}"
    );
    assert!(
        text.contains('✗'),
        "an unusable declaration keeps its row by default: {text}"
    );
    // …while the inert ones did collapse, or this test proves nothing.
    //
    // Matched on the ROW shape, not on the glyph anywhere: the legend at the
    // bottom spells out every glyph including this one, so `contains('○')` is
    // true of any output at all and would make the assertion vacuous.
    assert!(
        !text.lines().any(|l| l.starts_with("  ○")),
        "inert rows should be collapsed here: {text}"
    );
    assert!(text.contains("inert"), "and counted: {text}");
}

/// A skipped check is shown as skipped, not as inert — they mean different
/// things and the glyphs must not be interchangeable.
#[test]
fn list_distinguishes_skipped_from_inert() {
    let r = Repo::new();
    r.stage("Cargo.toml", "[package]\nname=\"t\"\n");
    r.stage("src/main.rs", "fn main() {}\n");
    r.commit("feat: a rust repo");
    r.git(&["config", "--add", "hook.skip", "pre-commit-clippy"]);

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("list")
        .current_dir(&r.dir)
        .output()
        .expect("run");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let clippy = text.lines().find(|l| l.contains("clippy")).unwrap_or("");
    assert!(clippy.contains('⊘'), "expected the skip glyph: {clippy}");
    assert!(clippy.contains("hook.skip"), "{clippy}");
}

// ---- list --json ---------------------------------------------------------

fn list_json(r: &Repo, extra: &[&str]) -> serde_json::Value {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_amont"));
    cmd.arg("list")
        .arg("--json")
        .args(extra)
        .current_dir(&r.dir);
    Repo::strip_git_env_impl(&mut cmd);
    let out = cmd.output().expect("run");
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "not valid JSON: {e}\n{}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

fn find_check<'a>(v: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    v["checks"]
        .as_array()
        .expect("checks array")
        .iter()
        .find(|c| c["id"] == id)
        .unwrap_or_else(|| panic!("{id} not found in {v}"))
}

#[test]
fn list_json_shape_matches_text() {
    let r = Repo::new();
    r.stage("Cargo.toml", "[package]\nname=\"t\"\n");
    r.stage("src/main.rs", "fn main() {}\n");
    r.commit("feat: a rust repo");

    let v = list_json(&r, &[]);
    assert!(v["stage_filter"].is_null());
    assert_eq!(v["pushed"], false);
    assert!(!v["checks"].as_array().unwrap().is_empty());

    let clippy = find_check(&v, "pre-commit-clippy");
    assert_eq!(clippy["status"], "runs");
    assert_eq!(clippy["source"], "builtin");
    assert!(clippy["command"].is_null());

    let ruff = find_check(&v, "pre-commit-ruff");
    assert_eq!(ruff["status"], "inert");
    assert!(ruff["reason"].as_str().unwrap().contains(".py"));
}

/// The one behaviour with no equivalent in the text output: a downgrade is
/// invisible to `git diff` but must not be invisible here, or an agent
/// reading only `declared_severity` would believe a downgraded check still
/// blocks.
#[test]
fn list_json_reflects_severity_override() {
    let r = Repo::new();
    r.stage("Cargo.toml", "[package]\nname=\"t\"\n");
    r.stage("src/main.rs", "fn main() {}\n");
    r.commit("feat: a rust repo");
    r.git(&["config", "amont.severity.pre-commit-clippy", "warn"]);

    let v = list_json(&r, &[]);
    let clippy = find_check(&v, "pre-commit-clippy");
    assert_eq!(clippy["declared_severity"], "block");
    assert_eq!(clippy["effective_severity"], "warn");
    assert_eq!(clippy["severity_overridden"], true);
}

#[test]
fn list_json_stage_filter() {
    let r = Repo::new();
    r.stage("Cargo.toml", "[package]\nname=\"t\"\n");
    r.commit("feat: init");

    let v = list_json(&r, &["--stage", "pre-push"]);
    assert_eq!(v["stage_filter"], "pre-push");
    for c in v["checks"].as_array().unwrap() {
        assert_eq!(c["stage"], "pre-push", "{c}");
    }
}

#[test]
fn list_json_rejects_an_unknown_stage() {
    let r = Repo::new();
    r.stage("a.txt", "x\n");
    r.commit("init");
    let out = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("list")
        .arg("--stage")
        .arg("nonsense")
        .current_dir(&r.dir)
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--stage"));
}

#[test]
fn list_json_declared_check_shows_its_command() {
    let r = Repo::new();
    r.stage(
        "amont.conf",
        "pre-commit  lint-shell  *.sh  block  scripts/lint.sh\n",
    );
    r.commit("chore: declare a check");
    let trusted = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("trust")
        .current_dir(&r.dir)
        .output()
        .expect("run");
    assert!(trusted.status.success(), "{trusted:?}");

    let v = list_json(&r, &[]);
    let declared = find_check(&v, "pre-commit-lint-shell");
    assert_eq!(declared["source"], "declared");
    assert_eq!(declared["command"], "scripts/lint.sh");
}

#[test]
fn list_pushed_no_upstream_fails_clearly() {
    let r = Repo::new();
    r.stage("a.txt", "x\n");
    r.commit("init");

    let out = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("list")
        .arg("--json")
        .arg("--pushed")
        .current_dir(&r.dir)
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(2));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON on stdout");
    assert!(v["error"].as_str().unwrap().contains("upstream"), "{v}");

    let out_text = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("list")
        .arg("--pushed")
        .current_dir(&r.dir)
        .output()
        .expect("run");
    assert_eq!(out_text.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out_text.stderr).contains("upstream"));
}

/// `--pushed` must reflect the pushed COMMIT RANGE, not the whole tracked
/// tree — `--pushed` scopes to `@{u}..HEAD` (what the NEXT push carries), so
/// a file already pushed in an EARLIER push and untouched since is in the
/// tracked tree (unscoped `list` sees it) but not in this diff.
#[test]
fn list_pushed_reports_only_the_push_range() {
    let r = Repo::new();
    r.stage("src/main.rs", "fn main() {}\n");
    r.stage("Cargo.toml", "[package]\nname=\"t\"\n");
    r.commit("feat: add rust");
    let origin = r.path(".git/test-origin.git");
    r.git(&["init", "-q", "--bare", origin.to_str().unwrap()]);
    r.git(&["remote", "add", "origin", origin.to_str().unwrap()]);
    // The rust files are fully pushed here.
    r.git(&["push", "-q", "--no-verify", "-u", "origin", "main"]);

    // A later, unrelated commit — never touches a .rs file.
    r.stage("notes.md", "hello\n");
    r.commit("docs: add notes, not yet pushed");

    let without = list_json(&r, &[]);
    let with_pushed = list_json(&r, &["--pushed"]);

    assert_eq!(
        find_check(&without, "pre-commit-clippy")["status"],
        "runs",
        "src/main.rs is still tracked, so unscoped list must still see it: {without}"
    );
    assert_eq!(
        find_check(&with_pushed, "pre-commit-clippy")["status"],
        "inert",
        "the pending push only carries notes.md — src/main.rs was pushed \
         earlier and this diff must not resurrect it: {with_pushed}"
    );
}
