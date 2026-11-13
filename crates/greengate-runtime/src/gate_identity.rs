//! What a stamp token says it was made by.
//!
//! A gate stamp is keyed by the git TREE, so it proves the content has not
//! changed since a check passed. It said nothing about WHICH check passed:
//! upgrade the tool a declared check runs (a new `node`, a reinstalled
//! `cargo`) and the old pass still skipped the new run. The tree already
//! covers everything in the repository, the manifest and the lockfiles
//! included, so this adds only what lives OUTSIDE it.
//!
//! A token is `<name>@<identity>`, and only the complete token authorizes a
//! skip (`hooks.gate-stamp-key`, ADR-0010):
//!
//! - **a declared check**: its full command line, its flags, and the identity
//!   of the program it runs. A program found on `PATH` is identified by the
//!   resolved binary's path, size and modification time, which a package
//!   manager's upgrade changes and which needs no command run (a script's own
//!   `--version` could do anything). A program named by a path inside the
//!   repository is the tree's business and adds nothing.
//! - **a built-in**: its name and amont's own version.
//!
//! An identity that cannot be worked out — the program is missing, git would
//! not hash the material — is `None`, and `None` is a cache miss everywhere:
//! no token is written, none is honoured, the gate runs. Never a skip on a
//! guess.

use crate::check::Stage;
use crate::manifest::{External, Kind, RunsOn};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// How much of the hash a token carries. Sixteen hex digits is far more than
/// a few dozen gates on one machine can collide in, and keeps a note line
/// readable.
const WIDTH: usize = 16;

/// The token for the gate called `name`, whether that is a declared check
/// (looked up by id, then by a pre-commit short name) or a built-in.
pub fn token(externals: &[External], name: &str) -> Option<String> {
    match declared(externals, name) {
        Some(ext) => external_token(ext, name),
        None => builtin_token(name),
    }
}

/// The declared check called `name`: by id, then by a pre-commit short name.
fn declared<'a>(externals: &'a [External], name: &str) -> Option<&'a External> {
    externals.iter().find(|e| e.id == name).or_else(|| {
        externals
            .iter()
            .find(|e| e.stage == Stage::PreCommit && e.short_name == name)
    })
}

/// Is the gate called `name` a declared command whose program is not
/// installed here (ADR-0018)? Such a gate has no identity, so no stamp or
/// record can ever answer for it. Any other reason a token cannot be made (a
/// hash git would not compute, a built-in) is not this: those gates stay owed.
pub fn program_missing(externals: &[External], name: &str) -> bool {
    match declared(externals, name).map(|ext| &ext.kind) {
        Some(Kind::Runnable { program, .. }) => binary_identity(program).is_none(),
        Some(Kind::Unusable { .. }) | None => false,
    }
}

/// The token for one declared check, written under `name` (the script name at
/// commit time, the id at push time).
pub fn external_token(ext: &External, name: &str) -> Option<String> {
    let Kind::Runnable {
        program,
        args,
        files,
        docs_skip,
        runs_on,
        ..
    } = &ext.kind
    else {
        return None;
    };
    let binary = binary_identity(program)?;
    // A pre-commit pass recorded before a declared command could find the
    // index being committed (`GREENGATE_INDEX_FILE`) may have judged the old one
    // under `commit -a`, through any wrapper: none of them counts, so each
    // pre-commit gate runs once more. `index` changes what the gate reads
    // again, so it is part of the token too. Pre-push tokens are unchanged.
    let commit_index = match (ext.stage, runs_on) {
        (Stage::PreCommit, RunsOn::Here { reads_index: true }) => "\0commit-index-v1\0index",
        (Stage::PreCommit, RunsOn::Here { reads_index: false } | RunsOn::AnyPlatform) => {
            "\0commit-index-v1"
        }
        (Stage::PrePush, _) => "",
    };
    let material = format!(
        "declared\0{name}\0{program}\0{}\0files={files}\0docs-skip={docs_skip}\0{binary}{commit_index}",
        args.join("\u{1f}")
    );
    seal(name, &material)
}

/// The versioned contract between amont and the gate-runner image: the Job
/// shape, the `fetch` protocol, the verdict rules (ADR-0016). A change to any
/// of them bumps this, which invalidates every cluster token at once.
pub const CLUSTER_CONTRACT: &str = "greengate-cluster-v1";

/// The token a CLUSTER pass of one declared check is written under, in
/// `refs/notes/greengate-gate-cluster` (ADR-0016). Separate from
/// [`external_token`] on purpose: the program runs in the pinned image, not
/// on this laptop, so the laptop's executable is never inspected — the image
/// digest stands where the binary identity stands there. `None` for a check
/// that is not runnable, or a hash git would not make.
pub(crate) fn cluster_token(
    ext: &External,
    name: &str,
    prepare: Option<&str>,
    image_digest: &str,
) -> Option<String> {
    let Kind::Runnable {
        program,
        args,
        files,
        docs_skip,
        ..
    } = &ext.kind
    else {
        return None;
    };
    let material = format!(
        "{CLUSTER_CONTRACT}\0{name}\0{program}\0{}\0files={files}\0docs-skip={docs_skip}\0\
         prepare={}\0image={image_digest}",
        args.join("\u{1f}"),
        prepare.unwrap_or("")
    );
    seal(name, &material)
}

/// A built-in gate's token.
pub fn builtin_token(name: &str) -> Option<String> {
    seal(
        name,
        &format!("builtin\0{name}\0{}", env!("CARGO_PKG_VERSION")),
    )
}

/// The name a token was written under, for display and accounting only.
pub fn base(token: &str) -> &str {
    token.split_once('@').map_or(token, |(name, _)| name)
}

fn seal(name: &str, material: &str) -> Option<String> {
    static MEMO: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    seal_with(
        MEMO.get_or_init(Default::default),
        name,
        material,
        crate::tree_cache::hash,
    )
}

fn seal_with(
    memo: &Mutex<HashMap<String, Option<String>>>,
    name: &str,
    material: &str,
    hasher: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let cached = memo
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(material)
        .cloned();
    let hash = match cached.flatten() {
        Some(h) => h,
        None => {
            // Only an answer is remembered. A failed hash is a cache miss for
            // this call alone; the next ask tries git again.
            let h = hasher(material)?;
            memo.lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(material.to_string(), Some(h.clone()));
            h
        }
    };
    Some(format!("{name}@{}", hash.get(..WIDTH).unwrap_or(&hash)))
}

/// Who a program is, as far as a skip needs to know.
fn binary_identity(program: &str) -> Option<String> {
    if program.contains('/') && !std::path::Path::new(program).is_absolute() {
        // Relative to the repository: its content is in the tree.
        return Some("in-tree".to_string());
    }
    let path = if program.contains('/') {
        std::path::PathBuf::from(program)
    } else {
        which(program)?
    };
    let canonical = std::fs::canonicalize(&path).ok()?;
    let meta = std::fs::metadata(&canonical).ok()?;
    let modified = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(format!("{}:{}:{modified}", canonical.display(), meta.len()))
}

/// The first executable called `program` on `PATH`.
fn which(program: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for suffix in ["", ".exe", ".cmd", ".bat"] {
            let candidate = dir.join(format!("{program}{suffix}"));
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &std::path::Path) -> bool {
    p.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_names_its_gate_and_is_stable() {
        let a = builtin_token("pre-push-cargo-test").expect("git hashes");
        let b = builtin_token("pre-push-cargo-test").expect("git hashes");
        assert_eq!(a, b);
        assert!(a.starts_with("pre-push-cargo-test@"));
        assert_eq!(base(&a), "pre-push-cargo-test");
        assert_eq!(a.len(), "pre-push-cargo-test@".len() + WIDTH);
    }

    #[test]
    fn different_gates_do_not_share_an_identity() {
        let a = builtin_token("pre-push-cargo-test").expect("hash");
        let b = builtin_token("pre-push-run-tests-js").expect("hash");
        assert_ne!(
            a.split_once('@').map(|x| x.1),
            b.split_once('@').map(|x| x.1)
        );
    }

    /// One failed hash (a fork the scheduler refused) must not decide this
    /// gate's identity for the rest of the process: the next ask tries again.
    #[test]
    fn a_failed_hash_is_not_remembered() {
        let memo = Mutex::new(HashMap::new());
        let calls = std::cell::Cell::new(0);
        let flaky = |_: &str| {
            calls.set(calls.get() + 1);
            (calls.get() > 1).then(|| "0123456789abcdef0123".to_string())
        };
        assert_eq!(seal_with(&memo, "gate", "m", flaky), None);
        assert_eq!(
            seal_with(&memo, "gate", "m", flaky).as_deref(),
            Some("gate@0123456789abcdef")
        );
        // A good answer IS remembered: no third hash.
        assert!(seal_with(&memo, "gate", "m", flaky).is_some());
        assert_eq!(calls.get(), 2);
    }

    /// The contract is hashed into every cluster identity: the green name
    /// changes every cluster token once, so a gate re-runs on the cluster
    /// (CHANGELOG, greengate 4.4). It fails if the old contract is back.
    #[test]
    fn the_cluster_contract_is_green_and_is_hashed_into_the_token() {
        assert_eq!(CLUSTER_CONTRACT, "greengate-cluster-v1");
        let ext = crate::manifest::parse(
            "pre-commit lint * block platform=any definitely-not-a-program-xyz --x\n",
        )
        .pop()
        .expect("one check");
        let now = cluster_token(&ext, "pre-commit-lint", None, "sha256:aa").expect("hash");
        let under = |contract: &str| {
            seal(
                "pre-commit-lint",
                &format!(
                    "{contract}\0pre-commit-lint\0definitely-not-a-program-xyz\0--x\0files=false\0docs-skip=false\0\
                     prepare=\0image=sha256:aa"
                ),
            )
        };
        // The token is the material under the green contract, and the same
        // material under the old one is a different token.
        assert_eq!(Some(now.clone()), under(CLUSTER_CONTRACT));
        assert_ne!(Some(now), under("amont-cluster-v1"));
    }

    /// The cluster identity follows the image digest and the prepare command,
    /// and never needs the program on this machine.
    #[test]
    fn a_cluster_token_follows_the_image_and_needs_no_local_binary() {
        let ext = crate::manifest::parse(
            "pre-commit lint * block platform=any definitely-not-a-program-xyz --x\n",
        )
        .pop()
        .expect("one check");
        assert_eq!(external_token(&ext, "lint"), None, "no local binary");
        let a = cluster_token(&ext, "pre-commit-lint", None, "sha256:aa").expect("hash");
        let b = cluster_token(&ext, "pre-commit-lint", None, "sha256:bb").expect("hash");
        let c = cluster_token(&ext, "pre-commit-lint", Some("make deps"), "sha256:aa").expect("h");
        assert!(a.starts_with("pre-commit-lint@"));
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_eq!(
            cluster_token(&ext, "pre-commit-lint", None, "sha256:aa"),
            Some(a)
        );
    }

    /// A pre-commit pass from before amont told a gate the index being
    /// committed does not count (it may have judged the old index), and the
    /// `index` marker changes what the gate reads again: both are in the
    /// token. A pre-push gate keeps the token it had.
    #[test]
    fn a_commit_time_token_follows_what_the_gate_could_read() {
        let one = |line: &str| crate::manifest::parse(line).pop().expect("one check");
        let plain = one("pre-commit lint * block sh -c true\n");
        let marked = one("pre-commit lint * block index sh -c true\n");
        let pushed = one("pre-push lint * block sh -c true\n");
        let plain_token = external_token(&plain, "lint").expect("sh is there");
        let marked_token = external_token(&marked, "lint").expect("sh is there");
        assert_ne!(plain_token, marked_token);
        let binary = binary_identity("sh").expect("sh is there");
        let before = seal(
            "lint",
            &format!("declared\0lint\0sh\0-c\u{1f}true\0files=false\0docs-skip=false\0{binary}"),
        )
        .expect("hash");
        assert_ne!(
            plain_token, before,
            "a blind pass recorded before no longer counts"
        );
        assert_eq!(
            external_token(&pushed, "lint").expect("sh is there"),
            before,
            "a pre-push token is unchanged"
        );
    }

    /// Only a declared command whose program cannot be found is "missing":
    /// a built-in, or an installed program, is not, whatever its token does.
    #[test]
    fn only_a_declared_program_that_is_not_there_is_missing() {
        let externals = crate::manifest::parse(
            "pre-push gone * block definitely-not-a-program-xyz --x\n\
             pre-push here * block sh -c true\n\
             pre-push broken * nonsense sh -c true\n",
        );
        assert!(
            externals
                .iter()
                .any(|e| e.id == "pre-push-broken" && matches!(e.kind, Kind::Unusable { .. })),
            "the fixture's third line is unusable"
        );
        assert!(!program_missing(&externals, "pre-push-broken"));
        assert!(program_missing(&externals, "pre-push-gone"));
        assert!(!program_missing(&externals, "pre-push-here"));
        assert!(!program_missing(&externals, "pre-push-cargo-test"));
        assert!(!program_missing(&externals, "no-such-gate"));
    }

    #[test]
    fn a_bare_name_has_no_namespace() {
        assert_eq!(base("typecheck"), "typecheck");
        assert_eq!(base("typecheck@abc"), "typecheck");
    }

    #[test]
    fn a_program_that_is_not_there_has_no_identity() {
        assert_eq!(binary_identity("definitely-not-a-program-xyz"), None);
    }

    #[test]
    #[cfg(unix)]
    fn a_program_on_path_is_identified_by_its_binary() {
        let a = binary_identity("sh").expect("sh is on every PATH");
        assert!(a.contains("sh"), "{a}");
        assert_eq!(binary_identity("sh"), Some(a));
    }

    #[test]
    fn a_repo_relative_script_is_the_trees_business() {
        assert_eq!(
            binary_identity("scripts/gate.sh").as_deref(),
            Some("in-tree")
        );
    }
}
