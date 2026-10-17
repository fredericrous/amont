//! The signed successor to [`crate::gate_stamp`]: an attestation CI can trust.
//!
//! `gate_stamp` answers a LOCAL question — "did the push gate already run on
//! this commit?" — and its notes deliberately never leave the machine, because
//! an unsigned note is only as honest as whoever can write the ref. This
//! module answers the REMOTE version of the same question: "may CI skip a
//! test job because the equivalent gate already passed here?" — and for that
//! the note has to travel, so it has to be signed.
//!
//! # What is amont's, and what is attest's
//!
//! The attestation FORMAT — the payload, a note's blocks, the `attest-inputs`
//! spec, fingerprints, the canonical tree, the note keys, signing, the
//! fetch-append-push publish loop and the verifier — is attest's, and amont
//! calls the `git_attest` library for every part of it (decisions ADR-0037).
//! There used to be a copy here, kept in step by hand; a fix to the format now
//! lands once.
//!
//! What stays here is amont's: WHEN to attest and WHICH gates a push vouches
//! for (the dispatcher's knowledge), its own config and key, the recursive-push
//! guard below, every line it prints, and the provenance record amont-agent
//! hands it (decisions ADR-0036), which it signs and publishes through the
//! same library under its own namespace.
//!
//! Every failure mode points the same direction as `gate_stamp`'s: no key, a
//! signer that errors, a note git refused, a notes push the remote rejected —
//! all mean "no attestation", and no attestation means CI RUNS the tests.
//! Nothing here can let an untested tree skip CI; it can only cost a
//! redundant run.
//!
//! One sharp edge is the notes push itself: `git push` from inside pre-push
//! runs pre-push again. The child carries [`PUSH_GUARD`] in its environment
//! and the dispatcher yields immediately when it sees it — checking a ref
//! list that is only ever `refs/notes/amont-attest` would be work spent
//! proving nothing.

use std::path::{Path, PathBuf};

use git_attest::format::block;
use git_attest::notes::{publish as publish_notes, Publish, Published};
use git_attest::produce::attestation;

use crate::pushrefs::PushRef;

pub use git_attest::format::{platform, FORMAT, INPUTS_REF, NAMESPACE, NOTES_REF};
pub use git_attest::notes::{classify_push as classify_push_stderr, PushFailure};

/// The notes ref, fully qualified — `update-ref -d` needs it.
pub const NOTES_FULL_REF: &str = "refs/notes/amont-attest";
pub const INPUTS_FULL_REF: &str = "refs/notes/amont-attest-inputs";

/// The provenance record (decisions ADR-0036, amont `docs/provenance.md`):
/// what a push carried besides its tests, the plan and the verdicts on it.
/// Its own ref, so CI's attestation verifier never reads it, and its own
/// namespace, so a provenance signature is never accepted as an attestation.
pub const PROVENANCE_REF: &str = "amont-provenance";
pub const PROVENANCE_FULL_REF: &str = "refs/notes/amont-provenance";
pub const PROVENANCE_NAMESPACE: &str = "amont-provenance";
pub const PROVENANCE_FORMAT: &str = "amont-provenance-v1";

/// Environment marker carried by the notes push so the recursive pre-push
/// invocation stands down. See the module doc.
pub const PUSH_GUARD: &str = "AMONT_ATTEST_PUSH";

/// The repository a hook stands in: git starts hooks at the worktree top.
fn here() -> &'static Path {
    Path::new(".")
}

/// The opt-in switch. Off by default: an attestation is a statement to
/// another system, and amont does not speak for a repository that never
/// asked it to.
const TOGGLE: &str = "amont.attest";

/// Where the signing key lives when the repository does not say.
const KEY_CONFIG: &str = "amont.attestKey";
const KEY_DEFAULT: &str = ".ssh/amont-attest";

/// Is the recursive-push marker set on THIS invocation?
pub fn push_guard_active() -> bool {
    std::env::var_os(PUSH_GUARD).is_some()
}

/// Has this repository opted in?
pub fn enabled(settings: &crate::config::Settings) -> bool {
    crate::config::boolean_or(settings, TOGGLE, false)
}

/// The signing key path: `amont.attestKey`, else `~/.ssh/amont-attest`.
///
/// Read like `amont.knownIdentity` is — a raw string through git, unset
/// collapsing to the default — because a path has no shape git could
/// validate for us anyway.
fn key_path() -> Option<PathBuf> {
    if let Some(k) = crate::git::stdout(&["config", "--get", KEY_CONFIG]) {
        if !k.is_empty() {
            return Some(PathBuf::from(k));
        }
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(KEY_DEFAULT))
}

/// pre-push, after every block gate has passed: attest each pushed tip and
/// send the notes refs to the remote being pushed.
///
/// `gates` is what the dispatcher saw actually PASS — `Warned` and
/// `Unavailable` never appear in it, because "could not run" is not
/// "passed". Empty means nothing testlike ran, and an attestation listing
/// no gates would be a signed way of saying nothing.
///
/// What each block says, and every key it is filed under (the tree and the
/// commit in the main ref; each declared gate's fingerprint and the canonical
/// tree in the inputs ref), is attest's [`attestation`]. The note goes on the
/// TREE as well as the commit: the tree is what the signature covers, so it
/// is the key that survives a squash-merge, an amend or a rebase, and the
/// commit note is what `git log --notes` shows and an older verifier finds.
///
/// Best-effort throughout, and quiet about it: pre-push has already printed
/// its verdicts, and a push that works minus its CI shortcut is not a
/// problem anyone needs to solve at push time.
pub fn attest_push(
    settings: &crate::config::Settings,
    remote: &str,
    refs: &[PushRef],
    gates: &[String],
) {
    if gates.is_empty() || remote.is_empty() || !enabled(settings) {
        return;
    }
    let Some(key) = key_path() else { return };
    if !key.exists() {
        crate::config::complain(
            TOGGLE,
            &format!("signing key {} does not exist", key.display()),
            "no attestation (CI will run the tests)",
        );
        return;
    }
    let gates_line = gates.join(" ");
    let producer = env!("CARGO_PKG_VERSION");
    let mut blocks: Vec<(String, String)> = Vec::new();
    let mut input_blocks: Vec<(String, String)> = Vec::new();
    let (mut n_inputs, mut n_canon) = (0, 0);
    for r in refs {
        if is_zero(&r.local_oid) {
            continue; // deleting a ref pushes no code
        }
        let spec = format!("{}^{{tree}}", r.local_oid);
        let Some(tree) = crate::git::stdout(&["rev-parse", &spec]) else {
            continue;
        };
        // The reasons a route is missing are attest's to give in CI's log;
        // at push time they are noise.
        let mut reasons = Vec::new();
        let att = attestation(
            here(),
            &tree,
            &gates_line,
            &platform(),
            producer,
            &mut reasons,
        );
        let Ok(sig) = git_attest::sign::sign(NAMESPACE, &att.payload, &key) else {
            continue;
        };
        let body = block(&att.payload, &sig);
        n_inputs += att.inputs.len();
        n_canon += usize::from(att.canon.is_some());
        for k in &att.input_keys {
            input_blocks.push((k.clone(), body.clone()));
        }
        blocks.push((tree, body.clone()));
        blocks.push((r.local_oid.clone(), body));
    }
    if blocks.is_empty() {
        return;
    }
    // Each ref on its own: a main ref that is already up to date never
    // short-circuits the inputs ref, which is how a push whose inputs ref
    // failed earlier repairs the missing keys.
    let main_ok = publish(remote, NOTES_REF, &blocks);
    let inputs_ok = input_blocks.is_empty() || publish(remote, INPUTS_REF, &input_blocks);
    if main_ok {
        crate::say!(
            "{} attested {} for CI ({}{})",
            crate::ui::valid_sign(),
            crate::ui::highlight(&gates_line),
            NOTES_REF,
            if input_blocks.is_empty() {
                String::new()
            } else if inputs_ok {
                match (n_inputs, n_canon) {
                    (0, _) => ", canonical tree".to_string(),
                    (n, 0) => format!(", {n} input fingerprints"),
                    (n, _) => format!(", {n} input fingerprints, canonical tree"),
                }
            } else {
                ", input fingerprints not published".to_string()
            },
        );
    }
}

/// Where amont-agent leaves each provenance payload: `<config dir>/amont-agent/
/// provenance`, the config dir resolved as amont-agent's `settings::config_dir`
/// does (`$CLAUDE_CONFIG_DIR`, else `$HOME/.claude`, else
/// `%USERPROFILE%\.claude`).
///
/// Never a git config key: a key could point amont at a directory nobody
/// guards, and the payload's `review` line is only as true as the guard on
/// amont-agent's store. `env` is the lookup, so a test needs no global state.
fn provenance_dir(env: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    let config = match env("CLAUDE_CONFIG_DIR") {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(env("HOME").or_else(|| env("USERPROFILE"))?).join(".claude"),
    };
    Some(config.join("amont-agent").join("provenance"))
}

/// The repository's name as amont-agent files it (its `repo_name`): the
/// directory holding the common git directory, so every worktree of a
/// repository shares one name. Resolved the same way, step for step, or the
/// two sides would look in different directories.
fn repo_name() -> Option<String> {
    let common = crate::git::stdout(&["rev-parse", "--git-common-dir"])?;
    if common.is_empty() {
        return None;
    }
    let common = PathBuf::from(common);
    let common = if common.is_absolute() {
        common
    } else {
        std::env::current_dir().ok()?.join(common)
    };
    let common = std::fs::canonicalize(&common).unwrap_or(common);
    common
        .parent()
        .and_then(std::path::Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
}

/// Sign and publish the provenance record amont-agent left for each pushed
/// commit (ADR-0036). Opt-in, key and fail-open as [`attest_push`], with one
/// difference: it does not wait for a gate to pass, because a push that changes
/// only documentation or its plan still carries a plan and a review.
pub fn provenance_push(settings: &crate::config::Settings, remote: &str, refs: &[PushRef]) {
    if remote.is_empty() || !enabled(settings) {
        return;
    }
    let (Some(dir), Some(name)) = (provenance_dir(|k| std::env::var_os(k)), repo_name()) else {
        return;
    };
    provenance_push_from(&dir.join(name), remote, refs);
}

/// [`provenance_push`] reading payloads from `dir`.
fn provenance_push_from(dir: &std::path::Path, remote: &str, refs: &[PushRef]) {
    let mut found: Vec<(PathBuf, String, String, String)> = Vec::new();
    for r in refs {
        if is_zero(&r.local_oid) {
            continue;
        }
        let file = dir.join(&r.local_oid);
        let Ok(payload) = std::fs::read_to_string(&file) else {
            continue; // no record for this commit: nothing to say
        };
        let spec = format!("{}^{{tree}}", r.local_oid);
        let Some(tree) = crate::git::stdout(&["rev-parse", &spec]) else {
            continue;
        };
        let first = payload.lines().next().unwrap_or_default();
        let claimed = payload.lines().find_map(|l| l.strip_prefix("tree "));
        if first != PROVENANCE_FORMAT || claimed != Some(tree.as_str()) {
            crate::say!(
                "  provenance for {} not signed: its payload is not an {PROVENANCE_FORMAT} record of this tree",
                &r.local_oid[..r.local_oid.len().min(7)]
            );
            continue;
        }
        // The canonical tree is a tree fact the signer owns (attest SPEC,
        // "The handoff"): a payload that states one is refused, never fixed
        // up, and the person is told which file to delete.
        if payload
            .lines()
            .any(|l| l.split_ascii_whitespace().next() == Some("canon"))
        {
            for line in canon_refusal(&r.local_oid, &file) {
                crate::say!("{line}");
            }
            continue;
        }
        // canon(T) whatever `planExempt` says: that governs skipping a gate,
        // not a tree's identity. No canon, no signature: kept for next time.
        let Ok(canon) = git_attest::fingerprint::canon(here(), &tree) else {
            crate::say!(
                "  provenance for {} not signed: its canonical tree cannot be computed;",
                &r.local_oid[..r.local_oid.len().min(7)]
            );
            crate::say!("    the payload is kept for the next push");
            continue;
        };
        let payload = with_canon(&payload, &canon);
        found.push((file, r.local_oid.clone(), tree, payload));
    }
    if found.is_empty() {
        return;
    }
    let Some(key) = key_path().filter(|k| k.exists()) else {
        crate::say!("  provenance not signed: no signing key");
        return;
    };
    let mut blocks: Vec<(String, String)> = Vec::new();
    let mut signed: Vec<PathBuf> = Vec::new();
    for (file, commit, tree, payload) in found {
        let Ok(sig) = git_attest::sign::sign(PROVENANCE_NAMESPACE, &payload, &key) else {
            continue;
        };
        let body = block(&payload, &sig);
        blocks.push((tree, body.clone()));
        blocks.push((commit, body));
        signed.push(file);
    }
    if blocks.is_empty() {
        crate::say!("  provenance not signed: the signer refused");
        return;
    }
    if publish(remote, PROVENANCE_REF, &blocks) {
        for file in &signed {
            if std::fs::remove_file(file).is_err() {
                crate::say!(
                    "  provenance published, but its payload {} could not be removed",
                    file.display()
                );
            }
        }
        crate::say!(
            "{} provenance published ({PROVENANCE_REF})",
            crate::ui::valid_sign()
        );
    } else {
        crate::say!("  provenance not published: kept for the next push");
    }
}

/// `payload` with `canon <canon>` inserted right after its first `tree` line,
/// where attest SPEC `amont-provenance-v1` places it.
fn with_canon(payload: &str, canon: &str) -> String {
    let mut out = String::with_capacity(payload.len() + canon.len() + 7);
    let mut done = false;
    for line in payload.split_inclusive('\n') {
        out.push_str(line);
        if !done && line.starts_with("tree ") {
            if !line.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&format!("canon {canon}\n"));
            done = true;
        }
    }
    out
}

/// What is said for a payload that carries its own `canon` line: the reason
/// and the fix, every line at most 80 columns for a path of usual length. The
/// file is named as its directory and its name, so neither line holds both.
fn canon_refusal(commit: &str, file: &std::path::Path) -> Vec<String> {
    let dir = file.parent().unwrap_or(file).display().to_string();
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    vec![
        format!(
            "  provenance for {} not signed: its payload carries a canon line",
            &commit[..commit.len().min(7)]
        ),
        "    (a pre-release amont-agent wrote it); delete the file".to_string(),
        format!("    {name} in"),
        format!("    {dir}"),
        "    and upgrade amont-agent".to_string(),
    ]
}

/// How many times a push that lost a race is retried before giving up.
const PUSH_ATTEMPTS: u32 = 4;

/// Publish `blocks` (object, block) to `notes_ref` (short name) on `remote`
/// through attest's loop: fetch the remote's ref into a temporary one, append
/// each block not already there, push, and go again on a lost race. Never
/// `notes add -f` (it erases every other producer's block) and never a push
/// of the local ref (it would re-publish a block origin revoked). The push
/// carries [`PUSH_GUARD`] so this hook's own pre-push stands down, and does
/// not wait between rounds: this is the commit path.
fn publish(remote: &str, notes_ref: &str, blocks: &[(String, String)]) -> bool {
    let how = Publish {
        remote,
        attempts: PUSH_ATTEMPTS,
        pause: false,
        identity: None,
        push_env: &[(PUSH_GUARD, "1")],
    };
    let mut log = Vec::new();
    matches!(
        publish_notes(here(), &how, notes_ref, blocks, &mut log),
        Published::Pushed | Published::AlreadyPresent
    )
}

/// `amont attest covered` — the verifying side, as a one-liner for CI that is
/// neither GitHub nor Forgejo: the gate names a VALID attestation covers for
/// the tree checked out here, space-separated, or `None`.
///
/// attest's verifier, the one its action and `git-attest covered` run: input
/// fingerprints, the canonical tree and every block of a note included.
/// `principal` of `None` accepts whoever the signature says, if that key is in
/// the signers file. Lines that say the repository's refs changed, or that
/// something committed is deprecated, go to stderr; fail-open is the caller's
/// contract, not its option.
pub fn covered(
    signers: &Path,
    principal: Option<&str>,
    require_platform: Option<&str>,
) -> Option<String> {
    let v = git_attest::verify::evaluate(here(), signers, principal, require_platform, &[], false);
    for line in v.loud.iter().chain(&v.warnings) {
        eprintln!("amont: {line}");
    }
    (!v.gates.is_empty()).then(|| v.gates.join(" "))
}

/// Where the repository keeps its `allowed_signers`, resolved from the
/// repository root as attest resolves it.
pub fn default_signers() -> Option<PathBuf> {
    git_attest::verify::default_signers(here())
}

/// A ref oid that is all zeros — git's spelling of "no object" in the
/// pre-push ref list, for any hash width.
fn is_zero(oid: &str) -> bool {
    !oid.is_empty() && oid.bytes().all(|b| b == b'0')
}

/// uninstall: forget the local ref. The copies already pushed to remotes are
/// statements we made and stand by; only OUR bookkeeping is removed — the
/// same line `gate_stamp::forget` draws.
pub fn forget() -> bool {
    crate::git::succeeds(&["update-ref", "-d", NOTES_FULL_REF])
}

/// The same, for a repository this process is not standing in.
pub fn forget_in(repo: &std::path::Path) -> bool {
    crate::git::succeeds_in(repo, &["update-ref", "-d", NOTES_FULL_REF])
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::process::Stdio;

    fn canon(tree: &str) -> Option<String> {
        git_attest::fingerprint::canon(here(), tree).ok()
    }

    fn sign(payload: &str, key: &Path) -> Option<String> {
        git_attest::sign::sign(NAMESPACE, payload, key).ok()
    }

    fn verify(payload: &str, sig: &str, signers: &Path, principal: &str) -> bool {
        verify_in(NAMESPACE, payload, sig, signers, principal)
    }

    fn verify_in(
        namespace: &str,
        payload: &str,
        sig: &str,
        signers: &Path,
        principal: &str,
    ) -> bool {
        git_attest::sign::verify(namespace, payload, sig, signers, Some(principal)).is_ok()
    }

    /// `amont.attest` and the key path come from git config, which each test
    /// sets on its own repo; no policy is in play.
    fn test_settings() -> crate::config::Settings {
        crate::config::Settings::default()
    }

    use std::io::Write;
    use std::path::Path;

    /// Real repositories and real keys: every function here is a conversation
    /// with git or ssh-keygen, and a mocked conversation tests the one we
    /// imagined. Same doctrine as `gate_stamp`'s tests.
    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("attest-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A fixture git call that FAILS where it fails.
    ///
    /// This used to discard the exit status, and that is how a rare flake
    /// stayed unreadable for a day: if the setup `git commit` did not
    /// happen, the test carried on to an unborn HEAD, and the panic landed
    /// three lines later on a missing gate stamp — a product-shaped
    /// failure for a fixture-shaped cause. Same rule the checks obey:
    /// git failing is not git answering.
    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "fixture: git {args:?} in {} exited {:?}: {}",
            dir.display(),
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn repo(name: &str) -> PathBuf {
        let d = dir(name);
        git(&d, &["init", "-q", "--template=", "."]);
        git(&d, &["config", "user.email", "t@t.test"]);
        git(&d, &["config", "user.name", "t"]);
        d
    }

    /// A throwaway ed25519 key plus the `allowed_signers` line CI would
    /// commit for it, namespace-pinned exactly as the docs instruct.
    fn keypair(d: &Path) -> (PathBuf, PathBuf) {
        let key = d.join("attest_key");
        let ok = std::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "test", "-f"])
            .arg(&key)
            .status()
            .expect("ssh-keygen must exist for these tests")
            .success();
        assert!(ok, "keygen failed");
        let pubkey = std::fs::read_to_string(key.with_extension("pub")).unwrap();
        let signers = d.join("allowed_signers");
        std::fs::write(
            &signers,
            format!("t@t.test namespaces=\"{NAMESPACE}\" {pubkey}"),
        )
        .unwrap();
        (key, signers)
    }

    /// The module talks to the repo at the process cwd; serialised against
    /// every other cwd-moving test via the crate-wide lock.
    fn in_repo<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
        let _guard = crate::TEST_CWD.lock().unwrap_or_else(|p| p.into_inner());
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir).unwrap();
        let r = f();
        std::env::set_current_dir(prev).unwrap();
        r
    }

    /// A bare remote plus a work repo opted in, ready to push.
    fn remote_and_work(name: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let d = dir(name);
        let (key, signers) = keypair(&d);
        let remote = d.join("remote.git");
        std::fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "-q", "--bare", "--template=", "."]);
        let work = repo(&format!("{name}-work"));
        git(
            &work,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&work, &["config", "amont.attest", "true"]);
        git(&work, &["config", "amont.attestKey", key.to_str().unwrap()]);
        std::fs::write(work.join("a.ts"), "x").unwrap();
        git(&work, &["add", "a.ts"]);
        git(&work, &["commit", "-qm", "chore: a"]);
        (d, work, remote, signers)
    }

    fn push_ref_for(work: &Path) -> PushRef {
        PushRef {
            local_ref: "refs/heads/main".into(),
            local_oid: git(work, &["rev-parse", "HEAD"]),
            remote_ref: "refs/heads/main".into(),
            remote_oid: "0".repeat(40),
        }
    }

    #[test]
    fn the_provenance_dir_is_resolved_like_amont_agents_config_dir() {
        use std::ffi::OsString;
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| OsString::from(v))
            }
        };
        assert_eq!(
            provenance_dir(env(&[("CLAUDE_CONFIG_DIR", "/c"), ("HOME", "/h")])),
            Some(PathBuf::from("/c/amont-agent/provenance"))
        );
        assert_eq!(
            provenance_dir(env(&[("HOME", "/h"), ("USERPROFILE", "/u")])),
            Some(PathBuf::from("/h/.claude/amont-agent/provenance"))
        );
        assert_eq!(
            provenance_dir(env(&[("USERPROFILE", "/u")])),
            Some(PathBuf::from("/u/.claude/amont-agent/provenance"))
        );
        assert_eq!(provenance_dir(env(&[])), None);
    }

    /// A repository with a bare origin, the toggle on and a key; one commit
    /// that carries a plan. Returns (scratch, work, remote, head, tree,
    /// signers).
    fn provenance_world(name: &str) -> (PathBuf, PathBuf, PathBuf, String, String, PathBuf) {
        provenance_world_in(name, None)
    }

    /// [`provenance_world`] in a repository of `object_format`.
    fn provenance_world_in(
        name: &str,
        object_format: Option<&str>,
    ) -> (PathBuf, PathBuf, PathBuf, String, String, PathBuf) {
        let d = dir(name);
        let (key, _) = keypair(&d);
        let pubkey = std::fs::read_to_string(key.with_extension("pub")).unwrap();
        let signers = d.join("allowed_signers_provenance");
        std::fs::write(
            &signers,
            format!("t@t.test namespaces=\"{NAMESPACE},{PROVENANCE_NAMESPACE}\" {pubkey}"),
        )
        .unwrap();
        let remote = d.join("remote.git");
        std::fs::create_dir_all(&remote).unwrap();
        let fmt = format!("--object-format={}", object_format.unwrap_or("sha1"));
        git(&remote, &["init", "-q", "--bare", "--template=", &fmt, "."]);
        let work = match object_format {
            None => repo(&format!("{name}-work")),
            Some(f) => {
                let w = dir(&format!("{name}-work"));
                let fmt = format!("--object-format={f}");
                git(&w, &["init", "-q", "--template=", &fmt, "."]);
                git(&w, &["config", "user.email", "t@t.test"]);
                git(&w, &["config", "user.name", "t"]);
                w
            }
        };
        git(
            &work,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&work, &["config", "amont.attest", "true"]);
        git(&work, &["config", "amont.attestKey", key.to_str().unwrap()]);
        std::fs::write(work.join("README.md"), "words").unwrap();
        std::fs::create_dir_all(work.join("docs/plans")).unwrap();
        std::fs::write(work.join("docs/plans/p.md"), "# P\n").unwrap();
        git(&work, &["add", "README.md", "docs/plans/p.md"]);
        git(&work, &["commit", "-qm", "docs: words"]);
        let head = git(&work, &["rev-parse", "HEAD"]);
        let tree = git(&work, &["rev-parse", "HEAD^{tree}"]);
        (d, work, remote, head, tree, signers)
    }

    fn record(head: &str, tree: &str) -> String {
        format!(
            "{PROVENANCE_FORMAT}\ncommit {head}\ntree {tree}\n\
             plan docs/plans/p.md {}\nreview reviewed {}\npreview not-required\n\
             producer amont-agent 0.0.0\n",
            "a".repeat(64),
            "c".repeat(64)
        )
    }

    /// canon(T) as attest SPEC defines it, by hand and independent of
    /// `attest_inputs`: the quoted listing, the oid of every regular file
    /// under the top-level `docs/plans/` zeroed, through `git hash-object`.
    fn canon_by_hand(work: &Path, tree: &str) -> String {
        let listing = git(
            work,
            &[
                "-c",
                "core.quotePath=true",
                "ls-tree",
                "-r",
                "--full-tree",
                tree,
            ],
        );
        let mut text = String::new();
        for line in listing.lines() {
            let (head, path) = line.split_once('\t').unwrap();
            let f: Vec<&str> = head.split(' ').collect();
            let plan = matches!(f[0], "100644" | "100755")
                && (path.starts_with("docs/plans/") || path.starts_with("\"docs/plans/"));
            if plan {
                text.push_str(&format!(
                    "{} {} {}\t{path}\n",
                    f[0],
                    f[1],
                    "0".repeat(f[2].len())
                ));
            } else {
                text.push_str(&format!("{line}\n"));
            }
        }
        let mut child = std::process::Command::new("git")
            .arg("-C")
            .arg(work)
            .args(["hash-object", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// The signed payload of `object` in the remote's provenance ref.
    fn provenance_payload(remote: &Path, object: &str) -> (String, String) {
        let note = git(remote, &["notes", "--ref", PROVENANCE_REF, "show", object]);
        let (p, sig) = note
            .split_once("\n\n")
            .expect("payload, blank line, signature");
        (format!("{p}\n"), sig.to_string())
    }

    fn push_of(head: &str) -> PushRef {
        PushRef {
            local_ref: "refs/heads/feat/x".into(),
            local_oid: head.to_string(),
            remote_ref: "refs/heads/feat/x".into(),
            remote_oid: "0".repeat(40),
        }
    }

    /// A payload left by amont-agent reaches the remote as a note on both
    /// the tree and the commit, verifies under its own namespace only, and
    /// leaves the attestation ref alone. The file is gone once published.
    #[test]
    fn a_provenance_payload_is_signed_and_published_by_tree_and_commit() {
        let (d, work, remote, head, tree, signers) = provenance_world("prov-e2e");
        let payloads = d.join("payloads");
        std::fs::create_dir_all(&payloads).unwrap();
        let file = payloads.join(&head);
        let body = record(&head, &tree);
        std::fs::write(&file, &body).unwrap();
        let gates = ["pre-push-cargo-test".to_string()];
        in_repo(&work, || {
            attest_push(&test_settings(), "origin", &[push_of(&head)], &gates)
        });
        let attested = git(&remote, &["notes", "--ref", NOTES_REF, "show", &head]);
        in_repo(&work, || {
            provenance_push_from(&payloads, "origin", &[push_of(&head)])
        });
        // The signer inserts canon(T) right after `tree`; everything else is
        // amont-agent's payload byte for byte.
        let canon = canon_by_hand(&work, &tree);
        assert_ne!(
            canon, tree,
            "the tree carries a plan, so canon(T) is not its oid"
        );
        let signed = body.replacen(
            &format!("tree {tree}\n"),
            &format!("tree {tree}\ncanon {canon}\n"),
            1,
        );
        for object in [&head, &tree] {
            let (p, sig) = provenance_payload(&remote, object);
            assert_eq!(p, signed, "the payload, with canon(T) inserted, is signed");
            assert_eq!(p.lines().nth(3), Some(format!("canon {canon}").as_str()));
            assert!(verify_in(
                PROVENANCE_NAMESPACE,
                &p,
                &sig,
                &signers,
                "t@t.test"
            ));
            assert!(
                !verify_in(NAMESPACE, &p, &sig, &signers, "t@t.test"),
                "a provenance signature is never an attestation"
            );
        }
        assert!(!file.exists(), "a published payload is deleted");
        assert_eq!(
            git(&remote, &["notes", "--ref", NOTES_REF, "show", &head]),
            attested,
            "the attestation note is byte-identical with a provenance payload present"
        );
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&work);
    }

    #[test]
    fn a_payload_for_another_tree_is_neither_signed_nor_deleted() {
        let (d, work, remote, head, _tree, _) = provenance_world("prov-mismatch");
        let payloads = d.join("payloads");
        std::fs::create_dir_all(&payloads).unwrap();
        let file = payloads.join(&head);
        std::fs::write(&file, record(&head, &"b".repeat(40))).unwrap();
        in_repo(&work, || {
            provenance_push_from(&payloads, "origin", &[push_of(&head)])
        });
        assert!(file.exists(), "a mismatched payload is kept");
        let refs = git(&remote, &["for-each-ref", PROVENANCE_FULL_REF]);
        assert!(refs.is_empty(), "nothing was published: {refs}");
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&work);
    }

    /// A payload that already states a canonical tree was written by a
    /// pre-release amont-agent: it is refused, never signed, never removed.
    #[test]
    fn a_payload_with_a_canon_line_is_refused_and_kept() {
        let (d, work, remote, head, tree, _) = provenance_world("prov-canon");
        let payloads = d.join("payloads");
        std::fs::create_dir_all(&payloads).unwrap();
        let file = payloads.join(&head);
        let old = record(&head, &tree).replacen(
            &format!("tree {tree}\n"),
            &format!("tree {tree}\ncanon {}\n", "c".repeat(64)),
            1,
        );
        std::fs::write(&file, &old).unwrap();
        in_repo(&work, || {
            provenance_push_from(&payloads, "origin", &[push_of(&head)])
        });
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            old,
            "kept as it was"
        );
        let refs = git(&remote, &["for-each-ref", PROVENANCE_FULL_REF]);
        assert!(refs.is_empty(), "nothing was published: {refs}");
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&work);
    }

    /// The refusal names the reason, the file and the fix, in lines of at
    /// most 80 columns for a usual path, a sha256 commit included.
    #[test]
    fn the_canon_refusal_fits_in_80_columns() {
        for commit in [
            "9f1c2ab0d6e4c1b2a3f405e6d7c8b9a0f1e2d3c4".to_string(),
            "e".repeat(64),
        ] {
            let file = PathBuf::from("/Users/someone/.claude/amont-agent/provenance/amont-agent")
                .join(&commit);
            let lines = canon_refusal(&commit, &file);
            assert!(lines[0].starts_with("  provenance for "), "{lines:?}");
            assert!(lines[0].contains(&commit[..7]), "{lines:?}");
            let all = lines.join("\n");
            assert!(all.contains("canon line") && all.contains("pre-release amont-agent"));
            assert!(all.contains(&format!("    {commit} in\n")), "{all}");
            assert!(all.contains("upgrade amont-agent"), "{all}");
            for l in &lines {
                assert!(
                    l.chars().count() <= 80,
                    "{} columns: {l}",
                    l.chars().count()
                );
            }
        }
    }

    /// No canon(T), no signature: the payload waits for the next push.
    #[test]
    fn a_failed_canon_keeps_the_payload() {
        let (d, work, remote, head, tree, _) = provenance_world("prov-nocanon");
        // The commit and its tree still resolve; listing the tree does not.
        let docs = git(&work, &["rev-parse", &format!("{tree}:docs")]);
        let objects = PathBuf::from(git(&work, &["rev-parse", "--git-path", "objects"]));
        let objects = if objects.is_absolute() {
            objects
        } else {
            work.join(objects)
        };
        std::fs::remove_file(objects.join(&docs[..2]).join(&docs[2..])).unwrap();
        let payloads = d.join("payloads");
        std::fs::create_dir_all(&payloads).unwrap();
        let file = payloads.join(&head);
        std::fs::write(&file, record(&head, &tree)).unwrap();
        in_repo(&work, || {
            assert_eq!(canon(&tree), None);
            provenance_push_from(&payloads, "origin", &[push_of(&head)])
        });
        assert!(file.exists(), "a payload without a canon is kept");
        let refs = git(&remote, &["for-each-ref", PROVENANCE_FULL_REF]);
        assert!(refs.is_empty(), "nothing was published: {refs}");
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&work);
    }

    /// The hook's cwd is the worktree, so canon(T) is of the repository's own
    /// object format: 64 hex in a sha256 repository.
    #[test]
    fn a_sha256_repository_signs_a_64_hex_canon() {
        let (d, work, remote, head, tree, _) = provenance_world_in("prov-sha256", Some("sha256"));
        assert_eq!(tree.len(), 64);
        let payloads = d.join("payloads");
        std::fs::create_dir_all(&payloads).unwrap();
        std::fs::write(payloads.join(&head), record(&head, &tree)).unwrap();
        in_repo(&work, || {
            provenance_push_from(&payloads, "origin", &[push_of(&head)])
        });
        let (p, _) = provenance_payload(&remote, &head);
        let canon = canon_by_hand(&work, &tree);
        assert_eq!(canon.len(), 64);
        assert_eq!(
            p.lines().nth(3),
            Some(format!("canon {canon}").as_str()),
            "{p}"
        );
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&work);
    }

    #[test]
    fn a_failed_publish_keeps_the_payload_for_the_next_push() {
        let (d, work, _remote, head, tree, _) = provenance_world("prov-fail");
        git(
            &work,
            &[
                "remote",
                "set-url",
                "origin",
                d.join("gone.git").to_str().unwrap(),
            ],
        );
        let payloads = d.join("payloads");
        std::fs::create_dir_all(&payloads).unwrap();
        let file = payloads.join(&head);
        std::fs::write(&file, record(&head, &tree)).unwrap();
        in_repo(&work, || {
            provenance_push_from(&payloads, "origin", &[push_of(&head)])
        });
        assert!(file.exists(), "an unpublished payload is kept");
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&work);
    }

    /// The payload directory comes from the environment only: a git config
    /// key naming a directory full of payloads is never read.
    #[test]
    fn a_config_key_cannot_point_amont_at_other_payloads() {
        let (d, work, remote, head, tree, _) = provenance_world("prov-config");
        let elsewhere = d.join("elsewhere");
        let name = work.file_name().unwrap().to_string_lossy().into_owned();
        std::fs::create_dir_all(elsewhere.join(&name)).unwrap();
        std::fs::write(elsewhere.join(&name).join(&head), record(&head, &tree)).unwrap();
        for key in ["amont.provenanceDir", "amont.agent.provenanceDir"] {
            git(&work, &["config", key, elsewhere.to_str().unwrap()]);
        }
        in_repo(&work, || {
            provenance_push(&test_settings(), "origin", &[push_of(&head)])
        });
        let refs = git(&remote, &["for-each-ref", PROVENANCE_FULL_REF]);
        assert!(refs.is_empty(), "a configured directory was read: {refs}");
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&work);
    }

    #[test]
    fn no_payload_publishes_nothing() {
        let (d, work, remote, head, _, _) = provenance_world("prov-none");
        let payloads = d.join("payloads");
        std::fs::create_dir_all(&payloads).unwrap();
        in_repo(&work, || {
            provenance_push_from(&payloads, "origin", &[push_of(&head)])
        });
        let refs = git(&remote, &["for-each-ref", PROVENANCE_FULL_REF]);
        assert!(refs.is_empty(), "{refs}");
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&work);
    }

    #[test]
    fn a_missing_key_signs_nothing() {
        assert!(sign("anything", Path::new("/nonexistent/key")).is_none());
    }

    /// The full journey: a repo with the toggle on pushes, and the BARE
    /// remote ends up holding a note whose payload verifies and matches the
    /// pushed tree. This is everything CI relies on, minus CI.
    #[test]
    fn an_enabled_push_leaves_a_verifiable_note_on_the_remote() {
        let d = dir("e2e");
        let (key, signers) = keypair(&d);
        let remote = d.join("remote.git");
        std::fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "-q", "--bare", "--template=", "."]);
        let work = repo("e2e-work");
        git(
            &work,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&work, &["config", "amont.attest", "true"]);
        git(&work, &["config", "amont.attestKey", key.to_str().unwrap()]);
        std::fs::write(work.join("a.ts"), "x").unwrap();
        git(&work, &["add", "a.ts"]);
        git(&work, &["commit", "-qm", "chore: a"]);
        let head = git(&work, &["rev-parse", "HEAD"]);
        let tree = git(&work, &["rev-parse", "HEAD^{tree}"]);
        let push_ref = PushRef {
            local_ref: "refs/heads/main".into(),
            local_oid: head.clone(),
            remote_ref: "refs/heads/main".into(),
            remote_oid: "0".repeat(40),
        };
        in_repo(&work, || {
            attest_push(
                &test_settings(),
                "origin",
                &[push_ref],
                &["pre-push-run-tests-js".into()],
            );
        });
        // The note exists on the REMOTE — the whole point is that it travels.
        let body = git(&remote, &["notes", "--ref", NOTES_REF, "show", &head]);
        assert!(!body.is_empty(), "no note reached the remote");
        let (p, sig) = body
            .split_once("\n\n")
            .expect("payload, blank line, signature");
        let p = format!("{p}\n"); // the blank-line split ate payload's trailing newline
        assert!(p.starts_with(FORMAT));
        assert!(
            p.contains(&format!("tree {tree}")),
            "attests the pushed tree"
        );
        assert!(
            verify(&p, sig, &signers, "t@t.test"),
            "the remote copy verifies"
        );
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&work);
    }

    /// Off by default: a repo that never opted in makes no statement, even
    /// with everything else in place.
    /// amont's half of input fingerprints: a push from a repository with a
    /// spec publishes the inputs ref too, and counts what it filed. What
    /// the block says (the `input` lines, the fingerprint, the key) is
    /// attest's, tested there (decisions ADR-0037).
    #[test]
    fn a_push_with_a_spec_publishes_the_inputs_ref() {
        let (_d, work, remote, _) = remote_and_work("inputs");
        std::fs::create_dir_all(work.join(".github")).unwrap();
        std::fs::write(
            work.join(".github/attest-inputs"),
            "pre-push-run-tests-js a.ts\n",
        )
        .unwrap();
        git(&work, &["add", ".github/attest-inputs"]);
        git(&work, &["commit", "-qm", "chore: spec"]);
        in_repo(&work, || {
            attest_push(
                &test_settings(),
                "origin",
                &[push_ref_for(&work)],
                &["pre-push-run-tests-js".into()],
            );
        });
        let listed = git(&remote, &["notes", "--ref", INPUTS_REF, "list"]);
        // How many keys (the input's, the canonical tree's) is attest's.
        assert!(
            !listed.trim().is_empty(),
            "the inputs ref reached the remote"
        );
    }

    #[test]
    fn no_opt_in_means_no_note() {
        let d = dir("optout");
        let (key, _) = keypair(&d);
        let remote = d.join("remote.git");
        std::fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "-q", "--bare", "--template=", "."]);
        let work = repo("optout-work");
        git(
            &work,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&work, &["config", "amont.attestKey", key.to_str().unwrap()]);
        std::fs::write(work.join("a.ts"), "x").unwrap();
        git(&work, &["add", "a.ts"]);
        git(&work, &["commit", "-qm", "chore: a"]);
        let head = git(&work, &["rev-parse", "HEAD"]);
        let push_ref = PushRef {
            local_ref: "refs/heads/main".into(),
            local_oid: head.clone(),
            remote_ref: "refs/heads/main".into(),
            remote_oid: "0".repeat(40),
        };
        in_repo(&work, || {
            attest_push(
                &test_settings(),
                "origin",
                &[push_ref],
                &["pre-push-run-tests-js".into()],
            );
        });
        assert!(
            git(&remote, &["notes", "--ref", NOTES_REF, "list"]).is_empty(),
            "an un-opted-in repo attested something"
        );
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&work);
    }

    /// A deletion pushes no code; an empty gate list says nothing. Neither
    /// may produce a note even in an enabled repo.
    #[test]
    fn deletions_and_empty_gates_attest_nothing() {
        let d = dir("nothing");
        let (key, _) = keypair(&d);
        let remote = d.join("remote.git");
        std::fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "-q", "--bare", "--template=", "."]);
        let work = repo("nothing-work");
        git(
            &work,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&work, &["config", "amont.attest", "true"]);
        git(&work, &["config", "amont.attestKey", key.to_str().unwrap()]);
        std::fs::write(work.join("a.ts"), "x").unwrap();
        git(&work, &["add", "a.ts"]);
        git(&work, &["commit", "-qm", "chore: a"]);
        let head = git(&work, &["rev-parse", "HEAD"]);
        let deletion = PushRef {
            local_ref: "(delete)".into(),
            local_oid: "0".repeat(40),
            remote_ref: "refs/heads/gone".into(),
            remote_oid: head.clone(),
        };
        let real = PushRef {
            local_ref: "refs/heads/main".into(),
            local_oid: head,
            remote_ref: "refs/heads/main".into(),
            remote_oid: "0".repeat(40),
        };
        in_repo(&work, || {
            attest_push(
                &test_settings(),
                "origin",
                &[deletion],
                &["pre-push-pytest".into()],
            );
            attest_push(&test_settings(), "origin", &[real], &[]);
        });
        assert!(git(&remote, &["notes", "--ref", NOTES_REF, "list"]).is_empty());
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&work);
    }

    /// The half CI actually calls, from CI's own vantage point: a fresh
    /// clone. `covered` fetches the notes ref itself, verifies, and answers
    /// with the gates — then stops answering the moment the tree drifts or
    /// the note is replaced by something unsigned.
    #[test]
    fn covered_answers_in_a_fresh_clone_and_rejects_drift_and_forgery() {
        let d = dir("covered");
        let (key, signers) = keypair(&d);
        let remote = d.join("remote.git");
        std::fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "-q", "--bare", "--template=", "."]);
        // The fixture pushes to `main`; a bare init on a machine whose
        // init.defaultBranch is the historical default leaves HEAD on
        // `master`, and a clone of that repository checks out NOTHING —
        // `covered` then answers None with a perfectly good note sitting in
        // the ref. Caught only in CI: dev machines set main globally.
        git(&remote, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        let work = repo("covered-work");
        git(
            &work,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&work, &["config", "amont.attest", "true"]);
        git(&work, &["config", "amont.attestKey", key.to_str().unwrap()]);
        std::fs::write(work.join("a.ts"), "x").unwrap();
        git(&work, &["add", "a.ts"]);
        git(&work, &["commit", "-qm", "chore: a"]);
        git(&work, &["push", "-q", "origin", "HEAD:main"]);
        let head = git(&work, &["rev-parse", "HEAD"]);
        let push_ref = PushRef {
            local_ref: "refs/heads/main".into(),
            local_oid: head.clone(),
            remote_ref: "refs/heads/main".into(),
            remote_oid: "0".repeat(40),
        };
        in_repo(&work, || {
            attest_push(
                &test_settings(),
                "origin",
                &[push_ref],
                &["pre-push-pytest".into()],
            );
        });
        let clone = d.join("ci-checkout");
        git(
            &d,
            &[
                "clone",
                "-q",
                "--template=",
                remote.to_str().unwrap(),
                clone.to_str().unwrap(),
            ],
        );
        in_repo(&clone, || {
            assert_eq!(
                covered(&signers, Some("t@t.test"), Some(&platform())).as_deref(),
                Some("pre-push-pytest"),
                "a fresh clone verifies the attestation and reads the gates"
            );
            assert_eq!(
                covered(&signers, Some("t@t.test"), None).as_deref(),
                Some("pre-push-pytest"),
                "`any` covers a platform-independent suite"
            );
            // The matrix case this exists for: another leg asking about a
            // suite that never ran there.
            assert_eq!(
                covered(&signers, Some("t@t.test"), Some("s390x-aix")),
                None,
                "a pass on one platform is not evidence about another"
            );
            assert_eq!(
                covered(&signers, Some("someone@else.test"), None),
                None,
                "an unlisted principal covers nothing"
            );
        });
        // Tree drift: a new commit in the checkout is not the attested tree.
        std::fs::write(clone.join("b.ts"), "y").unwrap();
        git(&clone, &["config", "user.email", "t@t.test"]);
        git(&clone, &["config", "user.name", "t"]);
        git(&clone, &["add", "b.ts"]);
        git(&clone, &["commit", "-qm", "chore: b"]);
        in_repo(&clone, || {
            assert_eq!(
                covered(&signers, Some("t@t.test"), None),
                None,
                "drifted tree"
            );
        });
        // Forgery: replace the remote's notes with unsigned ones. covered's
        // own fetch pulls them in, and they must read as "no attestation".
        //
        // BOTH keys, because an attestation is now findable by the tree as
        // well as by the commit. Forging one and leaving the other is not a
        // forgery — it is a genuine signed note the attacker failed to
        // reach, and covered is right to honour it. The test said "a foreign
        // note is not a stamp" and has to forge every place a note lives to
        // mean that.
        let head_tree = git(&work, &["rev-parse", "HEAD^{tree}"]);
        for object in [&head, &head_tree] {
            git(
                &work,
                &[
                    "notes", "--ref", NOTES_REF, "add", "-f", "-m", "garbage", object,
                ],
            );
        }
        git(
            &work,
            &[
                "push",
                "-q",
                "origin",
                &format!("+{NOTES_FULL_REF}:{NOTES_FULL_REF}"),
            ],
        );
        // The genuine block is also filed under the canonical tree's key in
        // the inputs ref, which attest's verifier reads (the frozen copy here
        // never did): every place a note lives includes that ref.
        git(
            &work,
            &["push", "-q", "origin", &format!(":{INPUTS_FULL_REF}")],
        );
        git(&clone, &["reset", "-q", "--hard", &head]);
        in_repo(&clone, || {
            assert_eq!(
                covered(&signers, Some("t@t.test"), None),
                None,
                "a foreign note is not a stamp"
            );
        });
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&work);
    }

    /// The silent-death case: a workflow step running with a
    /// `working-directory` inside the repo must still find the committed
    /// signers file. Before this, `default_signers` looked relative to the
    /// cwd, found nothing, and every such repo fail-opened forever with no
    /// symptom — CI stayed green and the gate simply never fired.
    #[test]
    fn default_signers_is_found_from_a_subdirectory() {
        let work = repo("signers-subdir");
        std::fs::create_dir_all(work.join(".forgejo")).unwrap();
        std::fs::write(work.join(".forgejo/allowed_signers"), "t@t.test x\n").unwrap();
        let sub = work.join("packages").join("thing");
        std::fs::create_dir_all(&sub).unwrap();
        in_repo(&sub, || {
            let found = default_signers().expect("resolved from the repo root, not the cwd");
            assert!(found.ends_with(".forgejo/allowed_signers"));
            assert!(found.exists(), "the path it returns must be usable as-is");
        });
        let _ = std::fs::remove_dir_all(&work);
    }

    #[test]
    fn zero_oids_of_any_width_are_zero() {
        assert!(is_zero(&"0".repeat(40)));
        assert!(is_zero(&"0".repeat(64)));
        assert!(!is_zero("0a0000"));
        assert!(!is_zero(""));
    }

    #[test]
    fn forget_removes_the_local_ref() {
        let work = repo("forget");
        std::fs::write(work.join("a.ts"), "x").unwrap();
        git(&work, &["add", "a.ts"]);
        git(&work, &["commit", "-qm", "chore: a"]);
        git(
            &work,
            &["notes", "--ref", NOTES_REF, "add", "-m", "x", "HEAD"],
        );
        in_repo(&work, forget);
        assert!(git(&work, &["notes", "--ref", NOTES_REF, "list"]).is_empty());
        let _ = std::fs::remove_dir_all(&work);
    }

    // --- attest 1.4.0 parity: optional paths --------------------------------

    // --- attest 1.4.0 parity: the notes ref is origin's mirror --------------
}
