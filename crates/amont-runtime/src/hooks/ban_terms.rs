//! pre-commit-ban-terms — refuse focused/debug leftovers in staged sources.
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
//!
//! Each term carries its own language: the file extensions it scans and the
//! blanker that understands their comment and string syntax. JS/TS terms came
//! first; Rust (`dbg!`) and Python (`breakpoint()`, `pdb.set_trace()`) get the
//! same treatment — a debug leftover is a debug leftover in any language.

use crate::check::Outcome;
use crate::git;
use crate::ui::{error_sign, highlight, valid_sign};

/// Everything any term scans. The registry declares the check's scope from
/// this constant, so a term cannot gain a language without the dashboard
/// learning about it — pinned by `the_scope_is_the_union_of_the_terms`.
pub const EXTS: &[&str] = &[".js", ".jsx", ".ts", ".tsx", ".vue", ".rs", ".py"];

const JS_LIKE: &[&str] = &[".js", ".jsx", ".ts", ".tsx", ".vue"];
const RUST: &[&str] = &[".rs"];
const PYTHON: &[&str] = &[".py"];

/// A banned form: a loose `git diff -G` prefilter, the extensions it applies
/// to, the blanker for that language, and the precise matcher.
struct Term {
    label: &'static str,
    prefilter: &'static str,
    exts: &'static [&'static str],
    blank: fn(&str) -> String,
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

/// `dbg!(…)` — the macro invocation, any delimiter. `xdbg!` and a function
/// named `dbg` both pass; `std::dbg!(` is still the macro (`:` is not an
/// identifier character, so the leading guard lets it through).
fn rust_dbg(src: &str) -> bool {
    let word = "dbg";
    let mut from = 0;
    while let Some(i) = src[from..].find(word) {
        let at = from + i;
        let after = at + word.len();
        let before_ok = src[..at]
            .chars()
            .next_back()
            .map(|c| !is_ident(c))
            .unwrap_or(true);
        let rest = &src[after..];
        if before_ok
            && rest.starts_with('!')
            && matches!(rest[1..].trim_start().chars().next(), Some('(' | '[' | '{'))
        {
            return true;
        }
        from = after;
    }
    false
}

/// `pdb.set_trace(` / `ipdb.set_trace(` — the import-and-call form. The
/// leading guard excludes identifier characters only, so `x.pdb.set_trace(`
/// is still caught while `xpdb.set_trace(` matches neither needle (the `i`
/// rejects the first, the `x` the second). Bare `set_trace(` is deliberately
/// not matched: without its module it is just a method name.
fn pdb_set_trace(src: &str) -> bool {
    for needle in ["pdb.set_trace", "ipdb.set_trace"] {
        let mut from = 0;
        while let Some(i) = src[from..].find(needle) {
            let at = from + i;
            let after = at + needle.len();
            let before_ok = src[..at]
                .chars()
                .next_back()
                .map(|c| !is_ident(c))
                .unwrap_or(true);
            if before_ok && src[after..].trim_start().starts_with('(') {
                return true;
            }
            from = at + needle.len();
        }
    }
    false
}

const TERMS: [Term; 7] = [
    Term {
        label: "fit",
        prefilter: r"\s*fit\(",
        exts: JS_LIKE,
        blank: blank_non_code,
        matches: |s| call_of(s, "fit"),
    },
    Term {
        label: "fdescribe",
        prefilter: r"\s*fdescribe\(",
        exts: JS_LIKE,
        blank: blank_non_code,
        matches: |s| call_of(s, "fdescribe"),
    },
    Term {
        label: "debugger",
        prefilter: "debugger;?",
        exts: JS_LIKE,
        blank: blank_non_code,
        matches: bare_debugger,
    },
    Term {
        label: "skipOnly",
        prefilter: r"(describe|context|it)\.(skip|only)",
        exts: JS_LIKE,
        blank: blank_non_code,
        matches: focused_suite,
    },
    Term {
        label: "dbg!",
        prefilter: "dbg!",
        exts: RUST,
        blank: blank_rust,
        matches: rust_dbg,
    },
    Term {
        label: "breakpoint",
        prefilter: "breakpoint",
        exts: PYTHON,
        blank: blank_python,
        matches: |s| call_of(s, "breakpoint"),
    },
    Term {
        label: "set_trace",
        prefilter: "set_trace",
        exts: PYTHON,
        blank: blank_python,
        matches: pdb_set_trace,
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

/// Is `word` exactly a raw-string prefix? `r"…"`, `br"…"`, `cr"…"` — an
/// identifier that merely ENDS in `r` (`attr"…"` is not Rust anyway) stays an
/// ordinary word because the tracker holds the whole contiguous run.
fn is_raw_prefix(word: &str) -> bool {
    matches!(word, "r" | "br" | "cr")
}

#[derive(Clone, Copy, PartialEq)]
enum R {
    Code,
    Line,
    Block(u32),
    Str,
    Raw(u32),
}

/// Blank comments and the insides of string/char literals in RUST source,
/// preserving length and line count like `blank_non_code` does for JS.
///
/// The shapes that differ from JS and earn their handling here:
///   - block comments NEST — `/* /* */ */` is one comment, and treating the
///     first `*/` as the end would scan the tail of the comment as code
///     (a false alarm, the direction to avoid);
///   - raw strings `r"…"` / `r#"…"#` (and the `br`/`cr` variants), where `\`
///     is literal and the terminator is `"` plus the opening's `#` count;
///   - `'` is either a char literal or a lifetime. Bounded lookahead tells
///     them apart: a char literal closes within a few characters, a lifetime
///     never closes. Misreading a lifetime as a char start would blank real
///     code (a missed warning); the case that must not be missed is `'"'`,
///     which read as a lifetime opens a phantom string and blanks the rest of
///     the file. The lookahead exists for that quote.
pub fn blank_rust(src: &str) -> String {
    let b: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut state = R::Code;
    let mut word = String::new();
    let mut i = 0;
    let keep = |c: char| if c == '\n' { '\n' } else { ' ' };

    while i < b.len() {
        let ch = b[i];
        let next = b.get(i + 1).copied();
        match state {
            R::Code => {
                if ch == '/' && next == Some('/') {
                    state = R::Line;
                    out.push_str("  ");
                    i += 2;
                } else if ch == '/' && next == Some('*') {
                    state = R::Block(1);
                    out.push_str("  ");
                    i += 2;
                } else if ch == '"' {
                    state = if is_raw_prefix(&word) {
                        R::Raw(0)
                    } else {
                        R::Str
                    };
                    word.clear();
                    out.push(ch);
                    i += 1;
                } else if ch == '#' && is_raw_prefix(&word) {
                    // `r#"…"#`, possibly with more hashes. A `#` NOT followed
                    // by hashes-then-quote is ordinary code (`r#ident` is a
                    // raw identifier, `#[attr]` an attribute).
                    let mut n = 0;
                    while b.get(i + n) == Some(&'#') {
                        n += 1;
                    }
                    if b.get(i + n) == Some(&'"') {
                        for _ in 0..n {
                            out.push('#');
                        }
                        out.push('"');
                        state = R::Raw(n as u32);
                        i += n + 1;
                    } else {
                        out.push(ch);
                        i += 1;
                    }
                    word.clear();
                } else if ch == '\'' {
                    // Char literal or lifetime. After `'\` the literal closes
                    // at the first quote (only `'\\'` puts a backslash before
                    // it, and that IS the escaped char); after `'x` it closes
                    // only as `'x'`. `'\u{10FFFF}'` is the longest escape, so
                    // the lookahead is bounded.
                    let char_end = match next {
                        Some('\\') => (i + 3..(i + 14).min(b.len())).find(|&j| b[j] == '\''),
                        Some(c) if c != '\'' => {
                            if b.get(i + 2) == Some(&'\'') {
                                Some(i + 2)
                            } else {
                                None
                            }
                        }
                        _ => None,
                    };
                    match char_end {
                        Some(j) => {
                            out.push('\'');
                            for c in &b[i + 1..j] {
                                out.push(keep(*c));
                            }
                            out.push('\'');
                            i = j + 1;
                        }
                        None => {
                            // a lifetime — its name flows on as ordinary code
                            out.push('\'');
                            i += 1;
                        }
                    }
                    word.clear();
                } else {
                    if ch.is_alphanumeric() || ch == '_' {
                        word.push(ch);
                    } else {
                        word.clear();
                    }
                    out.push(ch);
                    i += 1;
                }
            }
            R::Line => {
                if ch == '\n' {
                    state = R::Code;
                    out.push(ch);
                } else {
                    out.push(' ');
                }
                i += 1;
            }
            R::Block(depth) => {
                if ch == '/' && next == Some('*') {
                    state = R::Block(depth + 1);
                    out.push_str("  ");
                    i += 2;
                } else if ch == '*' && next == Some('/') {
                    state = if depth == 1 {
                        R::Code
                    } else {
                        R::Block(depth - 1)
                    };
                    out.push_str("  ");
                    i += 2;
                } else {
                    out.push(keep(ch));
                    i += 1;
                }
            }
            R::Str => {
                if ch == '\\' {
                    // Blank the escape AND what it escapes, so `\"` never
                    // closes — but keep an escaped newline a newline, since
                    // Rust strings span lines and the line count must hold.
                    out.push(' ');
                    if let Some(n) = next {
                        out.push(keep(n));
                    }
                    i += 2;
                } else if ch == '"' {
                    state = R::Code;
                    out.push(ch);
                    i += 1;
                } else {
                    out.push(keep(ch));
                    i += 1;
                }
            }
            R::Raw(hashes) => {
                let n = hashes as usize;
                if ch == '"' && (1..=n).all(|k| b.get(i + k) == Some(&'#')) {
                    out.push('"');
                    for _ in 0..n {
                        out.push('#');
                    }
                    state = R::Code;
                    i += n + 1;
                } else {
                    out.push(keep(ch));
                    i += 1;
                }
            }
        }
    }
    out
}

/// One open Python string literal: what closes it and how its insides behave.
#[derive(Clone, Copy)]
struct PyLit {
    quote: char,
    triple: bool,
    fstr: bool,
}

#[derive(Clone, Copy)]
enum P {
    Code,
    Comment,
    Lit(PyLit),
}

/// Blank comments and the insides of string literals in PYTHON source,
/// preserving length and line count.
///
/// The shapes that earn their handling here:
///   - `#` comments and the three quote forms: `'…'`, `"…"`, and the triples;
///   - prefixes (`r`, `b`, `f`, `u` and their pairs) read from the word the
///     tracker just saw, because `f"{breakpoint()}"` interpolates CODE: like
///     the JS blanker's `${…}`, blanking it would hide a banned call — a
///     missed warning, the direction this hook must never fail in. `{{` is
///     text, and interpolations nest (`f"{f'{x}'}"`), so open ones are a
///     stack of the literals to resume, not a flag;
///   - a backslash always blanks the next character, raw or not: in Python a
///     raw string still cannot end on a lone backslash, so `r"\"…` staying
///     open mirrors the real parser;
///   - an unterminated single-line string bails back to code at the newline
///     rather than blanking the rest of the file.
pub fn blank_python(src: &str) -> String {
    let b: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut state = P::Code;
    let mut word = String::new();
    let mut i = 0;
    // Open f-string interpolations, innermost last: brace depth inside the
    // interpolation, and the literal to resume when it closes.
    let mut subst: Vec<(u32, PyLit)> = Vec::new();
    let keep = |c: char| if c == '\n' { '\n' } else { ' ' };

    while i < b.len() {
        let ch = b[i];
        let next = b.get(i + 1).copied();
        match state {
            P::Code => {
                if ch == '#' {
                    state = P::Comment;
                    out.push(' ');
                    i += 1;
                } else if ch == '\'' || ch == '"' {
                    let prefix = !word.is_empty()
                        && word.len() <= 2
                        && word.chars().all(|c| "rbfuRBFU".contains(c));
                    let fstr = prefix && word.to_ascii_lowercase().contains('f');
                    let triple = next == Some(ch) && b.get(i + 2) == Some(&ch);
                    state = P::Lit(PyLit {
                        quote: ch,
                        triple,
                        fstr,
                    });
                    word.clear();
                    if triple {
                        out.push(ch);
                        out.push(ch);
                        out.push(ch);
                        i += 3;
                    } else {
                        out.push(ch);
                        i += 1;
                    }
                } else {
                    if !subst.is_empty() {
                        if ch == '{' {
                            subst.last_mut().expect("non-empty").0 += 1;
                        } else if ch == '}' {
                            let (depth, lit) = *subst.last().expect("non-empty");
                            if depth == 0 {
                                subst.pop();
                                state = P::Lit(lit);
                                out.push(ch);
                                i += 1;
                                continue;
                            }
                            subst.last_mut().expect("non-empty").0 -= 1;
                        }
                    }
                    if ch.is_alphanumeric() || ch == '_' {
                        word.push(ch);
                    } else {
                        word.clear();
                    }
                    out.push(ch);
                    i += 1;
                }
            }
            P::Comment => {
                if ch == '\n' {
                    state = P::Code;
                    out.push(ch);
                } else {
                    out.push(' ');
                }
                i += 1;
            }
            P::Lit(lit) => {
                if ch == '\\' {
                    out.push(' ');
                    if let Some(n) = next {
                        out.push(keep(n));
                    }
                    i += 2;
                } else if lit.triple
                    && ch == lit.quote
                    && next == Some(lit.quote)
                    && b.get(i + 2) == Some(&lit.quote)
                {
                    state = P::Code;
                    out.push(ch);
                    out.push(ch);
                    out.push(ch);
                    i += 3;
                } else if !lit.triple && ch == lit.quote {
                    state = P::Code;
                    out.push(ch);
                    i += 1;
                } else if !lit.triple && ch == '\n' {
                    // unterminated — bail so the rest of the file is scanned
                    state = P::Code;
                    out.push(ch);
                    i += 1;
                } else if lit.fstr && ch == '{' && next == Some('{') {
                    out.push_str("  ");
                    i += 2;
                } else if lit.fstr && ch == '{' {
                    subst.push((0, lit));
                    state = P::Code;
                    word.clear();
                    out.push(ch);
                    i += 1;
                } else if lit.fstr && ch == '}' && next == Some('}') {
                    out.push_str("  ");
                    i += 2;
                } else {
                    out.push(keep(ch));
                    i += 1;
                }
            }
        }
    }
    out
}

fn is_searchable(file: &str, exts: &[&str]) -> bool {
    let f = file.rsplit('/').next().unwrap_or(file);
    exts.iter().any(|e| f.ends_with(e))
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
            .filter(|f| is_searchable(f, term.exts))
            .filter(|f| !stem_matches_self(f))
            .filter(|file| {
                match git::stdout(&["show", &format!(":{file}")]) {
                    // Unreadable (binary, or vanished between the two git
                    // calls): keep the prefilter's verdict rather than
                    // silently clearing it.
                    None => true,
                    Some(content) => (term.matches)(&(term.blank)(&content)),
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
    fn each_term_searches_only_its_own_language() {
        for f in ["a.js", "a.jsx", "a.ts", "a.tsx", "a.vue", "dir/b.ts"] {
            assert!(is_searchable(f, JS_LIKE), "{f}");
        }
        for f in ["a.rs", "a.py", "a.md", "a.json", "README"] {
            assert!(!is_searchable(f, JS_LIKE), "{f}");
        }
        assert!(is_searchable("src/lib.rs", RUST));
        assert!(!is_searchable("a.ts", RUST));
        assert!(is_searchable("app/main.py", PYTHON));
        assert!(!is_searchable("a.pyi", PYTHON), "stubs never execute");
    }

    /// The registry declares the check's scope from `EXTS`; a term must not
    /// scan a language the dashboard does not know about, nor the reverse.
    #[test]
    fn the_scope_is_the_union_of_the_terms() {
        let mut union: Vec<&str> = TERMS.iter().flat_map(|t| t.exts.iter().copied()).collect();
        union.sort_unstable();
        union.dedup();
        let mut declared: Vec<&str> = EXTS.to_vec();
        declared.sort_unstable();
        assert_eq!(union, declared);
    }
}

#[cfg(test)]
mod rust_terms {
    use super::*;

    #[test]
    fn catches_the_macro_call() {
        assert!(rust_dbg("dbg!(x)"));
        assert!(rust_dbg("let y = dbg! (x);"));
        assert!(rust_dbg("std::dbg!(x)"));
        assert!(rust_dbg("dbg![x]"));
        assert!(rust_dbg("dbg!{x}"));
    }

    #[test]
    fn leaves_lookalikes_alone() {
        assert!(!rust_dbg("xdbg!(1)")); // preceded by a word char
        assert!(!rust_dbg("dbg(1)")); // a function, not the macro
        assert!(!rust_dbg("debug!(x)")); // a different macro
        assert!(!rust_dbg("dbg")); // the bare word
    }

    #[test]
    fn comments_and_strings_are_discussion() {
        for src in [
            "// dbg!(x)\nlet a = 1;",
            "/* dbg!(x) */",
            "/// docs mentioning dbg!(x)\nfn f() {}",
            "let s = \"dbg!(x)\";",
            "let s = r\"dbg!(x)\";",
            "let s = r#\"dbg!(\"x\")\"#;",
            "let s = br\"dbg!(x)\";",
        ] {
            assert!(!rust_dbg(&blank_rust(src)), "false alarm: {src}");
        }
    }

    /// Block comments NEST: the first `*/` must not end `/* /* … */`, or the
    /// tail of the comment is scanned as code — a false alarm.
    #[test]
    fn block_comments_nest() {
        assert!(!rust_dbg(&blank_rust("/* /* x */ dbg!(1) */")));
        assert!(rust_dbg(&blank_rust("/* /* x */ */ dbg!(1)")));
    }

    /// A raw string's terminator is `"` plus the opening's `#` count — a mere
    /// `"` inside `r#"…"#` must not close it, and the real terminator must.
    #[test]
    fn raw_string_hashes_are_honoured() {
        assert!(!rust_dbg(&blank_rust("let s = r#\"a\"b\"#;")));
        assert!(rust_dbg(&blank_rust("let s = r#\"a\"b\"#; dbg!(1);")));
        assert!(!rust_dbg(&blank_rust("let s = r##\"a\"# dbg!(1) \"##;")));
    }

    /// The quote-shaped char literals. Read as lifetimes they would open a
    /// phantom string and blank the rest of the file — a missed warning.
    #[test]
    fn a_char_literal_holding_a_quote_does_not_open_a_string() {
        assert!(rust_dbg(&blank_rust("let c = '\"'; dbg!(1);")));
        assert!(rust_dbg(&blank_rust("let c = '\\''; dbg!(1);")));
        assert!(rust_dbg(&blank_rust("let c = '\\\\'; dbg!(1);")));
        assert!(rust_dbg(&blank_rust("let c = '\\u{7f}'; dbg!(1);")));
    }

    /// The other direction: a lifetime is code, not an open char literal, so
    /// what follows it must still be scanned.
    #[test]
    fn a_lifetime_does_not_swallow_the_line() {
        assert!(rust_dbg(&blank_rust("fn f<'a>(x: &'a str) { dbg!(x); }")));
        assert!(rust_dbg(&blank_rust("let x: &'static str = s; dbg!(x);")));
    }

    #[test]
    fn blanking_preserves_length_and_lines() {
        let src = "let s = r#\"a\"b\"#;\n// dbg!(x)\nlet c = 'y';\n";
        let out = blank_rust(src);
        assert_eq!(out.len(), src.len());
        assert_eq!(out.lines().count(), src.lines().count());
    }

    /// This file names the term it bans — in strings and comments only, which
    /// the blanker must keep blanked or the hook makes its own source
    /// uncommittable.
    #[test]
    fn the_hooks_own_source_survives_its_own_scan() {
        assert!(!rust_dbg(&blank_rust(include_str!("ban_terms.rs"))));
    }
}

#[cfg(test)]
mod python_terms {
    use super::*;

    #[test]
    fn catches_the_debug_calls() {
        assert!(call_of("breakpoint()", "breakpoint"));
        assert!(call_of("    breakpoint ()", "breakpoint"));
        assert!(pdb_set_trace("pdb.set_trace()"));
        assert!(pdb_set_trace("ipdb.set_trace()"));
        assert!(pdb_set_trace("x.pdb.set_trace()"));
    }

    #[test]
    fn leaves_lookalikes_alone() {
        assert!(!call_of("self.breakpoint()", "breakpoint")); // somebody's API
        assert!(!call_of("my_breakpoint()", "breakpoint"));
        assert!(!pdb_set_trace("xpdb.set_trace()")); // neither pdb nor ipdb
        assert!(!pdb_set_trace("set_trace()")); // bare, no module
        assert!(!pdb_set_trace("pdb.set_trace")); // not the call form
    }

    #[test]
    fn comments_and_strings_are_discussion() {
        for src in [
            "# breakpoint()\nx = 1\n",
            "s = 'breakpoint()'\n",
            "s = \"pdb.set_trace()\"\n",
            "def f():\n    \"\"\"calls breakpoint() eventually\"\"\"\n",
            "s = '''ipdb.set_trace()'''\n",
            "s = f\"breakpoint( {x}\"\n", // f-string TEXT is still a string
            "s = f\"{{breakpoint()}}\"\n", // escaped braces are text
        ] {
            let b = blank_python(src);
            assert!(!call_of(&b, "breakpoint"), "false alarm: {src}");
            assert!(!pdb_set_trace(&b), "false alarm: {src}");
        }
    }

    /// An f-string interpolation is CODE — blanking it would hide a banned
    /// call, the direction this hook must never fail in. And they nest.
    #[test]
    fn an_interpolation_is_code() {
        assert!(call_of(
            &blank_python("s = f\"{breakpoint()}\"\n"),
            "breakpoint"
        ));
        assert!(call_of(
            &blank_python("s = f\"{f'{breakpoint()}'}\"\n"),
            "breakpoint"
        ));
        // a `}` closing a nested format spec must not end the interpolation
        assert!(call_of(
            &blank_python("s = f\"{x:{w}} {breakpoint()}\"\n"),
            "breakpoint"
        ));
    }

    /// In Python a raw string still cannot end on a lone backslash: the `\"`
    /// keeps the literal open, so what looks like code after it is string.
    #[test]
    fn a_raw_string_backslash_does_not_close_early() {
        let b = blank_python("s = r\"\\\"; breakpoint()\"\n");
        assert!(!call_of(&b, "breakpoint"));
    }

    /// An unterminated single-line string bails at the newline rather than
    /// blanking the rest of the file.
    #[test]
    fn an_unterminated_string_does_not_blank_the_next_line() {
        let b = blank_python("s = 'oops\nbreakpoint()\n");
        assert!(call_of(&b, "breakpoint"));
    }

    #[test]
    fn triple_quotes_span_lines_and_close_only_on_three() {
        let b = blank_python("s = \"\"\"\ntext \" and \"\" inside\nbreakpoint()\n\"\"\"\nx = 1\n");
        assert!(!call_of(&b, "breakpoint"));
        let b2 = blank_python("s = \"\"\"doc\"\"\"\nbreakpoint()\n");
        assert!(call_of(&b2, "breakpoint"));
    }

    #[test]
    fn blanking_preserves_length_and_lines() {
        let src = "# c\ns = f\"{x} y\"\nt = '''a\nb'''\n";
        let out = blank_python(src);
        assert_eq!(out.len(), src.len());
        assert_eq!(out.lines().count(), src.lines().count());
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
