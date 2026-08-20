//! The contract `amont list --json` publishes, pinned to the page that
//! documents it.
//!
//! A reader who guesses a field name does not get an error — they get
//! `null`, which reads as a perfectly plausible answer ("nothing is
//! skipped", "no override"). That is not a hypothetical failure mode: it
//! happened while verifying a release, and the wrong number looked right.
//! So the field names are checked in BOTH directions — every key the code
//! emits is documented, and every key the docs promise is emitted — and the
//! document declares a format id, like every other machine-readable thing
//! this tool writes.

mod common;
use common::Repo;

fn page() -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/checks.md");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Keys of one JSON object's OWN level — brace depth 1 within `text`,
/// skipping anything nested. Hand-rolled because this crate ships without a
/// JSON parser, and a test that pulled one in would be testing that parser's
/// idea of the document rather than the bytes we print.
fn keys_at_top(text: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let (mut depth, mut in_str, mut escaped) = (0i32, false, false);
    let mut current = String::new();
    let bytes: Vec<char> = text.chars().collect();
    for (i, c) in bytes.iter().enumerate() {
        if in_str {
            if escaped {
                escaped = false;
            } else if *c == '\\' {
                escaped = true;
            } else if *c == '"' {
                in_str = false;
                // A key is a string immediately followed by a colon, and
                // only at this object's own level.
                if depth == 1 && bytes.get(i + 1) == Some(&':') {
                    keys.push(std::mem::take(&mut current));
                } else {
                    current.clear();
                }
            } else {
                current.push(*c);
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' | '[' => depth += 1,
            '}' | ']' => depth -= 1,
            _ => {}
        }
    }
    keys
}

/// The first element of the `checks` array, as its own document.
fn first_check(json: &str) -> String {
    let at = json.find("\"checks\":[").expect("a checks array");
    let start = json[at..].find('{').expect("a check object") + at;
    let (mut depth, mut in_str, mut escaped) = (0i32, false, false);
    for (i, c) in json[start..].char_indices() {
        if in_str {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return json[start..start + i + 1].to_string();
                }
            }
            _ => {}
        }
    }
    panic!("unterminated check object");
}

/// Documented as a fenced field name — `` `id` `` — anywhere on the page.
fn documented(page: &str, field: &str) -> bool {
    page.contains(&format!("`{field}`"))
}

#[test]
fn the_json_document_declares_its_format() {
    let r = Repo::new();
    let out = r.run(&["list", "--json"]);
    assert!(
        out.stdout.contains("\"format\":\"amont-list-v1\""),
        "a machine contract must say which contract it is:\n{}",
        out.stdout
    );
    assert!(
        page().contains("amont-list-v1"),
        "and the page must name the same version"
    );
}

#[test]
fn every_emitted_field_is_documented_and_every_documented_field_is_emitted() {
    let r = Repo::new();
    let json = r.run(&["list", "--json"]).stdout;
    let doc = page();

    let envelope = keys_at_top(&json);
    assert!(
        envelope.contains(&"checks".to_string()),
        "the scanner found no envelope keys — it is broken, not the output: {envelope:?}"
    );
    for field in &envelope {
        assert!(
            documented(&doc, field),
            "`{field}` is emitted at the top level and documented nowhere in \
             docs/checks.md — a reader cannot know to ask for it"
        );
    }

    let check = first_check(&json);
    let fields = keys_at_top(&check);
    assert!(
        fields.contains(&"id".to_string()),
        "the scanner found no check keys — it is broken, not the output: {fields:?}"
    );
    for field in &fields {
        assert!(
            documented(&doc, field),
            "check field `{field}` is emitted and documented nowhere in \
             docs/checks.md"
        );
    }

    // The other direction: the table cannot promise a field the code stopped
    // emitting. Only the rows of the checks table are checked, since that is
    // the list this page claims is exhaustive.
    //
    // The table ends at the first line that is not a row — NOT at a blank
    // line found by splitting on "\n\n", which is what this did first. That
    // spelling never matched on Windows, where the checkout is CRLF, so the
    // "table" ran on into the rest of the page and every row of the checks
    // CATALOGUE was read as a promised JSON field. `str::lines` splits on
    // both endings and drops the `\r`, so the scan below is the same on
    // every platform.
    let after = doc
        .split("| field | what it says |")
        .nth(1)
        .expect("the field table");
    let mut rows = 0usize;
    for line in after.lines().map(str::trim) {
        if line.starts_with("| ---") || line.is_empty() {
            if rows > 0 {
                break; // a blank line after the rows: the table is over
            }
            continue; // the header's own tail, and the separator row
        }
        let Some(rest) = line.strip_prefix("| `") else {
            break; // prose again
        };
        let name = rest.split('`').next().expect("a fenced field name");
        assert!(
            fields.iter().any(|f| f == name),
            "docs/checks.md promises check field `{name}`, which `list --json` \
             does not emit: {fields:?}"
        );
        rows += 1;
    }
    assert!(
        rows >= fields.len(),
        "the table scan found only {rows} rows for {} emitted fields — the \
         scanner is broken, and a broken scanner passes vacuously",
        fields.len()
    );
}
