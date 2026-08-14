//! The status line the non-TUI commands keep on screen while they walk.
//!
//! `tui` already streams: rows appear as repositories are found, because a
//! spinner over a blank screen for seven seconds is not "visibility of system
//! status". The plain commands did not. `scan`, `fix`, `install` and
//! `uninstall` all begin with the same walk — one `git rev-parse` and one
//! `git ls-files` per repository, over as many repositories as the root holds
//! — and until now they printed ONE line saying the scan had started and then
//! nothing at all until it was over. A tool that says "scanning …" and goes
//! quiet for seven seconds is indistinguishable, from the outside, from one
//! that has hung, and "is it doing anything" is the question a fleet tool must
//! never raise.
//!
//! So this draws a single line, in place, carrying the two things a waiting
//! reader actually needs: that time is passing (the frame advances, the clock
//! runs) and WHAT is being looked at right now. The second is the one a
//! spinner alone cannot give — when a scan stalls, a count that stops moving
//! says something is slow, and the directory says *what* is slow.
//!
//! Three properties are load-bearing, and each is a test below:
//!
//!   * **Silent unless a person is watching.** Everything here is gated on
//!     stderr being a terminal. `--json` consumers read stdout and get exactly
//!     one document; a script capturing stderr gets nothing decorative; CI
//!     logs do not collect ten thousand spinner frames.
//!   * **Never wider than the terminal.** A line that wraps stops being one
//!     line, and an in-place redraw then leaves a trail of orphaned rows
//!     behind it instead of updating itself.
//!   * **Nothing from the disk reaches the terminal unescaped.** Every path on
//!     this line was found by walking directories somebody else owns, and a
//!     directory may be named with a CR or an ESC in it — on a line that is
//!     already using CR and CSI to redraw itself.

use std::io::{IsTerminal, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::scan::Progress;

/// Braille, advanced by elapsed time rather than by frame count: the walk
/// calls back at whatever rate the filesystem allows, so counting callbacks
/// would spin the frame at a speed that says more about directory sizes than
/// about progress.
const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// How often the line is allowed to be redrawn.
///
/// A repaint per directory entry would put more work in the terminal than in
/// the walk — and on a fleet-sized tree, more bytes on stderr than the report
/// itself. 12 frames a second is well above the ~100ms at which motion reads
/// as continuous and far below the callback rate.
const REPAINT: Duration = Duration::from_millis(80);

/// Fallback when the terminal will not say how wide it is. Every terminal is
/// at least this wide in practice, and guessing narrow only truncates a path.
const FALLBACK_WIDTH: usize = 80;

pub struct Bar {
    /// `None` when nothing is watching. The whole type is then a no-op, which
    /// is why the gate is a field rather than a check at every call site.
    live: Option<Live>,
}

struct Live {
    started: Instant,
    painted: Instant,
    dirs: usize,
    repos: usize,
    /// The last path seen, root-relative, raw. Sanitised at paint time by
    /// [`line`] rather than here, so the escaping cannot be forgotten by a
    /// future caller that assigns this field directly.
    current: String,
}

impl Bar {
    /// Announce the walk and start drawing.
    ///
    /// The announcement is a real line, left on screen: it names the root and
    /// the depth, which is exactly the pair a reader needs when the answer
    /// turns out to be "no repositories found" — see `looks_like_a_failed_scan`.
    /// The live line below it is transient and is erased by [`finish`].
    pub fn start(root: &Path, depth: usize) -> Bar {
        if !watching() {
            return Bar { live: None };
        }
        eprintln!(
            "  scanning {} · depth {} …",
            amont_runtime::ui::sanitize_path(root),
            depth
        );
        let now = Instant::now();
        let mut bar = Bar {
            live: Some(Live {
                started: now,
                // Back-dated by one interval so the first callback paints
                // immediately instead of after 80ms of blank.
                painted: now - REPAINT,
                dirs: 0,
                repos: 0,
                current: String::new(),
            }),
        };
        bar.paint();
        bar
    }

    /// Fold one walk event into the line.
    pub fn update(&mut self, p: Progress) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        match p {
            Progress::Visited { count, dir } => {
                live.dirs = count;
                live.current = dir.display().to_string();
            }
            Progress::Found(r) => {
                live.repos += 1;
                live.current = r.path.display().to_string();
            }
        }
        if live.painted.elapsed() >= REPAINT {
            self.paint();
        }
    }

    /// Erase the line. Called before anything is printed to stdout, so the
    /// report never lands on top of a half-drawn frame.
    ///
    /// Idempotent, and `Drop` calls it too: the alternative is an early return
    /// somewhere in `main` leaving a spinner as the last thing on the user's
    /// screen.
    pub fn finish(&mut self) {
        if self.live.take().is_some() {
            let mut err = std::io::stderr();
            let _ = write!(err, "\r\u{1b}[K");
            let _ = err.flush();
        }
    }

    fn paint(&mut self) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        live.painted = Instant::now();
        let secs = live.started.elapsed().as_secs_f64();
        let frame = FRAMES[((secs * 10.0) as usize) % FRAMES.len()];
        let s = line(frame, live.dirs, live.repos, secs, &live.current, width());
        let mut err = std::io::stderr();
        // CR home, then CSI K to erase what a previous — possibly longer —
        // frame left there. Erasing rather than padding to the full width:
        // padding writes a character into the last column, which on most
        // terminals is what triggers the wrap this whole function is avoiding.
        let _ = write!(err, "\r\u{1b}[K{s}");
        let _ = err.flush();
    }
}

impl Drop for Bar {
    fn drop(&mut self) {
        self.finish();
    }
}

/// The same line, for a phase whose SIZE is already known.
///
/// `fix` and `install` follow the scan with a planning pass that asks git,
/// per repository, whether each hook path is tracked before anything may be
/// written — a spawn or two per repository, which on a fleet is several
/// seconds of dead air between the scan line and the first apply line. Same
/// hang-shaped silence the scan bar exists to end, one phase later. The
/// denominator is the one thing this has that [`Bar`] cannot: the walk
/// discovers its total, a phase over scanned repositories starts with it,
/// so the line can say how far along it is instead of only how fast it goes.
pub struct Steps {
    /// `None` when nothing is watching — same gate, same reason as [`Bar`].
    live: Option<StepsLive>,
}

struct StepsLive {
    verb: &'static str,
    started: Instant,
    painted: Instant,
    done: usize,
    total: usize,
    /// Raw; sanitised at paint time by [`fit`], like [`Live::current`].
    current: String,
}

impl Steps {
    /// Start drawing. Unlike [`Bar::start`] there is no announcement line:
    /// the phase lives between two reports and leaves nothing behind.
    pub fn start(verb: &'static str, total: usize) -> Steps {
        if total == 0 || !watching() {
            return Steps { live: None };
        }
        let now = Instant::now();
        let mut steps = Steps {
            live: Some(StepsLive {
                verb,
                started: now,
                // Back-dated by one interval so the first step paints
                // immediately instead of after 80ms of blank.
                painted: now - REPAINT,
                done: 0,
                total,
                current: String::new(),
            }),
        };
        steps.paint();
        steps
    }

    /// One item entered the phase. Called BEFORE the work, so the line names
    /// what is being worked on now — when a repository stalls the pass, the
    /// path on screen is the one to blame.
    pub fn step(&mut self, current: &Path) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        live.done += 1;
        live.current = current.display().to_string();
        if live.painted.elapsed() >= REPAINT {
            self.paint();
        }
    }

    /// Erase the line — idempotent, and `Drop` calls it too, for the same
    /// reason [`Bar::finish`] exists: an early return must not leave a
    /// spinner as the last thing on screen.
    pub fn finish(&mut self) {
        if self.live.take().is_some() {
            let mut err = std::io::stderr();
            let _ = write!(err, "\r\u{1b}[K");
            let _ = err.flush();
        }
    }

    fn paint(&mut self) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        live.painted = Instant::now();
        let secs = live.started.elapsed().as_secs_f64();
        let frame = FRAMES[((secs * 10.0) as usize) % FRAMES.len()];
        let s = step_line(
            frame,
            live.verb,
            live.done,
            live.total,
            secs,
            &live.current,
            width(),
        );
        let mut err = std::io::stderr();
        let _ = write!(err, "\r\u{1b}[K{s}");
        let _ = err.flush();
    }
}

impl Drop for Steps {
    fn drop(&mut self) {
        self.finish();
    }
}

/// One counted frame: `⠋ planning 42/185 · 1.2s  Perso/some/repo`. Pure,
/// like [`line`], and sharing its tail through [`fit`] so the width and
/// sanitisation guarantees are one implementation, not two.
fn step_line(
    frame: char,
    verb: &str,
    done: usize,
    total: usize,
    secs: f64,
    current: &str,
    width: usize,
) -> String {
    if width == 0 {
        return String::new();
    }
    let head = format!("  {frame} {verb} {done}/{total} · {secs:.1}s  ");
    fit(head, current, width)
}

/// Is there a person on the other end of stderr?
///
/// `TERM=dumb` is part of the question, not a separate one: a dumb terminal is
/// attached to a human but cannot act on CR-and-erase, so animating at it
/// would print every frame as its own line — thousands of them, burying the
/// report. It gets the announcement from [`Bar::start`] and nothing more.
fn watching() -> bool {
    std::io::stderr().is_terminal() && std::env::var("TERM").map(|t| t != "dumb").unwrap_or(true)
}

fn width() -> usize {
    // Asked per paint rather than once: a terminal can be resized mid-scan,
    // and a stale width is exactly the wrap this is here to prevent. It is an
    // ioctl at most a dozen times a second.
    //
    // ZERO IS NOT A WIDTH, and this cost the first smoke test of this module:
    // run under `script` from a session with no controlling terminal, the pty
    // is created 0x0, the ioctl SUCCEEDS, and every frame rendered as the empty
    // string — a whole scan of erase sequences with nothing between them, which
    // is the exact silence this module exists to end. A successful answer of 0
    // means "I do not know", the same as an error, so it takes the same branch.
    match crossterm::terminal::size() {
        Ok((cols, _)) if cols > 0 => cols as usize,
        _ => FALLBACK_WIDTH,
    }
}

fn plural(n: usize, one: &'static str, many: &'static str) -> &'static str {
    if n == 1 {
        one
    } else {
        many
    }
}

/// One frame, at most `width` CHARACTERS wide.
///
/// Characters, not bytes: truncating a UTF-8 path by bytes splits a codepoint,
/// and the counts are approximate for wide glyphs either way — a CJK path name
/// is undercounted here, which errs toward a shorter line rather than a
/// wrapped one.
///
/// The counts win over the path. When the terminal is too narrow for both, the
/// path is what goes: "1240 dirs · 84 repos · 3.2s" still says the walk is
/// alive, while a path with no counts beside it says nothing about progress.
///
/// The path is truncated from the LEFT. `Perso/group/project/sub` cut to its
/// head is `Perso/group/…`, which is the part every sibling repository shares;
/// cut to its tail it is `…/project/sub`, which is the part that identifies it.
fn line(frame: char, dirs: usize, repos: usize, secs: f64, current: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    // Abbreviated, unlike the report's "directories visited" / "git
    // repositories": every character here is one the path does not get, and on
    // an 80-column terminal the long spellings eat a third of the line. The
    // words are spelled out in full the moment the walk ends and the report
    // prints them.
    let head = format!(
        "  {frame} {dirs} {} · {repos} {} · {secs:.1}s  ",
        plural(dirs, "dir", "dirs"),
        plural(repos, "repo", "repos"),
    );
    fit(head, current, width)
}

/// The shared back half of every frame: the head wins outright, then the
/// path takes what is left, cut from the LEFT so it keeps the part that
/// identifies a repository rather than the part every sibling shares.
///
/// Sanitised HERE, at the last moment before it is written, so no caller
/// can hand this module a raw path and have it reach the terminal. The
/// escaping happens before the truncation because it changes the length:
/// one ESC becomes the four characters `\x1b`.
fn fit(head: String, current: &str, width: usize) -> String {
    let head_len = head.chars().count();
    if head_len >= width {
        return head.chars().take(width).collect();
    }
    let current = amont_runtime::ui::sanitize(current);
    let room = width - head_len;
    let len = current.chars().count();
    if len <= room {
        return format!("{head}{current}");
    }
    // One character of the budget goes to the ellipsis that says it was cut.
    let tail: String = current.chars().skip(len - room.saturating_sub(1)).collect();
    format!("{head}…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line wider than the terminal wraps, and an in-place redraw of a
    /// wrapped line leaves the first row behind — one orphaned spinner frame
    /// per repaint, hundreds of them by the end of a scan.
    #[test]
    fn a_frame_never_exceeds_the_width() {
        let long = "Perso/some/deeply/nested/group/project/with-a-long-name/sub";
        for width in [0, 1, 5, 20, 39, 40, 41, 80, 200] {
            for (dirs, repos) in [(0, 0), (1240, 84), (999_999, 12_345)] {
                let s = line('⠋', dirs, repos, 12.75, long, width);
                assert!(
                    s.chars().count() <= width,
                    "width {width}: {:?} is {} chars",
                    s,
                    s.chars().count()
                );
            }
        }
    }

    /// The counts are the progress; the path is the detail. A terminal too
    /// narrow for both keeps the one that says the walk is alive.
    #[test]
    fn the_counts_outrank_the_path() {
        let s = line('⠋', 1240, 84, 3.25, "some/repo", 34);
        assert!(s.contains("1240 dirs"), "{s:?}");
        assert!(s.contains("84 repos"), "{s:?}");
        assert!(s.contains("3.2s"), "{s:?}");
        assert!(!s.contains("some/repo"), "the path should have gone: {s:?}");
    }

    /// The first frames of every scan are the ones at 1, and "1 dirs · 1
    /// repos" is the first thing a reader sees this tool print.
    #[test]
    fn one_of_something_is_singular() {
        let s = line('⠋', 1, 1, 0.1, "", 80);
        assert!(s.contains("1 dir ·"), "{s:?}");
        assert!(s.contains("1 repo ·"), "{s:?}");
        let s = line('⠋', 0, 2, 0.1, "", 80);
        assert!(s.contains("0 dirs ·"), "{s:?}");
        assert!(s.contains("2 repos ·"), "{s:?}");
    }

    /// Cut a path from the head and every sibling looks identical; cut it from
    /// the tail and it stops naming a repository at all.
    #[test]
    fn a_long_path_keeps_its_tail() {
        let s = line('⠋', 1, 1, 1.0, "a/very/long/path/to/the-repo", 40);
        assert!(s.ends_with("the-repo"), "{s:?}");
        assert!(s.contains('…'), "the cut must be visible: {s:?}");
        assert!(s.chars().count() <= 40, "{s:?}");
    }

    /// A short path is shown whole, with no ellipsis to suggest otherwise.
    #[test]
    fn a_short_path_is_shown_whole() {
        let s = line('⠋', 3, 1, 0.5, "Perso/amont", 80);
        assert!(s.ends_with("Perso/amont"), "{s:?}");
        assert!(!s.contains('…'), "nothing was cut: {s:?}");
    }

    /// Every path here came off a walk of somebody else's disk. This line is
    /// drawn with CR and CSI K — a directory named with a CR in it would redraw
    /// the line from the left mid-frame, and one with an ESC in it could move
    /// the cursor anywhere on the screen. `git init $'\e[2J'` is a legal
    /// repository name.
    #[test]
    fn a_hostile_directory_name_cannot_reach_the_terminal() {
        let s = line('⠋', 1, 1, 1.0, "evil\u{1b}[2Jname\rhere", 80);
        assert!(!s.contains('\u{1b}'), "an escape survived: {s:?}");
        assert!(!s.contains('\r'), "a carriage return survived: {s:?}");
        // Escaped, not dropped: the reader still sees that the name is odd.
        assert!(s.contains("\\x1b"), "{s:?}");
        assert!(s.contains("\\x0d"), "{s:?}");
    }

    /// Escaping lengthens the string — `\x1b` is four characters where the ESC
    /// was one — so a name that fitted before sanitising need not fit after.
    /// Truncating first would have made the width guarantee a lie for exactly
    /// the inputs that most need it.
    #[test]
    fn escaping_happens_before_the_width_is_enforced() {
        let hostile = "\u{1b}\u{1b}\u{1b}\u{1b}\u{1b}\u{1b}\u{1b}\u{1b}\u{1b}\u{1b}";
        for width in [20, 30, 44, 60] {
            let s = line('⠋', 1, 1, 1.0, hostile, width);
            assert!(s.chars().count() <= width, "width {width}: {s:?}");
            assert!(!s.contains('\u{1b}'), "{s:?}");
        }
    }

    /// A counted frame carries its denominator — the whole reason [`Steps`]
    /// exists over reusing [`Bar`] — and the tail keeps [`line`]'s rules.
    #[test]
    fn a_counted_frame_shows_its_denominator() {
        let s = step_line('⠋', "planning", 42, 185, 1.25, "Perso/some/repo", 80);
        assert!(s.contains("planning 42/185"), "{s:?}");
        assert!(s.contains("1.2s"), "{s:?}");
        assert!(s.ends_with("Perso/some/repo"), "{s:?}");
    }

    /// Same width guarantee as [`line`], through the same [`fit`].
    #[test]
    fn a_counted_frame_never_exceeds_the_width() {
        let long = "Perso/some/deeply/nested/group/project/with-a-long-name/sub";
        for width in [0, 1, 5, 20, 40, 80, 200] {
            let s = step_line('⠋', "planning", 184, 185, 12.75, long, width);
            assert!(
                s.chars().count() <= width,
                "width {width}: {:?} is {} chars",
                s,
                s.chars().count()
            );
        }
    }

    /// Same trust boundary as [`line`]: the paths here come off the same
    /// walk of somebody else's disk.
    #[test]
    fn a_hostile_path_cannot_reach_a_counted_frame() {
        let s = step_line('⠋', "planning", 1, 2, 1.0, "evil\u{1b}[2Jname\rhere", 80);
        assert!(!s.contains('\u{1b}'), "{s:?}");
        assert!(!s.contains('\r'), "{s:?}");
        assert!(s.contains("\\x1b"), "escaped, not dropped: {s:?}");
    }

    /// An empty phase draws nothing and a disabled one is inert — the paths
    /// every piped invocation and every empty fleet take.
    #[test]
    fn an_empty_or_unwatched_phase_is_inert() {
        let mut steps = Steps { live: None };
        steps.step(Path::new("x"));
        steps.finish();
        steps.finish();
        assert!(Steps::start("planning", 0).live.is_none());
    }

    /// Nothing is watching, so nothing is drawn — and `update` must not panic
    /// or touch stderr on the path every piped invocation takes.
    #[test]
    fn a_disabled_bar_is_inert() {
        let mut bar = Bar { live: None };
        bar.update(Progress::Visited {
            count: 1,
            dir: Path::new("x"),
        });
        bar.finish();
        bar.finish();
    }
}
