//! pre-commit-package-lock — keep package.json and its lockfile in step.
//!
//! Scoped per directory: a package.json that is not a real npm project (no
//! lockfile beside it — e.g. the `.git/hooks/package.json` type-marker) never
//! demands one, and in a monorepo one project's lockfile does not satisfy
//! another's.
//!
//! ## Deliberately non-interactive
//!
//! This check runs on one of up to twenty WORKER THREADS in pre-commit's
//! concurrent fan-out. It used to call `trust::confirm`, which blocks on
//! `read_line` from `/dev/tty` — so the whole commit stopped dead while the
//! other nineteen checks went on printing over the prompt, `thread::scope`
//! refused to return until somebody answered, and the question itself was
//! usually scrolled off the screen. The commit simply looked hung.
//!
//! A pre-scan phase would mean teaching the generic `&[&dyn Check]` fan-out
//! about "checks that may ask a question", for exactly one check — and
//! `confirm()` already returns false without a tty, so the interactive path was
//! the exception rather than the rule. So there is no question: a forgotten
//! lockfile FAILS, and the message names the two documented ways past it.
//!
//! `trust::confirm` itself stays; `install.rs` calls it from the
//! single-threaded install path, where a prompt is the whole point.

use super::common::{fail, hl, ok, repo_root, staged_files, warn};
use crate::check::Outcome;
use std::path::Path;

fn dir_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => ".",
    }
}

fn sibling(dir: &str, name: &str) -> String {
    if dir == "." {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

pub struct Verdict {
    /// package.json staged, a real lockfile exists on disk but is not staged.
    pub forgot_lock: Vec<String>,
    /// lockfile staged without its package.json.
    pub orphan_lock: Vec<String>,
}

pub fn classify(staged: &[String], lock_exists: impl Fn(&str) -> bool) -> Verdict {
    let is_staged = |p: &str| staged.iter().any(|s| s == p);
    let mut v = Verdict {
        forgot_lock: Vec::new(),
        orphan_lock: Vec::new(),
    };
    for f in staged {
        let dir = dir_of(f);
        let base = f.rsplit('/').next().unwrap_or(f);
        match base {
            "package.json" => {
                let lock = sibling(dir, "package-lock.json");
                if is_staged(&lock) {
                    continue; // both staged → in sync
                }
                if lock_exists(&lock) {
                    v.forgot_lock.push(f.clone());
                }
            }
            "package-lock.json" => {
                let pkg = sibling(dir, "package.json");
                if !is_staged(&pkg) {
                    v.orphan_lock.push(f.clone());
                }
            }
            _ => {}
        }
    }
    v
}

/// Report, and never ask — see the module doc for why.
pub fn run(settings: &crate::config::Settings, _args: &[std::ffi::OsString]) -> Outcome {
    let staged = staged_files(&[]);
    let root = repo_root();
    let v = classify(&staged, |lock| Path::new(&root).join(lock).is_file());

    if v.forgot_lock.is_empty() && v.orphan_lock.is_empty() {
        ok(settings, "package.json & package-lock.json look in sync");
        return Outcome::Passed;
    }
    for f in &v.orphan_lock {
        fail(&format!("{} staged without its package.json", hl(f)));
    }
    for f in &v.forgot_lock {
        warn(&format!(
            "{} changed but its package-lock.json is not staged",
            hl(f)
        ));
    }

    // Two documented ways past this, and the first is the one-time replacement
    // for answering "y" on every commit: a severity downgrade keeps the signal
    // and removes only the block, per-repository and visible in the dashboard.
    fail(&format!(
        "Run {} and stage the lockfile. To stop this blocking, {}; \
         to bypass once, {}",
        hl("npm install"),
        hl("git config amont.severity.package-lock warn"),
        hl("git -c hook.skip=package-lock commit")
    ));
    Outcome::Failed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str], locks: &[&str]) -> Verdict {
        let staged: Vec<String> = s.iter().map(|x| x.to_string()).collect();
        let locks: Vec<String> = locks.iter().map(|x| x.to_string()).collect();
        classify(&staged, move |l| locks.iter().any(|x| x == l))
    }

    #[test]
    fn both_staged_is_in_sync() {
        let r = v(
            &["package.json", "package-lock.json"],
            &["package-lock.json"],
        );
        assert!(r.forgot_lock.is_empty() && r.orphan_lock.is_empty());
    }

    #[test]
    fn package_json_alone_with_a_real_lock_on_disk_is_a_forgotten_lock() {
        let r = v(&["package.json"], &["package-lock.json"]);
        assert_eq!(r.forgot_lock, vec!["package.json".to_string()]);
    }

    /// The `.git/hooks/package.json` type-marker case: no lockfile beside it,
    /// so it is not an npm project and must not demand one.
    #[test]
    fn package_json_without_a_lock_on_disk_demands_nothing() {
        let r = v(&["package.json"], &[]);
        assert!(r.forgot_lock.is_empty() && r.orphan_lock.is_empty());
    }

    #[test]
    fn a_lock_without_its_package_json_is_an_orphan() {
        let r = v(&["package-lock.json"], &["package-lock.json"]);
        assert_eq!(r.orphan_lock, vec!["package-lock.json".to_string()]);
    }

    /// In a monorepo, one project's lockfile does not satisfy another's.
    #[test]
    fn scoping_is_per_directory() {
        let r = v(
            &["apps/a/package.json", "apps/b/package-lock.json"],
            &["apps/a/package-lock.json", "apps/b/package-lock.json"],
        );
        assert_eq!(r.forgot_lock, vec!["apps/a/package.json".to_string()]);
        assert_eq!(r.orphan_lock, vec!["apps/b/package-lock.json".to_string()]);
    }
}
