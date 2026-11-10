//! The tool's own name comes from one place: `greengate-runtime/src/names.rs`
//! (plan "amont ships as greengate", Phase 1).
//!
//! This scans every crate's `src/` for string literals that still say
//! `amont `, `amont: ` or `amont-fleet`, outside comments. What is left is
//! data the program reads or writes under the old name until plan 3a-2
//! (slot holder names, commit-tree messages, config keys in a hint), test
//! fixtures, and the frozen AGENTS.md block and the text that quotes it. Each file's count is
//! pinned below: a new literal fails here, and a file that drops one fails
//! too, so the list only shrinks on purpose.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Per file: how many old-name literals it may hold, and why.
const ALLOWED: &[(&str, usize, &str)] = &[
    ("greengate-fleet/src/attest.rs", 1, "a test's temp dir name"),
    (
        "greengate/src/main.rs",
        1,
        "the --help footer saying greengate was formerly amont",
    ),
    (
        "greengate-runtime/src/agents_md.rs",
        13,
        "the frozen AGENTS.md block (3a-1 keeps it byte-stable)",
    ),
    (
        "greengate-runtime/src/cluster.rs",
        1,
        "a commit-tree message stored in git",
    ),
    (
        "greengate-runtime/src/cluster/detached.rs",
        1,
        "a snapshot commit message stored in git",
    ),
    (
        "greengate-runtime/src/hookfile.rs",
        3,
        "test fixtures: shims a 2.x install wrote",
    ),
    (
        "greengate-runtime/src/hooks/audit.rs",
        8,
        "test fixtures: cargo tree output naming the old crate",
    ),
    (
        "greengate-runtime/src/install.rs",
        1,
        "the offer quoting the frozen AGENTS.md block",
    ),
    (
        "greengate-runtime/src/run_stamp.rs",
        1,
        "a commit-tree message stored in git",
    ),
    (
        "greengate-runtime/src/slot_cmd.rs",
        4,
        "the slot holder name other tools read (3a-2)",
    ),
    (
        "greengate-runtime/src/trust.rs",
        2,
        "a --json reason value, unchanged in 3a-1",
    ),
];

/// The three spellings of the old name as the tool's own.
const OLD: &[&str] = &["amont ", "amont: ", "amont-fleet"];

/// How many old-name occurrences sit inside string literals of `src`.
/// Comments are skipped; char literals and raw strings are understood well
/// enough for this codebase.
fn count(src: &str) -> usize {
    let bytes = src.as_bytes();
    let mut i = 0;
    let mut found = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            b'\'' => {
                // A char literal ('x', '\n', '"'); a lifetime has no closing quote.
                if bytes.get(i + 1) == Some(&b'\\') {
                    let mut j = i + 2;
                    while j < bytes.len() && bytes[j] != b'\'' {
                        j += 1;
                    }
                    i = j + 1;
                } else if let Some(width) = src[i + 1..].chars().next().map(char::len_utf8) {
                    // One char of any UTF-8 width, then the closing quote:
                    // `'x'`, `'—'`. A lifetime has no closing quote there.
                    if bytes.get(i + 1 + width) == Some(&b'\'') {
                        i += 2 + width;
                    } else {
                        i += 1;
                    }
                } else {
                    i += 1;
                }
            }
            b'r' if matches!(bytes.get(i + 1), Some(b'"') | Some(b'#'))
                && (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_')) =>
            {
                let mut j = i + 1;
                let mut hashes = 0;
                while bytes.get(j) == Some(&b'#') {
                    hashes += 1;
                    j += 1;
                }
                if bytes.get(j) != Some(&b'"') {
                    i += 1;
                    continue;
                }
                let start = j + 1;
                let mut end = start;
                loop {
                    if end >= bytes.len() {
                        break;
                    }
                    if bytes[end] == b'"'
                        && bytes[end + 1..]
                            .iter()
                            .take(hashes)
                            .filter(|c| **c == b'#')
                            .count()
                            == hashes
                    {
                        break;
                    }
                    end += 1;
                }
                found += hits(&src[start..end.min(bytes.len())]);
                i = end + 1 + hashes;
            }
            b'"' => {
                let start = i + 1;
                let mut j = start;
                while j < bytes.len() && bytes[j] != b'"' {
                    if bytes[j] == b'\\' {
                        j += 1;
                    }
                    j += 1;
                }
                found += hits(&src[start..j.min(bytes.len())]);
                i = j + 1;
            }
            _ => i += 1,
        }
    }
    found
}

/// Occurrences of an old spelling in one literal's text, at a word start.
fn hits(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut old_spellings = 0;
    for (at, _) in text.match_indices("amont") {
        let word_start =
            at == 0 || !(bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_');
        if word_start && OLD.iter().any(|o| text[at..].starts_with(o)) {
            old_spellings += 1;
        }
    }
    old_spellings
}

/// Every `.rs` file under `dir`, depth first.
fn sources(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).expect("readable source dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            found.extend(sources(&path));
        } else if path.extension().is_some_and(|e| e == "rs") {
            found.push(path);
        }
    }
    found
}

#[test]
fn the_old_name_is_said_only_where_it_is_allowed() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut found = BTreeMap::new();
    for krate in std::fs::read_dir(&crates).expect("crates dir") {
        let src = krate.expect("crate entry").path().join("src");
        if !src.is_dir() {
            continue;
        }
        for file in sources(&src) {
            if file.ends_with("greengate-runtime/src/names.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&file).expect("utf-8 source");
            let n = count(&text);
            if n > 0 {
                let rel = file
                    .strip_prefix(&crates)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                found.insert(rel, n);
            }
        }
    }
    let allowed: BTreeMap<String, usize> = ALLOWED
        .iter()
        .filter(|(_, n, _)| *n > 0)
        .map(|(f, n, _)| (f.to_string(), *n))
        .collect();
    assert_eq!(
        found, allowed,
        "old-name literals per file differ from the pinned list: build the name from \
         greengate_runtime::names (`{{BIN}}`, `{{FLEET_BIN}}`, `bin!()`), or, for a name read \
         or written as data, pin the new count in ALLOWED with its reason"
    );
}

#[test]
fn each_planted_spelling_is_counted() {
    assert_eq!(count(r#"eprintln!("amont land failed");"#), 1);
    assert_eq!(count(r#"let s = "amont: refused";"#), 1);
    assert_eq!(count(r#"let s = "run amont-fleet fix";"#), 1);
    assert_eq!(count("let s = r#\"see amont land\"#;"), 1);
}

#[test]
fn comments_names_read_and_char_literals_are_not() {
    assert_eq!(count("// amont land\n/// amont: x\n/* amont-fleet */"), 0);
    assert_eq!(
        count(r#"let k = "amont.conf"; let e = "AMONT_X"; let p = "amont-push-1";"#),
        0
    );
    assert_eq!(count(r#"let q = '"'; let s = "fine"; let t = '\''; "#), 0);
    assert_eq!(count(r#"let s = "greenamont land";"#), 0);
    // A multibyte char literal does not throw the string pairing off.
    assert_eq!(count("let c = '—'; let s = \"amont land\";"), 1);
    assert_eq!(
        count("fn f<'a>(x: &'a str) -> &'a str { \"amont land\" }"),
        1
    );
}

/// The names outside Rust say what `names.rs` says: the manifest grh pins and
/// the script that writes its release copy. A rename that changed `names.rs`
/// alone would otherwise publish a manifest under the old name.
#[test]
fn the_shipped_manifest_and_release_script_use_the_names_module() {
    use greengate_runtime::names::{BIN, REPO};
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest =
        std::fs::read_to_string(root.join("greenharness.toml")).expect("greenharness.toml");
    let line = |key: &str| {
        manifest
            .lines()
            .find(|l| l.starts_with(&format!("{key} = ")))
            .map(|l| {
                l.trim_start_matches(&format!("{key} = "))
                    .trim_matches('"')
                    .to_string()
            })
    };
    assert_eq!(line("name").as_deref(), Some(BIN), "[extension] name");
    assert_eq!(line("bin").as_deref(), Some(BIN), "[tool] bin");
    let script = std::fs::read_to_string(root.join("scripts/release-manifest.py"))
        .expect("release-manifest.py");
    assert!(
        script.contains(&format!("REPO = \"{REPO}\"")),
        "scripts/release-manifest.py names another repository than names::REPO"
    );
    assert!(
        script.contains(&format!("{BIN}-{{version}}-{{target}}.tar.gz")),
        "scripts/release-manifest.py reads tarballs under another name than names::BIN"
    );
    let tap = std::fs::read_to_string(root.join("scripts/bump-tap.py")).expect("bump-tap.py");
    assert!(
        tap.contains(&format!("REPO = \"{REPO}\"")),
        "scripts/bump-tap.py names another repository than names::REPO"
    );
}
