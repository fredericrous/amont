//! pre-push-pull-rebase — sync a branch with ITS OWN upstream before pushing,
//! and warn (never act) when the default branch has moved ahead.
//!
//! The hardening in the shell version is the point, and is preserved exactly:
//! an older version ran `git pull --rebase origin HEAD`, where the remote ref
//! `HEAD` resolves to the remote's DEFAULT branch — so every push silently
//! rebased onto main, autostashing uncommitted work. Hence: never touch a dirty
//! tree, rebase only onto the branch's own upstream, and abort cleanly on
//! conflict rather than leaving a half-rebased state.

use crate::check::Outcome;
use crate::git;
use crate::ui::{error_sign, highlight, valid_sign, warning_sign};

/// `ahead[[:space:]]+N,[[:space:]]*behind[[:space:]]+M` over `git status -sb`.
///
/// Both counts present means the branch and its upstream have diverged, and an
/// automatic rebase is exactly the wrong move. The COUNTS come back too, not
/// just the fact: "how far apart" is the first thing anyone wants to know, and
/// the predicate used to throw it away.
/// `word[[:space:]]+N` at the first eligible occurrence in `s` — shared by
/// [`divergence`] and [`behind_count`] so the question "does an `ahead`
/// or `behind` token here actually mean a count" has exactly one answer
/// rather than two copies that could drift.
fn count_after(s: &str, word: &str) -> Option<(u64, usize)> {
    let i = s.find(word)?;
    let after = &s[i + word.len()..];
    let trimmed = after.trim_start_matches([' ', '\t']);
    if trimmed.len() == after.len() {
        return None; // `word` must be followed by whitespace
    }
    let digits: String = trimmed.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    let consumed = i + word.len() + (after.len() - trimmed.len()) + digits.len();
    Some((digits.parse().ok()?, consumed))
}

pub fn divergence(status: &str) -> Option<(u64, u64)> {
    for line in status.lines() {
        let mut rest = line;
        while let Some((ahead, used)) = count_after(rest, "ahead") {
            let tail = &rest[used..];
            if let Some(t) = tail.strip_prefix(',') {
                if let Some((behind, _)) = count_after(t, "behind") {
                    // `behind` must be the NEXT token, not merely present later
                    // on the line.
                    if t.trim_start_matches([' ', '\t']).starts_with("behind") {
                        return Some((ahead, behind));
                    }
                }
            }
            rest = tail;
        }
    }
    None
}

/// `behind[[:space:]]+N` over `git status -sb`, present or not — UNLIKE
/// [`divergence`], which only answers when `ahead` is ALSO on the line.
///
/// `git status -sb` prints a bare `[behind M]` (no `ahead` at all) when the
/// branch has nothing of its own to push yet is missing upstream commits —
/// exactly "a sync would do something", independent of whether the branch is
/// also ahead. `divergence` cannot answer that question; it was built to
/// answer a narrower one (are the two DIVERGED, which needs both counts).
pub fn behind_count(status: &str) -> Option<u64> {
    status
        .lines()
        .find_map(|line| count_after(line, "behind").map(|(n, _)| n))
}

/// `^[[:space:]*+]+<name>$` over `git branch` output — the indentation git
/// always prints, plus the markers it puts in column 0.
///
/// `+` is not decoration: since git 2.23, a branch checked out in ANOTHER
/// WORKTREE is printed `+ main` rather than `  main`. That is a completely
/// ordinary state in this repository's own workflow, and without `+` in the
/// trim set the whole default-branch advisory (step 4) silently never fired —
/// `lists_branch` answered false for both `main` and `master`, and the function
/// returned before reaching it.
///
/// The `body.len() < stripped.len()` guard STAYS, and is why `for-each-ref` is
/// not the answer here: it is what stops a `## main...origin/main` status line
/// matching, and `+ main` still strips two characters, so the guard costs
/// nothing. `for-each-ref` produces undecorated lines the guard would reject.
pub fn lists_branch(branch_list: &str, name: &str) -> bool {
    branch_list.lines().any(|line| {
        let stripped = line.trim_end_matches(['\r']);
        let body = stripped.trim_start_matches([' ', '\t', '*', '+']);
        !body.is_empty() && body.len() < stripped.len() && body == name
    })
}

/// Left side of `git rev-list --left-right --count <a>...<b>`: how far the
/// default branch is ahead of us.
pub fn ahead_count(rev_list: &str) -> Option<u64> {
    rev_list.split_whitespace().next()?.parse().ok()
}

pub fn run(_args: &[std::ffi::OsString]) -> Outcome {
    // 1. Never auto-rebase a dirty tree: that autostashes real work and can
    //    leave a broken mid-rebase state during a push.
    if !git::stdout(&["status", "--porcelain"])
        .unwrap_or_default()
        .is_empty()
    {
        crate::say!(
            "{} Uncommitted changes — skipping pre-push pull-rebase.",
            warning_sign()
        );
        return Outcome::Passed;
    }

    // 2. Only sync a branch that HAS an upstream, and rebase onto that — never
    //    the default branch. A brand-new branch has nothing to sync.
    let Some(upstream) =
        git::stdout(&["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"])
    else {
        return Outcome::Passed;
    };

    // 2a. The upstream has to be a REAL remote, not merely something with a
    //     `/` in its name. `branch.<name>.remote` is what `git pull`/`fetch`
    //     itself reads to resolve `@{u}` — asking IT, rather than guessing
    //     the remote by splitting `upstream` on '/' and falling back to
    //     "origin", is the fix for a real incident: `git worktree add -b x
    //     <path> main` sets the new branch's upstream to the LOCAL branch
    //     `main` (no slash at all — `@{u}` prints bare `main`), the old
    //     fallback silently read that as `origin/main`, and the bare
    //     `git pull --rebase` two steps down synced from LOCAL main — not
    //     origin — while every check and message around it talked about
    //     origin. Move `main` in the meantime (a `git reset --hard` on it in
    //     another worktree, say) and the next push silently rebases onto
    //     wherever it ended up, no divergence check catching it because the
    //     hand-rolled remote guess was never asking the real question.
    let Some(current) = git::stdout(&["symbolic-ref", "--short", "HEAD"]) else {
        return Outcome::Passed; // detached HEAD: nothing to sync
    };
    let remote =
        git::stdout(&["config", "--get", &format!("branch.{current}.remote")]).unwrap_or_default();
    let is_a_real_remote = !remote.is_empty()
        && git::stdout(&["remote"])
            .unwrap_or_default()
            .lines()
            .any(|r| r == remote);
    if !is_a_real_remote {
        crate::say!(
            "{} {upstream} is not a remote-tracking branch — skipping sync.",
            warning_sign()
        );
        return Outcome::Passed;
    }
    let branch = upstream
        .strip_prefix(&format!("{remote}/"))
        .unwrap_or(&upstream);

    // Whether this check may MUTATE and use the NETWORK. Off, it becomes a
    // pure advisor over your last fetch: no ls-remote round-trip per push, no
    // rebase you did not type — the shape a check is expected to have. On (the
    // default, and the behaviour every install so far has had), it syncs a
    // behind branch for you and then asks for a second push.
    let auto = crate::config::boolean_or("amont.autoRebase", true);

    // 2b. The upstream can be configured locally but GONE on the remote — the
    //     normal state right after a PR squash-merges with delete-on-merge.
    //     `git pull --rebase` would fail on the missing ref and read as a
    //     conflict, wrongly blocking the push. Only asked when a rebase may
    //     actually happen: it is this check's per-push network round-trip.
    if auto {
        // Bounded, and the exit codes kept apart: `--exit-code` exits 2 for
        // "connected, the ref is gone" and 128 for "could not connect", and
        // this used to read both as the first — offline was reported as
        // "upstream deleted", which is a diagnosis someone acts on.
        let budget = super::common::network_probe_budget();
        match git::probe(
            &["ls-remote", "--exit-code", "--heads", &remote, branch],
            budget,
        ) {
            git::Probe::Exit(0) => {} // upstream exists — carry on to the sync
            git::Probe::Exit(2) => {
                crate::say!(
                    "{} Upstream {upstream} no longer exists on the remote (merged + auto-deleted?) — skipping sync.", warning_sign()
                );
                return Outcome::Passed;
            }
            git::Probe::Exit(_) | git::Probe::Failed => {
                crate::say!(
                    "{} Could not reach {remote} (offline?) — skipping sync.",
                    warning_sign()
                );
                return Outcome::Passed;
            }
            git::Probe::TimedOut(secs) => {
                crate::say!(
                    "{} {remote} did not answer within {secs}s — skipping sync.",
                    warning_sign()
                );
                return Outcome::Passed;
            }
        }
    }

    // 3. Diverged → warn and DO NOT rebase, but carry on to the default-branch
    //    check below (the shell fell through here too).
    let status = git::stdout(&["status", "-sb"]).unwrap_or_default();
    if let Some((ahead, behind)) = divergence(&status) {
        // Divergence has two causes and they want OPPOSITE actions, so this
        // says what it saw and lets you pick. The old copy prescribed
        // `git pull --rebase` unconditionally, which after a local rebase or
        // amend is the one command that undoes the work you are pushing — it
        // replays the upstream commits you just rewrote.
        //
        // The hook cannot tell the two apart: git does not tell a pre-push hook
        // whether `--force` was passed, and both cases are non-fast-forward.
        // Guessing wrong here costs someone their rebase, so it does not guess.
        crate::say!(
            "{} Branch and upstream have diverged ({ahead} ahead, {behind} behind) — not auto-rebasing.",
            warning_sign()
        );
        crate::say!(
            "    Rebased or amended locally? That is expected — push with {}.",
            highlight("git push --force-with-lease")
        );
        crate::say!(
            "    Someone else pushed here? Reconcile first with {} (or {}).",
            highlight("git pull --rebase"),
            highlight("git merge")
        );
    } else if behind_count(&status).unwrap_or(0) > 0 {
        if !auto {
            // Advisory, then STOP — before the test suite spends minutes on a
            // push the server will refuse as non-fast-forward anyway. Judged
            // from the local tracking ref (the last fetch): with auto off
            // this check does no network I/O at all.
            crate::say!(
                "{} Behind {upstream} — not auto-rebasing ({} is off).",
                warning_sign(),
                highlight("amont.autoRebase")
            );
            crate::say!("    Sync first: {}", highlight("git pull --rebase"));
            return Outcome::Failed;
        }
        // `behind_count > 0` gates the attempt itself, not just its outcome:
        // with nothing behind, the upstream has no commits to reconcile with,
        // so `pull --rebase` has nothing to do BY DEFINITION — and attempting
        // it anyway is not merely wasted work, it is a real subprocess rebase
        // that can choke on the local branch's own history for a
        // reconciliation nothing needed. A branch carrying a merge commit
        // (recording a hand-resolved reconciliation with a THIRD branch, say)
        // is exactly such a shape: `pull --rebase` tries to linearize it by
        // full reachability rather than first-parent, replays commits that
        // were already incorporated, and fails loudly over a push that was a
        // clean fast-forward all along.
        //
        // Explicit remote and branch, not a bare `pull --rebase` trusting
        // ambient `branch.*.remote`/`.merge` — `remote`/`branch` are already
        // the verified pair from 2a/2b, and repeating them here means this
        // sync can never again silently use something other than what was
        // just checked.
        // Under the full `amont.timeout` deadline — this is real work (a
        // fetch plus a rebase), not a probe, but it is still a network verb
        // and a hung fetch used to hold the push hostage with no clock
        // anywhere. The probe above only proved the remote ANSWERS; it says
        // nothing about how the transfer goes.
        match git::probe(
            &["pull", "--rebase", &remote, branch],
            super::common::check_timeout(),
        ) {
            git::Probe::Exit(0) => {
                // HEAD moved; the oids git handed THIS push on stdin have
                // not. Carrying on would run the suite against packages
                // selected from commits git is no longer pushing, and the
                // server refuses the stale objects as non-fast-forward
                // regardless. Fail fast, with the good news first.
                crate::say!(
                    "{} Rebased onto {upstream}. This push's refs predate the rebase — push again.",
                    warning_sign()
                );
                return Outcome::Failed;
            }
            git::Probe::TimedOut(secs) => {
                // Abort so the tree is never left half-rebased.
                let _ = git::succeeds(&["rebase", "--abort"]);
                crate::say!(
                    "{} pull --rebase did not finish within {secs}s (rebase aborted, tree restored).",
                    error_sign()
                );
                crate::say!("    Sync manually: {}", highlight("git pull --rebase"));
                return Outcome::Failed;
            }
            git::Probe::Exit(_) | git::Probe::Failed => {
                // Abort so the tree is never left half-rebased.
                let _ = git::succeeds(&["rebase", "--abort"]);
                crate::say!(
                    "{} pull --rebase hit conflicts (rebase aborted, tree restored).",
                    error_sign()
                );
                crate::say!("    Resolve manually: {}", highlight("git pull --rebase"));
                return Outcome::Failed;
            }
        }
    } else {
        crate::say!("{} Branch is in sync with its upstream", valid_sign());
    }

    // 4. Informational only — never acts.
    // UNTRIMMED, and that is not a detail. `git::stdout` trims the whole
    // buffer, which strips the two leading spaces off the FIRST line — so when
    // `git branch` happened to list the default branch first, `lists_branch`'s
    // decoration guard rejected it and the advisory below never fired. Whether
    // it fired depended on the alphabetical position of the branch you were on,
    // which is why it looked intermittent rather than broken.
    let branch_list = git::stdout_raw(&["branch"])
        .map(|out| String::from_utf8_lossy(&out).into_owned())
        .unwrap_or_default();
    let default_branch = if lists_branch(&branch_list, "main") {
        "main"
    } else if lists_branch(&branch_list, "master") {
        "master"
    } else {
        return Outcome::Passed;
    };

    // `remote`, not a hardcoded "origin". Step 2 above spends thirteen lines of
    // comment establishing `remote` as the VERIFIED one for this branch, and
    // then this step ignored it: in a repository whose remote is `upstream`,
    // the fetch failed (ignored) and `rev-list origin/<branch>...HEAD` returned
    // None, so the advisory was silently skipped.
    // The advisory's fetch is the other network round-trip; with auto off it
    // reads the last fetch's refs instead — stale is fine for an advisory.
    if auto {
        let _ = git::succeeds(&["fetch", &remote, default_branch]);
    }
    let range = format!("{remote}/{default_branch}...HEAD");
    if let Some(n) =
        git::stdout(&["rev-list", "--left-right", "--count", &range]).and_then(|s| ahead_count(&s))
    {
        if n > 0 {
            // The shell took `head -c 1` of this count — the FIRST CHARACTER —
            // so 12 commits ahead printed "1". The test was only ever
            // non-zero/zero, so the wrong number went unnoticed. Parsed properly
            // here.
            crate::say!(
                "{} {remote}/{default_branch} is ahead by {n} commit(s).",
                warning_sign()
            );
            crate::say!(
                "    Consider before merging: {}",
                highlight(&format!("git merge {remote}/{default_branch}"))
            );
        }
    }
    Outcome::Passed
}

#[cfg(test)]
mod tests {
    use super::{ahead_count, behind_count, divergence, lists_branch};

    #[test]
    fn detects_divergence_only_when_both_counts_are_present() {
        assert!(divergence("## feat/x...origin/feat/x [ahead 1, behind 2]").is_some());
        assert!(divergence("## a...b [ahead 12,  behind 3]").is_some());
        // ahead only, or behind only, is NOT divergence — a rebase is fine
        assert!(divergence("## feat/x...origin/feat/x [ahead 3]").is_none());
        assert!(divergence("## feat/x...origin/feat/x [behind 2]").is_none());
        assert!(divergence("## feat/x...origin/feat/x").is_none());
        assert!(divergence("## ahead-of-time...origin/x").is_none());
    }

    /// The counts are the message now, so a wrong one is a wrong message.
    /// Two digits especially: this file already carries a bug where a count of
    /// 12 printed as 1.
    #[test]
    fn reports_how_far_apart_the_two_are() {
        assert_eq!(
            divergence("## feat/x...origin/feat/x [ahead 1, behind 2]"),
            Some((1, 2))
        );
        assert_eq!(
            divergence("## a...b [ahead 12,  behind 34]"),
            Some((12, 34))
        );
    }

    /// A branch legitimately named `ahead-of-behind` must not be parsed as a
    /// pair of counts, and `behind` has to be the token straight after the
    /// comma rather than merely somewhere on the line.
    #[test]
    fn branch_names_are_not_mistaken_for_counts() {
        assert!(divergence("## ahead 3...origin/behind 4").is_none());
        // The word has to be followed by a SPACE, so a branch whose name runs
        // straight into digits is not read as a count.
        assert!(divergence("## a...b [ahead3, behind4]").is_none());
        assert!(divergence("## ahead12...origin/behind34").is_none());
        assert!(divergence("## a...b [ahead 3, xbehind 4]").is_none());
        assert!(divergence("## ahead-of/behind...origin/ahead-of/behind").is_none());
    }

    /// `git branch` puts a marker in column 0, and there are TWO of them.
    ///
    /// `*` is the current branch; `+` — since git 2.23 — is a branch checked
    /// out in ANOTHER WORKTREE. That second one is an entirely ordinary state
    /// in this repository's own workflow, and without it in the trim set the
    /// whole default-branch advisory silently never fired: `lists_branch`
    /// answered false for `main` and for `master`, and `run` returned before
    /// reaching step 4.
    #[test]
    fn recognises_git_branch_lines() {
        let list = "  feat/x\n* main\n  master-ish\n";
        assert!(lists_branch(list, "main"));
        assert!(!lists_branch(list, "master")); // master-ish is a different branch
        assert!(!lists_branch(list, "feat")); // must match the whole name
        assert!(lists_branch(list, "feat/x"));

        // The worktree marker.
        let elsewhere = "+ main\n* feat/x\n";
        assert!(
            lists_branch(elsewhere, "main"),
            "a branch checked out in another worktree is still a branch"
        );

        // The decoration guard is DELIBERATE, not incidental: an undecorated
        // line is not `git branch` output, and this is what stops a
        // `## main...origin/main` status line matching. It is also why this
        // cannot be switched to `for-each-ref`, whose lines carry no marker.
        assert!(!lists_branch("main\n", "main"));
        assert!(!lists_branch("## main...origin/main\n", "main"));

        // …which is why `run` must NOT read `git branch` through
        // `git::stdout`: that trims the whole buffer, eating the FIRST line's
        // indentation. When git listed the default branch first, this is
        // exactly the input the guard was handed, and the advisory silently
        // never fired.
        assert!(!lists_branch("main\n* feat/x\n", "main"));
        assert!(lists_branch("  main\n* feat/x\n", "main"));
    }

    /// A count of 12 must read as 12, not 1 — the shell's `head -c 1`.
    #[test]
    fn parses_the_full_ahead_count() {
        assert_eq!(ahead_count("12\t3"), Some(12));
        assert_eq!(ahead_count("0\t5"), Some(0));
        assert_eq!(ahead_count(""), None);
    }

    /// The whole point: `behind_count`, unlike `divergence`, answers even
    /// when `ahead` is not on the line at all.
    #[test]
    fn behind_count_does_not_require_ahead() {
        assert_eq!(
            behind_count("## feat/x...origin/feat/x [behind 2]"),
            Some(2)
        );
        assert_eq!(
            behind_count("## feat/x...origin/feat/x [ahead 1, behind 2]"),
            Some(2)
        );
    }

    /// Ahead-only, or fully in sync, is "nothing behind" — the shape that
    /// means a sync has nothing to reconcile.
    #[test]
    fn behind_count_is_none_when_there_is_nothing_behind() {
        assert_eq!(behind_count("## feat/x...origin/feat/x [ahead 3]"), None);
        assert_eq!(behind_count("## feat/x...origin/feat/x"), None);
        assert_eq!(behind_count(""), None);
    }

    /// The same false-positive guard `divergence` has: a branch merely
    /// NAMED with "behind" in it, glued to non-whitespace, is not a count.
    #[test]
    fn behind_count_is_not_fooled_by_a_branch_name() {
        assert!(behind_count("## my-behind-thing...origin/my-behind-thing").is_none());
        assert!(behind_count("## a...b [behind4]").is_none());
    }
}
