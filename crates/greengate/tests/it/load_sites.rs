//! Every `manifest::load` caller that builds checks resolves their triggers.
//!
//! A commit-time declaration fires on its push twin's files too, but only once
//! `Manifest::resolve_triggers` has judged it blocking, and `load` has no
//! settings to judge with: each caller that goes on to BUILD checks calls it
//! right after it builds its `Settings`. A caller that forgets is safe, since
//! an unresolved trigger is the declaration's own scope (the push just runs
//! the suite), but it is the silent kind of wrong, and the next entrypoint
//! someone adds is the one that will forget. The compiler cannot see it; this
//! can: it reads the sources, as `docs_counts.rs` reads the docs.
//!
//! A caller that builds no check (it reads only the tool pins, or the tree
//! gates, or the policy) says so in a comment saying "builds no check".

/// Every `.rs` file under `dir`, found without a dependency.
fn sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn every_manifest_load_that_builds_checks_resolves_its_triggers() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    sources(&root.join("greengate-runtime/src"), &mut files);
    sources(&root.join("greengate/src"), &mut files);
    let mut sites = 0;
    let mut unresolved = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("read source");
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") || !line.contains("manifest::load(") {
                continue;
            }
            sites += 1;
            // The resolve follows the Settings built from the manifest, a few
            // lines down; the "builds no check" note sits just above the load.
            let before = &lines[i.saturating_sub(4)..i];
            let after = &lines[i..(i + 16).min(lines.len())];
            let resolves = after.iter().any(|l| l.contains("resolve_triggers("));
            let says_so = before
                .iter()
                .chain(after.iter().take(1))
                .any(|l| l.to_lowercase().contains("builds no check"));
            if !resolves && !says_so {
                unresolved.push(format!("{}:{}", file.display(), i + 1));
            }
        }
    }
    assert!(
        sites >= 9,
        "the scan found {sites} `manifest::load` calls; it has gone blind (9 at the time of writing)"
    );
    assert!(
        unresolved.is_empty(),
        "these `manifest::load` calls neither call `resolve_triggers` nor say \"builds no check\": {unresolved:#?}"
    );
}
