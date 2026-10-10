//! Lines a tool prints to say it is waiting on a lock (ADR-0009,
//! `hooks.liveness`): the vocabulary, the framing that finds them in a
//! stream of bytes, and nothing that touches a clock.
//!
//! Two tiers, deliberately. A **marker** is a line the real binary prints,
//! matched exactly, and it pauses the silence clock: the tool has said what
//! it is doing and the clock would otherwise call that stuck. A line that
//! merely **looks like a wait** earns one retry after a silence kill and
//! nothing more, because a loose match on `lock` would also pause the clock
//! for a test suite whose test ids contain the word.

/// Which cargo lock a `Blocking waiting for file lock on …` line names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CargoLockWhat {
    BuildDirectory,
    PackageCache,
    /// A lock cargo names that this vocabulary does not (a future one).
    Other,
}

/// A declared wait: the tool and, where it says, the lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitKind {
    CargoLock(CargoLockWhat),
    UvLock,
}

impl WaitKind {
    /// The wait as a message names it: `the cargo lock on the build
    /// directory`, `the uv lock`.
    pub fn describe(self) -> &'static str {
        match self {
            WaitKind::CargoLock(CargoLockWhat::BuildDirectory) => {
                "the cargo lock on the build directory"
            }
            WaitKind::CargoLock(CargoLockWhat::PackageCache) => {
                "the cargo lock on the package cache"
            }
            WaitKind::CargoLock(CargoLockWhat::Other) => "a cargo lock",
            WaitKind::UvLock => "a uv lock",
        }
    }

    /// The short form the progress region has room for: `cargo lock`.
    pub fn short(self) -> &'static str {
        match self {
            WaitKind::CargoLock(_) => "cargo lock",
            WaitKind::UvLock => "uv lock",
        }
    }

    /// Who holds it, as far as a message can guess.
    pub fn holders(self) -> &'static str {
        match self {
            WaitKind::CargoLock(_) => {
                "another cargo holds it (a second worktree, rust-analyzer, a build in another session)"
            }
            WaitKind::UvLock => "another uv holds it (a second worktree, or an install in another session)",
        }
    }

    /// Stored in an `AtomicU8` on `Activity`; 0 is "none".
    pub fn code(self) -> u8 {
        match self {
            WaitKind::CargoLock(CargoLockWhat::BuildDirectory) => 1,
            WaitKind::CargoLock(CargoLockWhat::PackageCache) => 2,
            WaitKind::CargoLock(CargoLockWhat::Other) => 3,
            WaitKind::UvLock => 4,
        }
    }

    pub fn from_code(code: u8) -> Option<WaitKind> {
        Some(match code {
            1 => WaitKind::CargoLock(CargoLockWhat::BuildDirectory),
            2 => WaitKind::CargoLock(CargoLockWhat::PackageCache),
            3 => WaitKind::CargoLock(CargoLockWhat::Other),
            4 => WaitKind::UvLock,
            _ => return None,
        })
    }
}

/// `line` without its CSI escape sequences (`ESC [ … m` and the rest of the
/// family), so a tool told `CARGO_TERM_COLOR=always` still matches. Only
/// the `ESC [` form: that is what colour is, and a stray lone `ESC` is kept
/// as text rather than guessed at.
pub fn strip_csi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            // Parameter and intermediate bytes, then one final byte in @..~.
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// The exact line a tool prints while it waits on a lock, or `None`.
///
/// cargo (stderr): `    Blocking waiting for file lock on build directory`
/// and `… on package cache` — the status verb is right-aligned, so the
/// line is trimmed first, and the match is anchored at its start so a test
/// that prints the words mid-line is not a wait.
/// uv (stderr): `Waiting to acquire <kind> lock for \`<path>\``.
pub fn marker(line: &str) -> Option<WaitKind> {
    let line = line.trim();
    if let Some(what) = line.strip_prefix("Blocking waiting for file lock on ") {
        return Some(WaitKind::CargoLock(match what.trim() {
            "build directory" => CargoLockWhat::BuildDirectory,
            "package cache" => CargoLockWhat::PackageCache,
            _ => CargoLockWhat::Other,
        }));
    }
    if line.starts_with("Waiting to acquire ") && line.contains(" lock for `") {
        return Some(WaitKind::UvLock);
    }
    None
}

/// Whether a line reads like a wait, loosely: whole phrases only, so a test
/// id such as `test_lock.py::x` or a file named `lock.rs` is not one. The
/// second tier — what earns one retry after a silence kill, never a pause.
pub fn looks_like_wait(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    [
        "waiting for file lock",
        "waiting to acquire",
        "blocking waiting",
        "waiting for lock",
    ]
    .iter()
    .any(|phrase| lower.contains(phrase))
}

/// The longest line the framer keeps while waiting for its newline. A tool
/// that writes more than this without a newline is not printing status
/// lines; the fragment is dropped rather than grown.
pub const MAX_LINE: usize = 4096;

/// Turns a stream of byte chunks into complete lines, one chunk at a time.
/// What the stderr reader hands to [`marker`].
#[derive(Default)]
pub struct LineFramer {
    tail: Vec<u8>,
    /// The current line already overflowed [`MAX_LINE`]: its remainder, up
    /// to the next newline, is dropped too.
    overflowed: bool,
}

impl LineFramer {
    /// The complete lines `chunk` finishes, oldest first, with their newline
    /// (and a trailing `\r`) removed and invalid UTF-8 replaced.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        let mut lines = Vec::new();
        for byte in chunk {
            if *byte == b'\n' {
                if !self.overflowed {
                    let line = String::from_utf8_lossy(&self.tail).into_owned();
                    lines.push(line.trim_end_matches('\r').to_string());
                }
                self.tail.clear();
                self.overflowed = false;
                continue;
            }
            if self.overflowed {
                continue;
            }
            if self.tail.len() >= MAX_LINE {
                self.tail.clear();
                self.overflowed = true;
                continue;
            }
            self.tail.push(*byte);
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUILD_DIR: &str = "    Blocking waiting for file lock on build directory";
    const PACKAGE_CACHE: &str = "    Blocking waiting for file lock on package cache";
    /// The build-directory line as cargo prints it under
    /// `CARGO_TERM_COLOR=always`: the verb in bold cyan.
    const COLOURED: &str =
        "\u{1b}[1m\u{1b}[36m    Blocking\u{1b}[0m waiting for file lock on build directory";
    const UV: &str = "Waiting to acquire write lock for `/home/u/.cache/uv/environments-v2`";

    #[test]
    fn the_real_cargo_and_uv_lines_are_markers() {
        assert_eq!(
            marker(BUILD_DIR),
            Some(WaitKind::CargoLock(CargoLockWhat::BuildDirectory))
        );
        assert_eq!(
            marker(PACKAGE_CACHE),
            Some(WaitKind::CargoLock(CargoLockWhat::PackageCache))
        );
        assert_eq!(
            marker("Blocking waiting for file lock on the git checkouts"),
            Some(WaitKind::CargoLock(CargoLockWhat::Other))
        );
        assert_eq!(marker(UV), Some(WaitKind::UvLock));
    }

    /// A status line, the words mid-line, and a look-alike are not markers.
    #[test]
    fn other_lines_are_not_markers() {
        assert_eq!(marker("    Checking foo v0.1.0"), None);
        assert_eq!(
            marker("test prints Blocking waiting for file lock on build directory"),
            None
        );
        assert_eq!(marker("waiting for lock on x"), None);
        assert_eq!(marker(""), None);
    }

    /// Colour codes come before the verb on a terminal; stripped, the line
    /// is the plain one.
    #[test]
    fn a_coloured_line_matches_once_its_escapes_are_stripped() {
        assert_eq!(marker(COLOURED), None);
        assert_eq!(strip_csi(COLOURED), BUILD_DIR);
        assert_eq!(
            marker(&strip_csi(COLOURED)),
            Some(WaitKind::CargoLock(CargoLockWhat::BuildDirectory))
        );
        assert_eq!(strip_csi("plain"), "plain");
        assert_eq!(strip_csi("\u{1b}[2K\u{1b}[1Gline"), "line");
        // A lone escape is text, not a sequence.
        assert_eq!(strip_csi("a\u{1b}b"), "a\u{1b}b");
    }

    /// The loose tier accepts phrases and rejects the bare word.
    #[test]
    fn looks_like_wait_matches_phrases_not_words() {
        assert!(looks_like_wait("waiting for lock on x"));
        assert!(looks_like_wait(BUILD_DIR));
        assert!(looks_like_wait("Waiting to acquire write lock"));
        assert!(looks_like_wait("BLOCKING WAITING on the index"));
        assert!(!looks_like_wait("test_lock.py::test_it PASSED"));
        assert!(!looks_like_wait("src/lock.rs: 3 warnings"));
        assert!(!looks_like_wait("lock"));
    }

    /// The retry fixture's line sits exactly between the tiers.
    #[test]
    fn the_look_alike_line_is_no_marker() {
        let line = "waiting for lock on x";
        assert_eq!(marker(line), None);
        assert!(looks_like_wait(line));
    }

    /// A marker split across two reads is found when its newline arrives;
    /// the framer keeps nothing of a finished line.
    #[test]
    fn framing_joins_a_line_split_across_chunks() {
        let mut f = LineFramer::default();
        assert_eq!(f.feed(b"    Blocking waiting for"), Vec::<String>::new());
        assert_eq!(
            f.feed(b" file lock on build directory\n    Check"),
            vec![BUILD_DIR.to_string()]
        );
        assert_eq!(f.feed(b"ing foo\r\n"), vec!["    Checking foo".to_string()]);
        assert_eq!(f.feed(b"a\nb\n"), vec!["a".to_string(), "b".to_string()]);
    }

    /// A line longer than the cap is dropped whole, including the part
    /// that arrives after the cap, and the framer does not grow.
    #[test]
    fn a_line_past_the_cap_is_dropped_not_grown() {
        let mut f = LineFramer::default();
        let long = vec![b'x'; 5 * 1024];
        assert_eq!(f.feed(&long), Vec::<String>::new());
        assert!(f.tail.len() <= MAX_LINE);
        assert_eq!(f.feed(b"tail\nnext\n"), vec!["next".to_string()]);
    }

    #[test]
    fn kinds_round_trip_through_their_code() {
        for kind in [
            WaitKind::CargoLock(CargoLockWhat::BuildDirectory),
            WaitKind::CargoLock(CargoLockWhat::PackageCache),
            WaitKind::CargoLock(CargoLockWhat::Other),
            WaitKind::UvLock,
        ] {
            assert_eq!(WaitKind::from_code(kind.code()), Some(kind));
        }
        assert_eq!(WaitKind::from_code(0), None);
    }
}
