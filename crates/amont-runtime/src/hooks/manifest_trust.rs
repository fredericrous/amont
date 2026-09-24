//! pre-commit-manifest-trust — a commit that changes `amont.conf` cannot be
//! gated by the checks that file declares until they are trusted.
//!
//! Trust is keyed on the file's content (docs/trust.md), so editing
//! `amont.conf` makes every check it declares `Unavailable` — "could not run",
//! which never blocks — until somebody runs `amont trust`. For a pulled or
//! cloned manifest that is the whole point: a repository cannot run commands on
//! this machine until the person here reviews them.
//!
//! For the commit that CHANGES the manifest it is a hole. Its author wrote the
//! new content ("Your own `amont.conf` is trusted by you, once" — docs/trust.md),
//! yet the checks it declares stand down for exactly the commit that introduces
//! them, and everything else in that commit goes through ungated. That is how
//! an `.adr.yaml` the manifest's own `aval check` would have refused was
//! committed in duro-design-system, 2026-09-24.
//!
//! So this check blocks such a commit and names the fix: review, `amont trust`,
//! commit again. It never trusts anything itself — blocking runs no command —
//! so the threat model is unchanged. It fires only when `amont.conf` is staged
//! and not during a merge, rebase, cherry-pick or revert: a manifest arriving
//! from another branch is the pulled case, and stays a visible gap.
//! Downgrade it like any check: `git config amont.severity.pre-commit-manifest-trust warn`.

use std::path::Path;

use crate::check::Outcome;
use crate::hooks::common::{fail, hl, ok, repo_root, staged_files};
use crate::manifest::{Manifest, MANIFEST};
use crate::trust::{self, State};

pub fn run(settings: &crate::config::Settings, manifest: &Manifest) -> Outcome {
    if !staged_files(&[]).iter().any(|f| f == MANIFEST) {
        return Outcome::Inert;
    }
    let root = repo_root();
    run_in(settings, manifest, Path::new(&root))
}

pub fn run_in(settings: &crate::config::Settings, manifest: &Manifest, root: &Path) -> Outcome {
    // Nothing declared, nothing standing down: a manifest of policy lines or
    // pins alone gates no check of this commit.
    if manifest.externals.is_empty() {
        return Outcome::Passed;
    }
    match trust::state(root) {
        State::Trusted | State::NoManifest => {
            ok(
                settings,
                "amont.conf is trusted as staged; its checks gate this commit",
            );
            Outcome::Passed
        }
        state @ (State::Untrusted | State::Changed) => {
            let names: Vec<&str> = manifest
                .externals
                .iter()
                .map(|e| e.short_name.as_str())
                .collect();
            fail(&format!(
                "this commit changes {MANIFEST}, and the {} check(s) it declares ({}) \
                 cannot gate it: {}. Review it with {}, accept it, and commit again \
                 — otherwise this commit goes through with them unrun.",
                names.len(),
                names.join(", "),
                match state {
                    State::Changed => "it changed since it was trusted",
                    _ => "it has never been trusted here",
                },
                hl("amont trust"),
            ));
            Outcome::Failed
        }
    }
}
