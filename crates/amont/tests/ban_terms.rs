//! pre-commit-ban-terms, ported from its 22-case zsh suite.

mod common;
use common::Repo;

fn check_file(name: &str, src: &str) -> bool {
    let r = Repo::new();
    r.stage(name, src);
    r.hook("pre-commit-ban-terms", &[]).passed()
}

fn check(src: &str) -> bool {
    check_file("f.ts", src)
}

#[test]
fn rejects_the_banned_forms() {
    for src in [
        "fdescribe('x', () => {});\n",
        "fit('x', () => {});\n",
        "debugger;\n",
        "describe.skip('x');\n",
        "it.only('x');\n",
        "context.skip('x');\n",
    ] {
        assert!(!check(src), "should have been rejected: {src:?}");
    }
}

/// The false positives that forced the two-stage design. `describe.skipIf` is
/// vitest's real conditional API; `profit(` and `layout.fit(` merely contain
/// the banned identifier.
#[test]
fn accepts_the_lookalikes() {
    for src in [
        "describe('x', () => {});\n",
        "describe.skipIf(cond)('x');\n",
        "it.skipIf(cond)('x');\n",
        "describe.runIf(cond)('x');\n",
        "const x = profit(1);\n",
        "layout.fit(1);\n",
        "const d = debuggerUtils();\n",
    ] {
        assert!(check(src), "should have been accepted: {src:?}");
    }
}

/// A term NAMED in a comment or a string is discussion, not code.
#[test]
fn accepts_terms_in_comments_and_literals() {
    for src in [
        "// debugger;\n",
        "/* fit( */\n",
        "/** jsdoc mentioning debugger */\n",
        "const s = 'debugger';\n",
        "const t = `it.only`;\n",
    ] {
        assert!(check(src), "should have been accepted: {src:?}");
    }
}

#[test]
fn a_comment_does_not_excuse_a_real_violation_on_another_line() {
    assert!(!check("// mentions debugger\nconst x = 1;\ndebugger;\n"));
}

/// The tokenizer added in #29: an escaped slash immediately before the
/// terminator used to read as a line comment, blanking the rest of the line so
/// a real violation after it went unreported.
#[test]
fn a_regex_no_longer_hides_code_after_it() {
    assert!(!check(r"const re = /a\//; debugger;"));
    assert!(!check(r"const re = /\//; debugger;"));
}

/// The dangerous direction: a regex mistaken for division would have its
/// contents scanned and report a violation that does not exist.
#[test]
fn terms_inside_a_regex_are_not_violations() {
    assert!(check(r"const re = /it\.only/;"));
    assert!(check(r"const re = /debugger/;"));
}

#[test]
fn division_is_still_division() {
    assert!(!check("const x = a / b; debugger;"));
}

/// Removing a line containing a banned term is not committing one — `-G`
/// matches removed lines as readily as added ones.
#[test]
fn removing_a_violation_is_not_committing_one() {
    let r = Repo::new();
    r.stage("f.ts", "debugger;\n");
    r.commit("test: seed a violation");
    r.stage("f.ts", "const ok = 1;\n");
    assert!(r.hook("pre-commit-ban-terms", &[]).passed());
}

/// Only the languages the terms declare are searched.
#[test]
fn other_file_types_are_ignored() {
    let r = Repo::new();
    r.stage("notes.md", "debugger;\ndbg!(x)\nbreakpoint()\n");
    assert!(r.hook("pre-commit-ban-terms", &[]).passed());
}

/// A term stays in its own language: `debugger` is a fine Rust identifier,
/// and `dbg` a fine JS one.
#[test]
fn a_term_does_not_cross_languages() {
    assert!(check_file("f.rs", "let debugger = 1;\n"));
    assert!(check_file("f.ts", "const dbg = (x) => x; dbg!(1);\n"));
    assert!(check_file("f.py", "debugger = 1\n"));
}

#[test]
fn rust_debug_leftovers_are_rejected() {
    for src in [
        "fn main() { dbg!(1); }\n",
        "fn main() { std::dbg!(1); }\n",
        "fn f() -> u32 { dbg!(compute()) }\n",
    ] {
        assert!(
            !check_file("f.rs", src),
            "should have been rejected: {src:?}"
        );
    }
}

/// A term named in Rust comments, strings, or raw strings is discussion.
#[test]
fn rust_prose_and_literals_are_accepted() {
    for src in [
        "// dbg!(x) is banned here\nfn f() {}\n",
        "let s = \"dbg!(x)\";\n",
        "let s = r#\"dbg!(\"x\")\"#;\n",
        "/* /* nested */ dbg!(1) */\nfn f() {}\n",
        "fn f<'a>(x: &'a str) -> &'a str { x }\n",
    ] {
        assert!(
            check_file("f.rs", src),
            "should have been accepted: {src:?}"
        );
    }
}

/// The char literal that would open a phantom string if misread: code after
/// `'"'` must still be scanned.
#[test]
fn a_quote_char_literal_does_not_hide_rust_code() {
    assert!(!check_file("f.rs", "let c = '\"'; dbg!(1);\n"));
}

#[test]
fn python_debug_leftovers_are_rejected() {
    for src in [
        "breakpoint()\n",
        "import pdb; pdb.set_trace()\n",
        "import ipdb; ipdb.set_trace()\n",
        "s = f\"{breakpoint()}\"\n", // an f-string interpolation is code
    ] {
        assert!(
            !check_file("f.py", src),
            "should have been rejected: {src:?}"
        );
    }
}

#[test]
fn python_prose_and_literals_are_accepted() {
    for src in [
        "# breakpoint() lives here\nx = 1\n",
        "s = 'breakpoint()'\n",
        "def f():\n    \"\"\"docs mentioning pdb.set_trace()\"\"\"\n",
        "s = f\"breakpoint( {x}\"\n", // f-string TEXT is still a string
        "self.breakpoint(1)\n",       // somebody else's API
        "set_trace()\n",              // bare, no module
    ] {
        assert!(
            check_file("f.py", src),
            "should have been accepted: {src:?}"
        );
    }
}

/// End to end, through stage 1's `git diff -G` prefilter as well as the
/// blanker: a banned call inside a template substitution is real code and must
/// be rejected. This was silently accepted before the tokenizer fix.
#[test]
fn a_banned_call_inside_a_template_substitution_is_rejected() {
    assert!(!check("const s = `${fit(1)}`;\n"), "substitution is code");
    assert!(
        !check("const s = `${`${it.only(1)}`}`;\n"),
        "nested substitution is code"
    );
    assert!(
        !check("const s = `${ {a: 1} && fit(2) }`;\n"),
        "a brace inside the substitution must not end it"
    );
}

/// The other direction, which matters more: template TEXT is still a string,
/// so the stricter blanker must not start alarming on prose.
#[test]
fn template_text_is_still_not_code() {
    for src in [
        "const s = `fit(`;\n",
        "const s = `run debugger;`;\n",
        "const s = `see describe.only in the docs`;\n",
        "const s = `\\${fit(1)}`;\n",
    ] {
        assert!(check(src), "should have been accepted: {src:?}");
    }
}
