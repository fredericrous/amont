//! The generated guidance block, checked at the moment it is about to be
//! believed.
//!
//! `pre-commit-agents-md` says at commit time that `AGENTS.md` is behind the
//! binary. A session starts earlier than that, and an agent reads the file
//! at the start and follows it for the whole session — "give commits a
//! ten-minute timeout" from a block two releases old is wrong before any
//! commit happens. So the same question is asked here, once, when the
//! session opens: is the block in this checkout the one this amont would
//! write?
//!
//! Reading two files; no process. Silent when there is no block (opt-in),
//! when it matches, and when `amont.agent.agentsMdNotice` is `false` — the
//! same kill switches as everything else in this crate apply above it.

use std::path::Path;

use amont_runtime::agents_md::{self, CheckResult};
use amont_runtime::config;

/// `amont.agent.agentsMdNotice` — whether a session opening may mention a
/// stale guidance block. Boolean rather than a stance: there is nothing to
/// deny, and nothing to measure a rate of.
pub const KEY_NOTICE: &str = "amont.agent.agentsMdNotice";

pub fn notice(cwd: &Path) -> Option<String> {
    if crate::stance::switched_off() || !config::boolean_or(crate::stance::KEY_ENABLED, true) {
        return None;
    }
    let root = amont_runtime::git::stdout_in(cwd, &["rev-parse", "--show-toplevel"])?;
    let root = Path::new(&root);
    let block = root.join("AGENTS.md");
    let pointer = root.join("CLAUDE.md");
    let has_markers =
        |p: &Path| std::fs::read_to_string(p).is_ok_and(|s| s.contains(agents_md::START));
    if !has_markers(&block) && !has_markers(&pointer) {
        return None;
    }
    if !config::boolean_or(KEY_NOTICE, true) {
        return None;
    }
    let mut stale: Vec<&str> = Vec::new();
    if matches!(agents_md::check(&block), Ok(CheckResult::Drifted)) {
        stale.push("AGENTS.md");
    }
    if matches!(agents_md::check_pointer(&pointer), Ok(CheckResult::Drifted)) {
        stale.push("CLAUDE.md");
    }
    if stale.is_empty() {
        return None;
    }
    Some(format!(
        "amont-agent/agents-md: {} in this repository {} behind the block amont {} \
         generates — the hook list, budgets and conventions it states may be last \
         release's. `amont agents-md` regenerates it (commit the result); until then, \
         `amont list --json` is the current answer.",
        stale.join(" and "),
        if stale.len() == 1 { "is" } else { "are" },
        env!("CARGO_PKG_VERSION")
    ))
}
