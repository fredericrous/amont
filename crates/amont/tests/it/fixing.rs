//! Checks that repair what they find.
//!
//! Safe only because the pre-commit stage holds unstaged changes aside: the
//! tree contains the staged content and nothing else, so anything a formatter
//! touched is by definition part of this commit. Without that, re-staging would
//! sweep in work the author deliberately kept back — which is why this is the
//! last piece of the spec rather than the first.
//!
//! Fixing off by default, a repair re-staged, a clean file not reported and
//! unstaged work never swept in are steps of `flows/commit.rs`, which reuses
//! the probe below. What stays here is a failing re-stage, concurrent
//! fixers and prettier.

use crate::common;
use common::{missing, Repo};

/// A rewriting probe, in whatever form this platform can execute.
///
/// The same shape `tests/it/external.rs` uses, because it is the one fixture
/// proven to run on all three platforms — a `#!/bin/sh` file has no
/// interpreter on Windows, which is exactly how the first version of this test
/// failed there.
#[cfg(unix)]
pub(crate) fn rewriter(r: &Repo, name: &str) {
    r.write(
        name,
        "#!/bin/sh\nfor f in *.js; do [ -f \"$f\" ] || continue; sed -i.bak 's/BAD/GOOD/' \"$f\" && rm -f \"$f.bak\"; done\n",
    );
    let p = r.path(name);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    r.git(&["add", name]);
}

#[cfg(not(unix))]
pub(crate) fn rewriter(r: &Repo, name: &str) {
    r.write(
        name,
        "@echo off\r\nfor %%f in (*.js) do powershell -NoProfile -Command \"$c = Get-Content -Raw '%%f'; [IO.File]::WriteAllText('%%f', ($c -replace 'BAD','GOOD'))\"\r\n",
    );
    r.git(&["add", name]);
}

#[cfg(unix)]
pub(crate) const REWRITER: &str = "rewrite.sh";
#[cfg(not(unix))]
pub(crate) const REWRITER: &str = "rewrite.cmd";

/// The same probe, but rewriting exactly the ONE file it is handed — so
/// several of them can run at once without racing each other's `sed`.
#[cfg(unix)]
fn one_file_rewriter(r: &Repo, name: &str) {
    r.write(
        name,
        "#!/bin/sh\nf=\"$1\"\n[ -f \"$f\" ] || exit 0\nsed -i.bak 's/BAD/GOOD/' \"$f\" && rm -f \"$f.bak\"\n",
    );
    let p = r.path(name);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    r.git(&["add", name]);
}

#[cfg(not(unix))]
fn one_file_rewriter(r: &Repo, name: &str) {
    r.write(
        name,
        "@echo off\r\npowershell -NoProfile -Command \"$c = Get-Content -Raw '%1'; [IO.File]::WriteAllText('%1', ($c -replace 'BAD','GOOD'))\"\r\n",
    );
    r.git(&["add", name]);
}

#[cfg(unix)]
const ONE_FILE_REWRITER: &str = "rewrite-one.sh";
#[cfg(not(unix))]
const ONE_FILE_REWRITER: &str = "rewrite-one.cmd";

/// Declare it as an external so the test exercises the same machinery a user
/// would reach for, on every platform.
fn manifest_with_fixer(r: &Repo) {
    rewriter(r, REWRITER);
    r.stage(
        "amont.conf",
        &format!("pre-commit  fmt  *.js  block  fix ./{REWRITER}\n"),
    );
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("trust")
        .current_dir(&r.dir)
        .output()
        .expect("amont trust");
    assert!(out.status.success(), "could not trust the manifest");
}

/// A `git add` that FAILS must never be read as "nothing moved".
///
/// The old `restage` returned a bare `bool`, so a failed `git add` was
/// indistinguishable from a clean file. `prettier.rs` read
/// `if wrote && restage(&files)` and, on `false`, fell through to a second
/// `--check` against the NOW-REWRITTEN working tree — which passed. The hook
/// printed a pass while the INDEX still held the old content, so the commit
/// carried unformatted code and nobody was told.
///
/// A pre-existing `$GIT_DIR/index.lock` is how git itself is made to refuse.
/// It also survives the retry loop, which only exists for a lock another
/// process is about to release.
#[test]
fn a_failed_restage_blocks_and_names_the_files() {
    let r = Repo::new();
    manifest_with_fixer(&r);
    r.git(&["config", "amont.fix", "true"]);
    r.stage("a.js", "const a = BAD\n");

    // git refuses to write the index while this exists.
    std::fs::write(r.path(".git/index.lock"), "").expect("plant index.lock");
    let run = r.hook("pre-commit", &[]);
    let _ = std::fs::remove_file(r.path(".git/index.lock"));

    assert!(
        !run.passed(),
        "the index holds unfixed content and this called it a pass:\n{}",
        run.output()
    );
    assert!(
        run.says("a.js"),
        "the message must name what is stuck:\n{}",
        run.output()
    );
    // …and the file WAS rewritten on disk, which is what makes the stale
    // index dangerous rather than merely untidy.
    assert_eq!(
        std::fs::read_to_string(r.path("a.js")).expect("read"),
        "const a = GOOD\n"
    );
}

/// Several fixers repair at once, and every repair lands.
///
/// pre-commit runs its checks concurrently, and git takes `index.lock`
/// exclusively — so two simultaneous `git add`s in one repository make one of
/// them fail. Before `Restaged`, that failure was silently read as "nothing
/// moved" and the losing check's repair simply never reached the index.
#[test]
fn concurrent_restages_do_not_collide() {
    let r = Repo::new();
    // ONE FILE EACH, deliberately: three concurrent `sed -i` passes over the
    // same file would be a race in the FIXTURE, and this case is about the
    // race in `git add`.
    one_file_rewriter(&r, ONE_FILE_REWRITER);
    r.stage(
        "amont.conf",
        &format!(
            "pre-commit  fmt-a  *.js  block  fix ./{ONE_FILE_REWRITER} a.js\n\
             pre-commit  fmt-b  *.js  block  fix ./{ONE_FILE_REWRITER} b.js\n\
             pre-commit  fmt-c  *.js  block  fix ./{ONE_FILE_REWRITER} c.js\n"
        ),
    );
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("trust")
        .current_dir(&r.dir)
        .output()
        .expect("amont trust");
    assert!(out.status.success(), "could not trust the manifest");
    r.git(&["config", "amont.fix", "true"]);

    for name in ["a.js", "b.js", "c.js"] {
        r.stage(name, "const a = BAD\n");
    }

    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.output());
    for name in ["a.js", "b.js", "c.js"] {
        let staged = r.git(&["show", &format!(":{name}")]);
        assert_eq!(
            String::from_utf8_lossy(&staged.stdout),
            "const a = GOOD\n",
            "{name} was rewritten but never re-staged:\n{}",
            run.output()
        );
    }
}

// ---- prettier, the built-in fixer ---------------------------------------

fn prettier_repo() -> Repo {
    let r = Repo::new();
    r.write(".prettierrc", "{}\n");
    r.git(&["add", ".prettierrc"]);
    r
}

/// With fixing on, prettier repairs and re-stages rather than telling the
/// author to run the command it could have run itself.
#[test]
fn prettier_repairs_and_restages() {
    if missing("prettier") {
        return;
    }
    let r = prettier_repo();
    r.git(&["config", "amont.fix", "true"]);
    r.stage("a.ts", "const  x   =1\n");

    let run = r.hook("pre-commit-prettier", &[]);
    assert!(run.passed(), "{}", run.output());
    assert!(run.says("reformatted and re-staged"), "{}", run.output());
    let staged = r.git(&["show", ":a.ts"]);
    assert_eq!(String::from_utf8_lossy(&staged.stdout), "const x = 1;\n");
}

/// With fixing OFF, the file is left byte for byte as the author wrote it.
#[test]
fn prettier_does_not_touch_files_when_fixing_is_off() {
    if missing("prettier") {
        return;
    }
    let r = prettier_repo();
    r.stage("a.ts", "const  x   =1\n");

    let run = r.hook("pre-commit-prettier", &[]);
    assert!(!run.passed(), "{}", run.output());
    assert_eq!(
        std::fs::read_to_string(r.path("a.ts")).expect("read"),
        "const  x   =1\n",
        "it rewrote a file nobody asked it to rewrite"
    );
}

/// The prettier half of the conflation: the write succeeds, `git add` does
/// not, and the run must FAIL rather than re-checking the tree it just fixed
/// and calling that a pass.
#[test]
fn prettier_blocks_when_it_cannot_restage_what_it_wrote() {
    if missing("prettier") {
        return;
    }
    let r = prettier_repo();
    r.git(&["config", "amont.fix", "true"]);
    r.stage("a.ts", "const  x   =1\n");

    std::fs::write(r.path(".git/index.lock"), "").expect("plant index.lock");
    let run = r.hook("pre-commit-prettier", &[]);
    let _ = std::fs::remove_file(r.path(".git/index.lock"));

    assert!(
        !run.passed(),
        "the index still holds unformatted content and this passed:\n{}",
        run.output()
    );
    assert!(run.says("a.ts"), "{}", run.output());
    assert_eq!(
        std::fs::read_to_string(r.path("a.ts")).expect("read"),
        "const x = 1;\n",
        "prettier did write the file — that is what makes the stale index a bug"
    );
}
