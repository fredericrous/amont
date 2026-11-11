//! The small JSON scans two hooks share: where a string ends, where a bracket
//! closes, and the value of a top-level key. Brace-matched, string-aware and
//! dependency-free (this crate has no dependencies), and deliberately not a
//! parser: each caller asks one question of one document.

/// Index of the closing quote of the string whose content starts at `from`.
pub(super) fn string_end(bytes: &[u8], from: usize) -> Option<usize> {
    let mut escaped = false;
    for (i, &c) in bytes.iter().enumerate().skip(from) {
        if escaped {
            escaped = false;
        } else if c == b'\\' {
            escaped = true;
        } else if c == b'"' {
            return Some(i);
        }
    }
    None
}

/// Index of the `shut` matching the `open` at `from`, string-aware.
pub(super) fn bracket_end(bytes: &[u8], from: usize, open: u8, shut: u8) -> Option<usize> {
    let (mut depth, mut i) = (0usize, from);
    while i < bytes.len() {
        match bytes[i] {
            b'"' => i = string_end(bytes, i + 1)?,
            c if c == open => depth += 1,
            c if c == shut => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The text of the value of the top-level `key` of a JSON document, when that
/// value is an array or an object (brackets included). A depth-tracking scan:
/// a string only counts as a key at depth 1 with a `:` after it, so the same
/// word elsewhere cannot answer.
pub(super) fn top_level_value<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let bytes = json.as_bytes();
    let (mut depth, mut i) = (0usize, 0usize);
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                let start = i + 1;
                let close = string_end(bytes, start)?;
                let content = &json[start..close];
                i = close + 1;
                if depth != 1 {
                    continue;
                }
                let mut j = i;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j >= bytes.len() || bytes[j] != b':' || content != key {
                    continue;
                }
                let mut k = j + 1;
                while k < bytes.len() && bytes[k].is_ascii_whitespace() {
                    k += 1;
                }
                let (open, shut) = match bytes.get(k)? {
                    b'[' => (b'[', b']'),
                    b'{' => (b'{', b'}'),
                    _ => return None,
                };
                let end = bracket_end(bytes, k, open, shut)?;
                return Some(&json[k..=end]);
            }
            b'{' | b'[' => {
                depth += 1;
                i += 1;
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}
