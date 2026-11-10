//! A minimal JSON emitter for `amont list --json`.
//!
//! Twenty lines for a flat, known schema — not a general serialiser. See
//! `manifest.rs`'s module doc for why this project prefers that trade on the
//! commit path: this crate must stay dependency-free, and a hand-rolled
//! escaper for the one shape `CheckListing` needs costs less than a `serde`
//! tree that runs on every commit in 96 repositories.

/// Escape `s` per the JSON spec: `"`, `\`, and control characters.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

pub fn string_field(key: &str, value: &str) -> String {
    format!("\"{}\":\"{}\"", escape(key), escape(value))
}

pub fn bool_field(key: &str, value: bool) -> String {
    format!("\"{}\":{value}", escape(key))
}

/// A number, unquoted — a limit a reader will compare against is worth
/// emitting as one rather than as a string they have to parse back.
pub fn int_field(key: &str, value: i64) -> String {
    format!("\"{}\":{value}", escape(key))
}

/// `null` when `value` is `None` — used for `stage_filter` and `command`.
pub fn opt_int_field(key: &str, value: Option<i64>) -> String {
    match value {
        Some(v) => int_field(key, v),
        None => format!("\"{}\":null", escape(key)),
    }
}

pub fn opt_string_field(key: &str, value: Option<&str>) -> String {
    match value {
        Some(v) => string_field(key, v),
        None => format!("\"{}\":null", escape(key)),
    }
}

pub fn string_array_field(key: &str, values: &[String]) -> String {
    let items: Vec<String> = values
        .iter()
        .map(|v| format!("\"{}\"", escape(v)))
        .collect();
    format!("\"{}\":[{}]", escape(key), items.join(","))
}

/// Comma-join already-built `"key":value` fragments into `{...}`.
pub fn object(fields: &[String]) -> String {
    format!("{{{}}}", fields.join(","))
}

/// Comma-join already-built objects into `[...]`.
pub fn array(items: &[String]) -> String {
    format!("[{}]", items.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_quote_backslash_and_control_chars() {
        assert_eq!(escape("a\"b"), "a\\\"b");
        assert_eq!(escape("a\\b"), "a\\\\b");
        assert_eq!(escape("a\nb"), "a\\nb");
        assert_eq!(escape("a\rb"), "a\\rb");
        assert_eq!(escape("a\tb"), "a\\tb");
        assert_eq!(escape("a\u{1}b"), "a\\u0001b");
    }

    #[test]
    fn plain_text_is_untouched() {
        assert_eq!(escape("pre-commit-clippy"), "pre-commit-clippy");
        assert_eq!(escape(""), "");
    }

    #[test]
    fn fields_quote_both_key_and_string_value() {
        assert_eq!(
            string_field("id", "pre-commit-clippy"),
            "\"id\":\"pre-commit-clippy\""
        );
        assert_eq!(bool_field("pushed", true), "\"pushed\":true");
        assert_eq!(bool_field("pushed", false), "\"pushed\":false");
        assert_eq!(opt_string_field("command", None), "\"command\":null");
        assert_eq!(
            opt_string_field("command", Some("ruff check")),
            "\"command\":\"ruff check\""
        );
        assert_eq!(opt_int_field("last", None), "\"last\":null");
        assert_eq!(opt_int_field("last", Some(42)), "\"last\":42");
    }

    #[test]
    fn string_array_field_joins_and_escapes_each_element() {
        assert_eq!(string_array_field("scope_files", &[]), "\"scope_files\":[]");
        assert_eq!(
            string_array_field("scope_files", &[".rs".to_string(), "a\"b".to_string()]),
            "\"scope_files\":[\".rs\",\"a\\\"b\"]"
        );
    }

    #[test]
    fn object_and_array_join_with_commas() {
        assert_eq!(
            object(&["\"a\":1".into(), "\"b\":2".into()]),
            "{\"a\":1,\"b\":2}"
        );
        assert_eq!(array(&["1".into(), "2".into()]), "[1,2]");
        assert_eq!(object(&[]), "{}");
        assert_eq!(array(&[]), "[]");
    }
}
