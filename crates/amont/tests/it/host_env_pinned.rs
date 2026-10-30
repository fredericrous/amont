//! Every fixture starts amont through `common::amont_cmd`, which strips
//! git's exported environment and pins the host knobs (ADR-0009). A fixture
//! that built the command by hand ran unpinned whenever cargo did not read
//! `.cargo/config.toml`, queued its heavy check on the real host slots behind
//! the suite running it, and timed out on a busy host. This test fails on a
//! new hand-built command and names it.

use std::path::{Path, PathBuf};

/// Files that build the command themselves, on purpose.
const ALLOWED: &[(&str, &str)] = &[
    (
        "common/mod.rs",
        "defines `amont_cmd`, and `hook_watched_unpinned` for the load fixtures",
    ),
    (
        "it/busy_defers.rs",
        "removes the slot pin to make the host look busy",
    ),
    (
        "it/slot_run.rs",
        "removes the slot pin to queue on a slot directory of its own",
    ),
];

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read the tests directory") {
        let path = entry.expect("read a directory entry").path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn every_fixture_starts_amont_through_amont_cmd() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    // Assembled here, so this file does not match itself.
    let needles = [
        format!("Command::new(env!(\"{}\"))", "CARGO_BIN_EXE_amont"),
        format!("Command::new({}())", "bin"),
    ];
    let mut files = Vec::new();
    sources(&root, &mut files);
    let mut found = Vec::new();
    for file in files {
        let rel = file
            .strip_prefix(&root)
            .expect("under tests/")
            .to_string_lossy()
            .replace('\\', "/");
        if ALLOWED.iter().any(|(path, _)| *path == rel) {
            continue;
        }
        let text = std::fs::read_to_string(&file).expect("read a test source");
        for (i, line) in text.lines().enumerate() {
            if needles.iter().any(|n| line.contains(n.as_str())) {
                found.push(format!("tests/{rel}:{}", i + 1));
            }
        }
    }
    assert!(
        found.is_empty(),
        "start amont with `crate::common::amont_cmd()` instead (or add the file to \
         ALLOWED with its reason):\n{}",
        found.join("\n")
    );
}
