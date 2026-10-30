//! The declared journey: what a repository's own `amont.conf` does, from the
//! day it has none to the day it declares tree gates.
//!
//! One repository with the hooks installed (`amont init`), every verdict a
//! real `git commit` through them. Two committed probes, one that passes and
//! one that fails, each printing `probe-<name> ran`, so a step can tell a
//! check that judged from one that never started. Each step writes the
//! manifest its case needs, stages what that case staged, and asserts what
//! the commit says and whether it went in.
//!
//! Trust is keyed on content, and "never trusted here" differs from "changed
//! since": every step about a repository nobody has trusted comes before the
//! first `amont trust`.
//!
//! The journey, in order:
//! 1. no manifest: nothing mentions one, and `amont.conventions declared`
//!    holds the house rules back (the safety net stays) until a manifest,
//!    even a comment, declares the repository;
//! 2. a manifest nobody trusted: committing it is blocked (or downgraded on
//!    purpose), and a cloned one is inert and says so, its policy, pins and
//!    `set` lines with it; trusting it lets it run;
//! 3. an edit un-trusts it: a commit changing it is blocked until trusted,
//!    and a pulled edit is a gap, never a block;
//! 4. declared checks: block, pass, warn, scope, `hook.skip` and severity,
//!    a command that cannot start, a malformed line, the `files` marker and
//!    `$AMONT_FILES`, tool pins;
//! 5. policy lines: `severity` and `skip` against git config, `amont run`,
//!    `amont list` and `trust --show`, `set` thresholds, commit style,
//!    commands, refused keys and the version floor;
//! 6. scope: directory triggers, `docs-skip`, and the `+` opt-in half;
//! 7. tree gates (unix): off, cold and warmed in the background, proven,
//!    withheld, a lockfile moving the cache, a pinned tool at another
//!    version.

use super::{amont, git_commit, head};
use crate::common::Repo;
use crate::external::probe;
use std::process::Stdio;

#[cfg(unix)]
const PASS: &str = "pass.sh";
#[cfg(not(unix))]
const PASS: &str = "pass.cmd";
#[cfg(unix)]
const FAIL: &str = "fail.sh";
#[cfg(not(unix))]
const FAIL: &str = "fail.cmd";

/// A staged file `lint-json-yaml` must object to.
const BAD_JSON: &str = "{ not json\n";
/// A subject `commit-msg` refuses: no type.
const UNCONVENTIONAL: &str = "not a conventional subject at all";

/// Write the manifest and trust it, as its author does.
fn declare(r: &Repo, body: &str) {
    r.stage("amont.conf", body);
    let (ok, out) = amont(r, &["trust"]);
    step!("declare", ok, "amont trust:\n{out}");
}

fn commit(r: &Repo, msg: &str) -> (bool, String) {
    git_commit(r, &["-m", msg])
}

/// The person drops the last commit and what it carried.
fn undo(r: &Repo) {
    let out = r.git(&["reset", "-q", "--hard", "HEAD~1"]);
    assert!(out.status.success(), "reset to HEAD~1");
}

/// The person drops what is staged.
fn unstage(r: &Repo) {
    let out = r.git(&["reset", "-q", "--hard", "HEAD"]);
    assert!(out.status.success(), "reset to HEAD");
}

fn config(r: &Repo, key: &str, value: &str) {
    r.git(&["config", key, value]);
}

fn unset(r: &Repo, key: &str) {
    r.git(&["config", "--unset-all", key]);
}

/// `amont <args>`'s stdout alone: what `list` and `list --json` print.
fn stdout(r: &Repo, args: &[&str]) -> String {
    let mut cmd = crate::common::amont_cmd();
    cmd.args(args).current_dir(&r.dir).stdin(Stdio::null());
    Repo::strip_git_env_impl(&mut cmd);
    String::from_utf8_lossy(&cmd.output().expect("run amont").stdout).into_owned()
}

/// A 2 MB file, staged.
fn stage_blob(r: &Repo) {
    std::fs::write(r.path("blob.bin"), vec![b'x'; 2 * 1024 * 1024]).expect("write blob");
    r.git(&["add", "blob.bin"]);
}

/// One repository walks the whole journey.
///
/// Slower than a second: it is a flow, about seventy hooked commits on one
/// repository, standing in for the sixty-three repositories its steps used
/// to build one by one.
#[test]
fn the_declared_journey() {
    let r = Repo::new();
    probe(&r, PASS, 0);
    probe(&r, FAIL, 1);
    r.stage("seed.txt", "seed\n");
    r.commit("chore: seed");
    let (ok, out) = amont(&r, &["init"]);
    step!("install", ok, "amont init:\n{out}");

    // ---- 1. no manifest, and the conventions' reach ----------------------

    r.stage("a.txt", "hello\n");
    let (ok, out) = commit(&r, "feat: a");
    step!("no manifest", ok, "{out}");
    step!(
        "no manifest",
        !out.contains("amont.conf"),
        "nothing should mention a file that does not exist:\n{out}"
    );

    // The default is `everywhere`, and a typo in the mode is inert.
    r.stage("bad.json", BAD_JSON);
    let (ok, out) = commit(&r, "feat: bad json");
    step!("reach, default", !ok, "lint-json-yaml still blocks:\n{out}");
    config(&r, "amont.conventions", "declred");
    let (ok, out) = commit(&r, "feat: bad json");
    step!(
        "reach, misspelt",
        !ok,
        "a typo must not soften anything:\n{out}"
    );

    // Declared mode, no manifest: the house rules stand down, said once.
    config(&r, "amont.conventions", "declared");
    let (ok, out) = commit(&r, "feat: bad json");
    step!(
        "reach, undeclared",
        ok,
        "a house rule must not fire here:\n{out}"
    );
    step!(
        "reach, undeclared",
        out.contains("convention check(s) held back"),
        "held back, not silent:\n{out}"
    );
    step!(
        "reach, undeclared",
        out.contains("safety net still runs"),
        "the reader is told what still protects them:\n{out}"
    );
    undo(&r);

    // The safety net does not stand down.
    r.stage("f.ts", "debugger;\n");
    let (ok, out) = commit(&r, "feat: a leftover");
    step!("reach, safety net", !ok, "ban-terms is safety:\n{out}");
    unstage(&r);

    // commit-msg is pure convention.
    r.stage("note.txt", "one\n");
    let (ok, out) = commit(&r, UNCONVENTIONAL);
    step!("reach, commit-msg", ok, "conventions held back:\n{out}");

    // `amont list` says the mode out loud, in text and in JSON.
    let text = stdout(&r, &["list"]);
    step!(
        "reach, list",
        text.contains("conventions held back"),
        "{text}"
    );
    let json = stdout(&r, &["list", "--json"]);
    step!(
        "reach, list",
        json.contains("\"conventions_apply\":false"),
        "{json}"
    );

    // A manifest, even a comment, declares the repository.
    r.stage("amont.conf", "# declared\n");
    let (ok, out) = commit(&r, UNCONVENTIONAL);
    step!("reach, declared", !ok, "declared repo enforces:\n{out}");
    r.stage("amont.conf", "# this repository subscribes to amont\n");
    r.stage("bad.json", BAD_JSON);
    let (ok, out) = commit(&r, "feat: bad json");
    step!(
        "reach, declared",
        !ok,
        "declared repo gets the house rules:\n{out}"
    );
    step!(
        "reach, declared",
        !out.contains("held back"),
        "nothing is held back in a declared repo:\n{out}"
    );
    unstage(&r);
    unset(&r, "amont.conventions");

    // ---- 2. a manifest nobody trusted --------------------------------------

    // The author's first manifest: blocked, naming why, running nothing.
    let audit_pass = format!("pre-commit  audit  *  block  ./{PASS}\n");
    r.stage("amont.conf", &audit_pass);
    let (ok, out) = commit(&r, "feat: a manifest");
    step!("first manifest", !ok, "{out}");
    step!(
        "first manifest",
        out.contains("never been trusted"),
        "{out}"
    );
    step!("first manifest", !out.contains("probe-"), "{out}");

    // A block, and like every block it can be downgraded on purpose.
    config(&r, "amont.severity.pre-commit-manifest-trust", "warn");
    let (ok, out) = commit(&r, "feat: a manifest");
    step!("trust downgraded", ok, "{out}");
    step!("trust downgraded", out.contains("amont trust"), "{out}");
    step!("trust downgraded", !out.contains("probe-"), "{out}");
    unset(&r, "amont.severity.pre-commit-manifest-trust");

    // A clone: the manifest arrived in history, the commit is about
    // something else. Inert, and reported rather than dropped.
    let audit_fail = format!("pre-commit  audit  *  block  ./{FAIL}\n");
    r.stage("amont.conf", &audit_fail);
    r.commit("chore: cloned");
    r.stage("note.txt", "two\n");
    let (ok, out) = commit(&r, "feat: unrelated");
    step!(
        "untrusted",
        !out.contains("probe-"),
        "an untrusted manifest executed its command:\n{out}"
    );
    step!("untrusted", ok, "and it must not block either:\n{out}");
    step!("untrusted", out.contains("audit"), "{out}");
    step!(
        "untrusted",
        out.contains("trust"),
        "must say how to accept it:\n{out}"
    );
    step!("untrusted", out.contains("could not run"), "{out}");

    // Untrusted policy is inert, and says so.
    r.stage("amont.conf", "severity lint-json-yaml warn\n");
    r.stage("bad.json", BAD_JSON);
    let (ok, out) = commit(&r, "feat: bad json");
    step!(
        "untrusted policy",
        !ok,
        "policy must not apply untrusted:\n{out}"
    );
    step!(
        "untrusted policy",
        out.contains("policy not applied"),
        "withheld policy is announced:\n{out}"
    );
    unstage(&r);

    // An untrusted pin is not verified: verifying it executes a program.
    r.stage("amont.conf", "tool definitely-absent-tool-xyz 1.\n");
    r.stage("a.txt", "x\n");
    let (ok, out) = commit(&r, "feat: x");
    step!("untrusted pin", ok, "{out}");
    step!(
        "untrusted pin",
        !out.contains("definitely-absent-tool-xyz 1."),
        "an untrusted pin must not be verified:\n{out}"
    );

    // An untrusted `set` line does not bind commit-msg.
    r.stage("amont.conf", "set commit.subjectMax 20\n");
    let (ok, out) = commit(&r, "feat: much too long for twenty");
    step!(
        "untrusted set",
        ok,
        "untrusted policy must not bind:\n{out}"
    );

    // An untrusted manifest supplies no command for a snapshot.
    r.stage(
        "amont.conf",
        "set snapshotPrepare touch /tmp/amont-should-not-run\n",
    );
    let text = stdout(&r, &["list"]);
    step!(
        "untrusted prepare",
        !text.contains("amont-should-not-run"),
        "an untrusted manifest supplies no command: {text}"
    );
    unstage(&r);

    // Trusting the cloned manifest lets it run.
    r.stage("amont.conf", &audit_fail);
    r.commit("chore: pulled the audit back");
    let (ok, out) = amont(&r, &["trust"]);
    step!("trusted", ok, "{out}");
    r.stage("note.txt", "three\n");
    let (ok, out) = commit(&r, "feat: unrelated");
    step!(
        "trusted",
        out.contains("probe-"),
        "trusting did not enable it:\n{out}"
    );
    step!("trusted", !ok, "{out}");
    unstage(&r);

    // ---- 3. an edit un-trusts it --------------------------------------------

    declare(&r, &audit_pass);
    r.stage("note.txt", "four\n");
    let (ok, out) = commit(&r, "chore: a trusted manifest");
    step!("trusted manifest", ok, "{out}");
    step!("trusted manifest", out.contains("probe-"), "{out}");

    // The author's own edit is blocked until they trust it.
    let audit_extra = format!("{audit_pass}pre-commit  extra  *  block  ./{PASS}\n");
    r.stage("amont.conf", &audit_extra);
    let (ok, out) = commit(&r, "feat: an extra check");
    step!(
        "changed manifest",
        !ok,
        "an untrusted edit gated nothing:\n{out}"
    );
    step!(
        "changed manifest",
        out.contains("this commit changes amont.conf"),
        "must say why:\n{out}"
    );
    step!(
        "changed manifest",
        out.contains("amont trust"),
        "must name the fix:\n{out}"
    );
    step!(
        "changed manifest",
        !out.contains("probe-"),
        "blocking must run nothing:\n{out}"
    );
    let (ok, out) = amont(&r, &["trust"]);
    step!("changed manifest", ok, "{out}");
    let (ok, out) = commit(&r, "feat: an extra check");
    step!("changed manifest", ok, "{out}");
    step!(
        "changed manifest",
        out.contains("probe-"),
        "trusted, the checks gate it:\n{out}"
    );

    // A pull that adds a command does not inherit the consent.
    r.stage(
        "amont.conf",
        &format!("{audit_extra}pre-commit  more  *  block  ./{PASS}\n"),
    );
    r.commit("chore: pulled");
    r.stage("note.txt", "five\n");
    let (ok, out) = commit(&r, "feat: unrelated");
    step!(
        "pulled edit",
        !out.contains("probe-"),
        "an edited manifest kept its trust:\n{out}"
    );
    step!(
        "pulled edit",
        out.contains("changed"),
        "must say WHICH happened:\n{out}"
    );
    step!(
        "pulled edit",
        ok,
        "a pulled manifest is a gap, never a block:\n{out}"
    );

    // ---- 4. declared checks ---------------------------------------------------

    // A blocking failure stops the commit, by name; severity downgrades it
    // without disabling it, and `hook.skip` disables it out loud.
    declare(&r, &format!("pre-commit  lint-shell  *  block  ./{FAIL}\n"));
    r.stage("note.txt", "six\n");
    let (ok, out) = commit(&r, "feat: six");
    step!(
        "block",
        out.contains("probe-"),
        "the command never ran:\n{out}"
    );
    step!("block", !ok, "a blocking failure must stop the commit");
    step!("block", out.contains("lint-shell"), "by name:\n{out}");
    config(&r, "amont.severity.lint-shell", "warn");
    let (ok, out) = commit(&r, "feat: six");
    step!("severity", ok, "severity override ignored:\n{out}");
    step!(
        "severity",
        out.contains("probe-"),
        "downgraded, not disabled"
    );
    unset(&r, "amont.severity.lint-shell");
    config(&r, "hook.skip", "lint-shell");
    r.stage("note.txt", "seven\n");
    let (ok, out) = commit(&r, "feat: seven");
    step!("hook.skip", ok, "{out}");
    step!(
        "hook.skip",
        !out.contains("probe-"),
        "a skipped check must not run:\n{out}"
    );
    step!(
        "hook.skip",
        out.contains("skipped by"),
        "and the skip must be announced:\n{out}"
    );
    unset(&r, "hook.skip");

    declare(&r, &format!("pre-commit  lint-shell  *  block  ./{PASS}\n"));
    r.stage("note.txt", "eight\n");
    let (ok, out) = commit(&r, "feat: eight");
    step!("pass", out.contains("probe-"), "{out}");
    step!("pass", ok, "{out}");

    // The severity column means what it means for a built-in.
    declare(&r, &format!("pre-commit  smoke  *  warn  ./{FAIL}\n"));
    r.stage("note.txt", "nine\n");
    let (ok, out) = commit(&r, "feat: nine");
    step!("warn", out.contains("probe-"), "it must still RUN:\n{out}");
    step!("warn", ok, "warn must not block:\n{out}");
    step!("warn", out.contains("set to warn"), "{out}");

    // Scope is judged against the staged files.
    declare(
        &r,
        &format!("pre-commit  lint-shell  *.rs  block  ./{FAIL}\n"),
    );
    r.stage("note.txt", "ten\n");
    let (ok, out) = commit(&r, "feat: ten");
    step!(
        "scope",
        !out.contains("probe-"),
        "it ran on a commit it does not apply to:\n{out}"
    );
    step!("scope", ok, "{out}");
    r.stage("src/main.rs", "fn main() {}\n");
    let (_, out) = commit(&r, "feat: rust");
    step!("scope", out.contains("probe-"), "it must fire now:\n{out}");
    unstage(&r);

    // A bare scope token is an exact filename, anywhere in the tree.
    #[cfg(unix)]
    {
        declare(
            &r,
            &format!("pre-commit  lockcheck  package.json  block  ./{FAIL}\n"),
        );
        r.stage("not-package.json", "{}\n");
        let (ok, out) = commit(&r, "feat: a look-alike");
        step!(
            "filename scope",
            !out.contains("probe-") && ok,
            "a look-alike woke a filename-scoped check:\n{out}"
        );
        r.stage("apps/web/package.json", "{}\n");
        let (ok, out) = commit(&r, "feat: a package");
        step!(
            "filename scope",
            out.contains("probe-") && !ok,
            "the named file did not wake the check:\n{out}"
        );
        unstage(&r);
    }

    // A command that cannot be started judged nothing: a gap, not a failure.
    declare(
        &r,
        "pre-commit  lint-shell  *  block  ./no-such-binary-c8f2\n",
    );
    r.stage("note.txt", "eleven\n");
    let (ok, out) = commit(&r, "feat: eleven");
    step!(
        "missing command",
        ok,
        "an absent tool must not block:\n{out}"
    );
    step!(
        "missing command",
        out.contains("could not run"),
        "and must be reported as a gap:\n{out}"
    );

    // A line nobody can parse is reported, never silent, never a block.
    declare(&r, "pre-commit  lint-shell  *  LOUD  ./probe.sh\n");
    r.stage("note.txt", "twelve\n");
    let (ok, out) = commit(&r, "feat: twelve");
    step!("malformed", ok, "it must not block:\n{out}");
    step!("malformed", out.contains("lint-shell"), "{out}");
    step!(
        "malformed",
        out.contains("severity"),
        "say what was wrong:\n{out}"
    );
    step!("malformed", out.contains("line 1"), "and where:\n{out}");

    // The `files` marker hands the command its matched paths, `$AMONT_FILES`
    // carries them without it, and with nothing matched it does not run.
    #[cfg(unix)]
    {
        crate::common::fixture_exec(
            &r.path("argv.sh"),
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > argv.txt\nexit 0\n",
        );
        crate::common::fixture_exec(
            &r.path("envfiles.sh"),
            "#!/bin/sh\nprintf '%s' \"$AMONT_FILES\" > envfiles.txt\nexit 0\n",
        );
        r.git(&["add", "argv.sh", "envfiles.sh"]);
        r.commit("chore: two more probes");

        r.stage("a.sh", "#!/bin/sh\necho a\n");
        r.stage("b.rs", "fn main() {}\n");
        declare(&r, "pre-commit  lint-shell  *.sh  block  files ./argv.sh\n");
        let (ok, out) = commit(&r, "feat: shell");
        step!("files marker", ok, "{out}");
        let argv = std::fs::read_to_string(r.path("argv.txt")).expect("argv.txt");
        step!(
            "files marker",
            argv.lines().any(|l| l == "a.sh"),
            "matched path missing: {argv}"
        );
        step!(
            "files marker",
            !argv.lines().any(|l| l == "b.rs"),
            "a path outside the scope was handed over: {argv}"
        );
        undo(&r);
        std::fs::remove_file(r.path("argv.txt")).expect("remove argv.txt");

        r.stage("a.sh", "#!/bin/sh\necho a\n");
        r.stage("b.rs", "fn main() {}\n");
        declare(&r, "pre-commit  lint-shell  *.sh  block  ./envfiles.sh\n");
        let (ok, out) = commit(&r, "feat: shell");
        step!("AMONT_FILES", ok, "{out}");
        let env = std::fs::read_to_string(r.path("envfiles.txt")).expect("envfiles.txt");
        step!("AMONT_FILES", env.lines().any(|l| l == "a.sh"), "{env}");
        step!("AMONT_FILES", !env.lines().any(|l| l == "b.rs"), "{env}");
        undo(&r);
        std::fs::remove_file(r.path("envfiles.txt")).expect("remove envfiles.txt");

        declare(
            &r,
            &format!("pre-commit  smoke  *  block  files ./{FAIL}\n"),
        );
        r.commit("chore: park the manifest");
        let (ok, out) = git_commit(&r, &["--allow-empty", "-m", "chore: nothing staged"]);
        step!("files, nothing matched", ok, "{out}");
        step!(
            "files, nothing matched",
            !out.contains("probe-"),
            "ran with an empty file list:\n{out}"
        );
    }

    // Tool pins: skew is named and never blocks, a match costs nothing, a
    // missing tool is its own message, a malformed pin prescribes the shape.
    declare(&r, "tool git 999.999.\n");
    r.stage("a.txt", "pin one\n");
    let (ok, out) = commit(&r, "feat: pin one");
    step!("pin, skew", ok, "skew must never block:\n{out}");
    step!(
        "pin, skew",
        out.contains("999.999.") && out.contains("disagree with CI"),
        "the skew must be named:\n{out}"
    );
    declare(&r, "tool git version\n");
    r.stage("a.txt", "pin two\n");
    let (ok, out) = commit(&r, "feat: pin two");
    step!("pin, match", ok, "{out}");
    step!(
        "pin, match",
        !out.contains("disagree with CI")
            && !out.contains("would not run")
            && !out.contains("tool <program>"),
        "a satisfied pin must cost no output:\n{out}"
    );
    declare(&r, "tool definitely-absent-tool-xyz 1.\n");
    r.stage("a.txt", "pin three\n");
    let (ok, out) = commit(&r, "feat: pin three");
    step!("pin, missing", ok, "{out}");
    step!(
        "pin, missing",
        out.contains("would not run"),
        "a missing pinned tool must be named:\n{out}"
    );
    declare(&r, "tool git\n");
    r.stage("a.txt", "pin four\n");
    let (ok, out) = commit(&r, "feat: pin four");
    step!(
        "pin, malformed",
        ok,
        "a broken line warns, never blocks:\n{out}"
    );
    step!(
        "pin, malformed",
        out.contains("tool <program> <version-substring>"),
        "the shape must be prescribed:\n{out}"
    );

    // ---- 5. policy lines --------------------------------------------------------

    // A committed severity line downgrades a built-in; local config beats
    // it; `amont run` resolves it the same way as the dispatcher.
    declare(&r, "severity lint-json-yaml warn\n");
    r.stage("bad.json", BAD_JSON);
    config(&r, "amont.severity.lint-json-yaml", "block");
    let (ok, out) = commit(&r, "feat: bad json");
    step!("policy, local wins", !ok, "local block must win:\n{out}");
    unset(&r, "amont.severity.lint-json-yaml");
    let (ok, out) = amont(&r, &["run", "pre-commit-lint-json-yaml"]);
    step!(
        "policy, amont run",
        ok,
        "direct run must apply the same downgrade:\n{out}"
    );
    let (ok, out) = commit(&r, "feat: bad json");
    step!("policy, severity", ok, "warn must not block:\n{out}");
    step!(
        "policy, severity",
        out.contains("set to warn"),
        "the downgrade is said out loud:\n{out}"
    );
    undo(&r);

    // Specificity decides between keys: a policy full id beats a local
    // trigger key.
    declare(&r, "severity pre-commit-lint-json-yaml warn\n");
    config(&r, "amont.severity.pre-commit", "block");
    r.stage("bad.json", BAD_JSON);
    let (ok, out) = commit(&r, "feat: bad json");
    step!(
        "policy, full id",
        ok,
        "full-id policy outranks a trigger key:\n{out}"
    );
    undo(&r);
    unset(&r, "amont.severity.pre-commit");

    // A policy skip is the team's, announced under its own source, beside a
    // machine's.
    declare(&r, "skip lint-json-yaml\n");
    r.stage("bad.json", BAD_JSON);
    let (ok, out) = commit(&r, "feat: bad json");
    step!("policy, skip", ok, "skipped means skipped:\n{out}");
    step!(
        "policy, skip",
        out.contains("skipped by") && out.contains("amont.conf"),
        "the team's skip is named as such:\n{out}"
    );
    undo(&r);
    declare(&r, "skip lint-json-yaml\n");
    r.git(&["config", "--add", "hook.skip", "ban-terms"]);
    r.stage("bad.json", BAD_JSON);
    let (ok, out) = commit(&r, "feat: bad json");
    step!("policy, two skips", ok, "{out}");
    step!(
        "policy, two skips",
        out.contains("hook.skip") && out.contains("pre-commit-ban-terms"),
        "{out}"
    );
    step!(
        "policy, two skips",
        out.contains("amont.conf") && out.contains("pre-commit-lint-json-yaml"),
        "{out}"
    );
    undo(&r);
    unset(&r, "hook.skip");

    // A typo'd target is a positioned note.
    declare(&r, "severity clipy warn\n");
    r.stage("a.txt", "typo\n");
    let (ok, out) = commit(&r, "feat: typo");
    step!("policy, typo", ok, "{out}");
    step!(
        "policy, typo",
        out.contains("amont.conf:1") && out.contains("names no check here"),
        "the typo is pointed at:\n{out}"
    );

    // `amont list --json` and `trust --show` say where policy came from.
    declare(&r, "severity lint-json-yaml warn\nskip yamllint\n");
    let json = stdout(&r, &["list", "--json"]);
    step!(
        "policy, list",
        json.contains("\"severity_source\":\"policy\""),
        "{json}"
    );
    step!(
        "policy, list",
        json.contains("\"severity_overridden\":true"),
        "{json}"
    );
    step!(
        "policy, list",
        json.contains("skipped via amont.conf"),
        "{json}"
    );
    declare(&r, "severity lint-json-yaml warn\ntool jq 1.7\n");
    let (_, text) = amont(&r, &["trust", "--show"]);
    step!(
        "policy, trust --show",
        text.contains("sets policy for built-in checks"),
        "{text}"
    );
    step!(
        "policy, trust --show",
        text.contains("severity  lint-json-yaml  warn"),
        "{text}"
    );
    step!(
        "policy, trust --show",
        !text.contains("! tool pin"),
        "a pin is not broken: {text}"
    );

    // `set` ships a threshold, and local config still beats it.
    declare(&r, "set largeFileWarn 1\n");
    stage_blob(&r);
    let (ok, out) = commit(&r, "feat: a blob");
    step!("set, threshold", ok, "warn tier does not block:\n{out}");
    step!(
        "set, threshold",
        out.contains("blob.bin"),
        "the file is named:\n{out}"
    );
    undo(&r);
    declare(&r, "set largeFileWarn 1\n");
    config(&r, "amont.largeFileWarn", "1000");
    stage_blob(&r);
    let (ok, out) = commit(&r, "feat: a blob");
    step!("set, local wins", ok, "{out}");
    step!(
        "set, local wins",
        !out.contains("blob.bin"),
        "the local 1000 MB threshold wins:\n{out}"
    );
    undo(&r);
    unset(&r, "amont.largeFileWarn");

    // Commit style travels to commit-msg, a separate process.
    declare(&r, "set commit.subjectMax 20\n");
    r.stage("a.txt", "style\n");
    let before = head(&r);
    let (ok, out) = commit(&r, "feat: much too long for twenty");
    step!("set, commit style", !ok, "20-char budget enforced:\n{out}");
    step_eq!("set, commit style", head(&r), before);
    let (ok, out) = commit(&r, "feat: short");
    step!("set, commit style", ok, "{out}");

    // A value may be a whole command, and consent sees all of it.
    declare(
        &r,
        "set snapshotPrepare pnpm install --offline --frozen-lockfile\n",
    );
    let (_, text) = amont(&r, &["trust", "--show"]);
    step!(
        "set, command",
        text.contains("pnpm install --offline --frozen-lockfile"),
        "the whole command survives, not just its first word: {text}"
    );

    // A key policy may not reach is a loud gap.
    declare(&r, "set fix true\n");
    r.stage("a.txt", "fix\n");
    let (ok, out) = commit(&r, "feat: fix");
    step!("set, refused key", ok, "{out}");
    step!(
        "set, refused key",
        out.contains("not a policy-settable key"),
        "the refusal is named:\n{out}"
    );

    // The display tells the truth about where a value came from.
    declare(&r, "set commit.subjectMax 50\n");
    let text = stdout(&r, &["list"]);
    step!("set, list", text.contains("(amont.conf)"), "{text}");
    step!("set, list", text.contains("50"), "{text}");

    // The version floor: warn-only, silent when cleared, local config wins,
    // garbage is named.
    declare(&r, "set minVersion 99.0.0\n");
    r.stage("a.txt", "floor one\n");
    let (ok, out) = commit(&r, "feat: floor one");
    step!("floor", ok, "warn-only, never a gate:\n{out}");
    step!(
        "floor",
        out.contains("asks for amont") && out.contains("99.0.0"),
        "the floor is announced:\n{out}"
    );
    config(&r, "amont.minVersion", "0.1.0");
    r.stage("a.txt", "floor two\n");
    let (ok, out) = commit(&r, "feat: floor two");
    step!("floor, local wins", ok, "{out}");
    step!(
        "floor, local wins",
        !out.contains("asks for amont"),
        "local wins:\n{out}"
    );
    unset(&r, "amont.minVersion");
    declare(&r, "set minVersion 0.1.0\n");
    r.stage("a.txt", "floor three\n");
    let (ok, out) = commit(&r, "feat: floor three");
    step!("floor, cleared", ok, "{out}");
    step!("floor, cleared", !out.contains("asks for amont"), "{out}");
    declare(&r, "set minVersion banana\n");
    r.stage("a.txt", "floor four\n");
    let (ok, out) = commit(&r, "feat: floor four");
    step!("floor, garbage", ok, "{out}");
    step!(
        "floor, garbage",
        out.contains("not a version"),
        "bad values are named:\n{out}"
    );

    // ---- 6. scope ---------------------------------------------------------------

    // A directory trigger: not outside, not a look-alike, only inside.
    declare(
        &r,
        "pre-commit  plugin  claude-plugin/**/*.md  block  sh -c 'echo PLUGIN-RAN; exit 1'\n",
    );
    r.stage("README.md", "readme\n");
    let (ok, out) = commit(&r, "docs: readme");
    step!(
        "dir scope",
        !out.contains("PLUGIN-RAN"),
        "outside the dir:\n{out}"
    );
    step!("dir scope", ok, "and must not block:\n{out}");
    r.stage("claude-plugin-old/SKILL.md", "lookalike\n");
    let (_, out) = commit(&r, "docs: a look-alike");
    step!(
        "dir scope",
        !out.contains("PLUGIN-RAN"),
        "a lookalike dir:\n{out}"
    );
    r.stage("claude-plugin/skills/a/SKILL.md", "inside\n");
    let (ok, out) = commit(&r, "docs: a skill");
    step!(
        "dir scope",
        out.contains("PLUGIN-RAN"),
        "inside the dir:\n{out}"
    );
    step!("dir scope", !ok, "and the check's failure blocks");
    unstage(&r);

    // `docs-skip`: an edit to existing documentation skips, said out loud;
    // a new document, a code change and a decision record do not.
    declare(
        &r,
        "pre-commit  gate  *  block  docs-skip sh -c 'echo GATE-RAN; exit 1'\n",
    );
    r.stage("docs/guide.md", "one\n");
    r.stage("src/lib.rs", "code\n");
    r.stage("adr/0001-x.md", "status: active\n");
    r.commit("chore: base");
    r.stage("docs/guide.md", "two\n");
    let (ok, out) = commit(&r, "docs: guide");
    step!(
        "docs-skip",
        !out.contains("GATE-RAN"),
        "docs edit must skip:\n{out}"
    );
    step!(
        "docs-skip",
        out.contains("skipped"),
        "and the skip is said out loud:\n{out}"
    );
    step!("docs-skip", ok, "{out}");
    r.stage("docs/new.md", "new\n");
    let (_, out) = commit(&r, "docs: new");
    step!(
        "docs-skip",
        out.contains("GATE-RAN"),
        "an added doc must run:\n{out}"
    );
    unstage(&r);
    r.stage("src/lib.rs", "changed\n");
    let (_, out) = commit(&r, "feat: code");
    step!(
        "docs-skip",
        out.contains("GATE-RAN"),
        "a code edit must run:\n{out}"
    );
    unstage(&r);
    r.stage("adr/0001-x.md", "status: superseded\n");
    let (_, out) = commit(&r, "docs: supersede");
    step!(
        "docs-skip",
        out.contains("GATE-RAN"),
        "an ADR edit must run:\n{out}"
    );
    unstage(&r);

    // The `+` half: what the repository carries, not what the commit staged.
    // `.rubocop.yml` goes in with --no-verify, as it did before: YAML is
    // lint-json-yaml's to judge, and this step is not about it.
    declare(
        &r,
        "pre-commit  rubocop  *.rb+.rubocop.yml  block  sh -c 'echo RUBOCOP-RAN; exit 1'\n",
    );
    r.stage("app.rb", "puts 'hi'\n");
    let (ok, out) = commit(&r, "feat: ruby");
    step!(
        "opt-in",
        !out.contains("RUBOCOP-RAN"),
        "no .rubocop.yml, so it must not run:\n{out}"
    );
    step!("opt-in", ok, "and must not block:\n{out}");
    r.git(&["reset", "-q", "--soft", "HEAD~1"]);
    r.stage(".rubocop.yml", "AllCops:\n  NewCops: enable\n");
    let (ok, out) = commit(&r, "feat: ruby");
    step!(
        "opt-in",
        out.contains("RUBOCOP-RAN"),
        "with the config present it must run:\n{out}"
    );
    step!("opt-in", !ok, "and its failure must block:\n{out}");
    unstage(&r);
    declare(
        &r,
        "pre-commit  rubocop  *.rb+.rubocop.yml  block  sh -c 'echo RUBOCOP-RAN; exit 1'\n",
    );
    r.stage(".rubocop.yml", "AllCops:\n  NewCops: enable\n");
    r.commit("feat: adopt rubocop");
    r.stage("app.rb", "puts 'hi'\n");
    let (ok, out) = commit(&r, "feat: ruby");
    step!(
        "opt-in, carried",
        out.contains("RUBOCOP-RAN"),
        "the repo carries .rubocop.yml; staging it too must not be the condition:\n{out}"
    );
    step!("opt-in, carried", !ok, "and its failure must block:\n{out}");
    unstage(&r);

    // A row with nothing on the left is all opt-in.
    declare(
        &r,
        "pre-commit  bundle  +Gemfile  block  sh -c 'echo BUNDLE-RAN; exit 1'\n",
    );
    r.stage("notes.md", "no ruby anywhere\n");
    let (ok, out) = commit(&r, "docs: notes");
    step!(
        "opt-in, no trigger",
        !out.contains("BUNDLE-RAN"),
        "no Gemfile, so an all-opt-in row must stay quiet:\n{out}"
    );
    step!("opt-in, no trigger", ok, "{out}");
    r.stage("Gemfile", "source 'https://rubygems.org'\n");
    r.commit("feat: adopt bundler");
    r.stage("notes.md", "still no ruby, but the repo is a bundler one\n");
    let (ok, out) = commit(&r, "docs: notes");
    step!(
        "opt-in, no trigger",
        out.contains("BUNDLE-RAN"),
        "with a Gemfile it applies to any change:\n{out}"
    );
    step!("opt-in, no trigger", !ok, "{out}");
    unstage(&r);

    // ---- 7. tree gates ------------------------------------------------------------

    #[cfg(unix)]
    tree_gates(&r);
}

/// Tree gates at commit time (ADR-0024): beside the commit's checks, never
/// deciding it, stamping `tree:<name>` only on a tree they proved.
#[cfg(unix)]
fn tree_gates(r: &Repo) {
    use crate::tree_gates::{evidence, namespaces, stamped, trust_and_install, warm, workflow};
    use std::time::{Duration, Instant};

    // The CI step `pre-commit-tree-parity` requires, committed with the
    // gate as before, with --no-verify: the workflow is YAML.
    r.stage("amont.conf", "tree ok ruff * attest true\n");
    r.stage(".forgejo/workflows/ci.yaml", &workflow(&[("ok", "true")]));
    r.commit("chore: tree gates");
    trust_and_install(r);

    // Off: nothing runs, nothing is said, nothing is stamped.
    config(r, "amont.treeLint", "false");
    r.stage("a.txt", "lint off\n");
    let (ok, out) = commit(r, "feat: lint off");
    step!("tree lint off", ok, "{out}");
    step!("tree lint off", !out.contains("tree lint"), "{out}");
    step!(
        "tree lint off",
        !stamped(r, "HEAD").contains("tree:ok"),
        "{out}"
    );

    // Cold: the commit is not held, the cache warms in the background, and
    // the cold run is evidence on the tree.
    config(r, "amont.treeLint", "true");
    r.stage("a.txt", "hello\n");
    let (ok, out) = commit(r, "feat: a");
    step!("tree, cold", ok, "{out}");
    step!(
        "tree, cold",
        out.contains("tree lint cold: ok — warming ($GIT_DIR/amont-warm.log)"),
        "{out}"
    );
    step!(
        "tree, cold",
        !stamped(r, "HEAD").contains("tree:ok"),
        "{out}"
    );
    step_eq!(
        "tree, cold",
        evidence(r, "HEAD^{tree}", "tree-ok"),
        vec!["cold"],
        "{out}"
    );
    let until = Instant::now() + Duration::from_secs(20);
    let marker = || {
        namespaces(r, "ok").iter().any(|ns| {
            r.dir
                .join(".git/amont-cache/ok")
                .join(ns)
                .join(".complete")
                .is_file()
        })
    };
    while !marker() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(100));
    }
    step!(
        "tree, cold",
        marker(),
        "the background warm-up never completed"
    );

    // Warm: the next commit is proven, on the commit and on its tree.
    r.stage("b.txt", "b\n");
    let (ok, out) = commit(r, "feat: b");
    step!("tree, proven", ok, "{out}");
    step!(
        "tree, proven",
        stamped(r, "HEAD").contains("tree:ok"),
        "{out}"
    );
    step!(
        "tree, proven",
        stamped(r, "HEAD^{tree}").contains("tree:ok"),
        "{out}"
    );
    step_eq!(
        "tree, proven",
        evidence(r, "HEAD^{tree}", "tree-ok"),
        vec!["pass"],
        "{out}"
    );

    // An untracked file withholds the proof, and says so.
    r.write("stray.py", "x = 1\n");
    r.stage("c.txt", "c\n");
    let (ok, out) = commit(r, "feat: c");
    step!("tree, withheld", ok, "{out}");
    step!(
        "tree, withheld",
        out.contains("an untracked file is present (stray.py)"),
        "{out}"
    );
    step!(
        "tree, withheld",
        !stamped(r, "HEAD").contains("tree:ok"),
        "{out}"
    );
    step_eq!(
        "tree, withheld",
        evidence(r, "HEAD^{tree}", "tree-ok"),
        vec!["withheld"],
        "{out}"
    );
    std::fs::remove_file(r.path("stray.py")).expect("remove stray.py");

    // A lockfile change moves the namespace, starts cold, and warming drops
    // the old cache.
    warm(r);
    let before = namespaces(r, "ok");
    step_eq!("tree, lockfile", before.len(), 1, "{before:?}");
    r.stage("uv.lock", "version = 1\n");
    let (ok, out) = commit(r, "chore: upgrade");
    step!("tree, lockfile", ok, "{out}");
    step!(
        "tree, lockfile",
        out.contains("tree lint cold: ok"),
        "a new namespace starts cold: {out}"
    );
    step!(
        "tree, lockfile",
        !stamped(r, "HEAD").contains("tree:ok"),
        "{out}"
    );
    warm(r);
    let after = namespaces(r, "ok");
    step_eq!(
        "tree, lockfile",
        after.len(),
        1,
        "the old namespace was not deleted: {after:?}"
    );
    step!("tree, lockfile", after != before, "{after:?}");

    // A pin nothing installed here satisfies withholds, never blocks.
    let conf = std::fs::read_to_string(r.path("amont.conf")).expect("amont.conf");
    r.stage("amont.conf", &format!("{conf}tool ruff 99.99.99\n"));
    let out = r.git(&["commit", "-q", "--no-verify", "-m", "chore: pin"]);
    step!("tree, pinned", out.status.success());
    trust_and_install(r);
    warm(r);
    r.stage("b.txt", "b again\n");
    let (ok, out) = commit(r, "feat: b again");
    step!("tree, pinned", ok, "skew never blocks a commit: {out}");
    step!(
        "tree, pinned",
        out.contains("    ok — ruff is pinned to 99.99.99"),
        "{out}"
    );
    step!(
        "tree, pinned",
        !stamped(r, "HEAD").contains("tree:ok"),
        "{out}"
    );
}
