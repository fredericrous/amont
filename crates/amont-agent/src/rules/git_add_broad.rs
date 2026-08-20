//! `git-add-broad` — staging everything rather than what was worked on.
//!
//! `git add -A`, `-u` and `.` stage whatever happens to be dirty, which in a
//! shared checkout includes work that is not this task's. `git add` is additive
//! and un-stages nothing, so a broad add earlier in a session quietly widens
//! every commit after it.
//!
//! Improving on its own — 42.5 per thousand in early July, 5.4 by mid-August —
//! so it ships observing and is expected to stay there.
//!
//! ## Scoped is not broad
//!
//! `git add -A packages/dbt-duckdb/` is the deliberate, reviewed form: the flag
//! is broad but the pathspec is not. Firing on it would make the rule noise,
//! and the corpus is full of exactly that shape.

use crate::rules::{Evidence, Finding, Rule, Stance, Trend};
use crate::shell::Parsed;

pub const RULE: Rule = Rule {
    id: "git-add-broad",
    default_stance: Stance::Observe,
    evidence: Evidence {
        per_1000: 5.4,
        measured: "2026-08-20",
        trend: Trend::Improving,
    },
    examine,
    confirm: None,
};

fn examine(parsed: &Parsed) -> Option<Finding> {
    for cmd in parsed.clauses() {
        if cmd.program() != Some("git") || cmd.subcommand() != Some("add") {
            continue;
        }
        // operands()[0] is `add`; anything after it is a pathspec.
        let paths: Vec<&str> = cmd
            .operands()
            .iter()
            .skip(1)
            .map(|w| w.text.as_str())
            .collect();
        let dot = paths.iter().any(|p| *p == "." || *p == "./");
        let broad = cmd.has_flag("--all")
            || cmd.has_flag("--update")
            || cmd.has_short('A')
            || cmd.has_short('u');
        if !broad && !dot {
            continue;
        }
        // A pathspec other than `.` scopes the add. That is the reviewed form.
        if paths.iter().any(|p| *p != "." && *p != "./") {
            continue;
        }
        // `git add -p` is interactive: the human sees every hunk.
        if cmd.has_short('p') || cmd.has_flag("--patch") {
            continue;
        }
        return Some(Finding {
            reason: "this stages every modified file in the tree, not the files this \
                     change is about — and `git add` is additive, so anything staged \
                     earlier in the session stays staged too."
                .to_string(),
            remedy: "Name the paths, or scope the flag to a directory \
                     (`git add -A packages/thing/`). Run `git status` before \
                     committing to see what is actually staged."
                .to_string(),
            span: cmd.at..cmd.end,
        });
    }
    None
}
