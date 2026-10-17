//! How the shim finds the binary — driven through a real `sh`.
//!
//! The shim is the one piece of this project that is not Rust, and it is the
//! piece that runs first, in every repository, before any check and before any
//! trust decision. So these tests execute the actual file with `sh` rather than
//! asserting anything about its text.
//!
//! The case they exist for: git invokes a hook with the WORKING TREE as the
//! current directory, so a relative path in the resolution chain is a question
//! asked of the repository being committed to. An unbaked shim — a supported,
//! documented state, since `init.templateDir` pointed at the checkout keeps the
//! placeholder on purpose — used to test that token with `[ -x ]`, which meant
//! a cloned repository shipping an executable by that name answered it.

#![cfg(unix)]

use crate::common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::Repo;

/// The unbaked shim, as it ships and as every clone gets it.
fn template() -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../templates/hooks/pre-commit")
        .canonicalize()
        .expect("template path");
    std::fs::read_to_string(p).expect("read template")
}

/// Install `text` as this repo's `pre-commit` hook and make it executable.
fn install_hook(r: &Repo, text: &str) -> PathBuf {
    let hooks = r.dir.join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).expect("hooks dir");
    let p = hooks.join("pre-commit");
    std::fs::write(&p, text).expect("write hook");
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    p
}

/// The system utilities the shim itself needs (`dirname`, `basename`), and
/// nothing of ours. Emptying PATH entirely would kill the shim before it ever
/// reached the resolution chain these tests are about.
const MINIMAL_PATH: &str = "/usr/bin:/bin";

/// Whether a stray `amont` on `MINIMAL_PATH` would make these tests vacuous.
fn amont_on_minimal_path() -> bool {
    MINIMAL_PATH
        .split(':')
        .any(|d| Path::new(d).join("amont").exists())
}

/// Run the hook the way git does: through `sh`, from the worktree root, with an
/// environment that resolves no amont binary by itself.
///
/// `HOME` points at an empty directory, so candidate 3 finds nothing, and PATH
/// holds only system utilities, so candidate 4 finds nothing either. Anything
/// that runs here ran because the shim chose it.
fn run_hook(r: &Repo, hook: &Path, empty_home: &Path) -> Output {
    run_hook_with_path(r, hook, empty_home, MINIMAL_PATH)
}

/// As above, but with the search path spelled out — because the difference
/// between "the hook is broken" and "the repository's code just ran" is one
/// empty PATH component.
fn run_hook_with_path(r: &Repo, hook: &Path, empty_home: &Path, path: &str) -> Output {
    // `/bin/sh` by absolute path, so the constrained PATH below governs only
    // what the SHIM can resolve, not whether we can spawn the interpreter.
    Command::new("/bin/sh")
        .arg(hook)
        .current_dir(&r.dir)
        .env_remove("GIT_HOOKS_BIN")
        .env("HOME", empty_home)
        .env("PATH", path)
        .output()
        .expect("run hook")
}

fn empty_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "amont-shim-{}-{}-{}",
        std::process::id(),
        tag,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

/// A repository must not be able to answer the question "where is the binary?".
///
/// The clone plants an executable named for the placeholder token at its root —
/// the position the shim would test with `[ -x ]` while standing in the
/// worktree. It must never run, and the shim must fail loudly instead: an
/// unresolved binary is a missing check, and a missing check has to be visible.
#[test]
fn an_unbaked_shim_ignores_an_executable_the_repo_planted() {
    // A amont on the minimal PATH would resolve candidate 4 and make the
    // assertion below vacuous rather than wrong.
    if amont_on_minimal_path() {
        return;
    }
    let r = Repo::new();
    let home = empty_dir("home");
    let sentinel = empty_dir("sentinel").join("it-ran");

    // The payload, named exactly as the unsubstituted token.
    let payload = r.dir.join("__AMONT_BIN__");
    std::fs::write(
        &payload,
        format!("#!/bin/sh\ntouch {}\nexit 0\n", sentinel.display()),
    )
    .expect("write payload");
    std::fs::set_permissions(&payload, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let hook = install_hook(&r, &template());
    let out = run_hook(&r, &hook, &home);

    assert!(
        !sentinel.exists(),
        "the repository's own file was executed by the hook"
    );
    assert!(
        !out.status.success(),
        "an unresolvable binary must fail loudly, not pass"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("amont binary not found"),
        "expected the loud failure, got: {err}"
    );
}

/// The same plant, with the one PATH shape that turned it into code execution.
///
/// `PATH="/usr/bin:/bin:"` has a trailing empty component — the classic
/// `PATH="$PATH:$SOMETHING"`-with-`SOMETHING`-unset accident — and an empty
/// component means the current directory. Against the old shim this ran the
/// repository's file and exited 0, so the commit proceeded and the check
/// reported as passed. Nothing about the trust model applied: no
/// `amont.conf`, no prompt, no declaration.
#[test]
fn an_unbaked_shim_ignores_the_repo_even_when_path_holds_the_cwd() {
    let r = Repo::new();
    let home = empty_dir("home");
    let sentinel = empty_dir("sentinel").join("it-ran");

    let payload = r.dir.join("__AMONT_BIN__");
    std::fs::write(
        &payload,
        format!("#!/bin/sh\ntouch {}\nexit 0\n", sentinel.display()),
    )
    .expect("write payload");
    std::fs::set_permissions(&payload, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let hook = install_hook(&r, &template());
    let out = run_hook_with_path(&r, &hook, &home, &format!("{MINIMAL_PATH}:"));

    assert!(
        !sentinel.exists(),
        "the repository's own file was executed on commit"
    );
    assert!(
        !out.status.success(),
        "and the hook must not report success either"
    );
}

/// A directory by that name is the same trick: `[ -x dir ]` is true.
#[test]
fn an_unbaked_shim_ignores_a_directory_the_repo_planted() {
    // A amont on the minimal PATH would resolve candidate 4 and make the
    // assertion below vacuous rather than wrong.
    if amont_on_minimal_path() {
        return;
    }
    let r = Repo::new();
    let home = empty_dir("home");

    std::fs::create_dir_all(r.dir.join("__AMONT_BIN__")).expect("plant dir");

    let hook = install_hook(&r, &template());
    let out = run_hook(&r, &hook, &home);

    assert!(
        !out.status.success(),
        "a directory named for the token must not be treated as the binary"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("amont binary not found"),
        "expected the loud failure"
    );
}

/// The guard must not cost the feature: a properly baked absolute path is still
/// what runs, and it still receives `--hooks-dir` and the hook name.
#[test]
fn a_baked_absolute_path_still_wins() {
    let r = Repo::new();
    let home = empty_dir("home");
    let record = empty_dir("record").join("argv");

    let fake = empty_dir("bin").join("amont");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\nexit 0\n",
            record.display()
        ),
    )
    .expect("write fake binary");
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let baked = template().replace("__AMONT_BIN__", &fake.display().to_string());
    let hook = install_hook(&r, &baked);
    let out = run_hook(&r, &hook, &home);

    assert!(
        out.status.success(),
        "the baked binary should have run: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let argv = std::fs::read_to_string(&record).expect("the baked binary did not run");
    assert!(argv.contains("--hooks-dir"), "argv was {argv:?}");
    assert!(argv.contains("pre-commit"), "argv was {argv:?}");
}

/// A relative baked path is refused by the shim itself, even though something
/// executable sits at exactly that path in the worktree. This is the property
/// that makes the fix independent of who did the baking.
#[test]
fn a_relative_baked_path_is_refused_even_when_it_resolves() {
    // A amont on the minimal PATH would resolve candidate 4 and make the
    // assertion below vacuous rather than wrong.
    if amont_on_minimal_path() {
        return;
    }
    let r = Repo::new();
    let home = empty_dir("home");
    let sentinel = empty_dir("sentinel").join("it-ran");

    std::fs::create_dir_all(r.dir.join("tools")).expect("tools dir");
    let payload = r.dir.join("tools").join("amont");
    std::fs::write(
        &payload,
        format!("#!/bin/sh\ntouch {}\nexit 0\n", sentinel.display()),
    )
    .expect("write payload");
    std::fs::set_permissions(&payload, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let baked = template().replace("__AMONT_BIN__", "tools/amont");
    let hook = install_hook(&r, &baked);
    let out = run_hook(&r, &hook, &home);

    assert!(!sentinel.exists(), "a relative baked path was resolved");
    assert!(!out.status.success(), "must fail loudly instead");
}
