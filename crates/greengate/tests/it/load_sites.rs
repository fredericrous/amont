//! Every caller that builds checks loads its manifest through `load_resolved`.
//!
//! A commit-time declaration fires on its push twin's files too, but only once
//! `Manifest::resolve_triggers` has judged it blocking, and `load` has no
//! settings to judge with. `manifest::load_resolved` is `load`, then the
//! `Settings` built from its policy, then that resolve, in the one order that
//! is right; a caller that builds checks goes through it. A caller that forgets
//! is safe, since an unresolved trigger is the declaration's own scope (the
//! push just runs the suite), but it is the silent kind of wrong, and the next
//! entrypoint someone adds is the one that will forget. The compiler cannot see
//! it; this can: it reads the sources, as `docs_counts.rs` reads the docs.
//!
//! A caller that uses plain `manifest::load` builds no check (it reads only the
//! tool pins, the tree gates or the policy) and says so in a comment saying
//! "builds no check".

use std::path::{Path, PathBuf};

/// Every `.rs` file under `dir`, found without a dependency.
fn sources(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            found.extend(sources(&path));
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
    found
}

#[test]
fn every_manifest_load_that_builds_checks_resolves_its_triggers() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = sources(&root.join("greengate-runtime/src"));
    files.extend(sources(&root.join("greengate/src")));
    let (mut resolved, mut plain) = (0, 0);
    let mut unexplained = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("read source");
        let lines: Vec<&str> = text.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            if line.contains("manifest::load_resolved(") {
                resolved += 1;
            } else if line.contains("manifest::load(") {
                plain += 1;
                // The note sits just above the load, or on its line.
                let near = &lines[index.saturating_sub(4)..=index];
                if !near
                    .iter()
                    .any(|l| l.to_lowercase().contains("builds no check"))
                {
                    unexplained.push(format!("{}:{}", file.display(), index + 1));
                }
            }
        }
    }
    assert!(
        resolved >= 5 && plain >= 3,
        "the scan found {resolved} load_resolved and {plain} plain load calls; it has gone blind \
         (6 and 3 at the time of writing)"
    );
    assert!(
        unexplained.is_empty(),
        "these plain `manifest::load` calls do not say \"builds no check\" (a caller that builds \
         checks goes through `load_resolved`): {unexplained:#?}"
    );
}
