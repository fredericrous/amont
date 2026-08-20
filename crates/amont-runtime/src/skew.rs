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

/// The OTHER version skew: a binary older than the repository expects.
///
/// `absorb_newer_hook` above only fires when a shim names a hook this
/// binary has never heard of — a whole-hook gap. A binary one feature
/// behind answers every name and simply lacks a check, silently: dev B
/// does not have the secrets check and NOTHING can tell, because no
/// artifact in the repository says which amont the team means. This is
/// that artifact: `set minVersion 1.9.0` in a trusted `amont.conf` (or
/// plain `amont.minVersion` git config), compared here on every hook run.
///
/// Warn-only, deliberately, on both sides of the doctrine: blocking
/// commits for being out of date teaches `--no-verify`, and a binary too
/// old to KNOW about minVersion cannot honour it anyway — so the floor is
/// advice that gets loudly better with adoption, never a gate that lies
/// about being one.
pub fn announce_minimum() {
    let Some(want) = crate::config::string_value("amont.minVersion") else {
        return;
    };
    let Some(min) = parse_version(&want) else {
        crate::config::complain(
            "amont.minVersion",
            "not a version (want x.y.z)",
            "no minimum",
        );
        return;
    };
    let have = env!("CARGO_PKG_VERSION");
    let running = parse_version(have).expect("own version parses");
    if running < min {
        println!(
            "{} this repository asks for amont {} or newer — this binary is {have}. Upgrade amont.",
            crate::ui::warning_sign(),
            crate::ui::highlight(want.trim()),
        );
    }
}

/// `"1.9"` and `"1.9.0"` are the same version; anything non-numeric is not
/// a version at all.
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let mut parts = s.trim().splitn(3, '.');
    let major = parts.next()?.parse().ok()?;
    let minor = match parts.next() {
        Some(p) => p.parse().ok()?,
        None => 0,
    };
    let patch = match parts.next() {
        Some(p) => p.parse().ok()?,
        None => 0,
    };
    Some((major, minor, patch))
}

/// uninstall: the marker is OUR bookkeeping, gone with the hooks.
pub fn forget() {
    if let Some(path) = marker_path() {
        let _ = std::fs::remove_file(&path);
    }
}

#[cfg(test)]
mod tests {
    use super::parse_version;

    #[test]
    fn short_and_full_spellings_agree() {
        assert_eq!(parse_version("1.9"), parse_version("1.9.0"));
        assert_eq!(parse_version(" 1.9.3 "), Some((1, 9, 3)));
        assert_eq!(parse_version("2"), Some((2, 0, 0)));
    }

    #[test]
    fn ordering_is_numeric_not_lexical() {
        // "1.10.0" < "1.9.0" lexically — the whole reason this parses.
        assert!(parse_version("1.10.0").unwrap() > parse_version("1.9.0").unwrap());
    }

    #[test]
    fn garbage_is_not_a_version() {
        for bad in ["banana", "1.x", "", "v1.2.3", "1.2.3-rc1"] {
            assert_eq!(parse_version(bad), None, "{bad:?}");
        }
    }
}
