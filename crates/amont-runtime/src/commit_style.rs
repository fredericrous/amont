//! What `commit-msg` enforces, and how it decorates — as four `git config`
//! keys.
//!
//! `commit-msg` is an entrypoint rather than a `Check` (see `registry.rs`), so
//! `hook.skip` and `amont.severity.*` do not reach it, and git exempts it
//! from `--no-verify`. That left the hook that touches every single commit with
//! no dial at all: its two most divisive rules — the gitmoji and the 50
//! character description budget — could be complied with or uninstalled, and
//! nothing in between. Every other opinion this project holds has a dial. These
//! are the ones this one was missing.
//!
//! The keys are read through [`crate::config`], so a value git cannot parse
//! falls back to the shipped default **and says so**.

use crate::config::{self, Scope};

pub const KEY_GITMOJI: &str = "amont.commit.gitmoji";
pub const KEY_SUBJECT_MAX: &str = "amont.commit.subjectMax";
pub const KEY_DESCRIPTION_MAX: &str = "amont.commit.descriptionMax";
pub const KEY_BODY_WRAP: &str = "amont.commit.bodyWrap";

/// The prefix every key above shares — one `--get-regexp` finds the family.
const PREFIX: &str = "amont.commit.";

pub const DEFAULT_GITMOJI: Gitmoji = Gitmoji::None;
pub const DEFAULT_SUBJECT_MAX: usize = 72;
pub const DEFAULT_DESCRIPTION_MAX: usize = 50;
pub const DEFAULT_BODY_WRAP: usize = 72;

/// A limit below 1 is unsatisfiable and would block every commit forever from a
/// config file; above 1000 it is not a limit. Both are mistakes, and
/// [`config::integer_or`] reports them as such.
const LIMIT_RANGE: std::ops::RangeInclusive<i64> = 1..=1000;
/// The same, plus `0` — which for the wrap column means "leave my body alone",
/// the setting that keeps a stack trace or a fenced code block intact.
const WRAP_RANGE: std::ops::RangeInclusive<i64> = 0..=1000;

/// The shortest thing that can stand before a description: the shortest type
/// (`add`, `fix`) plus the required colon and space.
const SHORTEST_PREFIX: usize = 5;

/// Where the type's gitmoji goes, if anywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gitmoji {
    /// Leave the subject as written. The type prefix is still required and
    /// still validated — this decides decoration, never enforcement.
    None,
    /// `✨  feat: add a cart`
    Prefix,
    /// `feat: add a cart ✨` — the tooling-friendly placement: commitlint,
    /// changelog generators and `git log --grep '^feat'` all still see a clean
    /// conventional subject at the start of the line.
    Suffix,
    /// `✨  add a cart` — the emoji stands in for the type word.
    ///
    /// You still *write* `feat: add a cart`, and it is still validated as
    /// such; only what gets stored differs. Know what it costs: the stored
    /// history is no longer parseable by conventional-commit tooling, because
    /// the type is now carried by an emoji. This mode chooses how the log looks
    /// over what can read it, and that is a real trade, not a free one.
    Replace,
}

impl Gitmoji {
    pub const ALL: [Gitmoji; 4] = [
        Gitmoji::None,
        Gitmoji::Prefix,
        Gitmoji::Suffix,
        Gitmoji::Replace,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Gitmoji::None => "none",
            Gitmoji::Prefix => "prefix",
            Gitmoji::Suffix => "suffix",
            Gitmoji::Replace => "replace",
        }
    }

    pub fn parse(s: &str) -> Option<Gitmoji> {
        Gitmoji::ALL.into_iter().find(|g| g.as_str() == s)
    }

    /// The one-line description `amont setup` and `amont list` show, so
    /// the four words never have to be looked up elsewhere.
    pub fn explain(self) -> &'static str {
        match self {
            Gitmoji::None => "leave the subject as written",
            Gitmoji::Prefix => "before the type",
            Gitmoji::Suffix => "after the description — tooling still reads the type",
            Gitmoji::Replace => "instead of the type — conventional-commit tools stop reading it",
        }
    }

    /// `feat: add a cart` rendered in this placement, for a menu.
    pub fn example(self) -> String {
        render_subject(self, "feat", "", "", "add a cart")
    }
}

/// The one place a decorated subject line is built.
///
/// `commit-msg` writes the real thing and `amont setup` renders the menu
/// through the same function, so the example somebody chooses from is produced
/// by the code that will run — not by a string that has to be kept in step
/// with it.
pub fn render_subject(
    placement: Gitmoji,
    prefix: &str,
    scope: &str,
    breaking: &str,
    description: &str,
) -> String {
    let emoji = crate::vocabulary::emoji_for(prefix);
    let conventional = format!("{prefix}{scope}{breaking}: {description}");
    match placement {
        Gitmoji::None => conventional,
        Gitmoji::Prefix => format!("{emoji}  {conventional}"),
        Gitmoji::Suffix => format!("{conventional} {emoji}"),
        // The type word is what the emoji stands in for; a scope and a
        // breaking marker are not types and stay exactly where they were.
        Gitmoji::Replace => {
            let rest = format!("{scope}{breaking}");
            if rest.is_empty() {
                format!("{emoji}  {description}")
            } else {
                format!("{emoji}  {rest}: {description}")
            }
        }
    }
}

const GITMOJI_WORDS: [&str; 4] = ["none", "prefix", "suffix", "replace"];

/// Everything `commit-msg` needs to know about how this repository wants its
/// messages checked and formatted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub gitmoji: Gitmoji,
    pub subject_max: usize,
    pub description_max: usize,
    /// `0` means never wrap.
    pub body_wrap: usize,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            gitmoji: DEFAULT_GITMOJI,
            subject_max: DEFAULT_SUBJECT_MAX,
            description_max: DEFAULT_DESCRIPTION_MAX,
            body_wrap: DEFAULT_BODY_WRAP,
        }
    }
}

impl Style {
    /// Read the four keys from the repository's git config.
    ///
    /// One `--get-regexp` first, then a typed read only for the keys it named.
    /// With nothing configured — the overwhelming case, and the one on the
    /// commit path — that is a single extra process rather than four.
    ///
    /// Measured on the unconfigured path, 50 runs each: 26.2 ms before this
    /// landed, 26.0 ms after. A wash, because the prescan replaced a process
    /// rather than adding one — the `git remote get-url upstream` call the old
    /// fork-suppression heuristic made on every single commit is gone.
    pub fn resolve(settings: &crate::config::Settings) -> Style {
        let names = config::present(settings, PREFIX);
        if names.is_empty() {
            return Style::default();
        }
        let d = Style::default();
        Style {
            gitmoji: if config::is_present(&names, KEY_GITMOJI) {
                Gitmoji::parse(config::enumerated_or(
                    settings,
                    KEY_GITMOJI,
                    &GITMOJI_WORDS,
                    d.gitmoji.as_str(),
                ))
                .unwrap_or(d.gitmoji)
            } else {
                d.gitmoji
            },
            subject_max: read_limit(
                settings,
                &names,
                KEY_SUBJECT_MAX,
                d.subject_max,
                LIMIT_RANGE,
            ),
            description_max: read_limit(
                settings,
                &names,
                KEY_DESCRIPTION_MAX,
                d.description_max,
                LIMIT_RANGE,
            ),
            body_wrap: read_limit(settings, &names, KEY_BODY_WRAP, d.body_wrap, WRAP_RANGE),
        }
    }

    /// Settings that cannot do what they look like they do.
    ///
    /// Deliberately **not** printed by the hook. The commit path announces what
    /// is in effect; `amont list` and `amont setup` — the two commands
    /// whose whole job is reading configuration back — are where a setting that
    /// makes no sense belongs. Putting a coherence essay in front of every
    /// commit is how people learn to stop reading hook output.
    pub fn warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.description_max + SHORTEST_PREFIX > self.subject_max {
            out.push(format!(
                "{KEY_DESCRIPTION_MAX} ({}) can never bind — the subject limit is {} and the \
                 shortest prefix is {SHORTEST_PREFIX} characters",
                self.description_max, self.subject_max
            ));
        }
        out
    }
}

fn read_limit(
    settings: &crate::config::Settings,
    names: &std::collections::BTreeSet<String>,
    key: &str,
    default: usize,
    range: std::ops::RangeInclusive<i64>,
) -> usize {
    if !config::is_present(names, key) {
        return default;
    }
    config::integer_or(settings, key, default as i64, range).max(0) as usize
}

/// One row of `amont list`'s commit-style block.
pub struct Setting {
    pub key: &'static str,
    /// The words a human reads, e.g. `description max`.
    pub label: &'static str,
    pub value: String,
    pub default: String,
    /// The effective value differs from the shipped one — the same meaning
    /// `CheckListing::severity_overridden` carries.
    pub overridden: bool,
    /// Somebody set this key, wherever the value landed. Distinct from
    /// `overridden` on purpose: a key pinned to the default value is still
    /// worth showing the origin of, because a reader deciding whether to
    /// change it wants to know a file already mentions it.
    pub set_here: bool,
    pub scope: Scope,
}

/// The effective style, plus where each value came from.
///
/// Costs one `--show-origin` call per overridden key, so this is for `list` and
/// `setup` only — never the commit path. See [`config::scope_of`].
pub fn describe(settings: &crate::config::Settings) -> (Style, Vec<Setting>) {
    let style = Style::resolve(settings);
    let d = Style::default();
    let rows = vec![
        row(
            settings,
            KEY_GITMOJI,
            "gitmoji",
            style.gitmoji.as_str().to_string(),
            d.gitmoji.as_str().to_string(),
        ),
        row(
            settings,
            KEY_SUBJECT_MAX,
            "subject max",
            style.subject_max.to_string(),
            d.subject_max.to_string(),
        ),
        row(
            settings,
            KEY_DESCRIPTION_MAX,
            "description max",
            style.description_max.to_string(),
            d.description_max.to_string(),
        ),
        row(
            settings,
            KEY_BODY_WRAP,
            "body wrap",
            wrap_word(style.body_wrap),
            wrap_word(d.body_wrap),
        ),
    ];
    (style, rows)
}

/// `0` is a column number nobody set out to choose; the word says what it does.
fn wrap_word(n: usize) -> String {
    if n == 0 {
        "off".to_string()
    } else {
        n.to_string()
    }
}

fn row(
    settings: &crate::config::Settings,
    key: &'static str,
    label: &'static str,
    value: String,
    default: String,
) -> Setting {
    let scope = config::scope_of(settings, key);
    Setting {
        key,
        label,
        overridden: value != default,
        set_here: scope != Scope::Default,
        value,
        default,
        scope,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_defaults_are_what_the_docs_promise() {
        let d = Style::default();
        assert_eq!(d.gitmoji, Gitmoji::None);
        assert_eq!(d.subject_max, 72);
        assert_eq!(d.description_max, 50);
        assert_eq!(d.body_wrap, 72);
    }

    /// Every placement round-trips through the word that names it, because
    /// that word is what `git config` stores and `amont setup` writes.
    #[test]
    fn every_placement_parses_from_its_own_name() {
        for g in Gitmoji::ALL {
            assert_eq!(Gitmoji::parse(g.as_str()), Some(g), "{}", g.as_str());
            assert!(!g.explain().is_empty());
        }
        assert_eq!(Gitmoji::parse("sideways"), None);
        assert_eq!(GITMOJI_WORDS.len(), Gitmoji::ALL.len());
    }

    /// The words offered to `git config` and the variants the code knows must
    /// be the same set, or a value the wizard writes is one the hook rejects.
    #[test]
    fn the_accepted_words_are_exactly_the_placements() {
        for word in GITMOJI_WORDS {
            assert!(Gitmoji::parse(word).is_some(), "{word} has no variant");
        }
    }

    /// The defaults must not warn about themselves.
    #[test]
    fn the_defaults_are_coherent() {
        assert!(Style::default().warnings().is_empty());
    }

    /// A description budget the subject limit can never accommodate is a
    /// setting that silently does nothing — the exact shape of misconfiguration
    /// this project refuses to stay quiet about.
    #[test]
    fn a_description_budget_that_can_never_bind_is_reported() {
        let s = Style {
            description_max: 50,
            subject_max: 52,
            ..Style::default()
        };
        let w = s.warnings();
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains(KEY_DESCRIPTION_MAX), "{w:?}");

        // Exactly enough room is not a warning: `add: ` + 50 = 55.
        let ok = Style {
            description_max: 50,
            subject_max: 55,
            ..Style::default()
        };
        assert!(ok.warnings().is_empty(), "{:?}", ok.warnings());
    }

    #[test]
    fn a_zero_wrap_column_reads_as_off() {
        assert_eq!(wrap_word(0), "off");
        assert_eq!(wrap_word(72), "72");
    }
}
