//! A shell reader that gives rules an argv, not a string.
//!
//! Every false positive this crate is designed to avoid comes from the same
//! mistake: matching a pattern against the raw text of a command. The text of a
//! command contains things that are not the command — a commit message, a
//! `--body` value, a comment, the inside of a `$(…)`, the name of a file being
//! redirected to. A regex cannot tell those apart. Measured against 43,242 real
//! commands, a hand-written regex for one rule was wrong **one time in five**,
//! and the four causes were all this:
//!
//! ```text
//!   292  the match was inside a quoted string
//!   131  the verb and the pipe were in different clauses
//!    99  `git tag --sort=… | head` — the LISTING form, not the mutating one
//!     6  an explicit --dry-run
//! ```
//!
//! So rules never see a string. They see [`Simple`] commands with their words
//! already separated, quoting already resolved, substitutions already blanked,
//! and clause boundaries already drawn. A rule that asks
//! `has_flag("--force")` cannot be answered by the word `--force` sitting
//! inside a commit message, because that word is marked [`Word::quoted`] and
//! `has_flag` skips it.
//!
//! The blanking discipline is lifted from
//! `amont_runtime::hooks::ban_terms::blank_non_code`: blank rather than delete,
//! so byte offsets stay meaningful and a reported span still points at the
//! right part of the original text.
//!
//! ## Not understood means no opinion
//!
//! [`Parsed::Opaque`] is returned for anything this reader cannot claim to
//! understand — an unterminated quote, an unbalanced substitution, `eval`,
//! `sh -c`. Opaque never fires a rule. Guessing at a construct we cannot parse
//! is how a guard learns to be confidently wrong, and a guard that is
//! confidently wrong gets uninstalled.

/// Past this, a "command" is a generated payload rather than something a person
/// or a model composed, and parsing it cannot be worth the time. The largest
/// real command in the corpus was 13 KB.
const MAX_SRC: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connector {
    Pipe,
    AndAnd,
    OrOr,
    Semi,
    Amp,
}

impl Connector {
    /// Does this connector end a *pipeline*? `|` chains a command's output into
    /// the next; everything else starts an unrelated command. The distinction
    /// is the whole of `pipe-to-tail`'s correctness.
    pub fn is_pipe(self) -> bool {
        matches!(self, Connector::Pipe)
    }
}

/// One word of a command, after quoting is resolved.
///
/// `raw`, `expanded` and `at` are part of the lexer's contract rather than of
/// any current rule: `raw` is what a rule ABOUT quoting would read, `expanded`
/// marks a word whose value we cannot know, and `at` is what lets a finding
/// point at the original text. The rule that used `raw` was removed before the
/// first commit (see rules/mod.rs); the fields stay because dropping and
/// re-deriving them is how a lexer quietly loses the ability to explain itself.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Word {
    /// Quotes removed, substitutions blanked to spaces. What rules match on.
    pub text: String,
    /// Exactly as written, including quotes. Only a rule that is *about*
    /// quoting may read this — `fish-glob` is the one such rule.
    pub raw: String,
    /// Any part of this word sat inside quotes. A quoted word is never a flag.
    pub quoted: bool,
    /// Any part of it came from `$(…)`, backticks or `${…}`.
    pub expanded: bool,
    /// Byte offset of the word's start in the original source.
    pub at: usize,
}

/// A single command: its words, and the connectors on either side of it.
#[derive(Debug, Clone, Default)]
pub struct Simple {
    pub words: Vec<Word>,
    pub prev: Option<Connector>,
    pub next: Option<Connector>,
    /// Redirect targets, deliberately kept OUT of `words` so that
    /// `git push > --force` can never be read as a `--force` flag.
    pub redirects: Vec<(String, Word)>,
    /// Byte range of this clause within the original source.
    pub at: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opaque {
    UnterminatedQuote,
    UnterminatedSubstitution,
    UnterminatedHeredoc,
    IndirectExecution(&'static str),
    TooLong,
}

impl Opaque {
    pub fn why(&self) -> String {
        match self {
            Opaque::UnterminatedQuote => "an unterminated quote".into(),
            Opaque::UnterminatedSubstitution => "an unbalanced substitution".into(),
            Opaque::UnterminatedHeredoc => "a heredoc with no terminator".into(),
            Opaque::IndirectExecution(w) => format!("`{w}` runs a command we cannot read"),
            Opaque::TooLong => "a command too large to read".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Parsed {
    Clear(Vec<Simple>),
    Opaque(Opaque),
}

impl Parsed {
    pub fn clauses(&self) -> &[Simple] {
        match self {
            Parsed::Clear(c) => c,
            Parsed::Opaque(_) => &[],
        }
    }
}

/// Commands whose arguments are themselves a program we would have to be a
/// shell to read.
const INDIRECT: &[&str] = &["eval", "xargs", "source", "."];
/// These are only indirect when handed `-c`; `bash script.sh` is readable.
const SHELLS: &[&str] = &["sh", "bash", "zsh", "fish", "dash", "ksh"];

struct Build {
    text: Vec<u8>,
    raw: Vec<u8>,
    quoted: bool,
    expanded: bool,
    at: usize,
}

impl Build {
    fn new(at: usize) -> Self {
        Build {
            text: Vec::new(),
            raw: Vec::new(),
            quoted: false,
            expanded: false,
            at,
        }
    }
    fn finish(self) -> Option<Word> {
        if self.raw.is_empty() {
            return None;
        }
        Some(Word {
            text: String::from_utf8(self.text).ok()?,
            raw: String::from_utf8(self.raw).ok()?,
            quoted: self.quoted,
            expanded: self.expanded,
            at: self.at,
        })
    }
}

/// Read a shell command into clauses.
pub fn lex(src: &str) -> Parsed {
    if src.len() > MAX_SRC {
        return Parsed::Opaque(Opaque::TooLong);
    }
    let b = src.as_bytes();
    let n = b.len();
    let mut i = 0usize;

    let mut out: Vec<Simple> = Vec::new();
    let mut cur = Simple {
        at: 0,
        ..Default::default()
    };
    let mut word: Option<Build> = None;
    let mut redirect: Option<String> = None;
    // Heredoc tags awaiting their body, which begins at the next newline.
    let mut heredocs: Vec<Vec<u8>> = Vec::new();

    macro_rules! end_word {
        () => {
            if let Some(w) = word.take() {
                if let Some(w) = w.finish() {
                    match redirect.take() {
                        Some(op) => cur.redirects.push((op, w)),
                        None => cur.words.push(w),
                    }
                }
            }
        };
    }

    macro_rules! end_clause {
        ($conn:expr, $at:expr) => {{
            end_word!();
            cur.end = $at;
            if !cur.words.is_empty() || !cur.redirects.is_empty() {
                cur.next = $conn;
                let prev = $conn;
                out.push(std::mem::take(&mut cur));
                cur.prev = prev;
            } else {
                cur.prev = $conn;
            }
            cur.at = $at;
        }};
    }

    while i < n {
        let c = b[i];

        // A comment runs to end of line, but only where a word could start.
        if c == b'#' && word.is_none() {
            while i < n && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }

        match c {
            b'\'' => {
                let w = word.get_or_insert_with(|| Build::new(i));
                w.quoted = true;
                w.raw.push(c);
                i += 1;
                let start = i;
                while i < n && b[i] != b'\'' {
                    i += 1;
                }
                if i >= n {
                    return Parsed::Opaque(Opaque::UnterminatedQuote);
                }
                w.text.extend_from_slice(&b[start..i]);
                w.raw.extend_from_slice(&b[start..i]);
                w.raw.push(b'\'');
                i += 1;
            }
            b'"' => {
                let w = word.get_or_insert_with(|| Build::new(i));
                w.quoted = true;
                w.raw.push(c);
                i += 1;
                let mut closed = false;
                while i < n {
                    match b[i] {
                        b'"' => {
                            closed = true;
                            w.raw.push(b'"');
                            i += 1;
                            break;
                        }
                        b'\\' if i + 1 < n => {
                            w.raw.push(b'\\');
                            w.raw.push(b[i + 1]);
                            w.text.push(b[i + 1]);
                            i += 2;
                        }
                        b'$' if i + 1 < n && b[i + 1] == b'(' => {
                            let Some(close) = balanced(b, i + 1, b'(', b')') else {
                                return Parsed::Opaque(Opaque::UnterminatedSubstitution);
                            };
                            w.expanded = true;
                            w.raw.extend_from_slice(&b[i..=close]);
                            w.text.extend(std::iter::repeat(b' ').take(close - i + 1));
                            i = close + 1;
                        }
                        other => {
                            w.raw.push(other);
                            w.text.push(other);
                            i += 1;
                        }
                    }
                }
                if !closed {
                    return Parsed::Opaque(Opaque::UnterminatedQuote);
                }
            }
            b'\\' if i + 1 < n => {
                let w = word.get_or_insert_with(|| Build::new(i));
                // An escaped newline is a line continuation, not a word.
                if b[i + 1] == b'\n' {
                    i += 2;
                    continue;
                }
                w.quoted = true;
                w.raw.push(b'\\');
                w.raw.push(b[i + 1]);
                w.text.push(b[i + 1]);
                i += 2;
            }
            b'`' => {
                let w = word.get_or_insert_with(|| Build::new(i));
                let mut j = i + 1;
                while j < n && b[j] != b'`' {
                    j += 1;
                }
                if j >= n {
                    return Parsed::Opaque(Opaque::UnterminatedSubstitution);
                }
                w.expanded = true;
                w.raw.extend_from_slice(&b[i..=j]);
                w.text.extend(std::iter::repeat(b' ').take(j - i + 1));
                i = j + 1;
            }
            b'$' if i + 1 < n && (b[i + 1] == b'(' || b[i + 1] == b'{') => {
                let (open, close) = if b[i + 1] == b'(' {
                    (b'(', b')')
                } else {
                    (b'{', b'}')
                };
                let Some(end) = balanced(b, i + 1, open, close) else {
                    return Parsed::Opaque(Opaque::UnterminatedSubstitution);
                };
                let w = word.get_or_insert_with(|| Build::new(i));
                w.expanded = true;
                w.raw.extend_from_slice(&b[i..=end]);
                w.text.extend(std::iter::repeat(b' ').take(end - i + 1));
                i = end + 1;
            }
            b'<' if i + 1 < n && b[i + 1] == b'<' => {
                // A heredoc. The BODY is data and starts on the next line, but
                // the rest of THIS line is still command — `git commit -F-
                // <<'MSG' 2>&1 | tail -8` is a real pipe-to-tail, and treating
                // the whole command as opaque from the operator onward hid 89
                // true positives when measured. Read the operator's line; skip
                // the body at the newline.
                end_word!();
                i += 2;
                if i < n && b[i] == b'-' {
                    i += 1;
                }
                while i < n && (b[i] == b' ' || b[i] == b'\t') {
                    i += 1;
                }
                let mut tag = Vec::new();
                while i < n
                    && (b[i].is_ascii_alphanumeric()
                        || b[i] == b'_'
                        || b[i] == b'\''
                        || b[i] == b'"')
                {
                    if b[i] != b'\'' && b[i] != b'"' {
                        tag.push(b[i]);
                    }
                    i += 1;
                }
                heredocs.push(tag);
            }
            b'>' | b'<' => {
                end_word!();
                let start = i;
                i += 1;
                if i < n && b[i] == b'>' {
                    i += 1;
                }
                // `2>&1` / `>&2`: the `&N` belongs to the redirect, not to a
                // following clause. Consume it here so `&` is not read as a
                // background operator.
                if i < n && b[i] == b'&' {
                    i += 1;
                    while i < n && (b[i].is_ascii_digit() || b[i] == b'-') {
                        i += 1;
                    }
                    cur.redirects.push((
                        String::from_utf8_lossy(&b[start..i]).into_owned(),
                        Word {
                            text: String::new(),
                            raw: String::new(),
                            quoted: false,
                            expanded: false,
                            at: start,
                        },
                    ));
                    continue;
                }
                redirect = Some(String::from_utf8_lossy(&b[start..i]).into_owned());
            }
            b'0'..=b'9'
                if word.is_none()
                    && i + 1 < n
                    && (b[i + 1] == b'>' || b[i + 1] == b'<')
                    && !matches!(b.get(i + 2), Some(b'<')) =>
            {
                // A file-descriptor prefix on a redirect: `2>`, `2>>`.
                let start = i;
                i += 1;
                i += 1;
                if i < n && b[i] == b'>' {
                    i += 1;
                }
                if i < n && b[i] == b'&' {
                    i += 1;
                    while i < n && (b[i].is_ascii_digit() || b[i] == b'-') {
                        i += 1;
                    }
                    cur.redirects.push((
                        String::from_utf8_lossy(&b[start..i]).into_owned(),
                        Word {
                            text: String::new(),
                            raw: String::new(),
                            quoted: false,
                            expanded: false,
                            at: start,
                        },
                    ));
                    continue;
                }
                redirect = Some(String::from_utf8_lossy(&b[start..i]).into_owned());
            }
            b'|' => {
                let conn = if i + 1 < n && b[i + 1] == b'|' {
                    i += 2;
                    Connector::OrOr
                } else {
                    // `|&` pipes stderr too; it is still a pipe.
                    i += 1;
                    if i < n && b[i] == b'&' {
                        i += 1;
                    }
                    Connector::Pipe
                };
                end_clause!(Some(conn), i);
            }
            b'&' => {
                let conn = if i + 1 < n && b[i + 1] == b'&' {
                    i += 2;
                    Connector::AndAnd
                } else {
                    i += 1;
                    Connector::Amp
                };
                end_clause!(Some(conn), i);
            }
            b';' => {
                i += 1;
                end_clause!(Some(Connector::Semi), i);
            }
            b'\n' => {
                i += 1;
                end_clause!(Some(Connector::Semi), i);
                // The heredoc bodies queued on the previous line start here.
                while let Some(tag) = heredocs.first().cloned() {
                    heredocs.remove(0);
                    match find_terminator(b, i, &tag) {
                        Some(next) => i = next,
                        None => return Parsed::Opaque(Opaque::UnterminatedHeredoc),
                    }
                }
            }
            b'(' | b')' | b'{' | b'}' if word.is_none() => {
                // Grouping, and only when it STARTS a word: `{ cmd; }`, `(sub)`.
                // None of the rules need its semantics, and descending flat can
                // only ever LOSE a fire, never invent one.
                end_clause!(None, i);
                i += 1;
            }
            b'{' | b'}' => {
                // A brace ATTACHED to a word belongs to that word: `stash@{2}`,
                // `HEAD@{1}`, `refs/stash@{0}`. Ending the clause here truncated
                // the word to `stash@` and INVENTED a fire in bare-stash-pop —
                // the one thing the branch above promises never to do.
                let w = word.get_or_insert_with(|| Build::new(i));
                w.raw.push(b[i]);
                w.text.push(b[i]);
                i += 1;
            }
            b' ' | b'\t' | b'\r' => {
                end_word!();
                i += 1;
            }
            other => {
                let w = word.get_or_insert_with(|| Build::new(i));
                w.raw.push(other);
                w.text.push(other);
                i += 1;
            }
        }
    }
    if !heredocs.is_empty() {
        return Parsed::Opaque(Opaque::UnterminatedHeredoc);
    }
    end_clause!(None, n);

    for cmd in &out {
        if let Some(why) = indirect(cmd) {
            return Parsed::Opaque(Opaque::IndirectExecution(why));
        }
    }
    Parsed::Clear(out)
}

fn indirect(cmd: &Simple) -> Option<&'static str> {
    let p = cmd.program()?;
    if let Some(hit) = INDIRECT.iter().find(|k| **k == p) {
        return Some(hit);
    }
    if SHELLS.contains(&p) && cmd.has_flag("-c") {
        return SHELLS.iter().find(|s| **s == p).copied();
    }
    None
}

/// The index of the byte closing a run opened at `from`, honouring nesting.
fn balanced(b: &[u8], from: usize, open: u8, close: u8) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = from;
    while i < b.len() {
        if b[i] == open {
            depth += 1;
        } else if b[i] == close {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Index just past the heredoc terminator line starting the search at `from`.
fn find_terminator(b: &[u8], from: usize, tag: &[u8]) -> Option<usize> {
    let mut line = from;
    while line <= b.len() {
        let end = b[line..]
            .iter()
            .position(|&c| c == b'\n')
            .map(|p| line + p)
            .unwrap_or(b.len());
        let trimmed: &[u8] = {
            let s = &b[line..end];
            let a = s.iter().position(|c| !c.is_ascii_whitespace()).unwrap_or(0);
            let z = s
                .iter()
                .rposition(|c| !c.is_ascii_whitespace())
                .map(|p| p + 1)
                .unwrap_or(a);
            &s[a..z]
        };
        if trimmed == tag {
            return Some(if end < b.len() { end + 1 } else { b.len() });
        }
        if end >= b.len() {
            return None;
        }
        line = end + 1;
    }
    None
}

/// Wrappers that stand in front of the real program without changing what it
/// is. `sudo git push` is a `git push`.
const WRAPPERS: &[&str] = &[
    "sudo", "command", "builtin", "nice", "time", "timeout", "env",
];

/// git's own options, which sit before the subcommand.
const GIT_GLOBAL_VALUED: &[&str] = &["-C", "-c", "--git-dir", "--work-tree", "--exec-path"];
const GIT_GLOBAL_BARE: &[&str] = &[
    "--no-pager",
    "--paginate",
    "-p",
    "--bare",
    "--literal-pathspecs",
];

impl Simple {
    /// argv0, with leading `VAR=value` assignments and wrapper commands peeled
    /// off. Returns `None` when the command is only assignments or is empty.
    pub fn program(&self) -> Option<&str> {
        let mut idx = 0;
        loop {
            let w = self.words.get(idx)?;
            let t = w.text.as_str();
            // `FOO=1 git push` is a `git push`.
            if !w.quoted && t.contains('=') && !t.starts_with('-') {
                let name = &t[..t.find('=').unwrap()];
                if !name.is_empty() && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
                {
                    idx += 1;
                    continue;
                }
            }
            if WRAPPERS.contains(&t) {
                idx += 1;
                // `timeout 90 git push`: skip a bare numeric argument.
                if t == "timeout" || t == "nice" {
                    while self
                        .words
                        .get(idx)
                        .is_some_and(|w| w.text.bytes().all(|c| c.is_ascii_digit() || c == b'.'))
                        && self.words.get(idx).is_some_and(|w| !w.text.is_empty())
                    {
                        idx += 1;
                    }
                }
                continue;
            }
            return Some(t);
        }
    }

    fn program_index(&self) -> Option<usize> {
        let p = self.program()?;
        self.words.iter().position(|w| w.text == p)
    }

    /// The first operand after the program, skipping the program's own global
    /// options. An UNKNOWN leading `-x` yields `None` — giving up is the safe
    /// direction, because a wrong subcommand is a wrong rule.
    pub fn subcommand(&self) -> Option<&str> {
        let mut idx = self.program_index()? + 1;
        while let Some(w) = self.words.get(idx) {
            let t = w.text.as_str();
            if !t.starts_with('-') {
                return Some(t);
            }
            if GIT_GLOBAL_BARE.contains(&t) {
                idx += 1;
                continue;
            }
            if let Some(flag) = GIT_GLOBAL_VALUED.iter().find(|f| t == **f) {
                let _ = flag;
                idx += 2;
                continue;
            }
            if GIT_GLOBAL_VALUED
                .iter()
                .any(|f| t.starts_with(&format!("{f}=")))
            {
                idx += 1;
                continue;
            }
            return None;
        }
        None
    }

    /// Whole-token flag test. Stops at `--`, and **skips quoted words** — which
    /// is the single highest-leverage precision decision in this crate. It is
    /// what makes `gh pr create --body "…use --auto…"` not a `--auto`.
    pub fn has_flag(&self, flag: &str) -> bool {
        for w in &self.words {
            if !w.quoted && w.text == "--" {
                return false;
            }
            if w.quoted {
                continue;
            }
            if w.text == flag {
                return true;
            }
        }
        false
    }

    /// A letter inside a short cluster: `-Au` contains `u`. Stops at `--`.
    pub fn has_short(&self, c: char) -> bool {
        for w in &self.words {
            if !w.quoted && w.text == "--" {
                return false;
            }
            if w.quoted || w.text.len() < 2 {
                continue;
            }
            let t = w.text.as_str();
            if t.starts_with('-') && !t.starts_with("--") && t[1..].contains(c) {
                return true;
            }
        }
        false
    }

    /// `--flag=value` or `--flag value`.
    #[allow(dead_code)]
    pub fn flag_value(&self, flag: &str) -> Option<&str> {
        let eq = format!("{flag}=");
        for (i, w) in self.words.iter().enumerate() {
            if !w.quoted && w.text == "--" {
                return None;
            }
            if w.quoted {
                continue;
            }
            if let Some(v) = w.text.strip_prefix(&eq) {
                return Some(v);
            }
            if w.text == flag {
                return self.words.get(i + 1).map(|w| w.text.as_str());
            }
        }
        None
    }

    /// Non-flag words after the program, plus everything after `--`.
    pub fn operands(&self) -> Vec<&Word> {
        let Some(start) = self.program_index() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut after_ddash = false;
        for w in self.words.iter().skip(start + 1) {
            if !w.quoted && w.text == "--" {
                after_ddash = true;
                continue;
            }
            if after_ddash || !w.text.starts_with('-') {
                out.push(w);
            }
        }
        out
    }

    /// Any form of `--dry-run`. A dry run mutates nothing, so it disarms every
    /// rule that is about mutation.
    pub fn is_dry_run(&self) -> bool {
        self.words
            .iter()
            .any(|w| !w.quoted && (w.text == "--dry-run" || w.text.starts_with("--dry-run=")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clauses(src: &str) -> Vec<Simple> {
        match lex(src) {
            Parsed::Clear(c) => c,
            Parsed::Opaque(o) => panic!("expected a readable command, got {o:?}"),
        }
    }

    fn opaque(src: &str) -> Opaque {
        match lex(src) {
            Parsed::Opaque(o) => o,
            Parsed::Clear(_) => panic!("expected opacity"),
        }
    }

    /// The single most common false positive in the corpus: 292 of 501 measured
    /// misfires were a pattern sitting inside a quoted string. An operator
    /// inside quotes is text, and a word built from quoted text is never a flag.
    #[test]
    fn quotes_hide_operators_and_flags() {
        let c = clauses(r#"pkill -f "git push origin v1 | tail""#);
        assert_eq!(c.len(), 1, "the quoted pipe must not split the command");
        assert_eq!(c[0].program(), Some("pkill"));

        let c = clauses(r#"gh pr create --body "use --auto here""#);
        assert!(!c[0].has_flag("--auto"), "a quoted --auto is not a flag");
        assert!(c[0].has_flag("--body"), "an unquoted flag still is one");
    }

    /// `&&` and `;` start unrelated commands. 131 measured misfires were a verb
    /// in one clause and a pipe in another, which reads as plausible and is
    /// wrong.
    #[test]
    fn a_connector_starts_a_new_command() {
        let c = clauses("git push origin main && echo done | tail -1");
        assert_eq!(c.len(), 3);
        assert_eq!(c[0].program(), Some("git"));
        assert_eq!(c[0].next, Some(Connector::AndAnd));
        assert_eq!(c[1].program(), Some("echo"));
        assert_eq!(c[1].next, Some(Connector::Pipe));
        assert_eq!(c[2].program(), Some("tail"));
    }

    /// A pipe chains one command's output into the next; every other connector
    /// does not. `pipe-to-tail`'s entire correctness rests on the difference.
    #[test]
    fn only_a_pipe_is_a_pipe() {
        assert!(Connector::Pipe.is_pipe());
        for c in [
            Connector::AndAnd,
            Connector::OrOr,
            Connector::Semi,
            Connector::Amp,
        ] {
            assert!(!c.is_pipe(), "{c:?} is not a pipe");
        }
        // `|&` pipes stderr too, and is still a pipe.
        let c = clauses("git push |& tail -2");
        assert_eq!(c[0].next, Some(Connector::Pipe));
    }

    /// The heredoc BODY is data; the rest of the operator's own line is still
    /// command. Blanking from the operator instead of from the newline swallows
    /// `2>&1 | tail -8` and silently hid 89 true positives when measured.
    #[test]
    fn a_heredoc_body_is_data_but_its_own_line_is_not() {
        let c = clauses("git commit -F- <<'MSG' 2>&1 | tail -8\nsubject\nMSG\n");
        assert_eq!(c[0].program(), Some("git"));
        assert_eq!(c[0].subcommand(), Some("commit"));
        assert_eq!(
            c[0].next,
            Some(Connector::Pipe),
            "the pipe survives the heredoc"
        );
        assert!(
            c.iter().all(|s| s.program() != Some("subject")),
            "the body must not be read as commands"
        );
    }

    /// A heredoc we cannot find the end of means the rest of the text is of
    /// unknown kind. Guessing there is how a guard becomes confidently wrong.
    #[test]
    fn an_unterminated_heredoc_is_not_an_opinion() {
        assert_eq!(
            opaque("git commit -F- <<'MSG'\nbody\n"),
            Opaque::UnterminatedHeredoc
        );
    }

    #[test]
    fn an_unterminated_quote_is_not_an_opinion() {
        assert_eq!(opaque("git push \"origin"), Opaque::UnterminatedQuote);
        assert_eq!(opaque("git push 'origin"), Opaque::UnterminatedQuote);
    }

    /// We would have to BE a shell to know what `eval` runs.
    #[test]
    fn indirect_execution_is_not_inspected() {
        assert!(matches!(
            opaque("eval \"$cmd\""),
            Opaque::IndirectExecution(_)
        ));
        assert!(matches!(
            opaque("sh -c 'git push | tail'"),
            Opaque::IndirectExecution(_)
        ));
        // A shell running a FILE is readable; only `-c` hides a command.
        assert_eq!(clauses("bash deploy.sh")[0].program(), Some("bash"));
    }

    /// A substitution's contents are blanked, not deleted, so byte offsets into
    /// the original text stay meaningful for a reported span.
    #[test]
    fn a_substitution_is_blanked_and_the_word_keeps_its_length() {
        let c = clauses("echo $(git push | tail -1)");
        assert_eq!(c.len(), 1, "a pipe inside a substitution is not our pipe");
        let w = &c[0].words[1];
        assert!(w.expanded);
        assert_eq!(w.text.len(), w.raw.len(), "blanking preserves length");
        assert!(w.text.trim().is_empty());
    }

    #[test]
    fn a_comment_ends_the_command() {
        let c = clauses("git push origin main # then | tail -5");
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].operands().len(), 3);
    }

    /// A redirect target is not argv. Without this, `git push > --force` offers
    /// a `--force` flag that was never typed as one.
    #[test]
    fn a_brace_attached_to_a_word_stays_in_the_word() {
        // `stash@{2}` is ONE operand. Ending the clause at `{` truncated it to
        // `stash@`, which turned an explicit stash reference into a bare one —
        // the flattening comment promises to lose fires, never invent them.
        let c = clauses("git stash pop stash@{2}");
        assert_eq!(c.len(), 1);
        let ops: Vec<&str> = c[0].operands().iter().map(|w| w.text.as_str()).collect();
        assert_eq!(ops, vec!["stash", "pop", "stash@{2}"]);
        // A brace that STARTS a word is still grouping, not part of a word:
        // the program is `echo`, never `{`.
        let g = clauses("{ echo a; }");
        assert_eq!(
            g.iter().filter_map(|c| c.program()).collect::<Vec<_>>(),
            vec!["echo"]
        );
    }

    #[test]
    fn a_redirect_target_is_not_argv() {
        let c = clauses("git push > --force");
        assert!(!c[0].has_flag("--force"));
        assert_eq!(c[0].redirects.len(), 1);
        // `2>&1` is a redirect, not a background `&` starting a new command.
        let c = clauses("git push 2>&1 | tail -3");
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].next, Some(Connector::Pipe));
    }

    /// `--` ends the flags. `git add -- -A` stages a FILE called `-A`.
    #[test]
    fn flags_stop_at_the_double_dash() {
        let c = clauses("git add -- -A");
        assert!(!c[0].has_short('A'));
        assert!(c[0].operands().iter().any(|w| w.text == "-A"));
    }

    #[test]
    fn short_clusters_are_searched_by_letter() {
        let c = clauses("git add -Au");
        assert!(c[0].has_short('A') && c[0].has_short('u'));
        assert!(!c[0].has_short('p'));
    }

    /// `FOO=1 git push` and `sudo git push` are both a `git push`. A rule keyed
    /// on argv0 would miss every one of them.
    #[test]
    fn assignments_and_wrappers_are_not_the_program() {
        for src in [
            "GIT_SSH_COMMAND=ssh git push",
            "sudo git push",
            "timeout 90 git push",
            "command git push",
        ] {
            let c = clauses(src);
            assert_eq!(c[0].program(), Some("git"), "{src}");
            assert_eq!(c[0].subcommand(), Some("push"), "{src}");
        }
    }

    /// git's own options sit before the subcommand, so a naive "second word"
    /// read of `git -C dir push` finds `dir`.
    #[test]
    fn git_global_options_precede_the_subcommand() {
        assert_eq!(clauses("git -C /tmp/x push")[0].subcommand(), Some("push"));
        assert_eq!(clauses("git --no-pager log")[0].subcommand(), Some("log"));
        assert_eq!(
            clauses("git -c user.name=x commit")[0].subcommand(),
            Some("commit")
        );
        // An option we do not know is a reason to stop, not to guess.
        assert_eq!(clauses("git --future-flag push")[0].subcommand(), None);
    }

    #[test]
    fn a_flag_value_is_read_either_way_it_is_written() {
        assert_eq!(
            clauses("grep --include=*.py x")[0].flag_value("--include"),
            Some("*.py")
        );
        assert_eq!(
            clauses("grep --include *.py x")[0].flag_value("--include"),
            Some("*.py")
        );
    }

    #[test]
    fn a_dry_run_is_recognised_in_both_forms() {
        assert!(clauses("kubectl apply --dry-run=client -f x")[0].is_dry_run());
        assert!(clauses("git push --dry-run")[0].is_dry_run());
        assert!(!clauses("git push")[0].is_dry_run());
    }
}
