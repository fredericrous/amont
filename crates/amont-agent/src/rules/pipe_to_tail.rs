//! `pipe-to-tail` — a mutating command whose output is trimmed by a pipe.
//!
//! ```sh
//! git push origin main 2>&1 | tail -5      # reports 0 whatever git did
//! ```
//!
//! A pipeline's exit status is the status of its LAST command. `tail` succeeds
//! at tailing an error message, so a rejected push, a push killed by a timeout,
//! and a push that never left the machine all report success — and the trimming
//! discards the error text as well, so the failure is silent in both channels.
//! On 2026-08-20 that cost several pushes that reported exit 0 and had never
//! left the machine.
//!
//! ## Why this is the rule that needed a mechanism
//!
//! Of the habits measured across 43,242 real commands, three were already
//! correcting themselves and this one was not: 40–80 firings per thousand for
//! seven consecutive weeks, no trend. The difference is that the other three
//! are things amont eventually *punishes*, so a feedback loop forms. This one
//! makes failure look like success, so no loop can form — and amont's own
//! generated AGENTS.md block has warned about it the whole time.
//!
//! ## The exemptions are the rule
//!
//! A naive regex for this was wrong one time in five. Three of the four causes
//! are handled by the lexer (quoting, clause boundaries); the fourth is here:
//! `git tag` with no operand LISTS tags, and `git tag --sort=-v:refname | head`
//! is one of the most ordinary git commands there is.

use std::ops::Range;

use crate::rules::{Evidence, Finding, Rule, Stance, Trend};
use crate::shell::{Parsed, Simple};

pub const RULE: Rule = Rule {
    id: "pipe-to-tail",
    // The only rule that ships blocking, and the only one with the evidence to
    // justify it: seven consecutive weeks with no downward trend, while every
    // other measured habit halved. The usual ladder — observe, then advise,
    // then deny — exists to gather exactly the evidence that already exists
    // here, so walking it again would only delay a decision already made.
    //
    // It fires on roughly one Bash call in twenty-two, which is a lot for a
    // refusal. That is survivable only because the remedy is trivial (run the
    // command bare) and the refusal carries it. If it turns out to be wrong,
    // demotion is one command and takes effect immediately:
    //
    //     git config --global amont.agent.pipe-to-tail.stance observe
    default_stance: Stance::Deny,
    evidence: Evidence {
        per_1000: 62.3,
        measured: "2026-08-20",
        trend: Trend::Flat(7),
    },
    examine,
    confirm: None,
};

/// Commands that change something, by program and subcommand.
const MUTATING: &[(&str, &[&str])] = &[
    ("git", &["push", "commit", "tag"]),
    ("kubectl", &["apply", "delete", "replace"]),
    ("helm", &["install", "upgrade"]),
    ("npm", &["publish"]),
];

/// Sinks that TRIM. The defect is identical for `tee`, `less` and `jq`, and
/// they are deliberately absent: the measured rate that justifies this rule was
/// measured for exactly this set, and widening a rule in the same week it
/// graduates destroys the comparison that says whether the guard worked. The
/// backtester can price the wider form later.
const TRIMMING: &[&str] = &["tail", "head", "grep"];

/// `git tag` flags that mean "list", not "create". `--contains` matters most:
/// it takes a value, so `git tag --contains 9dd54d2` has an operand and would
/// otherwise read as a tag being created.
const TAG_LISTING: &[&str] = &[
    "-l",
    "--list",
    "-n",
    "--contains",
    "--no-contains",
    "--points-at",
    "--merged",
    "--no-merged",
    "--sort",
    "--format",
    "--column",
    "--omit-empty",
    "-d",
    "--delete",
    "-v",
    "--verify",
];

fn is_mutating(cmd: &Simple) -> bool {
    let Some(program) = cmd.program() else {
        return false;
    };
    let Some(sub) = cmd.subcommand() else {
        return false;
    };
    if !MUTATING
        .iter()
        .any(|(p, subs)| *p == program && subs.contains(&sub))
    {
        return false;
    }
    // A dry run mutates nothing. `kubectl apply --dry-run=client -o yaml | head`
    // is the standard way to render a manifest; blocking it on day one would
    // get the guard uninstalled on day one.
    if cmd.is_dry_run() {
        return false;
    }
    // `git push -n` is a dry run. (`-n` on `git commit` is `--no-verify`, which
    // is a different rule's business — hence the per-subcommand test.)
    if program == "git" && sub == "push" && cmd.has_short('n') {
        return false;
    }
    if program == "git" && sub == "tag" {
        return tag_creates(cmd);
    }
    true
}

/// Bare `git tag` lists; `git tag <name>` creates. Anything carrying a listing
/// or inspection flag is reading, whatever operands it has.
fn tag_creates(cmd: &Simple) -> bool {
    for w in &cmd.words {
        if w.quoted {
            continue;
        }
        let t = w.text.as_str();
        if TAG_LISTING
            .iter()
            .any(|f| t == *f || t.starts_with(&format!("{f}=")))
        {
            return false;
        }
    }
    // operands() excludes the program and the subcommand's own flags; the
    // subcommand word itself is the first operand, so a creating form has two.
    cmd.operands().len() > 1
}

fn examine(parsed: &Parsed) -> Option<Finding> {
    let clauses = parsed.clauses();
    for (i, cmd) in clauses.iter().enumerate() {
        // Only a command that feeds a pipe can have its status swallowed. This
        // is also what exempts `echo msg | git commit -F -`: there git is the
        // SINK, so the pipeline's status is git's status and nothing is hidden.
        if !cmd.next.is_some_and(|c| c.is_pipe()) {
            continue;
        }
        if !is_mutating(cmd) {
            continue;
        }
        let Some(sink) = pipeline_sink(clauses, i) else {
            continue;
        };
        let Some(sink_program) = sink.program() else {
            continue;
        };
        if !TRIMMING.contains(&sink_program) {
            continue;
        }
        let verb = describe(cmd);
        return Some(Finding {
            reason: format!(
                "`{verb}` pipes into `{sink_program}`, so the pipeline reports \
                 {sink_program}'s exit status, not {verb}'s. A failed, rejected or \
                 timed-out run reads as success, and the trimming discards the error \
                 text as well."
            ),
            remedy: format!(
                "Run `{verb}` on its own and read its output afterwards. Then verify \
                 the effect rather than the exit code."
            ),
            span: Range {
                start: cmd.at,
                end: sink.end.min(usize::MAX),
            },
        });
    }
    None
}

/// The last stage of the pipeline that `from` participates in.
fn pipeline_sink(clauses: &[Simple], from: usize) -> Option<&Simple> {
    let mut i = from;
    while clauses.get(i)?.next.is_some_and(|c| c.is_pipe()) {
        i += 1;
    }
    clauses.get(i)
}

fn describe(cmd: &Simple) -> String {
    match (cmd.program(), cmd.subcommand()) {
        (Some(p), Some(s)) => format!("{p} {s}"),
        (Some(p), None) => p.to_string(),
        _ => "the command".to_string(),
    }
}
