//! The quiet excerpt over the golden fixtures shared with amont-agent
//! (fleet plan 2026-10-11). The canonical fixtures live in amont-agent's
//! `tests/fixtures/excerpt/`; this copy is pinned by its manifest's hash, and
//! CI compares that manifest with amont-agent's (`excerpt fixtures` step).

use std::path::{Path, PathBuf};

use amont_runtime::excerpt::{Accumulator, DEFAULT_BOUND};

/// The sha256 of `MANIFEST.sha256`, the same constant amont-agent pins.
const MANIFEST_PIN: &str = "a00c12dd56c47d7368f00b6cc4853832cf600cfa04fd90882fcb16ba419d41fc";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/excerpt")
        .join(name)
}

fn excerpt(name: &str) -> String {
    let bytes = std::fs::read(fixture(name)).unwrap();
    let mut acc = Accumulator::new(DEFAULT_BOUND);
    acc.feed(&bytes);
    acc.finish("LOG")
}

#[test]
fn a_failing_cargo_test_shows_the_whole_panic_and_no_build_noise() {
    let out = excerpt("cargo-test-fail.txt");
    for must in [
        "panicked at src/render.rs:88:9",
        "left: \"alpha\\nbeta\\ngamma\"",
        "right: \"alpha\\nBETA\\ngamma\"",
        "empty input must be refused,",
        "but the renderer returned 0 bytes",
        "and then kept going",
        "test result: FAILED. 2 passed; 2 failed",
    ] {
        assert!(out.contains(must), "missing {must:?} in\n{out}");
    }
    assert!(!out.contains("Compiling"), "{out}");
    assert!(!out.contains("parse::reads_header"), "{out}");
    assert_eq!(
        out.matches("panicked at src/render.rs:88:9").count(),
        1,
        "{out}"
    );
}

#[test]
fn the_other_runners_show_their_failure() {
    let py = excerpt("pytest-fail.txt");
    assert!(py.contains("assert 0.30000000000000004 == 0.3"), "{py}");
    assert!(py.contains("1 failed, 2 passed"), "{py}");
    let vi = excerpt("vitest-fail.txt");
    assert!(vi.contains("AssertionError: expected '$1,000.00'"), "{vi}");
    assert!(vi.contains("× formats currency"), "{vi}");
    let es = excerpt("eslint-fail.txt");
    assert!(es.contains("'foo' is not defined"), "{es}");
    assert!(es.contains("4 problems (3 errors, 1 warning)"), "{es}");
}

#[test]
fn a_passing_run_that_says_zero_failed_starts_no_block() {
    // A pass prints one ok line and no excerpt at all; this is the stronger
    // claim underneath it: nothing in a passing run looks like a failure, so
    // the excerpt falls back to the run's last lines, unchanged.
    let out = excerpt("pass.txt");
    let body = std::fs::read_to_string(fixture("pass.txt")).unwrap();
    let want: Vec<&str> = body.lines().collect();
    let want = want[want.len().saturating_sub(40)..].join("\n");
    assert_eq!(out.trim_end(), want.trim_matches('\n').trim_end(), "{out}");
}

#[cfg(unix)]
fn sha256_of(path: &Path) -> String {
    use std::process::Command;
    for argv in [["sha256sum"].as_slice(), ["shasum", "-a", "256"].as_slice()] {
        if let Ok(o) = Command::new(argv[0]).args(&argv[1..]).arg(path).output() {
            if o.status.success() {
                return String::from_utf8_lossy(&o.stdout)
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_string();
            }
        }
    }
    panic!("neither sha256sum nor shasum is available");
}

#[cfg(unix)]
#[test]
fn the_manifest_is_pinned_and_matches_the_fixtures() {
    let manifest = fixture("MANIFEST.sha256");
    assert_eq!(
        sha256_of(&manifest),
        MANIFEST_PIN,
        "MANIFEST.sha256 changed: it must match amont-agent's copy, pin and all"
    );
    let body = std::fs::read_to_string(&manifest).unwrap();
    let mut listed = Vec::new();
    for line in body.lines().filter(|l| !l.trim().is_empty()) {
        let (hash, name) = line.split_once("  ").expect("`<sha>  <name>` lines");
        assert_eq!(
            sha256_of(&fixture(name)),
            hash,
            "{name} differs from the manifest"
        );
        listed.push(name.to_string());
    }
    let mut on_disk: Vec<String> = std::fs::read_dir(fixture(""))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "MANIFEST.sha256")
        .collect();
    on_disk.sort();
    listed.sort();
    assert_eq!(listed, on_disk, "every fixture is listed, and only those");
}
