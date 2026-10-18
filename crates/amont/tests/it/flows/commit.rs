//! The commit journey: what a person meets between `git add` and a commit
//! that exists.
//!
//! One repository, installed the way a person installs it (`amont trust`,
//! `amont init`), with two declarations: a blocking `suite` over `*.txt`
//! that logs each run, and a `fmt` fixer over `*.js`. Every commit below is
//! a real `git commit` through the installed hooks, so pre-commit,
//! commit-msg and post-commit all take part, as they do for the person.
//!
//! The journey, in order:
//! 1. the checks judge what is staged, and unstaged work survives the hook;
//! 2. a blocked commit leaves the refs and the working tree as they were,
//!    and says every reason, not only the first;
//! 3. fixers repair only when asked, re-stage what they touched and nothing
//!    else;
//! 4. `--no-verify` gets the commit through, unstamped and counted;
//! 5. gate stamps: a gate that passed on a tree is not run again for it,
//!    and only for it.

use crate::common::{missing, Repo};
use crate::fixing::{rewriter, REWRITER};
use std::process::{Command, Stdio};

const SEED: &str = "{\n  \"a\": 1\n}\n";
const VALID: &str = "{\n  \"a\": 2\n}\n";
const BROKEN: &str = "{ THIS IS NOT JSON\n";
const REUSED: &str = "passed on this exact tree earlier — not repeating it here";

/// `amont <args>` from the repository, as a person types it: no hook mode,
/// and their own git config (trust lives there).
fn amont(r: &Repo, args: &[&str]) -> (bool, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_amont"));
    cmd.args(args).current_dir(&r.dir).stdin(Stdio::null());
    Repo::strip_git_env_impl(&mut cmd);
    let out = cmd.output().expect("run amont");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// A hooked `git commit`, with everything the hooks said.
fn git_commit(r: &Repo, args: &[&str]) -> (bool, String) {
    let mut all = vec!["commit", "-q"];
    all.extend_from_slice(args);
    let out = r.git(&all);
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn read(r: &Repo, path: &str) -> String {
    std::fs::read_to_string(r.path(path)).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

fn git_out(r: &Repo, args: &[&str]) -> String {
    String::from_utf8_lossy(&r.git(args).stdout)
        .trim_end()
        .to_string()
}

fn head(r: &Repo) -> String {
    git_out(r, &["rev-parse", "HEAD"])
}

/// The paths the last commit changed.
fn head_files(r: &Repo) -> String {
    git_out(
        r,
        &["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"],
    )
}

/// The hold's store: present only while a hook has unstaged work parked.
fn held(r: &Repo) -> bool {
    r.path(".git/amont-held").exists()
}

/// How many times `suite` has run: its log grows a byte per run.
fn runs(r: &Repo) -> usize {
    std::fs::read_to_string(r.path("gate.log"))
        .map(|s| s.len())
        .unwrap_or(0)
}

/// `amont list --json`'s bypass count for one declaration.
fn bypasses(r: &Repo, script: &str) -> i64 {
    let (_, out) = amont(r, &["list", "--json"]);
    let v: serde_json::Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("list --json: {e}\n{out}"));
    v["bypasses"]["by_script"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| s["script"] == script)
        .filter_map(|s| s["count"].as_i64())
        .sum()
}

/// A conflicted file, built rather than written so this file carries no
/// marker of its own.
fn conflicted() -> String {
    format!(
        "{}\nours\n{}\ntheirs\n{}\n",
        "<".repeat(7),
        "=".repeat(7),
        ">".repeat(7)
    )
}

/// One repository walks the whole journey.
///
/// Slower than a second: it is a flow, about thirty hooked commits on one
/// repository, standing in for the twelve repositories its steps used to
/// build one by one.
#[test]
fn the_commit_journey() {
    let r = Repo::new();
    r.stage("x.json", SEED);
    r.stage("é.json", SEED);
    r.stage("other.txt", "committed\n");
    // `.cjs`: out of the fixer's `*.js` reach, which rewrites every match.
    r.stage(
        "gate.cjs",
        "const fs = require('fs');\n\
         fs.appendFileSync('gate.log', 'x');\n\
         process.exit(fs.existsSync('fail') ? 1 : 0);\n",
    );
    rewriter(&r, REWRITER);
    r.stage(
        "amont.conf",
        &format!(
            "pre-commit  suite  *.txt  block  node gate.cjs\n\
             pre-commit  fmt    *.js   block  fix ./{REWRITER}\n"
        ),
    );
    r.commit("chore: seed");
    for verb in ["trust", "init"] {
        let (ok, out) = amont(&r, &[verb]);
        step!("install", ok, "amont {verb}:\n{out}");
    }

    // ---- 1. staged-only checks; unstaged and untracked work survive ------

    // Valid JSON staged, broken work kept back on disk, on an ASCII and a
    // non-ASCII path (git quotes the latter unless told not to), and a file
    // git does not track at all.
    r.stage("x.json", VALID);
    r.write("x.json", BROKEN);
    r.stage("é.json", VALID);
    r.write("é.json", BROKEN);
    r.write("scratch.txt", "not staged, not tracked\n");
    let (ok, out) = git_commit(&r, &["-m", "feat: a valid change"]);
    step!(
        "staged-only",
        ok,
        "judged the working tree, not the commit:\n{out}"
    );
    step_eq!(
        "staged-only",
        git_out(&r, &["show", "HEAD:x.json"]),
        VALID.trim_end()
    );
    step_eq!(
        "staged-only",
        read(&r, "x.json"),
        BROKEN,
        "unstaged work was not restored"
    );
    step_eq!(
        "staged-only",
        read(&r, "é.json"),
        BROKEN,
        "unstaged work on a non-ASCII path was not restored"
    );
    step_eq!(
        "staged-only",
        read(&r, "scratch.txt"),
        "not staged, not tracked\n",
        "an untracked file was moved"
    );

    // The false negative: the tree is fine and the staged content is not.
    r.stage("é.json", VALID); // that file's work, finished: clean again
    let before = head(&r);
    r.stage("x.json", BROKEN);
    r.write("x.json", "{\n  \"a\": 9\n}\n");
    let (ok, out) = git_commit(&r, &["-m", "feat: a broken change"]);
    step!(
        "blocked",
        !ok,
        "a broken staged change passed because the tree was fine:\n{out}"
    );
    step!(
        "blocked",
        out.contains("pre-commit-lint-json-yaml"),
        "{out}"
    );
    step!("blocked", out.contains("x.json"), "{out}");
    step_eq!("blocked", head(&r), before, "a blocked commit moved HEAD");
    step_eq!(
        "blocked",
        read(&r, "x.json"),
        "{\n  \"a\": 9\n}\n",
        "a blocked commit lost the unstaged change"
    );
    step!("blocked", !held(&r), "held files left behind after a block");
    std::fs::remove_file(r.path("scratch.txt")).expect("remove scratch");

    // ---- 2. every failure is reported, then the fixed commit goes in -----

    r.stage("x.json", "{\n  \"a\": 9\n}\n");
    r.stage("bad.txt", &conflicted());
    r.stage("bad.ts", "fit('x', () => {});\n");
    let (ok, out) = git_commit(&r, &["-m", "feat: two mistakes"]);
    step!("every failure", !ok, "{out}");
    step!(
        "every failure",
        out.contains("pre-commit-merge-conflict"),
        "missing merge-conflict:\n{out}"
    );
    step!(
        "every failure",
        out.contains("pre-commit-ban-terms"),
        "missing ban-terms:\n{out}"
    );

    // Nothing unstaged now: the hold has nothing to park and parks nothing.
    r.stage("bad.txt", "resolved\n");
    r.stage("bad.ts", "it('x', () => {});\n");
    let (ok, out) = git_commit(&r, &["-m", "fix: both mistakes"]);
    step!("clean", ok, "{out}");
    step!(
        "clean",
        !out.contains("skipped by"),
        "no skip, no notice:\n{out}"
    );
    step_eq!("clean", read(&r, "x.json"), "{\n  \"a\": 9\n}\n");
    step!("clean", !held(&r), "held something with nothing to hold");

    // ---- 3. fixers ---------------------------------------------------------

    r.git(&["config", "amont.fix", "true"]);
    r.stage("a.js", "const a = BAD\n");
    let (ok, out) = git_commit(&r, &["-m", "feat: a"]);
    step!(
        "fix",
        ok,
        "the repair should let the commit through:\n{out}"
    );
    step_eq!("fix", read(&r, "a.js"), "const a = GOOD\n");
    // Staged, not merely written: the commit carries the repair.
    step_eq!("fix", git_out(&r, &["show", ":a.js"]), "const a = GOOD");
    step_eq!("fix", git_out(&r, &["show", "HEAD:a.js"]), "const a = GOOD");
    step!("fix", out.contains("fixed and re-staged"), "{out}");

    // Nothing to repair must not report a repair.
    r.stage("c.js", "const a = GOOD\n");
    let (ok, out) = git_commit(&r, &["-m", "feat: c"]);
    step!("fix, clean", ok, "{out}");
    step!("fix, clean", !out.contains("fixed and re-staged"), "{out}");

    // A repair stages what it touched, never the work kept back.
    r.stage("b.js", "const a = BAD\n");
    r.write("other.txt", "work in progress, not staged\n");
    let (ok, out) = git_commit(&r, &["-m", "feat: b"]);
    step!("fix, kept back", ok, "{out}");
    let files = head_files(&r);
    step!(
        "fix, kept back",
        files.lines().any(|f| f == "b.js"),
        "the repair was not committed: {files}"
    );
    step!(
        "fix, kept back",
        !files.contains("other.txt"),
        "the repair swept in unstaged work: {files}"
    );
    step_eq!(
        "fix, kept back",
        read(&r, "other.txt"),
        "work in progress, not staged\n",
        "and the unstaged work must still be there"
    );

    // Off (the default): the fixer is not even invoked, and the check that
    // did not run says so instead of rolling up as clean, without blocking.
    r.git(&["config", "--unset", "amont.fix"]);
    r.stage("d.js", "const a = BAD\n");
    let (ok, out) = git_commit(&r, &["-m", "feat: d"]);
    step_eq!(
        "fix off",
        read(&r, "d.js"),
        "const a = BAD\n",
        "it edited a file without being asked"
    );
    step!("fix off", !out.contains("fixed and re-staged"), "{out}");
    step!(
        "fix off",
        out.contains("could not run"),
        "a check that never ran was rolled up as clean:\n{out}"
    );
    step!(
        "fix off",
        out.contains("amont.fix"),
        "the gap must name its reason:\n{out}"
    );
    step!("fix off", ok, "{out}");

    // ---- 4. --no-verify ------------------------------------------------------

    // The work kept back goes in unchecked: `suite` covers it and never ran.
    let dodged = bypasses(&r, "suite");
    r.git(&["add", "other.txt"]);
    let (ok, out) = git_commit(&r, &["--no-verify", "-m", "chore: wip"]);
    step!("no-verify", ok, "{out}");
    step_eq!("no-verify", head_files(&r), "other.txt");
    step!(
        "no-verify",
        !r.git(&["notes", "--ref", "amont-gate", "show", "HEAD"])
            .status
            .success(),
        "a commit that dodged its gate carries a stamp"
    );
    step_eq!(
        "no-verify",
        bypasses(&r, "suite"),
        dodged + 1,
        "the dodge is counted, under the gate's name"
    );

    // ---- 5. gate stamps --------------------------------------------------------

    if missing("node") {
        return;
    }

    // `suite` passes, then commit-msg refuses the subject.
    let n = runs(&r);
    let before = head(&r);
    r.stage("a.txt", "hello\n");
    let (ok, out) = git_commit(&r, &["-m", "just a message with no type"]);
    step!(
        "refused message",
        !ok,
        "commit-msg was meant to refuse:\n{out}"
    );
    step_eq!("refused message", runs(&r), n + 1, "the gate ran once");
    step!(
        "refused message",
        !out.contains(REUSED),
        "nothing was vouched for yet:\n{out}"
    );
    step_eq!("refused message", head(&r), before);

    // The retry on the same tree repeats nothing, and is stamped as if it had.
    let (ok, out) = git_commit(&r, &["-m", "feat: through the gate"]);
    step!("retry", ok, "{out}");
    step!(
        "retry",
        out.contains(REUSED),
        "the reuse is said out loud:\n{out}"
    );
    step_eq!("retry", runs(&r), n + 1, "the retry repeated nothing");
    let note = r.git(&["notes", "--ref", "amont-gate", "show", &head(&r)]);
    step!(
        "retry",
        note.status.success(),
        "the retried commit has no gate stamp"
    );
    step!(
        "retry",
        String::from_utf8_lossy(&note.stdout).contains("suite"),
        "the stamp names the gate"
    );

    // A stamp is a statement about ONE tree: a refusal on one content
    // vouches nothing for the next.
    let n = runs(&r);
    r.stage("a.txt", "hello again\n");
    let (ok, _) = git_commit(&r, &["-m", "just a message with no type"]);
    step!("changed tree", !ok);
    r.stage("a.txt", "hello once more\n");
    let (ok, out) = git_commit(&r, &["-m", "feat: more"]);
    step!("changed tree", ok, "{out}");
    step!(
        "changed tree",
        !out.contains(REUSED),
        "new content is not vouched for:\n{out}"
    );
    step_eq!("changed tree", runs(&r), n + 2);

    // Undone with `reset --soft` and made again: the tree note vouches.
    let n = runs(&r);
    step!(
        "re-commit",
        r.git(&["reset", "-q", "--soft", "HEAD~1"]).status.success()
    );
    let (ok, out) = git_commit(&r, &["-m", "feat: same content, new commit"]);
    step!("re-commit", ok, "{out}");
    step!(
        "re-commit",
        out.contains(REUSED),
        "the tree note vouches for the re-commit:\n{out}"
    );
    step_eq!("re-commit", runs(&r), n);

    // `amont.commitStamps false`: every attempt runs.
    let n = runs(&r);
    r.git(&["config", "amont.commitStamps", "false"]);
    r.stage("a.txt", "switched off\n");
    let (ok, _) = git_commit(&r, &["-m", "just a message with no type"]);
    step!("stamps off", !ok);
    let (ok, out) = git_commit(&r, &["-m", "feat: through the gate"]);
    step!("stamps off", ok, "{out}");
    step!("stamps off", !out.contains(REUSED), "reuse is off:\n{out}");
    step_eq!("stamps off", runs(&r), n + 2);
    r.git(&["config", "--unset", "amont.commitStamps"]);

    // A gate that FAILED vouches for nothing: the retry on the same tree
    // (`fail` is untracked, not staged) runs it again.
    let n = runs(&r);
    r.write("fail", "");
    r.stage("a.txt", "blocked\n");
    let (ok, out) = git_commit(&r, &["-m", "feat: blocked"]);
    step!("failed gate", !ok, "the gate was meant to block:\n{out}");
    step!(
        "failed gate",
        out.contains("suite"),
        "the block names the gate:\n{out}"
    );
    step_eq!("failed gate", runs(&r), n + 1);
    std::fs::remove_file(r.path("fail")).expect("remove fail");
    let (ok, out) = git_commit(&r, &["-m", "feat: through"]);
    step!("failed gate", ok, "{out}");
    step!(
        "failed gate",
        !out.contains(REUSED),
        "a rejection is not a stamp:\n{out}"
    );
    step_eq!("failed gate", runs(&r), n + 2);
}
