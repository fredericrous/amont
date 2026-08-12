//! pre-commit-usual-name — warn the first time an author commits under a
//! given name/email, so a misconfigured `user.name` is noticed at the first
//! commit rather than after twenty.
//!
//! Warning only: it never blocks a commit.

use crate::check::Outcome;
use crate::git;
use crate::ui::{highlight, warning_sign};

/// Identities already vouched for here — local, never committed, OURS to
/// remove on uninstall.
const KNOWN_KEY: &str = "amont.knownIdentity";

pub fn run(_args: &[std::ffi::OsString]) -> Outcome {
    // An empty repo has no history to compare against.
    if !git::succeeds(&["log", "-1"]) {
        return Outcome::Passed;
    }

    let name = git::stdout(&["config", "user.name"]).unwrap_or_default();
    let email = git::stdout(&["config", "user.email"]).unwrap_or_default();
    let full = format!("{name} <{email}>");

    // The memo, before the walk. `shortlog --all` visits every reachable
    // commit, and this check used to run it on EVERY commit to re-answer a
    // question whose answer only ever hardens: milliseconds here, tens of
    // seconds on a monorepo-sized history, growing forever — the classic
    // scale cliff that shows up two years after adoption. An identity seen
    // once is recorded in local config and never walked for again.
    // `amont uninstall` removes the key with the rest of our bookkeeping.
    let known = git::stdout(&["config", "--get-all", KNOWN_KEY]).unwrap_or_default();
    if known.lines().any(|l| l.trim() == full) {
        return Outcome::Passed;
    }

    // FIXED-STRING containment, never a pattern: a real name can carry regex
    // metacharacters (O'Brien, "Foo (Bar)") and as a pattern those would
    // misfire — `(dev)` is a group, and would match an author who never
    // committed. This is why the shell version used rg -F / grep -F.
    let seen = git::stdout(&["shortlog", "-s", "-n", "-e", "--all"])
        .is_some_and(|log| log.contains(&full));

    if seen {
        // Best-effort: a failed write only means walking again next commit.
        let _ = git::succeeds(&["config", "--add", KNOWN_KEY, &full]);
        return Outcome::Passed;
    }

    // A new identity is the one thing this check exists to notice, so it is
    // `Warned` and not `Passed` — the commit proceeds either way, but the
    // dashboard should not count a fresh identity as a clean run.
    if !seen {
        println!(
            "{} It is the first time you commit as {}",
            warning_sign(),
            highlight(&full)
        );
        return Outcome::Warned;
    }
    Outcome::Passed
}

#[cfg(test)]
mod tests {
    /// The property that matters, isolated from git: containment is literal.
    fn seen(log: &str, full: &str) -> bool {
        log.contains(full)
    }

    #[test]
    fn matches_an_existing_author_literally() {
        let log = "    12\ttest all mighty <test@domain.test>\n     3\tOther <o@x.test>";
        assert!(seen(log, "test all mighty <test@domain.test>"));
        assert!(!seen(log, "test mighty <test@domain.test>"));
    }

    /// As a REGEX, "test (dev) all mighty" would match "test dev all mighty"
    /// via the group — the false negative that lets a misconfigured identity
    /// pass unnoticed. Containment cannot do that.
    #[test]
    fn regex_metacharacters_stay_literal() {
        let log = "     1\ttest dev all mighty <t@x.test>";
        assert!(!seen(log, "test (dev) all mighty <t@x.test>"));

        let log2 = "     1\ttest (dev) all mighty <t@x.test>";
        assert!(seen(log2, "test (dev) all mighty <t@x.test>"));
    }
}
