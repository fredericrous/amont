//! A shim newer than its binary — the one version skew an installed fleet
//! actually meets.
//!
//! Shims and binary ship together but upgrade separately: a template
//! directory refreshed by a newer amont bakes its full shim set into every
//! `git init`, while the binary on PATH can lag releases behind. Each shim
//! passes its own filename, and a binary that had never heard the name used
//! to answer `unknown hook` at exit 2 — on EVERY commit, in every fresh
//! repository, which reads as breakage when it is only age. (Observed live:
//! the day post-commit shipped, every machine with an older binary said it
//! on every commit.)
//!
//! The graceful reading: in hook mode an unknown name is not a usage error,
//! it is a message from the future. Say so ONCE per binary version per
//! repository, name the fix, and exit 0. Fail-open is the safe direction
//! for the same reason it is at the gate — a hook this binary does not know
//! is a hook that does not exist yet, git runs hooks that do not exist by
//! not running them, and anything the missing hook would have recorded or
//! enforced returns the moment the binary catches up. Blocking commits
//! because the binary is old would teach exactly the `--no-verify` habit
//! this project exists to unteach.
//!
//! Once-per-version, not once-per-commit, via a marker in `$GIT_DIR` —
//! versioned like its siblings (`amont-gate`, `amont-bypasses`), removed by
//! `amont uninstall`, harmlessly stale after an upgrade (a NEW version that
//! still does not know some hook warns afresh, which is correct).

use std::path::PathBuf;

/// First line of the marker. Bump on shape change; an old marker then reads
/// as "not warned yet", which only repeats one line.
pub const FORMAT: &str = "amont-skew-v1";

/// The marker's filename inside `$GIT_DIR` — worktree-private, like the
/// gate marker: the warning belongs where the commits happen.
const MARKER: &str = "amont-skew";

/// Hook mode met a name this binary does not know. Warn once per binary
/// version per repository, then absorb silently. Always returns exit 0.
pub fn absorb_newer_hook(hook: &str) -> i32 {
    let version = env!("CARGO_PKG_VERSION");
    if !already_warned(version) {
        eprintln!(
            "amont: the {hook} shim is newer than this binary ({version}) — the hook did nothing.\n\
             Upgrade amont to match the shims (said once per binary version)."
        );
        remember(version);
    }
    0
}

fn marker_path() -> Option<PathBuf> {
    let dir = crate::git::stdout(&["rev-parse", "--git-dir"])?;
    Some(std::path::Path::new(&dir).join(MARKER))
}

fn already_warned(version: &str) -> bool {
    let Some(path) = marker_path() else {
        return false; // outside a repository: warn, remember nothing
    };
    let Ok(body) = std::fs::read_to_string(&path) else {
        return false;
    };
    let mut lines = body.lines();
    lines.next() == Some(FORMAT) && lines.next() == Some(version)
}

fn remember(version: &str) {
    let Some(path) = marker_path() else { return };
    let _ = std::fs::write(&path, format!("{FORMAT}\n{version}\n"));
}

/// uninstall: the marker is OUR bookkeeping, gone with the hooks.
pub fn forget() {
    if let Some(path) = marker_path() {
        let _ = std::fs::remove_file(&path);
    }
}
