//! pre-commit-ban-terms, ported from its 22-case zsh suite.
//!
//! Every case goes through the hook as a commit would: stage 1's `git diff -G`
//! prefilter, then the blanker and matcher on the staged content. The cases are
//! staged together, one file each, so one repository and one hook run cover
//! them all; the blanker's own edge cases are unit tests beside it.

use crate::common;
use common::Repo;

/// Each of these must be reported, at this line. `ext` picks the language.
const REJECTED: &[(&str, &str, u32)] = &[
    ("ts", "fdescribe('x', () => {});\n", 1),
    ("ts", "fit('x', () => {});\n", 1),
    ("ts", "debugger;\n", 1),
    ("ts", "describe.skip('x');\n", 1),
    ("ts", "it.only('x');\n", 1),
    ("ts", "context.skip('x');\n", 1),
    // a comment does not excuse a real violation on another line
    ("ts", "// mentions debugger\nconst x = 1;\ndebugger;\n", 3),
    // an escaped slash before a regex's end once read as `//` and hid the rest
    ("ts", r"const re = /a\//; debugger;", 1),
    ("ts", r"const re = /\//; debugger;", 1),
    ("ts", "const x = a / b; debugger;", 1),
    // a substitution is code, nested or holding braces
    ("ts", "const s = `${fit(1)}`;\n", 1),
    ("ts", "const s = `${`${it.only(1)}`}`;\n", 1),
    ("ts", "const s = `${ {a: 1} && fit(2) }`;\n", 1),
    ("rs", "fn main() { dbg!(1); }\n", 1),
    ("rs", "fn main() { std::dbg!(1); }\n", 1),
    ("rs", "fn f() -> u32 { dbg!(compute()) }\n", 1),
    // `'"'` must not open a phantom string
    ("rs", "let c = '\"'; dbg!(1);\n", 1),
    ("py", "breakpoint()\n", 1),
    ("py", "import pdb; pdb.set_trace()\n", 1),
    ("py", "import ipdb; ipdb.set_trace()\n", 1),
    ("py", "s = f\"{breakpoint()}\"\n", 1), // an f-string interpolation is code
];

/// None of these may be reported: lookalikes, terms named in comments and
/// literals, terms in another language's file, and files no term scans.
const ACCEPTED: &[(&str, &str)] = &[
    ("ts", "describe('x', () => {});\n"),
    // vitest's real conditional API, and identifiers that merely contain a term
    ("ts", "describe.skipIf(cond)('x');\n"),
    ("ts", "it.skipIf(cond)('x');\n"),
    ("ts", "describe.runIf(cond)('x');\n"),
    ("ts", "const x = profit(1);\n"),
    ("ts", "layout.fit(1);\n"),
    ("ts", "const d = debuggerUtils();\n"),
    ("ts", "// debugger;\n"),
    ("ts", "/* fit( */\n"),
    ("ts", "/** jsdoc mentioning debugger */\n"),
    ("ts", "const s = 'debugger';\n"),
    ("ts", "const t = `it.only`;\n"),
    // a regex mistaken for division would have its contents scanned
    ("ts", r"const re = /it\.only/;"),
    ("ts", r"const re = /debugger/;"),
    // template TEXT is still a string
    ("ts", "const s = `fit(`;\n"),
    ("ts", "const s = `run debugger;`;\n"),
    ("ts", "const s = `see describe.only in the docs`;\n"),
    ("ts", "const s = `\\${fit(1)}`;\n"),
    ("ts", "const dbg = (x) => x; dbg!(1);\n"),
    ("rs", "let debugger = 1;\n"),
    ("rs", "// dbg!(x) is banned here\nfn f() {}\n"),
    ("rs", "let s = \"dbg!(x)\";\n"),
    ("rs", "let s = r#\"dbg!(\"x\")\"#;\n"),
    ("rs", "/* /* nested */ dbg!(1) */\nfn f() {}\n"),
    ("rs", "fn f<'a>(x: &'a str) -> &'a str { x }\n"),
    ("py", "debugger = 1\n"),
    ("py", "# breakpoint() lives here\nx = 1\n"),
    ("py", "s = 'breakpoint()'\n"),
    (
        "py",
        "def f():\n    \"\"\"docs mentioning pdb.set_trace()\"\"\"\n",
    ),
    ("py", "s = f\"breakpoint( {x}\"\n"), // f-string TEXT is still a string
    ("py", "self.breakpoint(1)\n"),       // somebody else's API
    ("py", "set_trace()\n"),              // bare, no module
    ("md", "debugger;\ndbg!(x)\nbreakpoint()\n"),
];

#[test]
fn every_banned_form_is_reported_at_its_line() {
    let r = Repo::new();
    for (i, (ext, src, _)) in REJECTED.iter().enumerate() {
        r.stage(&format!("r{i}.{ext}"), src);
    }
    let run = r.hook("pre-commit-ban-terms", &[]);
    assert!(!run.passed());
    for (i, (ext, src, line)) in REJECTED.iter().enumerate() {
        assert!(
            run.says(&format!("r{i}.{ext}:{line}:")),
            "should have been rejected at line {line}: {src:?}\n{}",
            run.output()
        );
    }
}

#[test]
fn lookalikes_prose_and_other_languages_pass() {
    let r = Repo::new();
    for (i, (ext, src)) in ACCEPTED.iter().enumerate() {
        r.stage(&format!("a{i}.{ext}"), src);
    }
    let run = r.hook("pre-commit-ban-terms", &[]);
    assert!(run.passed(), "{}", run.output());
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
