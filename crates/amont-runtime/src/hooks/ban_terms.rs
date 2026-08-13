//! pre-commit-ban-terms — refuse focused/debug leftovers in staged JS/TS.
//!
//! Two stages, kept exactly as the JS had them:
//!   1. `git diff --cached -G<loose>` picks candidate files cheaply, and keeps
//!      the check scoped to what this commit touches — a pre-existing violation
//!      in an untouched part of an edited file is not this commit's problem.
//!   2. Each candidate is re-checked against its STAGED content with comments
//!      and string literals blanked, which is where correctness lives.
//!
//! Stage 1 stays deliberately loose (POSIX regex, flavour varies by platform);
//! a loose prefilter costs one extra file read, a strict one misses violations.

use crate::check::Outcome;
use crate::git;
use crate::ui::{error_sign, highlight, valid_sign};

/// (label, loose `git diff -G` prefilter, precise matcher)
struct Term {
    label: &'static str,
    prefilter: &'static str,
    matches: fn(&str) -> bool,
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// `(?<![\w.$])<word>` — not preceded by an identifier char, a dot or a `$`.
/// The dot is what keeps `foo.fit(` out; without it `layout.fit(` is a false
/// positive.
fn preceded_ok(src: &str, at: usize) -> bool {
    src[..at]
        .chars()
        .next_back()
        .map(|c| !(is_ident(c) || c == '.'))
        .unwrap_or(true)
}

/// `<word>\s*\(` — the call form.
fn call_of(src: &str, word: &str) -> bool {
    let mut from = 0;
    while let Some(i) = src[from..].find(word) {
        let at = from + i;
        let after = at + word.len();
        if preceded_ok(src, at) && src[after..].trim_start().starts_with('(') {
            return true;
        }
        from = at + word.len();
    }
    false
}

/// `(?<![\w.$])debugger(?![\w$])` — the bare statement, so `debuggerish` and
/// `x.debugger` both pass.
fn bare_debugger(src: &str) -> bool {
    let word = "debugger";
    let mut from = 0;
    while let Some(i) = src[from..].find(word) {
        let at = from + i;
        let after = at + word.len();
        let next_ok = src[after..]
            .chars()
            .next()
            .map(|c| !is_ident(c))
            .unwrap_or(true);
        if preceded_ok(src, at) && next_ok {
            return true;
        }
        from = at + word.len();
    }
    false
}

/// `(?<![\w$])(describe|context|it)\.(skip|only)(?![\w$])`
///
/// The trailing guard is the whole point: `describe.skipIf(...)` is vitest's
/// legitimate conditional API and must pass, while `describe.skip` must not.
/// Note the LEADING guard here excludes only identifier chars, not `.` — that
/// matches the JS, so `foo.describe.skip` is still caught.
fn focused_suite(src: &str) -> bool {
    for head in ["describe", "context", "it"] {
        for tail in ["skip", "only"] {
            let needle = format!("{head}.{tail}");
            let mut from = 0;
            while let Some(i) = src[from..].find(&needle) {
                let at = from + i;
                let after = at + needle.len();
                let before_ok = src[..at]
                    .chars()
                    .next_back()
                    .map(|c| !is_ident(c))
                    .unwrap_or(true);
                let after_ok = src[after..]
                    .chars()
                    .next()
                    .map(|c| !is_ident(c))
                    .unwrap_or(true);
                if before_ok && after_ok {
                    return true;
                }
                from = at + needle.len();
            }
        }
    }
    false
}

const TERMS: [Term; 4] = [
    Term {
        label: "fit",
        prefilter: r"\s*fit\(",
        matches: |s| call_of(s, "fit"),
    },
    Term {
        label: "fdescribe",
        prefilter: r"\s*fdescribe\(",
        matches: |s| call_of(s, "fdescribe"),
    },
    Term {
        label: "debugger",
        prefilter: "debugger;?",
        matches: bare_debugger,
    },
    Term {
        label: "skipOnly",
        prefilter: r"(describe|context|it)\.(skip|only)",
        matches: focused_suite,
    },
];

#[derive(Clone, Copy, PartialEq)]
enum S {
    Code,
    Line,
    Block,
    Single,
    Double,
    Template,
    Regex,
}

/// Keywords after which a `/` begins a regex, not a division.
const REGEX_KEYWORDS: [&str; 13] = [
    "return",
    "typeof",
    "case",
    "in",
    "of",
    "delete",
    "void",
    "instanceof",
    "new",
    "do",
    "else",
    "yield",
    "await",
];

/// Can a `/` here start a regex? Decided from the last significant code
/// character, and the word it belongs to when it is one.
fn regex_can_start(prev: Option<char>, word: &str) -> bool {
    match prev {
        // start of input
        None => true,
        Some(c) if "(,=:[!&|?{};+-*%~^<>".contains(c) => true,
        // an identifier or number ends a value → `/` divides it …
        Some(c) if c.is_alphanumeric() || c == '_' || c == '$' => REGEX_KEYWORDS.contains(&word),
        // … as do `)` and `]`; `}` is ambiguous and treated as allowing a
        // regex, since a false alarm is the worse error (see the type doc).
        Some(_) => false,
    }
}

/// Blank comments and the insides of string/template/REGEX literals, preserving
/// length and line count so offsets still line up — blanked, not deleted, for
/// that reason.
///
/// Now handles regex literals. It previously did not, and `/a\\/b/` read as a
/// line comment: everything after it on that line was blanked, so a real
/// `debugger;` sharing the line went unreported. Missed warnings rather than
/// false alarms, but missed all the same.
///
/// Telling a regex from division needs the preceding token, which is why this
/// is a tokenizer and not a set of rules over characters. The two ways to be
/// wrong are NOT symmetric:
///   - division mistaken for a regex → blanks to the next `/`, so a violation
///     there is MISSED. Safe direction.
///   - a regex mistaken for division → its contents are scanned as code, so
///     `/it\\.only/` would be reported as a violation that does not exist.
///     A false alarm blocking a commit — the direction to avoid, and why the
///     "regex allowed" set below is generous.
pub fn blank_non_code(src: &str) -> String {
    let b: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut state = S::Code;
    let mut i = 0;
    // The token before the current position, for the regex-vs-division call.
    let mut prev_significant: Option<char> = None;
    let mut word = String::new();
    let mut in_class = false;
    // Open `${…}` substitutions, innermost last; each entry counts the `{` we
    // are nested under, so the matching `}` returns us to template text.
    // A stack, not a flag: substitutions nest — `` `${`${x}`}` ``.
    let mut subst: Vec<u32> = Vec::new();
    let keep = |c: char| if c == '\n' { '\n' } else { ' ' };

    while i < b.len() {
        let ch = b[i];
        let next = b.get(i + 1).copied();
        match state {
            S::Code => {
                if ch == '/' && next == Some('/') {
                    state = S::Line;
                    out.push_str("  ");
                    i += 2;
                } else if ch == '/' && next == Some('*') {
                    state = S::Block;
                    out.push_str("  ");
                    i += 2;
                } else if ch == '/' && regex_can_start(prev_significant, &word) {
                    state = S::Regex;
                    in_class = false;
                    out.push(ch);
                    i += 1;
                } else if ch == '\'' || ch == '"' || ch == '`' {
                    state = match ch {
                        '\'' => S::Single,
                        '"' => S::Double,
                        _ => S::Template,
                    };
                    out.push(ch);
                    i += 1;
                } else {
                    if !subst.is_empty() {
                        if ch == '{' {
                            *subst.last_mut().expect("non-empty") += 1;
                        } else if ch == '}' {
                            let depth = subst.last_mut().expect("non-empty");
                            if *depth == 0 {
                                subst.pop();
                                state = S::Template;
                                out.push(ch);
                                i += 1;
                                continue;
                            }
                            *depth -= 1;
                        }
                    }
                    if !ch.is_whitespace() {
                        prev_significant = Some(ch);
                        if ch.is_alphanumeric() || ch == '_' || ch == '$' {
                            word.push(ch);
                        } else {
                            word.clear();
                        }
                    }
                    out.push(ch);
                    i += 1;
                }
            }
            S::Regex => {
                // `\` escapes anything; `[…]` is a class in which `/` is literal.
                if ch == '\\' {
                    out.push_str(if next.is_none() { " " } else { "  " });
                    i += 2;
                    continue;
                }
                if ch == '[' {
                    in_class = true;
                } else if ch == ']' {
                    in_class = false;
                } else if ch == '/' && !in_class {
                    state = S::Code;
                    prev_significant = Some('/');
                    word.clear();
                    out.push(ch);
                    i += 1;
                    continue;
                } else if ch == '\n' {
                    // An unterminated regex cannot span lines — bail back to
                    // code rather than blanking the rest of the file.
                    state = S::Code;
                }
                out.push(keep(ch));
                i += 1;
            }
            S::Line => {
                if ch == '\n' {
                    state = S::Code;
                    out.push(ch);
                } else {
                    out.push(' ');
                }
                i += 1;
            }
            S::Block => {
                if ch == '*' && next == Some('/') {
                    state = S::Code;
                    out.push_str("  ");
                    i += 2;
                } else {
                    out.push(keep(ch));
                    i += 1;
                }
            }
            S::Template => {
                if ch == '\\' {
                    out.push_str(if next.is_none() { " " } else { "  " });
                    i += 2;
                    continue;
                }
                // `${…}` is CODE. Blanking it hid every banned call written
                // inside a substitution — `` `${it.only(x)}` `` read as string
                // content. That is a MISSED warning, which is the direction
                // this hook must never fail in.
                if ch == '$' && next == Some('{') {
                    subst.push(0);
                    state = S::Code;
                    prev_significant = Some('{');
                    word.clear();
                    out.push_str("${");
                    i += 2;
                    continue;
                }
                if ch == '`' {
                    state = S::Code;
                    prev_significant = Some('`');
                    word.clear();
                    out.push(ch);
                    i += 1;
                    continue;
                }
                out.push(keep(ch));
                i += 1;
            }
            _ => {
                if ch == '\\' {
                    // Blank the escape AND what it escapes, so \" never closes.
                    out.push_str(if next.is_none() { " " } else { "  " });
                    i += 2;
                    continue;
                }
                let closes = matches!((state, ch), (S::Single, '\'') | (S::Double, '"'));
                if closes {
                    state = S::Code;
                    out.push(ch);
                } else {
                    out.push(keep(ch));
                }
                i += 1;
            }
        }
    }
    out
}

fn is_searchable(file: &str) -> bool {
    let f = file.rsplit('/').next().unwrap_or(file);
    [".js", ".jsx", ".ts", ".tsx", ".vue"]
        .iter()
        .any(|e| f.ends_with(e))
}

pub fn run(hook_name: &str, _args: &[std::ffi::OsString]) -> Outcome {
    // This file necessarily NAMES every term it bans, so it must never flag
    // itself. Compare on the file STEM against the hook name we were invoked
    // as: the hook is checked from two layouts — installed at .git/hooks/<name>
    // and as source at templates/hooks/<name> — and a path-relative comparison
    // never matched from the second, which once made this very file
    // uncommittable.
    //
    // NOT argv[0]: that is now the `amont` binary, so deriving the name from
    // it excluded nothing and the hook flagged its own source. Caught by the
    // existing suite.
    let stem_matches_self = |file: &str| {
        let base = file.rsplit('/').next().unwrap_or(file);
        let stem = base.split_once('.').map(|(s, _)| s).unwrap_or(base);
        stem == hook_name
    };

    let mut found_any = false;
    for term in &TERMS {
        let arg = format!("-G{}", term.prefilter);
        let Some(out) =
            git::stdout_paths(&["diff", "--cached", &arg, "--diff-filter=d", "--name-only"])
        else {
            continue;
        };
        let matches: Vec<&str> = out
            .iter()
            .map(String::as_str)
            .filter(|f| is_searchable(f))
            .filter(|f| !stem_matches_self(f))
            .filter(|file| {
                match git::stdout(&["show", &format!(":{file}")]) {
                    // Unreadable (binary, or vanished between the two git
                    // calls): keep the prefilter's verdict rather than
                    // silently clearing it.
                    None => true,
                    Some(content) => (term.matches)(&blank_non_code(&content)),
                }
            })
            .collect();

        if !matches.is_empty() {
            if !found_any {
                crate::say!("  {} Unwanted terms found", error_sign().trim());
            }
            found_any = true;
            crate::say!(
                "    The following files contains '{}' in them:",
                highlight(term.label)
            );
            for m in matches {
                crate::say!("    - {}", highlight(m));
            }
        }
    }
    if found_any {
        return Outcome::Failed;
    }
    crate::say!("  {} No unwanted terms were found", valid_sign().trim());
    Outcome::Passed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catches_the_banned_forms() {
        assert!(call_of("fit('x', () => {})", "fit"));
        assert!(call_of("  fit (", "fit"));
        assert!(call_of("fdescribe('x')", "fdescribe"));
        assert!(bare_debugger("  debugger;"));
        assert!(bare_debugger("debugger"));
        assert!(focused_suite("describe.skip('x')"));
        assert!(focused_suite("it.only('x')"));
        assert!(focused_suite("context.skip('x')"));
    }

    /// The false positives that forced the two-stage design.
    #[test]
    fn leaves_lookalikes_alone() {
        assert!(!call_of("profit(", "fit")); // preceded by a word char
        assert!(!call_of("layout.fit(", "fit")); // preceded by a dot
        assert!(!bare_debugger("debuggerish")); // trailing guard
        assert!(!bare_debugger("x.debugger")); // preceded by a dot
        assert!(!focused_suite("describe.skipIf(cond)")); // vitest's real API
        assert!(!focused_suite("it.onlyWhen(x)"));
    }

    #[test]
    fn blanks_comments_and_strings_keeping_layout() {
        let src = "a\n// debugger;\nb";
        let out = blank_non_code(src);
        assert_eq!(out.len(), src.len(), "length must be preserved");
        assert_eq!(out.lines().count(), src.lines().count());
        assert!(!bare_debugger(&out), "a term in a comment is discussion");

        assert!(!bare_debugger(&blank_non_code("const s = 'debugger';")));
        assert!(!bare_debugger(&blank_non_code("const s = `debugger`;")));
        assert!(!call_of(&blank_non_code("/* fit( */"), "fit"));
    }

    #[test]
    fn an_escape_never_closes_a_string() {
        // If \" closed the run, the trailing code would be scanned as code.
        let out = blank_non_code(r#"const s = "a\"b"; debugger;"#);
        assert!(
            bare_debugger(&out),
            "real code after the string must survive"
        );
        assert!(!call_of(&blank_non_code(r#"const s = "a\"fit(";"#), "fit"));
    }

    /// The bug the tokenizer exists for, with the input that ACTUALLY triggers
    /// it: an escaped slash immediately before the closing one puts two `/`
    /// characters next to each other in the stream, which the old blanker read
    /// as a line comment — blanking the rest of the line, so the real
    /// `debugger;` after it went unreported.
    ///
    /// `/a\/b/` does NOT trigger it (the slashes are not adjacent); my first
    /// version of this test used that and passed against both implementations,
    /// proving nothing. Differentially confirmed: old exits 0 on these, new
    /// exits 1.
    #[test]
    fn an_escaped_slash_before_the_terminator_no_longer_swallows_the_line() {
        for src in [
            r"const re = /a\//; debugger;",
            r"const re = /\//; debugger;",
        ] {
            assert!(
                bare_debugger(&blank_non_code(src)),
                "code after the regex must still be scanned: {src}"
            );
        }
        // and the same shape with nothing to find must stay quiet
        assert!(!bare_debugger(&blank_non_code(
            r"const re = /a\//; const ok = 1;"
        )));
    }

    /// The dangerous direction: a regex mistaken for division would have its
    /// contents scanned, reporting a violation that does not exist.
    #[test]
    fn terms_inside_a_regex_literal_are_not_violations() {
        for src in [
            r"const re = /it\.only/;",
            r"if (x) { const r = /debugger/; }",
            r"foo(/fdescribe\(/);",
            r"return /describe\.skip/;",
            r"const r = /[/]debugger/;", // `/` inside a class does not end it
        ] {
            let b = blank_non_code(src);
            assert!(!bare_debugger(&b), "false alarm: {src}");
            assert!(!focused_suite(&b), "false alarm: {src}");
            assert!(!call_of(&b, "fdescribe"), "false alarm: {src}");
        }
    }

    /// Division must stay division, or the blanker eats real code.
    #[test]
    fn division_is_not_treated_as_a_regex() {
        let src = "const x = a / b; debugger;";
        assert!(bare_debugger(&blank_non_code(src)));
        let src2 = "const x = (a + b) / c; debugger;";
        assert!(bare_debugger(&blank_non_code(src2)));
    }

    #[test]
    fn an_unterminated_regex_does_not_blank_the_rest_of_the_file() {
        let src = "const r = /oops
debugger;";
        assert!(bare_debugger(&blank_non_code(src)));
    }

    #[test]
    fn blanking_still_preserves_length_and_lines() {
        let src = "const re = /a\\/b/;\ndebugger;\n// x\n";
        let out = blank_non_code(src);
        assert_eq!(out.len(), src.len());
        assert_eq!(out.lines().count(), src.lines().count());
    }

    #[test]
    fn only_js_like_files_are_searched() {
        for f in ["a.js", "a.jsx", "a.ts", "a.tsx", "a.vue", "dir/b.ts"] {
            assert!(is_searchable(f), "{f}");
        }
        for f in ["a.rs", "a.md", "a.json", "README"] {
            assert!(!is_searchable(f), "{f}");
        }
    }
}

#[cfg(test)]
mod template_substitutions {
    use super::*;

    /// `${…}` inside a template literal is CODE, not string content. Blanking
    /// it hid every banned call written in a substitution — a MISSED warning,
    /// which is the direction this hook must never fail in.
    #[test]
    fn a_substitution_is_code() {
        let b = blank_non_code("const s = `${fit(1)}`;");
        assert!(call_of(&b, "fit"), "blanked to {b:?}");
    }

    /// A stack, not a flag: substitutions nest. Before the fix this case
    /// reported correctly by ACCIDENT — the scanner lost track and left the
    /// inner text unblanked, which happened to expose the call.
    #[test]
    fn substitutions_nest() {
        let b = blank_non_code("const s = `${`${fit(1)}`}`;");
        assert!(call_of(&b, "fit"), "blanked to {b:?}");
        assert!(
            b.contains("${`${fit(1)}`}"),
            "nesting must be tracked, not merely survived: {b:?}"
        );
    }

    /// Braces INSIDE the substitution must not close it early.
    #[test]
    fn braces_inside_a_substitution_do_not_close_it() {
        let b = blank_non_code("const s = `${ {a: 1}.a }` + 'fit(';");
        assert!(
            !call_of(&b, "fit"),
            "the string literal must stay blanked: {b:?}"
        );
        let b2 = blank_non_code("const s = `${ {a: 1}.a } ${fit(2)}`;");
        assert!(call_of(&b2, "fit"), "blanked to {b2:?}");

        // The discriminating case for DEPTH COUNTING. Treating the first `}` as
        // the end of the substitution puts the rest of it back into template
        // text, so the call after the object literal is blanked and missed.
        // Popping unconditionally passes every other case here.
        let b3 = blank_non_code("const s = `${ {a: 1} && fit(2) }`;");
        assert!(
            call_of(&b3, "fit"),
            "a `}}` closing a nested object must not end the substitution: {b3:?}"
        );
    }

    /// Template TEXT is still string content, and an escaped `\${` is text too.
    #[test]
    fn template_text_is_still_blanked() {
        assert!(!call_of(&blank_non_code("const s = `fit(`;"), "fit"));
        assert!(
            !call_of(&blank_non_code(r#"const s = `\${fit(1)}`;"#), "fit"),
            "an escaped dollar does not open a substitution"
        );
    }

    /// Comments, strings and regexes inside a substitution are handled by the
    /// ordinary code path, so they must still be blanked there.
    #[test]
    fn nested_constructs_inside_a_substitution() {
        assert!(!call_of(
            &blank_non_code("const s = `${/* fit(1) */ x}`;"),
            "fit"
        ));
        assert!(!call_of(
            &blank_non_code(r#"const s = `${"fit("}`;"#),
            "fit"
        ));
        assert!(!call_of(
            &blank_non_code(r"const s = `${/fit\(/.test(y)}`;"),
            "fit"
        ));
    }

    /// An unbalanced `}` in ordinary code must not be mistaken for the end of a
    /// substitution that was never opened.
    #[test]
    fn a_stray_brace_in_code_is_harmless() {
        let b = blank_non_code("function f() { return 1; }\nfit(() => {});");
        assert!(call_of(&b, "fit"), "blanked to {b:?}");
    }
}
