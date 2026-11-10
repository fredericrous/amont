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
    let b = src.as_bytes();
    let mut i = 0;
    let mut n = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            b'\'' => {
                // A char literal ('x', '\n', '"'); a lifetime has no closing quote.
                if b.get(i + 1) == Some(&b'\\') {
                    let mut j = i + 2;
                    while j < b.len() && b[j] != b'\'' {
                        j += 1;
                    }
                    i = j + 1;
                } else if b.get(i + 2) == Some(&b'\'') {
                    i += 3;
                } else {
                    i += 1;
                }
            }
            b'r' if matches!(b.get(i + 1), Some(b'"') | Some(b'#'))
                && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')) =>
            {
                let mut j = i + 1;
                let mut hashes = 0;
                while b.get(j) == Some(&b'#') {
                    hashes += 1;
                    j += 1;
                }
                if b.get(j) != Some(&b'"') {
                    i += 1;
                    continue;
                }
                let start = j + 1;
                let mut end = start;
                loop {
                    if end >= b.len() {
                        break;
                    }
                    if b[end] == b'"'
                        && b[end + 1..]
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
                n += hits(&src[start..end.min(b.len())]);
                i = end + 1 + hashes;
            }
            b'"' => {
                let start = i + 1;
                let mut j = start;
                while j < b.len() && b[j] != b'"' {
                    if b[j] == b'\\' {
                        j += 1;
                    }
                    j += 1;
                }
                n += hits(&src[start..j.min(b.len())]);
                i = j + 1;
            }
            _ => i += 1,
        }
    }
    n
}

/// Occurrences of an old spelling in one literal's text, at a word start.
fn hits(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut n = 0;
    for (at, _) in text.match_indices("amont") {
        let word_start =
            at == 0 || !(bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_');
        if word_start && OLD.iter().any(|o| text[at..].starts_with(o)) {
            n += 1;
        }
    }
    n
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("readable source dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
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
        let mut files = Vec::new();
        sources(&src, &mut files);
        for file in files {
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
}
