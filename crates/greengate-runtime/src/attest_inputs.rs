//! amont's side of attest's input fingerprints and canonical tree.
//!
//! The format itself (the `attest-inputs` grammar, fingerprints, the
//! canonical tree, note keys, the opt-out, the completeness proof) belongs to
//! the `git_attest` library, attest's one implementation of it (decisions
//! ADR-0037). What stays here is amont's own knowledge on top of it: which
//! route its LOCAL gate stamps take, and the key those stamps are filed
//! under. The hook runs at the worktree top, so "the repository" is `.`.

use std::path::Path;

use git_attest::spec::{at, coverage, plan_exempt, PlanExempt, DEPRECATED_OPT_OUT};

pub use git_attest::spec::{conf_says_plan_exempt_off, CoverageError, Findings, Spec, EXEMPT};

/// The repository a hook stands in: git starts hooks at the worktree top.
fn here() -> &'static Path {
    Path::new(".")
}

/// The spec `tree` commits, when it is there and valid. No spec, an invalid
/// or ambiguous one, a tree git cannot list: `None`, and the gate stamps fall
/// back to the tree (or, with no spec at all, the canonical route).
pub(crate) fn spec_at(tree: &str) -> Option<Spec> {
    match at(here(), tree) {
        Ok(Some((_, spec))) => Some(spec),
        _ => None,
    }
}

/// The fingerprint of `gate` on `tree`, as attest computes it.
pub(crate) fn fingerprint(tree: &str, spec: &Spec, gate: &str) -> Option<String> {
    git_attest::fingerprint::fingerprint(here(), tree, spec, gate).ok()
}

/// The synthetic key of (gate, fingerprint) in the inputs ref.
pub(crate) fn input_key(gate: &str, fp: &str) -> Option<String> {
    git_attest::fingerprint::key(here(), gate, fp).ok()
}

/// Does `gate` declare `file`, itself or a directory above it? Asked of
/// attest's coverage rule, so the ancestry is attest's, not a copy.
pub(crate) fn declares(spec: &Spec, gate: &str, file: &str) -> bool {
    coverage(
        spec,
        &[gate.to_string()],
        &[file.as_bytes().to_vec()],
        |_| true,
    )
    .is_empty()
}

/// The canonical fingerprint of `tree` when its gates are reused canonically:
/// no spec at all (a successful listing that lists neither location), the
/// repository has not opted out, and git could compute it. An invalid,
/// ambiguous or unlistable spec opens no canonical route, so no canonical
/// stamps.
pub(crate) fn canonical_route(tree: &str) -> Option<String> {
    match at(here(), tree) {
        Ok(None) => {}
        _ => return None,
    }
    if plan_exempt(here(), tree).is_off() {
        return None;
    }
    git_attest::fingerprint::canon(here(), tree).ok()
}

/// The key under which the local gate stamps of a canonical fingerprint are
/// filed in amont's own inputs ref: one note per fingerprint, holding the
/// tokens of every gate that passed on it. amont's own preimage, never
/// attest's, so a stamp can never be read as an attestation.
pub(crate) fn canon_gate_key(canon: &str) -> Option<String> {
    crate::git::stdout_piped_in(
        here(),
        &["hash-object", "--stdin"],
        format!("amont-gate-canon {canon}\n").as_bytes(),
    )
    .filter(|s| git_attest::format::is_oid(s))
}

/// The line to print when HEAD opts out of the canonical tree through
/// `set planExempt off` in `amont.conf` alone: still honoured, deprecated,
/// and read by attest only until 2.0.0 (decisions ADR-0037).
pub fn deprecated_opt_out() -> Option<&'static str> {
    let tree = crate::git::stdout(&["rev-parse", "--verify", "--quiet", "HEAD^{tree}"])?;
    (plan_exempt(here(), &tree) == PlanExempt::OffByAmontConf).then_some(DEPRECATED_OPT_OUT)
}

/// Do the required gates declare, completely, everything `tree` tracks?
/// attest's proof (`git-attest check-inputs` runs the same one).
pub fn check(tree: &str, required: &[String]) -> Result<Findings, CoverageError> {
    git_attest::spec::check_coverage(here(), tree, required)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tree git cannot list has no known spec, so it opens no route, and a
    /// failing listing is not an absent opt-out: fail narrow.
    #[test]
    fn a_tree_git_cannot_list_opens_no_route() {
        let bogus = "0123456789012345678901234567890123456789";
        assert_eq!(spec_at(bogus), None);
        assert_eq!(canonical_route(bogus), None);
        assert!(plan_exempt(here(), bogus).is_off());
    }

    #[test]
    fn a_gate_declares_a_file_or_a_directory_above_it() {
        let spec = git_attest::spec::parse_spec(b"lint src amont.conf\ntest src\n").unwrap();
        assert!(declares(&spec, "lint", "amont.conf"));
        assert!(declares(&spec, "lint", "src/main.rs"));
        assert!(!declares(&spec, "test", "amont.conf"));
        assert!(
            !declares(&spec, "fmt", "amont.conf"),
            "an undeclared gate declares nothing"
        );
    }
}
