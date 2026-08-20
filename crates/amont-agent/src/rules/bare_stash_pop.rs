//! `bare-stash-pop` — `git stash pop` with no explicit stash reference.
//!
//! `refs/stash` is shared across every worktree of a repository. A bare `pop`
//! or `apply` takes `stash@{0}` — whichever worktree pushed last, which in a
//! parallel-agent setup is very often not this one. The failure is quiet: the
//! wrong changes land in the wrong tree and look like your own work.
//!
//! Rare, and kept anyway: this rule is priced on the cost of one miss rather
//! than on frequency.

use crate::rules::{Confirmed, Context, Evidence, Finding, Rule, Stance, Trend};
use crate::shell::Parsed;

pub const RULE: Rule = Rule {
    id: "bare-stash-pop",
    default_stance: Stance::Observe,
    evidence: Evidence {
        per_1000: 0.2,
        measured: "2026-08-20",
        trend: Trend::Rare,
    },
    examine,
    confirm: Some(confirm),
};

fn examine(parsed: &Parsed) -> Option<Finding> {
    for cmd in parsed.clauses() {
        if cmd.program() != Some("git") || cmd.subcommand() != Some("stash") {
            continue;
        }
        let ops = cmd.operands();
        // operands()[0] is `stash` itself; the verb follows it.
        let verb = ops.get(1)?.text.as_str();
        if verb != "pop" && verb != "apply" {
            continue;
        }
        // An explicit reference makes the choice deliberate, which is all this
        // rule wants.
        if ops.iter().skip(2).any(|w| names_a_stash(&w.text)) {
            continue;
        }
        return Some(Finding {
            reason: format!(
                "`git stash {verb}` with no reference takes stash@{{0}}, and refs/stash \
                 is shared by every worktree of this repository — so it can restore \
                 another worktree's changes into this one."
            ),
            remedy: "Run `git stash list`, identify the entry that belongs to this \
                     worktree, and name it: `git stash pop 'stash@{N}'`."
                .to_string(),
            span: cmd.at..cmd.end,
        });
    }
    None
}

fn names_a_stash(t: &str) -> bool {
    t.starts_with("stash@{")
        || t.starts_with("refs/stash")
        || (t.len() >= 7 && t.bytes().all(|c| c.is_ascii_hexdigit()))
}

/// The risk is *shared* refs/stash, which is a fact about this checkout rather
/// than about the command. One `git worktree list` answers it, and only on the
/// rare occasion the rule fires.
fn confirm(ctx: &Context, _f: &Finding) -> Confirmed {
    let out = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(ctx.cwd)
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let n = String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| l.starts_with("worktree "))
                .count();
            if n > 1 {
                Confirmed::Yes
            } else {
                Confirmed::No("this repository has a single worktree")
            }
        }
        // Git would not answer. Not confirmed is always silence.
        _ => Confirmed::No("git would not list the worktrees"),
    }
}
