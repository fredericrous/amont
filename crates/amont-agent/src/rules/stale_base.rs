//! `stale-base` — a new branch or worktree started from a checkout that
//! `origin` has moved past.
//!
//! The session-opening notice (see `stale`) says the checkout is behind. This
//! rule catches the moment that gap is about to be inherited: `git worktree
//! add`, `git checkout -b`, `git switch -c` with no start point, or a local
//! one, while `origin/main` is ahead of it. Everything built on that branch
//! is then built against code that no longer exists on the remote, and the
//! first sign is a rebase conflict — or a feature that turns out to have
//! already landed.
//!
//! ## What is not a fire
//!
//! A start point on a remote (`origin/main`, `forgejo/main`,
//! `refs/remotes/…`) is the deliberate form; it is what the notice recommends,
//! and firing on it would make the remedy trip the rule. `--track`/`-t` names
//! a remote branch by construction. `--detach` creates no branch. Checking out
//! an EXISTING branch into a worktree (`git worktree add ../x feat/thing`)
//! creates nothing from `HEAD` and is not judged.
//!
//! ## `confirm` is where the fact lives
//!
//! `examine` sees only the command, and a checkout that is exactly at
//! `origin/main` starts branches from `HEAD` all day without harm. So the
//! rule fires on shape and `confirm` measures the distance — refreshing
//! `origin/main` first, on the same throttle and budget as the session
//! notice — and stays silent unless the start point is actually behind.

use crate::rules::{Confirmed, Context, Evidence, Finding, Rule, Stance, Trend};
use crate::shell::{Parsed, Simple};

pub const RULE: Rule = Rule {
    id: "stale-base",
    // Ships advising rather than observing: it refuses nothing, it speaks only
    // when `confirm` has measured a real gap, and the failure it names is one
    // no correcting loop can see — nothing fails when you build on stale code.
    // The shape itself is falling on its own (24 per thousand in early July,
    // ~1 by August, as worktrees-from-origin became the habit); the stance is
    // priced on the cost of a miss, not on the rate.
    default_stance: Stance::Advise,
    evidence: Evidence {
        per_1000: 1.0,
        measured: "2026-08-24",
        trend: Trend::Improving,
    },
    examine,
    confirm: Some(confirm),
};

/// What a matching command is about to do.
pub struct Creation {
    /// The start point as written, or `None` for `HEAD`.
    pub start: Option<String>,
}

fn examine(parsed: &Parsed) -> Option<Finding> {
    let (cmd, creation) = detect(parsed)?;
    let from = creation.start.clone().unwrap_or_else(|| "HEAD".to_string());
    Some(Finding {
        reason: format!(
            "this creates a branch from `{from}`, and a branch inherits whatever its \
             start point is missing — every commit origin/main has gained since the \
             last fetch stays invisible on it."
        ),
        remedy: "Start it from the remote instead: `git fetch origin && git worktree \
                 add <path> -b <name> origin/main` (or `git switch -c <name> \
                 origin/main`)."
            .to_string(),
        span: cmd.at..cmd.end,
    })
}

/// The first clause that creates a branch from a local start point.
pub fn detect(parsed: &Parsed) -> Option<(&Simple, Creation)> {
    for cmd in parsed.clauses() {
        if cmd.program() != Some("git") {
            continue;
        }
        if cmd.is_dry_run() {
            continue;
        }
        let creation = match cmd.subcommand()? {
            "worktree" => worktree_add(cmd),
            "checkout" => checkout_or_switch(cmd, &["-b", "-B"], &[]),
            "switch" => checkout_or_switch(cmd, &["-c", "-C"], &["--create", "--force-create"]),
            _ => None,
        };
        if let Some(c) = creation {
            return Some((cmd, c));
        }
    }
    None
}

/// `git worktree add [-b|-B <name>] <path> [<commit-ish>]`.
///
/// With no `-b` and no commit-ish, git creates a branch named after the path,
/// from `HEAD` — the quietest form of the mistake, and the most common one.
fn worktree_add(cmd: &Simple) -> Option<Creation> {
    let mut words = cmd
        .words
        .iter()
        .skip_while(|w| w.text != "worktree")
        .skip(1);
    if words.next().map(|w| w.text.as_str()) != Some("add") {
        return None;
    }
    if cmd.has_flag("--detach") {
        return None;
    }
    let mut named = false;
    let mut positional: Vec<&str> = Vec::new();
    let mut words = words.peekable();
    while let Some(w) = words.next() {
        let t = w.text.as_str();
        if w.quoted {
            positional.push(t);
            continue;
        }
        if t == "-b" || t == "-B" {
            named = true;
            let _ = words.next();
            continue;
        }
        if t.starts_with('-') {
            continue;
        }
        positional.push(t);
    }
    match (named, positional.as_slice()) {
        // `-b name path`: a start point after the path, or HEAD.
        (true, [_path]) => Some(Creation { start: None }),
        (true, [_path, start, ..]) => local(start),
        // `git worktree add ../x` — a new branch from HEAD, named by the path.
        (false, [_path]) => Some(Creation { start: None }),
        // `git worktree add ../x <existing>`: checks out, creates nothing.
        _ => None,
    }
}

/// `git checkout -b <name> [<start>]` and `git switch -c <name> [<start>]`.
fn checkout_or_switch(cmd: &Simple, shorts: &[&str], longs: &[&str]) -> Option<Creation> {
    if cmd.has_flag("--track") || cmd.has_short('t') || cmd.has_flag("--detach") {
        return None;
    }
    let sub = cmd.subcommand()?;
    let mut words = cmd
        .words
        .iter()
        .skip_while(|w| w.text != sub)
        .skip(1)
        .peekable();
    let mut creating = false;
    let mut positional: Vec<&str> = Vec::new();
    while let Some(w) = words.next() {
        let t = w.text.as_str();
        if w.quoted {
            positional.push(t);
            continue;
        }
        if shorts.contains(&t) || longs.contains(&t) {
            creating = true;
            let _ = words.next(); // the new branch's name
            continue;
        }
        if t.starts_with('-') {
            continue;
        }
        positional.push(t);
    }
    if !creating {
        return None;
    }
    match positional.as_slice() {
        [] => Some(Creation { start: None }),
        [start, ..] => local(start),
    }
}

/// Remote names common enough to recognise without asking git. `examine` is
/// pure, so it cannot list this repository's remotes; `confirm` does, and
/// catches any `<remote>/<branch>` this list does not.
const REMOTE_NAMES: &[&str] = &[
    "origin", "upstream", "forgejo", "gitea", "github", "gitlab", "fork",
];

/// A start point is judged only when it is local. A remote-tracking ref is
/// the remedy, not the mistake.
fn local(start: &str) -> Option<Creation> {
    if start.starts_with("refs/remotes/") || start == "FETCH_HEAD" {
        return None;
    }
    if let Some((remote, _)) = start.split_once('/') {
        if REMOTE_NAMES.contains(&remote) {
            return None;
        }
    }
    Some(Creation {
        start: Some(start.to_string()),
    })
}

/// The command only has the shape; whether the start point is actually behind
/// is a fact about the repository, measured here.
fn confirm(ctx: &Context, _f: &Finding) -> Confirmed {
    let Some((_, creation)) = detect(ctx.parsed) else {
        return Confirmed::No("the command no longer matches");
    };
    let from = creation.start.unwrap_or_else(|| "HEAD".to_string());
    // `feat/x` and `myremote/x` look the same to `examine`. Ask git which.
    if from.contains('/')
        && amont_runtime::git::succeeds_in(
            ctx.cwd,
            &[
                "rev-parse",
                "-q",
                "--verify",
                &format!("refs/remotes/{from}"),
            ],
        )
    {
        return Confirmed::No("the start point is a remote-tracking ref");
    }
    match crate::stale::measure(ctx.cwd, &from) {
        Some(d) if d.behind > 0 => Confirmed::Yes,
        Some(_) => Confirmed::No("the start point is not behind origin"),
        None => Confirmed::No("nothing to compare against"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::lex;

    fn start_of(command: &str) -> Option<Option<String>> {
        detect(&lex(command)).map(|(_, c)| c.start)
    }

    #[test]
    fn a_worktree_with_no_start_point_is_from_head() {
        assert_eq!(start_of("git worktree add ../x -b feat/y"), Some(None));
        assert_eq!(start_of("git worktree add -b feat/y ../x"), Some(None));
        assert_eq!(start_of("git worktree add ../x"), Some(None));
    }

    #[test]
    fn a_local_start_point_is_named() {
        assert_eq!(
            start_of("git worktree add ../x -b feat/y main"),
            Some(Some("main".into()))
        );
        assert_eq!(
            start_of("git checkout -b feat/y develop"),
            Some(Some("develop".into()))
        );
    }

    #[test]
    fn a_remote_start_point_is_the_remedy_not_the_mistake() {
        for c in [
            "git fetch origin -q && git worktree add ../x -b feat/y origin/main",
            "git worktree add -b feat/y ../x origin/main",
            "git checkout -b feat/y origin/main",
            "git switch -c feat/y origin/main",
            "git switch -c feat/y upstream/main",
            "git fetch forgejo main -q && git checkout -B fix/x forgejo/main 2>&1 | tail -1",
            "git checkout -b feat/y --track origin/feat/y",
            "git checkout -t origin/feat/y",
        ] {
            assert_eq!(start_of(c), None, "{c}");
        }
    }

    #[test]
    fn checking_out_an_existing_branch_creates_nothing() {
        for c in [
            "git worktree add ../x feat/existing",
            "git worktree add --detach ../x",
            "git checkout main",
            "git switch feat/y",
            "git worktree list",
            "git worktree remove ../x",
            "git worktree add --dry-run ../x",
        ] {
            assert_eq!(start_of(c), None, "{c}");
        }
    }

    #[test]
    fn switch_and_checkout_create_forms() {
        assert_eq!(start_of("git switch -c feat/y"), Some(None));
        assert_eq!(start_of("git switch --create feat/y"), Some(None));
        assert_eq!(start_of("git checkout -B feat/y"), Some(None));
    }
}
