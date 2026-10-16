//! What each repository says to attest, for the step that retires the old
//! opt-out (decisions ADR-0037).
//!
//! `git-attest` 2.0.0 stops reading `set planExempt off` in `amont.conf`; it
//! may ship once no repository opts out of the canonical tree through that
//! line alone. Adding `@plan-exempt off` beside it is safe where the spec
//! declares no gates, and costs the input fingerprints of every attest before
//! 1.6.0 where it does, so the sweep also needs each repository's pinned
//! attest. Read from HEAD's tree, through attest's own library: the fleet
//! has no copy of the format either.

use std::path::Path;

use serde::Serialize;

use git_attest::spec::{at, plan_exempt, PlanExempt};

/// One repository's answer.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct AttestState {
    /// Only `set planExempt off` in `amont.conf` opts this repository out
    /// of the canonical tree: the line attest reads until 2.0.0.
    pub deprecated_opt_out: bool,
    /// HEAD's `attest-inputs` declares at least one gate.
    pub spec_with_gates: bool,
    /// Every `uses: fredericrous/attest[/sign]@<ref>` in its workflows.
    pub pins: Vec<AttestPin>,
}

/// One `uses:` line naming an attest action.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AttestPin {
    /// The workflow file, relative to the repository.
    pub file: String,
    /// `attest` or `attest/sign`.
    pub action: String,
    /// What follows the `@`: a tag, a moving major such as `v1`, or a sha.
    pub reference: String,
    /// The release it names, `X.Y.Z`: the tag itself, or the `# vX.Y.Z`
    /// comment beside a sha. `None` for a moving major (it follows the
    /// releases) and for a bare sha.
    pub version: Option<String>,
}

impl AttestState {
    /// Pins to a release before `major.minor`, or to a sha nobody named:
    /// the consumers a sweep must bump first. A moving major follows the
    /// releases and is never counted.
    pub fn pins_before(&self, major: u64, minor: u64) -> bool {
        self.pins.iter().any(|p| match &p.version {
            Some(v) => parse(v).is_some_and(|(a, b)| (a, b) < (major, minor)),
            None => !is_moving_major(&p.reference),
        })
    }
}

/// What `repo` says to attest. A repository git cannot read reports
/// nothing deprecated, nothing declared: this is a count, not a verdict.
pub fn read(repo: &Path) -> AttestState {
    let tree = "HEAD^{tree}";
    AttestState {
        deprecated_opt_out: plan_exempt(repo, tree) == PlanExempt::OffByAmontConf,
        spec_with_gates: matches!(at(repo, tree), Ok(Some((_, spec))) if !spec.gates.is_empty()),
        pins: pins(repo),
    }
}

/// The attest pins of the workflows on disk, GitHub's and Forgejo's.
fn pins(repo: &Path) -> Vec<AttestPin> {
    let mut out = Vec::new();
    for dir in [".github/workflows", ".forgejo/workflows"] {
        let Ok(entries) = std::fs::read_dir(repo.join(dir)) else {
            continue;
        };
        let mut files: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("yml" | "yaml")))
            .collect();
        files.sort();
        for path in files {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let file = format!(
                "{dir}/{}",
                path.file_name().unwrap_or_default().to_string_lossy()
            );
            out.extend(text.lines().filter_map(|l| pin_on(l, &file)));
        }
    }
    out
}

/// The pin a workflow line carries, if any:
/// `uses: [https://github.com/]fredericrous/attest[/sign]@<ref> [# vX.Y.Z]`.
fn pin_on(line: &str, file: &str) -> Option<AttestPin> {
    let rest = line
        .trim_start()
        .strip_prefix("- ")
        .unwrap_or(line.trim_start());
    let rest = rest.strip_prefix("uses:")?.trim_start();
    let rest = rest.strip_prefix("https://github.com/").unwrap_or(rest);
    let rest = rest.strip_prefix("fredericrous/attest")?;
    let (action, rest) = match rest.strip_prefix("/sign@") {
        Some(r) => ("attest/sign", r),
        None => ("attest", rest.strip_prefix('@')?),
    };
    let mut words = rest.split_whitespace();
    let reference = words.next()?.trim_matches(|c| c == '"' || c == '\'');
    let comment = match (words.next(), words.next()) {
        (Some("#"), Some(tag)) => tag.strip_prefix('v').filter(|v| parse(v).is_some()),
        _ => None,
    };
    let version = match reference.strip_prefix('v') {
        Some(v) if is_release(v) => Some(v.to_string()),
        _ => comment.map(str::to_string),
    };
    Some(AttestPin {
        file: file.to_string(),
        action: action.to_string(),
        reference: reference.to_string(),
        version,
    })
}

/// `X.Y.Z`, all digits.
fn is_release(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// `vN`: the alias that moves with the releases.
fn is_moving_major(reference: &str) -> bool {
    reference
        .strip_prefix('v')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// `(major, minor)` of an `X.Y.Z`.
fn parse(v: &str) -> Option<(u64, u64)> {
    if !is_release(v) {
        return None;
    }
    let mut parts = v.split('.').map(|p| p.parse::<u64>().ok());
    Some((parts.next()??, parts.next()??))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(line: &str) -> Option<AttestPin> {
        pin_on(line, ".github/workflows/ci.yaml")
    }

    #[test]
    fn every_pin_shape_is_read() {
        let p = pin("      - uses: fredericrous/attest@v1").unwrap();
        assert_eq!(
            (p.action.as_str(), p.reference.as_str(), p.version),
            ("attest", "v1", None)
        );
        let p = pin("        uses: fredericrous/attest/sign@v1.7.0").unwrap();
        assert_eq!(
            (p.action.as_str(), p.version.as_deref()),
            ("attest/sign", Some("1.7.0"))
        );
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let p = pin(&format!("  - uses: fredericrous/attest@{sha} # v1.6.0")).unwrap();
        assert_eq!(
            (p.reference.as_str(), p.version.as_deref()),
            (sha, Some("1.6.0"))
        );
        let p = pin(&format!(
            "uses: https://github.com/fredericrous/attest@{sha}"
        ))
        .unwrap();
        assert_eq!(p.version, None);
        assert_eq!(pin("uses: fredericrous/attestation@v1"), None);
        assert_eq!(pin("uses: actions/checkout@v4"), None);
        assert_eq!(pin("# uses: fredericrous/attest@v1"), None);
    }

    #[test]
    fn a_moving_major_is_current_and_a_bare_sha_is_not() {
        let state = |line: &str| AttestState {
            pins: vec![pin(line).unwrap()],
            ..AttestState::default()
        };
        assert!(!state("uses: fredericrous/attest@v1").pins_before(1, 7));
        assert!(state("uses: fredericrous/attest@v1.6.0").pins_before(1, 7));
        assert!(!state("uses: fredericrous/attest@v1.7.0").pins_before(1, 7));
        assert!(
            state("uses: fredericrous/attest@0123456789abcdef0123456789abcdef01234567")
                .pins_before(1, 7)
        );
        assert!(!AttestState::default().pins_before(1, 7));
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "user.email=t@example.org",
                "-c",
                "user.name=t",
            ])
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git")
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn the_old_opt_out_and_a_declared_spec_are_read_from_head() {
        let dir = std::env::temp_dir().join(format!("amont-fleet-attest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".github/workflows")).unwrap();
        git(&dir, &["init", "-q"]);
        std::fs::write(dir.join("amont.conf"), "set planExempt off\n").unwrap();
        std::fs::write(dir.join(".github/attest-inputs"), "ci-test src\n").unwrap();
        std::fs::write(
            dir.join(".github/workflows/ci.yaml"),
            "steps:\n  - uses: fredericrous/attest@v1\n",
        )
        .unwrap();
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-qm", "init"]);
        let s = read(&dir);
        assert!(s.deprecated_opt_out);
        assert!(s.spec_with_gates);
        assert_eq!(s.pins.len(), 1);

        // Both lines, as the sweep writes them: no longer deprecated alone.
        std::fs::write(
            dir.join(".github/attest-inputs"),
            "@plan-exempt off\nci-test src\n",
        )
        .unwrap();
        git(&dir, &["commit", "-qam", "directive"]);
        let s = read(&dir);
        assert!(!s.deprecated_opt_out);
        assert!(s.spec_with_gates);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
