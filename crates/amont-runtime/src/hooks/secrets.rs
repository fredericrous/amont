//! Secrets never leave the machine — caught at commit, and again at push.
//!
//! The severity-matches-irreversibility argument at its most extreme. A
//! staged credential is a ten-second fix: unstage it. A PUSHED credential
//! is not a history problem, it is an incident — the secret is compromised
//! the moment it leaves the machine, and the remedy stops being `git
//! commit --amend` and becomes rotation. So this check has two halves:
//!
//! - **pre-commit** scans the STAGED content (which, under the staged-only
//!   hold, is exactly what the working tree holds) and blocks;
//! - **pre-push** scans the content every pushed commit ADDS — including
//!   commits made with `--no-verify`, from other tools, or three commits
//!   ago — because the push is the boundary that cannot be taken back.
//!
//! Detection is a curated set of literal token shapes, not entropy: private
//! key headers, cloud access key ids, the well-known API token prefixes.
//! Entropy heuristics are where secret scanners get noisy, and a noisy
//! blocker is a blocker people learn to delete. A line that is a KNOWN
//! false positive (a test fixture, documentation) opts out with the pragma
//! `amont:allow-secret` on the same line — visible in review, greppable,
//! and narrower than skipping the whole check.
//!
//! Findings are REDACTED: the report names the kind and the place, never
//! the matched text. A hook that echoes a secret into scrollback (and into
//! CI logs, and into anything recording the terminal) has widened the leak
//! it exists to prevent.
//!
//! The token shapes below are assembled with `concat!` so this source file
//! never contains a contiguous matchable pattern — the check must survive
//! scanning its own repository (see `the_scanner_does_not_flag_its_own_source`).

use crate::check::{Outcome, Severity};
use crate::finding::Finding;
use crate::pushrefs::PushRef;

use super::common;

/// Skip lines carrying this pragma — the surgical opt-out for fixtures.
const ALLOW: &str = "amont:allow-secret";

/// Per-file ceiling: a secret in the first two megabytes is a secret found,
/// and a generated bundle beyond it is noise this check has no business in.
const MAX_BYTES: usize = 2 * 1024 * 1024;

/// What was found — the KIND is all the report ever says about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    PrivateKey,
    AwsAccessKeyId,
    GithubToken,
    SlackToken,
    GoogleApiKey,
    StripeLiveKey,
    NpmToken,
    ApiKey,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::PrivateKey => "a private key",
            Kind::AwsAccessKeyId => "an AWS access key id",
            Kind::GithubToken => "a GitHub token",
            Kind::SlackToken => "a Slack token",
            Kind::GoogleApiKey => "a Google API key",
            Kind::StripeLiveKey => "a Stripe live key",
            Kind::NpmToken => "an npm token",
            Kind::ApiKey => "an API key",
        }
    }
}

fn is_token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// At least `n` token characters follow `text[at..]`.
fn token_run(text: &str, at: usize, n: usize) -> bool {
    text.as_bytes()[at..]
        .iter()
        .take_while(|b| is_token_char(**b))
        .count()
        >= n
}

/// The byte before a match must not itself be a token character — `XAKIA…`
/// is part of some longer word, not a key id.
fn boundary_before(text: &str, at: usize) -> bool {
    at == 0 || !is_token_char(text.as_bytes()[at - 1])
}

/// Every occurrence of `prefix` followed by ≥ `min` token characters.
fn has_prefixed_token(line: &str, prefix: &str, min: usize) -> bool {
    let mut from = 0;
    while let Some(i) = line[from..].find(prefix) {
        let at = from + i;
        if boundary_before(line, at) && token_run(line, at + prefix.len(), min) {
            return true;
        }
        from = at + prefix.len();
    }
    false
}

/// What this line carries, if anything. Pure — the whole detector is this
/// function, and the tests drive it directly.
pub(crate) fn sniff(line: &str) -> Option<Kind> {
    if line.contains(ALLOW) {
        return None;
    }
    // The PEM header: a BEGIN marker and the private-key tail on one
    // line. RSA, EC, DSA, OPENSSH, PGP, and the bare form all share it.
    // (Spelled via concat! so this file cannot flag itself — comments
    // included, since a secret in a comment is still a secret.)
    if line.contains(concat!("-----", "BEGIN ")) && line.contains(concat!("PRIVATE", " KEY-----")) {
        return Some(Kind::PrivateKey);
    }
    // AWS access key ids: AKIA (long-term) / ASIA (temporary) + 16 more.
    for p in [concat!("AK", "IA"), concat!("AS", "IA")] {
        let mut from = 0;
        while let Some(i) = line[from..].find(p) {
            let at = from + i;
            let rest = &line.as_bytes()[at + 4..];
            if boundary_before(line, at)
                && rest.len() >= 16
                && rest[..16]
                    .iter()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
            {
                return Some(Kind::AwsAccessKeyId);
            }
            from = at + 4;
        }
    }
    // GitHub: classic ghp_/gho_/ghu_/ghs_/ghr_ + 36, fine-grained
    // github_pat_ + a long tail.
    for p in [
        concat!("gh", "p_"),
        concat!("gh", "o_"),
        concat!("gh", "u_"),
        concat!("gh", "s_"),
        concat!("gh", "r_"),
    ] {
        if has_prefixed_token(line, p, 36) {
            return Some(Kind::GithubToken);
        }
    }
    if has_prefixed_token(line, concat!("github_", "pat_"), 60) {
        return Some(Kind::GithubToken);
    }
    // Slack: xoxb-/xoxp-/xoxa-/xoxr-/xoxs- + a real tail.
    for p in [
        concat!("xox", "b-"),
        concat!("xox", "p-"),
        concat!("xox", "a-"),
        concat!("xox", "r-"),
        concat!("xox", "s-"),
    ] {
        if has_prefixed_token(line, p, 10) {
            return Some(Kind::SlackToken);
        }
    }
    // Google API keys are AIza + exactly 35 more; ≥ 30 keeps rotated
    // variants without matching prose.
    if has_prefixed_token(line, concat!("AI", "za"), 30) {
        return Some(Kind::GoogleApiKey);
    }
    // Stripe LIVE keys only — sk_test_ is designed to be committed.
    for p in [concat!("sk_", "live_"), concat!("rk_", "live_")] {
        if has_prefixed_token(line, p, 20) {
            return Some(Kind::StripeLiveKey);
        }
    }
    if has_prefixed_token(line, concat!("np", "m_"), 36) {
        return Some(Kind::NpmToken);
    }
    // OpenAI / Anthropic project and API keys. The bare `sk-` prefix is
    // too common in ordinary identifiers to gate on; the vendored forms
    // are unambiguous.
    for p in [concat!("sk-", "proj-"), concat!("sk-", "ant-")] {
        if has_prefixed_token(line, p, 20) {
            return Some(Kind::ApiKey);
        }
    }
    None
}

/// git's own binary heuristic: a NUL in the first 8000 bytes.
fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|b| *b == 0)
}

/// Scan one text, collecting redacted findings as `(line-number, kind)`.
fn scan(text: &str) -> Vec<(usize, Kind)> {
    text.lines()
        .enumerate()
        .filter_map(|(i, line)| sniff(line).map(|k| (i + 1, k)))
        .collect()
}

/// The check's own short name, and the `check` field of every finding it makes.
pub const NAME: &str = "secrets";

/// Findings for one file's text. PURE — no git, no filesystem.
///
/// The message names the KIND and nothing else. A report that echoed the
/// matched text would be the leak this check exists to prevent, and would
/// copy it into terminal scrollback, CI logs and editor diagnostics on the
/// way. `Finding::message` is printed verbatim by everything downstream, so
/// this is the boundary where that rule has to hold.
pub fn findings(file: &str, text: &str) -> Vec<Finding> {
    scan(text)
        .into_iter()
        .map(|(line, kind)| {
            Finding::new(
                NAME,
                crate::ui::sanitize(file),
                Severity::Block,
                format!(
                    "{} — unstage it; once pushed it is not history, it is an incident",
                    kind.name()
                ),
            )
            .at_line(line)
        })
        .collect()
}

/// Is this worth reading as text at all? git's own binary heuristic, plus the
/// per-file ceiling. Shared so `amont check` skips exactly what the hook skips.
pub fn is_scannable(bytes: &[u8]) -> bool {
    !looks_binary(bytes) && bytes.len() <= MAX_BYTES
}

/// pre-commit: the staged content. Under the staged-only hold the working
/// tree IS the commit's content, so reading the files is reading the stage.
pub fn staged() -> Outcome {
    let files = common::staged_files(&[]);
    let root = common::repo_root();
    let mut found = false;
    for f in &files {
        let path = std::path::Path::new(&root).join(f);
        let Ok(bytes) = std::fs::read(&path) else {
            continue; // deleted or unreadable: nothing staged to leak
        };
        if !is_scannable(&bytes) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        for finding in findings(f, &text) {
            found = true;
            common::fail(&format!(
                "secrets: {} — {}",
                finding.location(),
                finding.message
            ));
        }
    }
    if found {
        return Outcome::Failed;
    }
    common::ok("No secrets staged");
    Outcome::Passed
}

/// pre-push: every line every pushed commit ADDS — the last moment a
/// secret is recoverable at all. `--no-verify` skipped the commit half;
/// it does not skip this one.
pub fn pushed(refs: &[PushRef]) -> Outcome {
    let zero = crate::git::stdout(&["hash-object", "--stdin"])
        .map(|h| "0".repeat(h.len()))
        .unwrap_or_else(|| "0".repeat(40));
    let mut found = false;
    let mut checked_any_ref = false;
    for r in refs {
        if r.local_oid == zero {
            continue; // deleting a ref pushes no content
        }
        let commits: Vec<String> = crate::pushrefs::commits_and_files_for(r, &zero)
            .into_iter()
            .map(|(c, _)| c)
            .collect();
        if commits.is_empty() && r.remote_oid != zero {
            // An up-to-date or forced-same push; nothing new leaves.
            continue;
        }
        checked_any_ref = true;
        for commit in &commits {
            let Some(diff) = crate::git::stdout(&["show", "--no-color", "--format=", commit])
            else {
                common::warn(
                    "secrets: git would not show a pushed commit — the push was \
                     NOT fully scanned",
                );
                return Outcome::Unavailable;
            };
            let mut file = String::from("?");
            for line in diff.lines() {
                if let Some(rest) = line.strip_prefix("+++ b/") {
                    file = rest.to_string();
                    continue;
                }
                let Some(added) = line.strip_prefix('+') else {
                    continue;
                };
                if let Some(kind) = sniff(added) {
                    found = true;
                    common::fail(&format!(
                        "secrets: {} added by commit {} in {} — this push would \
                         publish it; rewrite the history first (the secret may \
                         already need rotating)",
                        kind.name(),
                        &commit[..commit.len().min(12)],
                        crate::ui::sanitize(&file),
                    ));
                }
            }
        }
    }
    if found {
        return Outcome::Failed;
    }
    let _ = checked_any_ref; // a push of nothing is a clean push
    common::ok("No secrets in the pushed commits");
    Outcome::Passed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixtures are ASSEMBLED so this file never contains a contiguous
    /// secret shape — the same trick the shipped patterns use.
    fn pem() -> String {
        format!("{}{} RSA {}{}", "-----", "BEGIN", "PRIVATE", " KEY-----")
    }
    fn aws() -> String {
        format!("{}{}{}", "AK", "IA", "IOSFODNN7EXAMPLE")
    }
    fn gh() -> String {
        format!("{}{}{}", "gh", "p_", "a".repeat(36))
    }

    #[test]
    fn the_known_shapes_are_recognised() {
        assert_eq!(sniff(&pem()), Some(Kind::PrivateKey));
        assert_eq!(
            sniff(&format!("key = {}", aws())),
            Some(Kind::AwsAccessKeyId)
        );
        assert_eq!(sniff(&format!("token: {}", gh())), Some(Kind::GithubToken));
        assert_eq!(
            sniff(&format!("SLACK={}{}", "xox", "b-1234567890-abc")),
            Some(Kind::SlackToken)
        );
        assert_eq!(
            sniff(&format!("{}{}", "AI", "za".to_owned() + &"D".repeat(35))),
            Some(Kind::GoogleApiKey)
        );
        assert_eq!(
            sniff(&format!("{}{}{}", "sk_", "live_", "a".repeat(24))),
            Some(Kind::StripeLiveKey)
        );
        assert_eq!(
            sniff(&format!(
                "{}{}{}",
                "sk-",
                "ant-",
                "api03-".to_owned() + &"x".repeat(20)
            )),
            Some(Kind::ApiKey)
        );
    }

    /// The shapes are shapes, not prefixes: too short, wrong charset, or
    /// glued to a longer word is prose, not a credential.
    #[test]
    fn lookalikes_are_left_alone() {
        assert_eq!(sniff("AKIAI is the prefix"), None); // too short
        assert_eq!(sniff(&format!("X{}", aws())), None); // no boundary before
        assert_eq!(sniff("ghp_short"), None);
        assert_eq!(sniff("the sk-1234 identifier"), None); // bare sk- is not gated
                                                           // Stripe TEST keys are designed to be committed — and assembled
                                                           // here, because GitHub's own push protection flags the contiguous
                                                           // spelling even inside the test that proves we ignore it.
        assert_eq!(
            sniff(&format!("{}{}{}", "sk_", "test_", "a".repeat(24))),
            None
        );
        assert_eq!(sniff("xoxb- alone"), None);
        assert_eq!(sniff(""), None);
    }

    /// The pragma is the surgical opt-out — same line, visible in review.
    #[test]
    fn the_allow_pragma_skips_the_line() {
        let line = format!("{} // {}", aws(), ALLOW);
        assert_eq!(sniff(&line), None);
    }

    /// The check must survive its own repository: the shipped source
    /// assembles every pattern, so scanning this very file finds nothing.
    #[test]
    fn the_scanner_does_not_flag_its_own_source() {
        let own = include_str!("secrets.rs");
        assert!(
            scan(own).is_empty(),
            "the scanner flagged its own source: {:?}",
            scan(own)
        );
    }

    /// A NUL says binary, and binary is out of scope.
    #[test]
    fn binary_content_is_skipped() {
        assert!(looks_binary(b"\x00PNG"));
        assert!(!looks_binary(b"just text"));
    }

    /// Line numbers are 1-based and every finding is kept.
    #[test]
    fn scan_reports_each_line_once() {
        let text = format!("clean\n{}\nclean\n{}\n", pem(), aws());
        let hits = scan(&text);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0], (2, Kind::PrivateKey));
        assert_eq!(hits[1], (4, Kind::AwsAccessKeyId));
    }
}
