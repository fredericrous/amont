//! The signs the hooks print.
//!
//! **Colour is the terminal's decision, not ours.** These used to be 256-colour
//! codes — `38;5;112`, `38;5;160`, `38;5;208` — which live in the fixed xterm
//! cube. A terminal theme remaps only indices 0–15; anything above renders
//! identically whatever palette the user chose. So a carefully themed terminal
//! was being overridden by three numbers picked years ago.
//!
//! The base ANSI colours ARE theme-controlled, so `32` means "whatever this
//! terminal calls green". That is the whole fix: output follows a `vivid`
//! palette, a Solarized profile or a high-contrast one, without reading any
//! configuration.
//!
//! `LS_COLORS` is deliberately NOT consulted. It maps FILE TYPES — `di`, `ex`,
//! `mi` — while this output needs ok/warning/error. There is no honest
//! correspondence, and the nearest candidates are worse than nothing: under
//! `vivid lava`, `mi` and `or` are dark grey on red, which as a foreground for
//! `✗` is close to invisible.
//!
//! The earlier note here said the signs were "kept byte-identical" with the zsh
//! originals so a user could not tell which implementation ran. That mattered
//! during the migration and no longer does — no zsh hook remains to match.

use std::sync::OnceLock;

/// Base ANSI, deliberately. See the module docs.
const GREEN: &str = "32";
const RED: &str = "31";
const YELLOW: &str = "33";

/// Does the caller want colour at all?
///
/// Only when stdout — where every sign and highlighted fragment goes — is a
/// terminal (decisions ADR-0021, `cli.output.no-color-off-terminal`): escape
/// codes in a CI log, a file, or a string an agent captured are noise the
/// reader has to strip. Under a git hook stdout is git's stderr, which is the
/// terminal when a person commits. The cursor-moving region on stderr has its
/// own gate, `live::watching`.
///
/// `NO_COLOR` per no-color.org: present and NON-EMPTY disables it, whatever the
/// value. `TERM=dumb` is a terminal that cannot render SGR. `FORCE_COLOR`
/// (non-empty, not `0` or `false`) turns colour on through a pipe, for a
/// caller that will render it; `NO_COLOR` and `TERM=dumb` still win.
///
/// Read once — a hook is a short-lived process and its environment does not
/// change underneath it.
pub fn colors_enabled() -> bool {
    use std::io::IsTerminal;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        wants_colour(&ColourEnv {
            no_color: std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()),
            dumb: std::env::var("TERM").is_ok_and(|t| t == "dumb"),
            force: std::env::var("FORCE_COLOR").is_ok_and(|v| forces(&v)),
            terminal: std::io::stdout().is_terminal(),
        })
    })
}

/// What [`colors_enabled`] decides from: read once from the environment, and
/// passed in so the decision is testable without touching the process's.
struct ColourEnv {
    no_color: bool,
    dumb: bool,
    force: bool,
    terminal: bool,
}

fn wants_colour(env: &ColourEnv) -> bool {
    if env.no_color || env.dumb {
        return false;
    }
    env.force || env.terminal
}

/// `FORCE_COLOR` as the ecosystem reads it: set and not empty, `0` or `false`.
fn forces(value: &str) -> bool {
    !matches!(value.trim(), "" | "0" | "false")
}

fn paint(text: &str, sgr: &str) -> String {
    if colors_enabled() {
        format!("\u{1b}[{sgr}m{text}\u{1b}[0m")
    } else {
        text.to_string()
    }
}

fn sign(glyph: &str, sgr: &str) -> String {
    format!("  {}", paint(glyph, sgr))
}

/// The glyph carries the meaning and the colour only reinforces it, so under
/// `NO_COLOR` — and for the ~8% of men with red-green colour vision deficiency
/// — `✓ ✗ !` stay distinguishable on their own.
pub fn valid_sign() -> &'static str {
    static S: OnceLock<String> = OnceLock::new();
    S.get_or_init(|| sign("✓", GREEN))
}

pub fn error_sign() -> &'static str {
    static S: OnceLock<String> = OnceLock::new();
    S.get_or_init(|| sign("✗", RED))
}

pub fn warning_sign() -> &'static str {
    static S: OnceLock<String> = OnceLock::new();
    S.get_or_init(|| sign("!", YELLOW))
}

/// A check that never started because one before it blocked (ADR-0020).
/// The word `skipped` always follows it, so the glyph is never the only
/// carrier of the meaning.
pub fn skipped_sign() -> String {
    sign("↷", YELLOW)
}

/// Emphasise a fragment inside a message, in the terminal's own accent.
///
/// Sanitises what it is given, because most of what is highlighted came from
/// somewhere else: a declared check's name, a program it wants to run, a term
/// matched in a staged file. An escape sequence inside would also break this
/// function's own painting — the reset it emits is no longer the last word —
/// so this is as much about the colouring being correct as about the text
/// being safe.
pub fn highlight(text: &str) -> String {
    paint(&sanitize(text), YELLOW)
}

/// Text from a repository, made safe to hand a terminal.
///
/// A name or a command in `amont.conf` is chosen by whoever wrote the
/// repository, and the trust prompt exists precisely so a person can read the
/// declarations before accepting them. `\x1b[8m` is the conceal attribute:
/// putting it in one declaration's name HID THE NEXT ONE from that prompt, so
/// the reader saw two declarations, pressed y, and got three. The trust model
/// was never bypassed — the rendering lied about what was being trusted, which
/// is the same outcome by a shorter route.
///
/// What is escaped, and why each:
///   * C0 (`< 0x20`) — ESC starts every sequence; CR redraws the line; BEL,
///     backspace and the rest are all display control. TAB becomes a single
///     space instead, because these strings sit in aligned columns and a real
///     tab would break the layout it is trying to preserve.
///   * DEL and C1 (`0x80..=0x9f`) — a terminal in 8-bit mode takes `0x9b` as
///     CSI directly, with no ESC in sight.
///   * The bidi overrides (`U+202A..=202E`, `U+2066..=2069`) and the
///     directional marks — "Trojan Source": they reorder what is displayed
///     without changing a byte of what is parsed.
///
/// Everything else passes through untouched, including all other UTF-8 and
/// emoji: this is a display guard, not a charset policy.
pub fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\t' => out.push(' '),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c if ('\u{80}'..='\u{9f}').contains(&c) => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => {
                out.push_str(&format!("\\u{{{:04x}}}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// The same, for a path that came off a walk of somebody's disk.
///
/// `Path::display()` does not escape control bytes, and the fleet prints paths
/// it found by scanning directories it does not own.
pub fn sanitize_path(p: &std::path::Path) -> String {
    sanitize(&p.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::{forces, wants_colour, ColourEnv};

    fn env(no_color: bool, dumb: bool, force: bool, terminal: bool) -> ColourEnv {
        ColourEnv {
            no_color,
            dumb,
            force,
            terminal,
        }
    }

    /// Colour only on a terminal, or when the caller forces it; the off
    /// switches win over the force.
    #[test]
    fn colour_follows_the_terminal_and_the_switches() {
        assert!(wants_colour(&env(false, false, false, true)), "a terminal");
        assert!(!wants_colour(&env(false, false, false, false)), "a pipe");
        assert!(
            wants_colour(&env(false, false, true, false)),
            "forced through a pipe"
        );
        assert!(
            !wants_colour(&env(true, false, true, true)),
            "NO_COLOR beats force"
        );
        assert!(
            !wants_colour(&env(false, true, true, true)),
            "TERM=dumb beats force"
        );
    }

    #[test]
    fn force_color_values() {
        for on in ["1", "2", "3", "true", "always"] {
            assert!(forces(on), "{on}");
        }
        for off in ["", "0", "false", " 0 "] {
            assert!(!forces(off), "{off:?}");
        }
    }

    use super::*;

    /// Every byte a terminal would act on, and nothing else.
    #[test]
    fn sanitize_escapes_what_a_terminal_would_obey() {
        // The one that hid a declaration from the trust prompt.
        assert_eq!(sanitize("a\u{1b}[8mb"), "a\\x1b[8mb");
        assert_eq!(sanitize("bell\u{7}"), "bell\\x07");
        assert_eq!(sanitize("cr\r"), "cr\\x0d");
        assert_eq!(sanitize("del\u{7f}"), "del\\x7f");
        // 8-bit CSI: no ESC in sight.
        assert_eq!(sanitize("csi\u{9b}"), "csi\\x9b");
        // Trojan Source.
        assert_eq!(sanitize("rtl\u{202e}"), "rtl\\u{202e}");
        assert_eq!(sanitize("iso\u{2066}"), "iso\\u{2066}");
        // A tab keeps its width without keeping its behaviour.
        assert_eq!(sanitize("a\tb"), "a b");
    }

    /// It is a display guard, not a charset policy.
    #[test]
    fn sanitize_leaves_ordinary_text_alone() {
        for s in ["plain ascii", "café", "日本語", "✓ ✗ !", "a/b-c_d.e", "🐛"] {
            assert_eq!(sanitize(s), s, "{s:?} should pass through");
        }
    }

    /// `highlight` wraps text in its own escapes, so text that carries an
    /// escape of its own would break the wrapping as well as the reader.
    #[test]
    fn highlight_emits_only_its_own_escapes() {
        let out = highlight("a\u{1b}[0m\u{1b}[8mb");
        let escapes = out.matches('\u{1b}').count();
        // Either colour is off (0) or it is exactly the pair this function
        // writes — never the ones that came in with the text.
        assert!(
            escapes == 0 || escapes == 2,
            "{out:?} has {escapes} escapes"
        );
        // The `[8m` TEXT survives, harmlessly — what must not survive is the
        // ESC that would make a terminal read it as a command.
        assert!(
            !out.contains("\u{1b}[8m"),
            "a conceal sequence survived: {out:?}"
        );
        assert!(
            !out.contains("\u{1b}[0m\u{1b}[8m"),
            "an injected reset survived: {out:?}"
        );
    }

    /// The point of the change: nothing emits a code above 15, because those
    /// ignore the terminal's palette entirely.
    #[test]
    fn no_sign_uses_the_fixed_256_colour_cube() {
        for s in [valid_sign(), error_sign(), warning_sign()] {
            assert!(
                !s.contains("38;5;"),
                "a 256-colour code overrides the user's theme: {s:?}"
            );
        }
    }

    #[test]
    fn signs_carry_a_distinct_glyph_not_only_a_colour() {
        assert!(valid_sign().contains('✓'));
        assert!(error_sign().contains('✗'));
        assert!(warning_sign().contains('!'));
    }

    /// No source file may emit a 256-colour escape directly.
    ///
    /// The migration to base ANSI converted the sign helpers but missed a raw
    /// `\u{1b}[38;5;208m` in the pre-push error line, which therefore ignored
    /// the terminal theme for two more PRs. Checking the helpers was not enough
    /// because the offender did not use them.
    #[test]
    fn no_source_file_emits_a_256_colour_escape() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        let mut offenders = Vec::new();
        fn walk(d: &std::path::Path, out: &mut Vec<String>) {
            for e in std::fs::read_dir(d).expect("src").flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    for (i, line) in std::fs::read_to_string(&p)
                        .unwrap_or_default()
                        .lines()
                        .enumerate()
                    {
                        let mentions_the_pattern =
                            line.contains("offenders") || line.trim_start().starts_with("//");
                        if mentions_the_pattern {
                            continue;
                        }
                        // Split so this line is not itself an offender.
                        if line.contains(concat!("[38", ";5;")) {
                            out.push(format!("{}:{}", p.display(), i + 1));
                        }
                    }
                }
            }
        }
        walk(std::path::Path::new(dir), &mut offenders);
        assert!(
            offenders.is_empty(),
            "a fixed 256-colour code overrides the user's terminal theme: {offenders:?}"
        );
    }

    /// A success glyph must never be painted with the warning accent.
    ///
    /// The mechanical rewrite from 256-colour codes turned `color("✓", "112")`
    /// into `highlight("✓")` in two places, which renders a tick in the warning
    /// colour — the kind of damage a regex does quietly. Sign glyphs belong to
    /// the sign functions; `highlight` is for emphasising words.
    #[test]
    fn glyphs_are_not_routed_through_highlight() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        let mut offenders = Vec::new();
        fn walk(d: &std::path::Path, out: &mut Vec<String>) {
            for e in std::fs::read_dir(d).expect("src").flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    let body = std::fs::read_to_string(&p).unwrap_or_default();
                    for (i, line) in body.lines().enumerate() {
                        let is_test_or_doc =
                            line.trim_start().starts_with("//") || line.contains("offenders");
                        if is_test_or_doc {
                            continue;
                        }
                        for g in ["\\u{2713}", "\\u{2717}", "✓", "✗"] {
                            if line.contains("highlight(") && line.contains(g) {
                                out.push(format!("{}:{}", p.display(), i + 1));
                            }
                        }
                    }
                }
            }
        }
        walk(std::path::Path::new(dir), &mut offenders);
        assert!(
            offenders.is_empty(),
            "a status glyph is being painted as emphasis: {offenders:?}"
        );
    }

    /// `colors_enabled` memoises, so the predicate is tested directly rather
    /// than through a cache whichever test happens to fill first.
    #[test]
    fn no_color_is_honoured_per_the_standard() {
        fn decide(no_color: Option<&str>, term: &str) -> bool {
            let nc = no_color.map(|v| !v.is_empty()).unwrap_or(false);
            !nc && term != "dumb"
        }
        assert!(decide(None, "xterm-256color"), "colour by default");
        assert!(!decide(Some("1"), "xterm-256color"), "any value disables");
        assert!(!decide(Some("0"), "xterm-256color"), "even \"0\" disables");
        assert!(
            decide(Some(""), "xterm-256color"),
            "an EMPTY value does NOT disable — no-color.org is explicit"
        );
        assert!(!decide(None, "dumb"), "a dumb terminal cannot render SGR");
    }
}
