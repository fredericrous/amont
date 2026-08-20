//! Executing a `FixPlan`. The only code here that deletes anything.
//!
//! Three rules, each bought with an incident:
//!
//! 1. **Re-plan, never trust a stale plan.** The caller shows a preview and the
//!    user confirms; between those moments the tree can change. Applying the
//!    plan that was displayed would act on a world that no longer exists, so
//!    apply takes the plan it just computed and verifies its refusals again.
//!
//! 2. **A refusal suppresses the whole repository.** Not the offending file —
//!    the repo. A half-applied fix is precisely how a repo ends up holding both
//!    `pre-commit-ruff.zsh` and `pre-commit-ruff`, running ruff twice, silently,
//!    on whichever repos a loop happened to reach.
//!
//! 3. **Remove before write, and stop the repo on the first error.** Writing
//!    first would leave the retired file next to its replacement, which is the
//!    double-run state again.
//!
//! `make install` destroyed tracked source twice in this repo's history, both
//! times because its guard failed OPEN. Nothing here deletes a path it has not
//! positively established is safe.
//!
//! ## Rule 1 was a claim, not a fact
//!
//! For two releases the doc above said "verifies its refusals again" and only
//! ONE of the five was re-checked. `Unmanaged`, `ForeignHook` and the hooks
//! directory's location were all taken on trust from a plan built before the
//! user was even shown a preview, and the write itself went through
//! `std::fs::write`, which FOLLOWS SYMLINKS.
//!
//! The verified consequence: a repository with four tracked dispatchers under
//! `shared/` and `.git/hooks/*` symlinked to them. `fix --apply` reported
//! `4 written` and left all four TRACKED FILES MODIFIED. The plan's own
//! symlink guard had never been asked, and the one guard that did run — the
//! tracked check — was asking about the link, which is untracked, sits in
//! `.git`, and has nothing alarming about it.
//!
//! So every write and every remove now goes through
//! [`amont_runtime::hookfile`], which is the single owner of "is this ours,
//! and may we touch it?" — it never follows a link, never guesses, and stages
//! every write to a sibling temporary that is `rename`d into place, so
//! REPLACING a link is the only thing that can happen and writing THROUGH one
//! is not a code path that exists. That is a stronger statement than "we check
//! first", because a check can be raced and this cannot.
//!
//! Deleting `write_executable` in favour of `hookfile::stage` + `commit_all`
//! also strengthens `written_shims_are_executable`: the mode is set on the
//! temporary before the rename, so the file is never visible at its
//! destination in a non-executable state — where git would silently not run it.

use serde::Serialize;

use crate::fix::{tracked_refusal, FixPlan, Intent};
use crate::shim;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    /// Files were removed and/or written.
    Applied { removed: usize, written: usize },
    /// Nothing needed doing.
    Unchanged,
    /// The plan refused; nothing was touched.
    Refused,
    /// Something failed partway. Names what stopped it.
    Failed { error: String, at: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplyReport {
    pub repo: std::path::PathBuf,
    pub outcome: Outcome,
}

/// Apply one plan. Returns what actually happened, never what was intended.
pub fn apply(plan: &FixPlan) -> Outcome {
    if plan.refused() {
        return Outcome::Refused;
    }
    if let Some(outcome) = reverify(plan) {
        return outcome;
    }
    if plan.is_noop() {
        return Outcome::Unchanged;
    }

    // Re-check tracked-ness at the moment of action, for a REMOVE and a
    // WRITE alike. The preview may be old, and this is the guard that failed
    // open twice before — both times on a delete, but a write that landed on
    // tracked source (a repo whose `core.hooksPath`, say, points somewhere the
    // planner did not expect when the plan was built) would overwrite it
    // exactly as destructively, just without `rm` in the name.
    for path in plan
        .remove
        .iter()
        .map(|r| &r.path)
        .chain(plan.write.iter().map(|w| &w.path))
    {
        if let Some(refusal) = tracked_refusal(path) {
            return Outcome::Failed {
                error: match refusal {
                    crate::fix::Refusal::TrackedUnknown { why, .. } => {
                        format!("cannot tell whether the path is tracked by git ({why})")
                    }
                    _ => "path is tracked by git".to_string(),
                },
                at: path.display().to_string(),
            };
        }
    }

    // Removals first — writing first would leave a retired file beside its
    // replacement, which is the double-run state. Each goes through
    // `guard_remove` with `expect_ours = false`: the plan has already
    // established what each of these is (ours-but-retired, the node-era
    // `package.json`, or an explicitly opted-in stranger), and `guard_remove`
    // is here for the question the plan cannot answer twice — whether the path
    // is tracked NOW.
    let mut removed = 0;
    for r in &plan.remove {
        if amont_runtime::hookfile::classify(&r.path) == amont_runtime::hookfile::HookFile::Absent {
            // Already gone is the desired state, and must not be counted as
            // work done.
            continue;
        }
        if let Err(refuse) = amont_runtime::hookfile::guard_remove(&r.path, false) {
            return Outcome::Failed {
                error: refuse.explain(),
                at: refuse.path().display().to_string(),
            };
        }
        if let Err(e) = amont_runtime::hookfile::remove_regular(&r.path) {
            return Outcome::Failed {
                error: e.to_string(),
                at: r.path.display().to_string(),
            };
        }
        removed += 1;
    }

    // Every shim is STAGED before any of them lands. Guarding four paths and
    // then writing four files leaves a window in which the third write fails
    // and the repository holds two new dispatchers and two old ones; staging
    // moves every anticipatable failure (no space, no permission, a read-only
    // directory) before the first destination is touched. `commit_all` then
    // `rename`s, which REPLACES a symlink rather than following it.
    let mut staged = Vec::new();
    for write in plan.write.iter().filter(|write| write.changes) {
        // `force = false`, always. There is no `--force` on the fleet path and
        // there should not be: a fleet-wide override is a decision made once
        // about ninety-six repositories nobody looked at.
        if let Err(refuse) = amont_runtime::hookfile::guard_write(&write.path, false) {
            return Outcome::Failed {
                error: refuse.explain(),
                at: refuse.path().display().to_string(),
            };
        }
        match amont_runtime::hookfile::stage(&write.path, &shim::render(&write.baked), true) {
            Ok(s) => staged.push(s),
            Err(e) => {
                return Outcome::Failed {
                    error: e.to_string(),
                    at: write.path.display().to_string(),
                }
            }
        }
    }
    let mut written = match amont_runtime::hookfile::commit_all(staged) {
        Ok(landed) => landed.len(),
        Err(failure) => {
            return Outcome::Failed {
                error: failure.to_string(),
                at: failure.at.display().to_string(),
            }
        }
    };

    if let Some(w) = &plan.write_agents_md {
        // Re-check at the moment of action, same rule as `is_tracked` above:
        // the scan this plan was built from may be stale, and a repo that
        // reached `MatchesGenerated` in between must not be overwritten.
        match amont_runtime::agents_md::check(&w.path) {
            Ok(amont_runtime::agents_md::CheckResult::MatchesGenerated) => {}
            Ok(_) => match amont_runtime::agents_md::write(&w.path) {
                Ok(()) => written += 1,
                Err(e) => {
                    return Outcome::Failed {
                        error: e,
                        at: w.path.display().to_string(),
                    }
                }
            },
            Err(e) => {
                return Outcome::Failed {
                    error: e,
                    at: w.path.display().to_string(),
                }
            }
        }
        // The CLAUDE.md signpost is half of the pair — writing the block
        // without it leaves Claude Code, which loads CLAUDE.md and not
        // AGENTS.md, seeing no guidance at all. Same re-check-then-write
        // rule, and `NotPresent` counts as "write it": across the fleet the
        // signpost is usually the file that does not exist yet.
        if w.path.file_name().is_some_and(|n| n != "CLAUDE.md") {
            let pointer = w.path.with_file_name("CLAUDE.md");
            match amont_runtime::agents_md::check_pointer(&pointer) {
                Ok(amont_runtime::agents_md::CheckResult::MatchesGenerated) => {}
                Ok(_) => match amont_runtime::agents_md::write_pointer(&pointer) {
                    Ok(()) => written += 1,
                    Err(e) => {
                        return Outcome::Failed {
                            error: e,
                            at: pointer.display().to_string(),
                        }
                    }
                },
                Err(e) => {
                    return Outcome::Failed {
                        error: e,
                        at: pointer.display().to_string(),
                    }
                }
            }
        }
    }

    Outcome::Applied { removed, written }
}

/// Ask the plan's own refusals again, against the filesystem as it is NOW.
///
/// `Some(Outcome::Refused)` means the world stopped matching the plan between
/// the scan and this moment — which is not a failure, it is the plan correctly
/// declining to act on a world that no longer exists. `None` means proceed.
///
/// Each of the three was, until this function existed, taken on trust:
///
/// - **Unmanaged.** `plan` reads `repo.managed` out of a scan that may be
///   minutes old and, in the TUI, may predate several keystrokes. A repository
///   that stopped being ours in that window would be repaired anyway — the one
///   thing standing between this tool and an application's data repository.
/// - **ForeignHook.** Activation is the mode that writes four dispatchers into
///   a repository nobody had installed into, and the check that somebody else's
///   `pre-commit` is not sitting at one of those paths ran once, at plan time.
/// - **The hooks directory.** Re-resolved rather than re-read from the plan,
///   because `core.hooksPath` is config and config changes. If it now resolves
///   anywhere other than where the plan resolved it, every path in the plan
///   names a file in the wrong directory.
fn reverify(plan: &FixPlan) -> Option<Outcome> {
    // `inside()`, not a match on `In`: it admits both `In` and a BENIGN
    // `Redirected` — the deliberate in-repo `core.hooksPath` that `plan()`
    // follows — and answers `None` for Outside, Unknown and a hostile
    // redirect. Matching only `In` here refused at apply time the exact plans
    // `plan()` had just promised for every repository keeping its hooks in a
    // chosen directory like `tooling/hooks`.
    let resolved = crate::scan::hooks_dir_for(&plan.repo_abs);
    let Some(hooks) = resolved.inside().map(std::path::Path::to_path_buf) else {
        // Outside, unresolvable, or taken over. Either way not somewhere we write.
        return Some(Outcome::Refused);
    };
    if plan.hooks.inside() != Some(hooks.as_path()) {
        return Some(Outcome::Refused);
    }
    if plan.intent == Intent::Repair && !crate::scan::is_managed(&hooks) {
        return Some(Outcome::Refused);
    }
    if plan.intent == Intent::Activate
        && !shim::DISPATCHERS
            .iter()
            .all(|n| crate::fix::is_absent_or_ours(&hooks.join(n)))
    {
        return Some(Outcome::Refused);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fix::Intent;
    use crate::fix::{plan, FixPlan, Refusal, WriteShim};
    use crate::scan;
    use std::path::{Path, PathBuf};

    /// Build a repo on disk and scan it, so the plan is derived the same way
    /// the real tool derives it rather than hand-assembled.
    fn fixture(name: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("apply-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let hooks = root.join("r/.git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        (root, hooks)
    }

    fn scan_one(root: &Path, binary: &str) -> (scan::Repo, PathBuf) {
        // A fixture has nothing to draw, so it discards the progress — the one
        // caller for which the silence `scan` no longer offers by default is
        // actually correct.
        let s = scan::scan(root, 3, binary, &mut |_| {});
        let r = s.repos.into_iter().next().expect("one repo");
        let abs = root.join(&r.path);
        (r, abs)
    }

    /// A retired per-check shim of OURS — it carries the marker, which is the
    /// whole reason `fix` may remove it.
    ///
    /// These fixtures used to omit the marker line, so what they were actually
    /// exercising was the removal of a file this tool did NOT write. That path
    /// is gone by default (see `Warning::UnrecognizedSubHook`), and the tests
    /// that meant to cover `stale_ours` now say so.
    const STALE_OURS: &str =
        "#!/bin/sh\n# git-templates hook shim.\nexec x --hooks-dir y pre-commit-ruff\n";

    fn healthy_shims(hooks: &Path, binary: &str) {
        for n in shim::DISPATCHERS {
            std::fs::write(hooks.join(n), shim::render(binary)).unwrap();
        }
    }

    #[test]
    fn removes_the_stale_and_writes_the_shims() {
        let (root, hooks) = fixture("basic");
        healthy_shims(&hooks, "/bin/gh");
        std::fs::write(hooks.join("pre-commit-ruff"), STALE_OURS).unwrap();
        std::fs::write(hooks.join("package.json"), "{\"//\":\"Forces Node\"}").unwrap();

        let (repo, abs) = scan_one(&root, "/bin/gh");
        let p = plan(&repo, &abs, "/bin/gh", Intent::Repair, false, false);
        let out = apply(&p);

        assert!(
            matches!(out, Outcome::Applied { removed: 2, .. }),
            "{out:?}"
        );
        assert!(!hooks.join("pre-commit-ruff").exists());
        assert!(!hooks.join("package.json").exists());
        for n in shim::DISPATCHERS {
            assert!(hooks.join(n).exists(), "{n} must survive");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The property that makes a fix safe to re-run, and the one that catches a
    /// plan which "succeeds" by doing the same work forever.
    #[test]
    fn applying_twice_is_a_no_op_the_second_time() {
        let (root, hooks) = fixture("idempotent");
        healthy_shims(&hooks, "/bin/gh");
        std::fs::write(hooks.join("pre-commit-ruff"), STALE_OURS).unwrap();

        let (repo, abs) = scan_one(&root, "/bin/gh");
        assert!(matches!(
            apply(&plan(&repo, &abs, "/bin/gh", Intent::Repair, false, false)),
            Outcome::Applied { .. }
        ));

        // Re-scan: the world changed, so the plan must be recomputed.
        let (repo2, abs2) = scan_one(&root, "/bin/gh");
        let p2 = plan(&repo2, &abs2, "/bin/gh", Intent::Repair, false, false);
        assert!(p2.is_noop(), "second plan should be empty: {p2:?}");
        assert_eq!(apply(&p2), Outcome::Unchanged);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// What the tool does for a repo whose shims are missing entirely — the
    /// state `Coinbase-OAuth2` is in, minus the managed flag.
    #[test]
    fn writes_missing_dispatchers() {
        let (root, hooks) = fixture("missing");
        healthy_shims(&hooks, "/bin/gh");
        std::fs::remove_file(hooks.join("pre-push")).unwrap();

        let (repo, abs) = scan_one(&root, "/bin/gh");
        let out = apply(&plan(&repo, &abs, "/bin/gh", Intent::Repair, false, false));
        assert!(
            matches!(out, Outcome::Applied { written: 1, .. }),
            "{out:?}"
        );
        assert_eq!(
            std::fs::read_to_string(hooks.join("pre-push")).unwrap(),
            shim::render("/bin/gh")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The write-side proof for a deliberate in-repo `core.hooksPath`:
    /// `plan()` follows the redirect, and `apply()` must not refuse at
    /// reverify what plan just promised. Regression: `reverify` accepted only
    /// `HooksDir::In`, so every benign-redirect plan — previewed with four
    /// writes — died as `Refused` at apply time.
    #[test]
    fn applies_at_a_benign_redirected_hooks_path() {
        let root = std::env::temp_dir().join(format!("apply-redirect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("r");
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .expect("git");
        };
        git(&["init", "-q", "--template=", "."]);
        git(&["config", "core.hooksPath", "tooling/hooks"]);
        let hooks = dir.join("tooling/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        // A shim of ours baked to an old binary, so the repo scans managed and
        // the plan genuinely has something to write.
        std::fs::write(hooks.join("pre-commit"), shim::render("/old/binary")).unwrap();

        let (repo, abs) = scan_one(&root, "/bin/gh");
        let p = plan(&repo, &abs, "/bin/gh", Intent::Repair, false, false);
        assert!(!p.refused(), "{:?}", p.refuse);
        let out = apply(&p);
        assert!(matches!(out, Outcome::Applied { .. }), "{out:?}");
        for n in shim::DISPATCHERS {
            assert_eq!(
                std::fs::read_to_string(hooks.join(n)).unwrap(),
                shim::render("/bin/gh"),
                "{n} must be written at the redirect"
            );
        }
        assert!(
            !dir.join(".git/hooks/pre-commit").exists(),
            "nothing may be written where git does not dispatch"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The block is a plain text file, never chmod'd — unlike a shim, git
    /// never executes it.
    #[test]
    fn a_planned_agents_md_write_is_a_plain_file_not_a_shim() {
        let (root, hooks) = fixture("agents-md-write");
        healthy_shims(&hooks, "/bin/gh");

        let (repo, abs) = scan_one(&root, "/bin/gh");
        let p = plan(&repo, &abs, "/bin/gh", Intent::Repair, true, false);
        let out = apply(&p);
        assert!(matches!(out, Outcome::Applied { .. }), "{out:?}");

        let agents_md = abs.join("AGENTS.md");
        assert_eq!(
            std::fs::read_to_string(&agents_md).unwrap(),
            amont_runtime::agents_md::generate_block()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&agents_md).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0, "AGENTS.md must not be made executable");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The world can change between the scan a plan was built from and the
    /// moment it is applied — the same reason `is_tracked` re-checks. A repo
    /// that reached the generated block in that window must not be
    /// overwritten (which would be a no-op anyway, but re-checking is what
    /// makes that true rather than assumed).
    #[test]
    fn an_agents_md_that_became_current_between_scan_and_apply_is_left_alone() {
        let (root, hooks) = fixture("agents-md-race");
        healthy_shims(&hooks, "/bin/gh");

        let (repo, abs) = scan_one(&root, "/bin/gh");
        let p = plan(&repo, &abs, "/bin/gh", Intent::Repair, true, false);
        assert!(p.write_agents_md.is_some(), "plan expected a write");

        // Something else won the race and wrote the current block first.
        std::fs::write(
            abs.join("AGENTS.md"),
            amont_runtime::agents_md::generate_block(),
        )
        .unwrap();

        let out = apply(&p);
        assert!(
            matches!(out, Outcome::Applied { written: 0, .. }),
            "must not count a write it skipped: {out:?}"
        );
        assert_eq!(
            std::fs::read_to_string(abs.join("AGENTS.md")).unwrap(),
            amont_runtime::agents_md::generate_block(),
            "must still be exactly the generated block, not doubled or corrupted"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_refused_plan_touches_nothing() {
        let (root, hooks) = fixture("refused");
        // No shim dispatches to the binary, so the repo is unmanaged.
        std::fs::write(hooks.join("pre-commit"), "#!/bin/zsh\necho legacy\n").unwrap();
        let before = std::fs::read_to_string(hooks.join("pre-commit")).unwrap();

        let (repo, abs) = scan_one(&root, "/bin/gh");
        let p = plan(&repo, &abs, "/bin/gh", Intent::Repair, false, false);
        assert_eq!(p.refuse, vec![Refusal::Unmanaged]);
        assert_eq!(apply(&p), Outcome::Refused);
        assert_eq!(
            std::fs::read_to_string(hooks.join("pre-commit")).unwrap(),
            before,
            "an unmanaged repo must be left exactly as found"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Removal happens before writing. Reversing them would leave a retired
    /// file beside its replacement — the double-run state.
    #[test]
    fn removals_precede_writes() {
        let (root, hooks) = fixture("order");
        healthy_shims(&hooks, "/bin/gh");
        std::fs::write(hooks.join("pre-push-old"), STALE_OURS).unwrap();
        let (repo, abs) = scan_one(&root, "/bin/gh");
        let p = plan(&repo, &abs, "/bin/gh", Intent::Repair, false, false);
        let removals: Vec<_> = p.remove.iter().map(|r| r.path.clone()).collect();
        assert!(!removals.is_empty());
        apply(&p);
        for r in removals {
            assert!(!r.exists(), "{} should be gone", r.display());
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn written_shims_are_executable() {
        use std::os::unix::fs::PermissionsExt;
        let (root, hooks) = fixture("mode");
        healthy_shims(&hooks, "/bin/gh");
        std::fs::remove_file(hooks.join("commit-msg")).unwrap();
        let (repo, abs) = scan_one(&root, "/bin/gh");
        apply(&plan(&repo, &abs, "/bin/gh", Intent::Repair, false, false));
        let mode = std::fs::metadata(hooks.join("commit-msg"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o111,
            0o111,
            "git will not run a non-executable hook"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The write-side counterpart to the re-check `apply` already does for a
    /// `remove`. Built by hand rather than through `plan()` — `plan()`
    /// already refuses a write like this at PLAN time, so going through it
    /// would only prove the two layers agree, not that `apply` refuses on
    /// its own the way its own module doc says every action here must.
    #[test]
    fn a_write_that_became_tracked_is_refused_not_overwritten() {
        let dir = std::env::temp_dir().join(format!("apply-tracked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .expect("git");
        };
        git(&["init", "-q", "--template=", "."]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        let target = dir.join("tracked-shim");
        std::fs::write(&target, "original\n").unwrap();
        git(&["add", "tracked-shim"]);
        git(&["commit", "-q", "--no-verify", "-m", "chore: seed"]);
        // The repository has to be one `apply` will still act on after its
        // re-verification pass — which is the point of that pass, and did not
        // exist when this test was written. One real shim makes it managed.
        let hooks = dir.join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::write(hooks.join("pre-commit"), shim::render("/bin/gh")).unwrap();

        let p = FixPlan {
            repo: PathBuf::from("r"),
            repo_abs: dir.clone(),
            intent: Intent::Repair,
            hooks: crate::scan::HooksDir::In {
                path: hooks.clone(),
            },
            refuse: Vec::new(),
            warn: Vec::new(),
            remove: Vec::new(),
            write: vec![WriteShim {
                path: target.clone(),
                baked: "/bin/gh".to_string(),
                changes: true,
            }],
            write_agents_md: None,
        };

        assert_eq!(
            apply(&p),
            Outcome::Failed {
                error: "path is tracked by git".into(),
                at: target.display().to_string(),
            }
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "original\n",
            "a tracked file must never be overwritten by apply"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `Refusal::Unmanaged` is decided from a scan, and a scan is a photograph.
    /// In the TUI it can predate several keystrokes; on the CLI it predates the
    /// preview the user read. A repository that stopped being ours in that
    /// window would have been repaired anyway, because `apply` took the plan's
    /// word for it — and "never adopt a repo" is the rule standing between this
    /// tool and an application's data repository.
    ///
    /// Lives here rather than in `tests/write_safety.rs` for one reason: the
    /// window between plan and apply cannot be opened from outside the process.
    /// `fix --apply` computes and applies in one breath, so a CLI-driven test
    /// could only ever prove that the two layers agree.
    #[test]
    fn a_repo_that_became_unmanaged_between_plan_and_apply_is_refused() {
        let (root, hooks) = fixture("unmanaged-race");
        healthy_shims(&hooks, "/bin/gh");
        std::fs::remove_file(hooks.join("pre-push")).unwrap();

        let (repo, abs) = scan_one(&root, "/bin/gh");
        let p = plan(&repo, &abs, "/bin/gh", Intent::Repair, false, false);
        assert!(!p.is_noop() && !p.refused(), "fixture: {p:?}");

        // Somebody took our shims out — an `uninstall`, a `git clean`, a
        // colleague — after the plan was built.
        for n in shim::DISPATCHERS {
            let _ = std::fs::remove_file(hooks.join(n));
        }
        std::fs::write(hooks.join("pre-commit"), "#!/bin/sh\necho mine\n").unwrap();
        let before = std::fs::read_to_string(hooks.join("pre-commit")).unwrap();

        assert_eq!(apply(&p), Outcome::Refused);
        assert_eq!(
            std::fs::read_to_string(hooks.join("pre-commit")).unwrap(),
            before,
            "a repo that stopped being ours must be left exactly as found"
        );
        assert!(
            !hooks.join("pre-push").exists(),
            "and nothing may be written into it"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The same window, on the activation side. `install` is the mode that
    /// writes four dispatchers into a repository nobody had installed into, and
    /// the check that somebody else's `pre-commit` is not already sitting at one
    /// of those paths ran once, at plan time.
    #[test]
    fn a_dispatcher_that_became_foreign_between_plan_and_apply_is_refused() {
        let (root, hooks) = fixture("foreign-race");

        let (repo, abs) = scan_one(&root, "/bin/gh");
        let p = plan(&repo, &abs, "/bin/gh", Intent::Activate, false, false);
        assert!(!p.is_noop() && !p.refused(), "fixture: {p:?}");

        // Somebody wrote their own hook in the meantime.
        let theirs = "#!/bin/sh\n# my own commit-msg, thanks\nexec my-linter \"$@\"\n";
        std::fs::write(hooks.join("commit-msg"), theirs).unwrap();

        assert_eq!(apply(&p), Outcome::Refused);
        assert_eq!(
            std::fs::read_to_string(hooks.join("commit-msg")).unwrap(),
            theirs,
            "activation must never overwrite a hook somebody wrote"
        );
        assert!(
            !hooks.join("pre-commit").exists(),
            "and one foreign dispatcher suppresses the WHOLE repo, not just itself"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
