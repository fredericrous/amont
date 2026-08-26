//! The npm packaging describes the same six targets the release builds.
//!
//! Four files have to agree about that list — `release.yaml`'s build matrix,
//! `scripts/npm-pack.sh`'s table, `npm/amont/bin/native.js`'s resolution map, and
//! `npm/amont/package.json.in`'s `optionalDependencies` — and nothing compiles
//! any of them together. Every way they can disagree fails at a moment nothing
//! can be taken back:
//!
//!   * a target built but not packed → `amont` resolves to nothing on that
//!     platform, and the person finds out at their first commit;
//!   * a target packed but not built → `npm-pack.sh` dies mid-release, after
//!     the GitHub release has already published;
//!   * a package laid out but not declared → npm never installs it, and the
//!     wrapper reports "no native binary" on a platform that has one;
//!   * a package declared but not laid out → `npm publish` rejects `amont` for
//!     a dependency version that does not exist.
//!
//! So the four lists are read off disk and compared. A Rust test is a strange
//! home for it and it is the only place that runs on every commit.

mod common;

use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/amont; the files live at the repo root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root")
        .to_path_buf()
}

fn read(rel: &str) -> String {
    let p = root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Rust targets named in `release.yaml`'s build matrix.
fn release_targets() -> Vec<String> {
    read(".github/workflows/release.yaml")
        .lines()
        .filter_map(|l| l.trim().strip_prefix("- target: ").map(str::to_string))
        .collect()
}

/// `(rust-target, npm-package)` pairs from `npm-pack.sh`'s table.
fn packed_pairs() -> Vec<(String, String)> {
    read("scripts/npm-pack.sh")
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('"') && l.contains("|amont-"))
        .filter_map(|l| {
            let row = l.trim_start_matches('"').trim_end_matches('"');
            let mut it = row.split('|');
            Some((it.next()?.to_string(), it.next()?.to_string()))
        })
        .collect()
}

/// Package names in `amont`'s `optionalDependencies`.
fn declared_packages() -> Vec<String> {
    let text = read("npm/amont/package.json.in");
    let (_, after) = text
        .split_once("\"optionalDependencies\"")
        .expect("optionalDependencies block");
    after
        .lines()
        .take_while(|l| !l.contains('}'))
        .filter_map(|l| {
            let l = l.trim();
            l.strip_prefix('"')?
                .split_once('"')
                .map(|(name, _)| name.to_string())
        })
        .collect()
}

/// Package names the JS wrapper knows how to resolve.
fn wrapper_packages() -> Vec<String> {
    // The map lives in `native.js` rather than in the bin wrapper: it moved
    // there when a second wrapper needed the same resolution logic, and it
    // stays there because one copy is one place to read it from.
    let text = read("npm/amont/bin/native.js");
    let (_, after) = text.split_once("const PACKAGES").expect("PACKAGES map");
    after
        .lines()
        .take_while(|l| !l.starts_with("};"))
        .flat_map(|l| {
            l.match_indices("\"amont-")
                .filter_map(|(i, _)| {
                    let rest = &l[i + 1..];
                    rest.split_once('"').map(|(name, _)| name.to_string())
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v.dedup();
    v
}

#[test]
fn every_released_target_is_packed_for_npm() {
    let built = sorted(release_targets());
    let packed = sorted(packed_pairs().into_iter().map(|(t, _)| t).collect());
    assert_eq!(
        built, packed,
        "release.yaml builds one set of targets and npm-pack.sh packs another"
    );
}

#[test]
fn the_packed_packages_are_exactly_the_declared_ones() {
    let packed = sorted(packed_pairs().into_iter().map(|(_, p)| p).collect());
    let declared = sorted(declared_packages());
    assert_eq!(
        packed, declared,
        "npm-pack.sh lays out one set of packages and amont declares another \
         as optionalDependencies — npm rejects a version that does not exist"
    );
}

#[test]
fn the_wrapper_can_resolve_every_package_that_is_published() {
    let declared = sorted(declared_packages());
    let known = sorted(wrapper_packages());
    assert_eq!(
        declared, known,
        "bin/native.js resolves a different set than npm installs — a platform \
         with a binary would be told it has none"
    );
}

/// Existence is not runnability: when a package manager that ignores `libc`
/// installs BOTH linux-x64 packages, the gnu build is listed first and fails
/// at exec on a musl host. The wrapper must fall through to the next candidate
/// on a spawn-level failure — and must NOT fall through on a real exit code,
/// which is a binary having answered.
///
/// Runs the real `bin/amont.js` under node against a fake package tree:
/// the first candidate is executable garbage (spawn fails), the second is a
/// script that exits 7. The 7 coming back is the fallback working end to end.
#[cfg(unix)]
#[test]
fn the_wrapper_tries_the_next_candidate_when_a_spawn_fails() {
    use std::os::unix::fs::PermissionsExt;
    if common::missing("node") {
        return;
    }
    let dir = std::env::temp_dir().join(format!("npm-wrapper-fallback-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let modules = dir.join("node_modules");

    // The wrapper itself, where npm would put it — resolution walks up from
    // the file's own directory, exactly as in a real install.
    let wrapper = modules.join("amont/bin/amont.js");
    // `amont.js` is a two-line shim over `native.js`; the fixture needs both or
    // the require fails before any of the resolution under test happens.
    let native = modules.join("amont/bin/native.js");
    std::fs::create_dir_all(wrapper.parent().unwrap()).unwrap();
    std::fs::copy(root().join("npm/amont/bin/amont.js"), &wrapper).unwrap();
    std::fs::copy(root().join("npm/amont/bin/native.js"), &native).unwrap();

    // Candidate one: exists, is executable, cannot run — the gnu-on-musl
    // shape, faithfully. A glibc binary on musl fails exec with ENOENT (the
    // dynamic loader named in its header is not there), which a shebang
    // naming a nonexistent interpreter reproduces exactly. NOT garbage bytes:
    // those fail with ENOEXEC, and on Linux `execvp` retries ENOEXEC through
    // /bin/sh — the garbage RUNS, exits 127, and that is an answer, not a
    // spawn failure.
    let gnu = modules.join("amont-linux-x64-gnu/bin/amont");
    std::fs::create_dir_all(gnu.parent().unwrap()).unwrap();
    std::fs::write(&gnu, "#!/nonexistent/ld-linux-x86-64.so.2\n").unwrap();
    std::fs::set_permissions(&gnu, std::fs::Permissions::from_mode(0o755)).unwrap();

    // Candidate two: runs, and answers with a distinctive exit code.
    let musl = modules.join("amont-linux-x64-musl/bin/amont");
    std::fs::create_dir_all(musl.parent().unwrap()).unwrap();
    std::fs::write(&musl, "#!/bin/sh\nexit 7\n").unwrap();
    std::fs::set_permissions(&musl, std::fs::Permissions::from_mode(0o755)).unwrap();

    // The host running this test is darwin or linux-gnu, so pin the wrapper's
    // view of the platform to the case under test instead of the machine's.
    let run = dir.join("run.js");
    std::fs::write(
        &run,
        "Object.defineProperty(process, \"platform\", { value: \"linux\" });\n\
         Object.defineProperty(process, \"arch\", { value: \"x64\" });\n\
         require(\"./node_modules/amont/bin/amont.js\");\n",
    )
    .unwrap();

    let out = std::process::Command::new("node")
        .arg(&run)
        .current_dir(&dir)
        .output()
        .expect("node");
    assert_eq!(
        out.status.code(),
        Some(7),
        "the second candidate's exit code must come back:\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The control: with no runnable candidate at all, the wrapper names what
    // it tried instead of exiting with a code that looks like an answer.
    std::fs::remove_file(&musl).unwrap();
    let out = std::process::Command::new("node")
        .arg(&run)
        .current_dir(&dir)
        .output()
        .expect("node");
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("no runnable native binary") && err.contains("amont-linux-x64-gnu"),
        "the failure must name what was tried:\n{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The wrapper must never be what a hook execs.
///
/// `init` bakes `current_exe()`, which under npm is the native binary inside the
/// platform package. If this ever became `node_modules/.bin/amont`, every commit
/// in every repository would pay node's start-up — on a tool whose module docs
/// argue about kilobytes of binary size. Cheap to assert, and invisible if it
/// regresses.
#[test]
fn init_bakes_the_native_binary_not_the_node_wrapper() {
    let src = read("crates/amont-runtime/src/install.rs");

    // Line-based, NOT `find("\n}\n")`. On Windows the checkout is CRLF, so that
    // needle never matched, the `unwrap_or(len())` fallback took the whole rest
    // of the FILE as the body, and the `on_path_already` in `install_binary`
    // three functions further down failed this assertion. `str::lines()` splits
    // on `\n` and strips a trailing `\r`, so it reads both checkouts the same.
    let init_body: Vec<&str> = src
        .lines()
        .skip_while(|l| !l.starts_with("pub fn init()"))
        .take_while(|l| !l.starts_with('}') || l.starts_with("pub fn init()"))
        .collect();
    assert!(
        init_body.len() > 1,
        "could not find the body of pub fn init()"
    );
    let init_body = init_body.join("\n");

    assert!(
        init_body.contains("current_exe()"),
        "init no longer bakes current_exe()"
    );
    assert!(
        !init_body.contains("on_path_already"),
        "init consults PATH — under npm that resolves to the JS wrapper, and \
         every commit would spawn node"
    );
}
