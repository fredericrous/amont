//! The refs git feeds `pre-push` on stdin, read ONCE and shared.
//!
//! git writes one line per ref being pushed:
//!
//! ```text
//! <local ref> <local oid> <remote ref> <remote oid>
//! ```
//!
//! This exists because stdin can only be consumed once. While checks were
//! separate processes with INHERITED stdin, whichever ran first drained it and
//! the rest saw EOF — silently. Two repos in the fleet had a custom
//! `pre-push-branch-protect.sh` whose `while read` loop sorted BEFORE
//! `pre-push-run-tests-js`, so the test gate received no refs and ran nothing.
//! Nobody noticed, because "no refs" and "nothing to test" look identical.
//!
//! Reading it in one place and lending the result to every check removes that
//! whole class of bug, and makes it impossible to reintroduce by adding a
//! second stdin reader.

use std::io::BufRead;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushRef {
    pub local_ref: String,
    pub local_oid: String,
    pub remote_ref: String,
    pub remote_oid: String,
}

/// Lazily-read pushed refs. `OnceLock` rather than `OnceCell` because
/// pre-commit runs its checks on threads and `Ctx` must therefore be `Sync`.
#[derive(Default)]
pub struct PushRefs(OnceLock<Vec<PushRef>>);

impl PushRefs {
    /// Read on first use. A check that never asks never blocks on stdin —
    /// which matters, because pre-commit's stdin is not a ref list.
    pub fn get(&self) -> &[PushRef] {
        self.0.get_or_init(|| parse(std::io::stdin().lock()))
    }

    /// Pre-populated, so `.get()` never touches stdin at all.
    ///
    /// For a context with no real `pre-push` invocation to read refs from —
    /// `amont run <pre-push-check>` is the one today — where `.get()`
    /// would otherwise block reading a TTY that has no ref list coming, or
    /// (piped from `/dev/null`, say) read nothing and let the check treat an
    /// empty ref list as "nothing to check": branch-protect calls that "no
    /// push to a protected branch", and a scope-gated suite just never loops.
    pub fn preloaded(refs: Vec<PushRef>) -> PushRefs {
        let cell = OnceLock::new();
        let _ = cell.set(refs);
        PushRefs(cell)
    }
}

/// Synthesise the one `PushRef` a standalone invocation — no real `pre-push`
/// on the other end of stdin — has no other way to obtain: `@{u}..HEAD`.
///
/// Shared by `list --pushed` and `amont run <pre-push-check>`, both of
/// which need to answer "what would this check see" without an actual push
/// in flight.
pub fn synthetic_from_upstream() -> Result<PushRef, String> {
    // No upstream means a branch that has never been pushed. That used to be
    // the end of it — but the branch that has never been pushed is exactly
    // the one about to be, and `amont run pre-push` exists to rehearse that
    // push before git opens a connection for it. So fall back to what the
    // first push would be judged against: the remote's default branch.
    let upstream =
        match crate::git::stdout(&["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]) {
            Some(u) => u,
            None => match DEFAULT_BASES.iter().find(|b| {
                crate::git::succeeds(&["rev-parse", "-q", "--verify", &format!("{b}^{{commit}}")])
            }) {
                Some(b) => b.to_string(),
                None => {
                    return Err("no upstream configured for the current branch and no \
                     origin/HEAD, origin/main or origin/master to diff against — \
                     nothing has been fetched yet"
                        .to_string())
                }
            },
        };
    let Some(local_oid) = crate::git::stdout(&["rev-parse", "HEAD"]) else {
        return Err("could not resolve HEAD".to_string());
    };
    let Some(remote_oid) = crate::git::stdout(&["rev-parse", &upstream]) else {
        return Err(format!("could not resolve upstream {upstream}"));
    };
    let local_ref =
        crate::git::stdout(&["symbolic-ref", "-q", "HEAD"]).unwrap_or_else(|| "HEAD".to_string());
    Ok(PushRef {
        local_ref: local_ref.clone(),
        local_oid,
        remote_ref: local_ref,
        remote_oid,
    })
}

/// What a never-pushed branch is measured against, in order of preference:
/// the remote's advertised default, then the two names it is almost always
/// called.
const DEFAULT_BASES: &[&str] = &["origin/HEAD", "origin/main", "origin/master"];

pub fn parse<R: BufRead>(r: R) -> Vec<PushRef> {
    r.lines()
        .map_while(Result::ok)
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            let (a, b, c, d) = (f.next()?, f.next()?, f.next()?, f.next()?);
            Some(PushRef {
                local_ref: a.to_owned(),
                local_oid: b.to_owned(),
                remote_ref: c.to_owned(),
                remote_oid: d.to_owned(),
            })
        })
        .collect()
}

/// Every path touched by the refs being pushed.
///
/// Shared because two callers need exactly this list and would otherwise write
/// the zero-oid and range handling twice: `cargo-test` decides whether a suite
/// is worth running, and a declared pre-push check decides whether its `scope`
/// applies. A copy that got the delete case wrong would run a test suite on a
/// branch deletion.
pub fn changed_files(refs: &[PushRef]) -> Vec<String> {
    let zero = crate::git::stdout(&["hash-object", "--stdin"])
        .map(|h| "0".repeat(h.len()))
        .unwrap_or_else(|| "0".repeat(40));
    let mut changed: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for r in refs {
        changed.extend(changed_files_for(r, &zero));
    }
    changed.into_iter().collect()
}

/// Every path touched by ONE ref being pushed.
///
/// Split out from `changed_files` so a caller that runs a suite in a
/// per-ref worktree — `where_to_run` takes one tip, not the whole ref
/// list, for exactly this reason — can ask "what did THIS ref change"
/// instead of the aggregate across every ref in the push.
pub fn changed_files_for(r: &PushRef, zero: &str) -> Vec<String> {
    if r.local_oid == zero {
        return Vec::new(); // deleting a ref pushes no code
    }
    if r.remote_oid == zero {
        // A brand-new ref: no remote tree to diff against, so the
        // `remote..local` trick below does not apply. Walk every commit
        // this push would introduce that no remote-tracking ref already
        // has — `rev-list --not --remotes` — rather than just the tip's
        // own diff against its parent, which silently missed every
        // earlier commit on a multi-commit new branch: a crate added two
        // commits back, with a docs-only commit on top, reported only the
        // docs file as changed, so a scope-gated check like
        // `pre-push-cargo-test` never ran.
        let commits = crate::git::stdout(&["rev-list", &r.local_oid, "--not", "--remotes"]);
        return match commits {
            Some(commits) if !commits.is_empty() => diff_tree_stdin(&commits),
            // No remote-tracking ref to exclude anything against (e.g. no
            // remote configured at all) — fall back to the tip alone,
            // which is at least what the check always did before.
            _ => diff_tree_stdin(&r.local_oid),
        };
    }
    range_changed_files(&r.remote_oid, &r.local_oid)
}

/// `(commit, files)` for every commit ONE ref would push — the same walks as
/// [`changed_files_for`], keeping the commit identities its union discards.
///
/// For a caller matching per-commit STATE (a `gate_stamp` note) against
/// per-commit CHANGES, the union is useless: it says the ref touched `a.ts`
/// without saying which commit did, and the whole question is whether THAT
/// commit was checked. One `diff-tree` per commit rather than a parse of
/// `--stdin` output with commit ids left in: a push is a handful of commits,
/// and this reuses `diff_tree_stdin`'s tested flag set (`-z` for unusual
/// bytes, `-m` for merges) instead of growing a second parser for the
/// interleaved id-and-paths stream.
pub fn commits_and_files_for(r: &PushRef, zero: &str) -> Vec<(String, Vec<String>)> {
    if r.local_oid == zero {
        return Vec::new(); // deleting a ref pushes no code
    }
    let commits = if r.remote_oid == zero {
        // Same shape as `changed_files_for`'s new-branch arm, same fallback.
        match crate::git::stdout(&["rev-list", &r.local_oid, "--not", "--remotes"]) {
            Some(commits) if !commits.is_empty() => commits,
            _ => r.local_oid.clone(),
        }
    } else {
        let range = format!("{}..{}", r.remote_oid, r.local_oid);
        match crate::git::stdout(&["rev-list", &range]) {
            Some(commits) if !commits.is_empty() => commits,
            _ => return Vec::new(),
        }
    };
    commits
        .lines()
        .map(|c| (c.to_string(), diff_tree_stdin(c)))
        .collect()
}

/// Every path touched by ANY commit reachable in `remote..local`, not just
/// the net difference between the two endpoint trees.
///
/// `diff-tree remote..local` looks like the obvious tool, and is wrong: for
/// `diff`/`diff-tree`, a two-dot range is shorthand for a straight two-tree
/// comparison (`diff-tree remote local`) — unlike `log`, where the identical
/// syntax means a commit walk. A file changed by one commit and reverted by
/// a later one in the same push nets to "unchanged" between the endpoints,
/// so a scope-gated check never learns the file was touched at all.
/// `rev-list` walks the commits; `diff-tree --stdin` diffs each one against
/// its own parent, and the per-commit results are unioned here.
fn range_changed_files(remote_oid: &str, local_oid: &str) -> Vec<String> {
    let range = format!("{remote_oid}..{local_oid}");
    let Some(commits) = crate::git::stdout(&["rev-list", &range]) else {
        return Vec::new();
    };
    if commits.is_empty() {
        return Vec::new();
    }
    diff_tree_stdin(&commits)
}

/// Feed a newline-separated list of commit ids through `diff-tree --stdin`,
/// diffing each against its own parent(s), and return the union of paths.
///
/// `-z`, not bare `--name-only`. Without it git QUOTES any path holding an
/// "unusual" byte, non-ASCII included: `é.ts` prints as the nine-byte literal
/// `"\303\251.ts"`. That string ends with neither `.ts` nor anything else the
/// callers look for, so `is_js`, `is_rust_path` and `Scope::matches` all miss
/// it and a scope-gated pre-push check silently never runs on the very commit
/// that changed the file. `git::split_nul_paths` is the same parser
/// `stdout_paths` uses, kept in one tested place rather than copied here.
fn diff_tree_stdin(commits: &str) -> Vec<String> {
    // `git::stdout` trims the trailing newline `rev-list` itself always
    // writes — and `diff-tree --stdin` treats "no newline after this hash"
    // as "the line isn't finished yet", silently dropping the LAST commit
    // rather than reading it. Put the newline back before feeding it in.
    //
    // `-m`: without it, `diff-tree` shows NOTHING for a merge commit at all
    // — confirmed empirically, not merely documented — so a file edited only
    // to resolve a conflict (never touched by either parent individually) is
    // invisible to a scope-gated check. `-m` diffs a merge against EACH
    // parent and unions the results, which is exactly "did this commit,
    // merge or not, touch this path" — the caller already de-duplicates the
    // combined list, so the extra per-parent repeats cost nothing.
    crate::git::stdout_piped_raw(
        &[
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-r",
            "-m",
            "-z",
            "--stdin",
        ],
        &format!("{}\n", commits.trim_end()),
    )
    .map(|raw| crate::git::split_nul_paths(&raw))
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_four_fields() {
        let got = parse(&b"refs/heads/x aaa refs/heads/y bbb\n"[..]);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].local_ref, "refs/heads/x");
        assert_eq!(got[0].remote_ref, "refs/heads/y");
        assert_eq!(got[0].remote_oid, "bbb");
    }

    #[test]
    fn several_refs_and_junk_lines() {
        let got = parse(&b"a 1 b 2\ngarbage\n\nc 3 d 4\n"[..]);
        assert_eq!(got.len(), 2, "short lines are skipped, not fatal");
        assert_eq!(got[1].local_ref, "c");
    }

    #[test]
    fn empty_stdin_is_no_refs_not_an_error() {
        assert!(parse(&b""[..]).is_empty());
    }

    /// Mirrors `git::a_non_ascii_path_is_not_reinterpreted_as_its_quoted_form`,
    /// because this module used to parse `diff-tree` output ITSELF, by lines,
    /// with no `-z`.
    ///
    /// Under bare `--name-only`, git prints `é.ts` as the nine-byte literal
    /// `"\303\251.ts"` — backslashes, digits and quotes standing in for two
    /// UTF-8 bytes. That string ends with neither `.ts` nor `.rs`, so `is_js`,
    /// `is_rust_path` and `Scope::matches` all said "not mine" and the
    /// scope-gated pre-push check silently never ran on the one commit that
    /// changed the file. Feeding the same real bytes through `-z` output must
    /// hand the path back untouched.
    #[test]
    fn a_non_ascii_path_survives_the_diff_tree_parse() {
        let mut raw = "src/é.ts".as_bytes().to_vec();
        raw.push(0);
        raw.extend_from_slice(b"src/plain.ts\0");
        let got = crate::git::split_nul_paths(&raw);
        assert_eq!(
            got,
            vec!["src/é.ts".to_string(), "src/plain.ts".to_string()]
        );
        assert!(
            got[0].ends_with(".ts"),
            "the quoted form ends with a quote, not an extension, and is why \
             a scope-gated check never fired: {:?}",
            got[0]
        );
    }
}
