//! pre-commit-agents-md — the generated guidance block is behind the binary.
//!
//! `AGENTS.md` carries a block this binary generates: which checks run, the
//! branch contract, the commit style, the two clocks. An agent reads it at
//! the start of a session and believes it for the whole session — so a block
//! written by last month's amont is not stale documentation, it is wrong
//! instructions being followed. Nothing noticed until `amont-fleet` ran.
//!
//! Warn, never block: the file is guidance, and the commit at hand is about
//! something else. With `amont.fix true` it is the same shape as prettier —
//! regenerate, re-stage, say so. Silent when there is no block at all: the
//! block is opt-in, and a repository without one has not opted in.
//!
//! ## Cost
//!
//! Reading two files. The generated block renders the two budgets, which
//! are `git config` reads — but memoised per process, and NOT paid at all
//! unless the file carries our markers: the marker test comes first, before
//! anything is generated, so a repository that never opted in adds no spawn
//! to the commit path.

use std::path::{Path, PathBuf};

use crate::agents_md::{self, CheckResult};
use crate::check::Outcome;
use crate::hooks::common::{fail, fixing_enabled, hl, ok, repo_root, restage, warn, Restaged};

/// The two files, relative to the repository root.
const BLOCK: &str = "AGENTS.md";
const POINTER: &str = "CLAUDE.md";

pub fn run() -> Outcome {
    let root = PathBuf::from(repo_root());
    run_in(&root)
}

/// Does `path` carry our markers at all? Answered from the file alone, so
/// the block is never generated for a repository that has not opted in.
fn has_markers(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|s| s.contains(agents_md::START))
}

pub fn run_in(root: &Path) -> Outcome {
    let block = root.join(BLOCK);
    let pointer = root.join(POINTER);
    if !has_markers(&block) && !has_markers(&pointer) {
        return Outcome::Passed;
    }
    let mut drifted: Vec<&str> = Vec::new();
    for (name, path, result) in [
        (BLOCK, &block, agents_md::check(&block)),
        (POINTER, &pointer, agents_md::check_pointer(&pointer)),
    ] {
        match result {
            Ok(CheckResult::Drifted) => drifted.push(name),
            Ok(CheckResult::MatchesGenerated) | Ok(CheckResult::NotPresent) => {}
            // An unpaired marker is a hand-edit gone wrong; say so, once,
            // and let the commit through — this is guidance, not code.
            Err(why) => {
                warn(&why);
                let _ = path;
            }
        }
    }
    if drifted.is_empty() {
        ok("AGENTS.md matches what this amont generates");
        return Outcome::Passed;
    }
    let names = drifted.join(" and ");
    if fixing_enabled() {
        let mut written: Vec<String> = Vec::new();
        for name in &drifted {
            let result = if *name == BLOCK {
                agents_md::write(&block)
            } else {
                agents_md::write_pointer(&pointer)
            };
            match result {
                Ok(()) => written.push((*name).to_string()),
                Err(why) => {
                    fail(&format!("{name} could not be regenerated: {why}"));
                    return Outcome::Failed;
                }
            }
        }
        match restage(&written) {
            Restaged::Staged => {
                ok(&format!(
                    "{names} regenerated for amont {} and re-staged",
                    env!("CARGO_PKG_VERSION")
                ));
                return Outcome::Fixed;
            }
            Restaged::Failed(stuck) => {
                fail(&format!(
                    "{names} regenerated but {} failed — the index still holds the \
                     stale block: {}",
                    hl("git add"),
                    stuck.join(", ")
                ));
                return Outcome::Failed;
            }
            // Regenerated on disk, but the index already held it (the
            // file was not staged in this commit). Say so as a warning.
            Restaged::Nothing => {}
        }
    }
    warn(&format!(
        "{names} {} behind what amont {} generates — an agent reading it follows \
         last release's instructions. Run {} (or {} and this check does it).",
        if drifted.len() == 1 { "is" } else { "are" },
        env!("CARGO_PKG_VERSION"),
        hl("amont agents-md"),
        hl("git config amont.fix true")
    ));
    Outcome::Warned
}
