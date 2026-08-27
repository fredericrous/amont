//! The signed successor to [`crate::gate_stamp`]: an attestation CI can trust.
//!
//! `gate_stamp` answers a LOCAL question — "did the push gate already run on
//! this commit?" — and its notes deliberately never leave the machine, because
//! an unsigned note is only as honest as whoever can write the ref. This
//! module answers the REMOTE version of the same question: "may CI skip a
//! test job because the equivalent gate already passed here?" — and for that
//! the note has to travel, so it has to be signed.
//!
//! Shape of the note, attached to each pushed tip in `refs/notes/amont-attest`:
//!
//! ```text
//! amont-attest-v1
//! tree <tree the gates ran against>
//! gates <names of the pre-push checks that PASSED>
//! amont <version that produced it>
//!
//! -----BEGIN SSH SIGNATURE-----
//! …signature over the four lines above…
//! -----END SSH SIGNATURE-----
//! ```
//!
//! The signature covers the **tree**, not the commit: tests read content, not
//! messages, so a reword or a tree-preserving rebase keeps its attestation —
//! the same reasoning as `gate_stamp`'s tree binding. CI's skip condition is
//! tree equality with its own checkout plus a valid signature over exactly
//! that tree, verified with stock `ssh-keygen -Y verify` against an
//! `allowed_signers` file committed in the consuming repository. amont itself
//! still never runs in CI (`docs/ci.md`) — CI verifies a document.
//!
//! # Where each half lives now
//!
//! The **producer** — [`attest_push`], `sign`, `key_path`, [`enabled`] — is
//! this crate's, and stays. It needs the gate names only `dispatch` knows,
//! reads amont's own config, and coordinates the recursive-push guard below.
//!
//! The **consumer** — [`verify`], [`covered`], [`split_note`],
//! [`default_signers`], [`first_principal`] — was extracted to
//! <https://github.com/fredericrous/attest>, because reading a signed document
//! is the part every OTHER repository needs and amont is a strange thing to
//! install just to do it. The CI templates call that action instead of the
//! ~30 lines of shell they used to carry.
//!
//! The copy here is therefore **frozen: bug fixes only**. New work — better
//! diagnostics, more formats, other forges — happens in that repository, and
//! its `tests/conformance.sh` is the contract both sides answer to. `amont
//! attest covered` keeps working for anyone already calling it.
//!
//! Signing uses `ssh-keygen -Y sign` as a subprocess, like every other tool
//! this crate talks to. Hand-rolling ed25519 in a zero-dependency crate would
//! be the one thing worse than a dependency.
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

use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::pushrefs::PushRef;

/// First token of every note body. Versioned like `gate_stamp::FORMAT`: a
/// future amont that changes the payload bumps this, and CI's verifier reads
/// an unknown version as "no attestation".
///
/// v1 → v2 added the `platform` line. The bump is the point: a v1 verifier
/// has no idea the tests it is about to skip ran on a different operating
/// system, and reading v2 as unknown makes it run them. Fail-safe in the
/// only direction this module ever fails.
pub const FORMAT: &str = "amont-attest-v2";

/// The notes ref, spelled the way `git notes --ref` wants it.
pub const NOTES_REF: &str = "amont-attest";

/// The same ref, fully qualified — the push refspec and `update-ref -d` both
/// need it.
pub const NOTES_FULL_REF: &str = "refs/notes/amont-attest";

/// The `ssh-keygen -Y` namespace, on both the signing and verifying side.
/// Namespaces exist so a signature minted for one purpose cannot be replayed
/// for another; an `allowed_signers` entry pinned to this namespace accepts
/// nothing else.
pub const NAMESPACE: &str = "amont-attest";

/// Environment marker carried by the notes push so the recursive pre-push
/// invocation stands down. See the module doc.
pub const PUSH_GUARD: &str = "AMONT_ATTEST_PUSH";

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
pub fn enabled() -> bool {
    crate::config::boolean_or(TOGGLE, false)
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

/// Where a suite ran, as `<arch>-<os>` — `aarch64-macos`, `x86_64-linux`,
/// `x86_64-windows`.
///
/// Coarser than a target triple on purpose: the libc flavour is not
/// something `std` can answer, and the question a CI matrix actually asks is
/// "did this run on MY leg". Coarse and honest beats precise and guessed.
pub fn platform() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

/// The exact bytes the signature covers. One datum per line, trailing
/// newline included — CI reconstructs this from the note text, so the shape
/// is a contract, not a convenience.
///
/// `platform` is signed alongside the gates because a pass is a pass **on
/// something**: `cargo test` green on an arm64 Mac says nothing about the
/// Windows leg of a matrix, and a note that omitted where it ran invited
/// exactly that skip.
pub fn payload(tree: &str, gates: &[String]) -> String {
    format!(
        "{FORMAT}\ntree {tree}\ngates {}\nplatform {}\namont {}\n",
        gates.join(" "),
        platform(),
        env!("CARGO_PKG_VERSION")
    )
}

/// `ssh-keygen -Y sign` over `payload`, armored signature back. `None` for
/// every failure — a missing binary, a missing key, a signer that said no —
/// because an attestation we cannot mint is simply one CI never sees.
fn sign(payload: &str, key: &std::path::Path) -> Option<String> {
    use std::io::Write;
    let mut child = Command::new("ssh-keygen")
        .args(["-Y", "sign", "-n", NAMESPACE, "-f"])
        .arg(key)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(payload.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    let sig = String::from_utf8_lossy(&out.stdout).trim().to_string();
    sig.starts_with("-----BEGIN SSH SIGNATURE-----")
        .then_some(sig)
}

/// `ssh-keygen -Y verify`: is `sig` a valid signature over `payload` by a
/// `principal` key listed in `allowed_signers` for our namespace?
///
/// The runtime never gates on this — CI verifies with its own stock tooling —
/// but owning the verifying half keeps the roundtrip honest in tests and
/// gives a future `amont attest verify` its engine.
pub fn verify(
    payload: &str,
    sig: &str,
    allowed_signers: &std::path::Path,
    principal: &str,
) -> bool {
    use std::io::Write;
    // -Y verify takes the signature as a FILE; the payload rides stdin.
    let sig_file = std::env::temp_dir().join(format!(
        "amont-attest-verify-{}-{:p}.sig",
        std::process::id(),
        &sig
    ));
    if std::fs::write(&sig_file, format!("{sig}\n")).is_err() {
        return false;
    }
    let ok = (|| {
        let mut child = Command::new("ssh-keygen")
            .args(["-Y", "verify", "-n", NAMESPACE, "-I", principal, "-f"])
            .arg(allowed_signers)
            .arg("-s")
            .arg(&sig_file)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        child.stdin.take()?.write_all(payload.as_bytes()).ok()?;
        child.wait().ok().map(|s| s.success())
    })()
    .unwrap_or(false);
    let _ = std::fs::remove_file(&sig_file);
    ok
}

/// pre-push, after every block gate has passed: attest each pushed tip and
/// send the notes ref to the remote being pushed.
///
/// `gates` is what the dispatcher saw actually PASS — `Warned` and
/// `Unavailable` never appear in it, because "could not run" is not
/// "passed". Empty means nothing testlike ran, and an attestation listing
/// no gates would be a signed way of saying nothing.
///
/// Best-effort throughout, and quiet about it: pre-push has already printed
/// its verdicts, and a push that works minus its CI shortcut is not a
/// problem anyone needs to solve at push time.
pub fn attest_push(remote: &str, refs: &[PushRef], gates: &[String]) {
    if gates.is_empty() || remote.is_empty() || !enabled() {
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
    let mut noted = false;
    for r in refs {
        if is_zero(&r.local_oid) {
            continue; // deleting a ref pushes no code
        }
        let spec = format!("{}^{{tree}}", r.local_oid);
        let Some(tree) = crate::git::stdout(&["rev-parse", &spec]) else {
            continue;
        };
        let body = match sign(&payload(&tree, gates), &key) {
            Some(sig) => format!("{}\n{sig}", payload(&tree, gates)),
            None => continue,
        };
        // The note goes on the TREE as well as the commit, and the tree is
        // the key that matches what the signature already covers.
        //
        // Keying only by commit is why the verifier had to HUNT — `HEAD`,
        // then `HEAD^2` for a pull request's merge commit — and the hunt has
        // a floor it cannot reach past: a squash-merge onto a main that has
        // moved produces a commit with neither the note nor a parent that
        // has it, while an attestation for that exact tree may be sitting in
        // the ref. Signed for the content, findable only by the container.
        //
        // Keyed by tree, the lookup is one step and survives squash-merge,
        // amend and rebase — every rewrite that preserves content. Which is
        // the whole claim the payload makes: `tree <sha>`, signed.
        //
        // Both, not either: the commit note is what `git log --notes` shows
        // and what an older verifier looks for, so dropping it would break
        // consumers mid-upgrade for no gain.
        let _ =
            crate::git::succeeds(&["notes", "--ref", NOTES_REF, "add", "-f", "-m", &body, &tree]);
        if crate::git::succeeds(&[
            "notes",
            "--ref",
            NOTES_REF,
            "add",
            "-f",
            "-m",
            &body,
            &r.local_oid,
        ]) {
            noted = true;
        }
    }
    if noted && push_notes(remote) {
        crate::say!(
            "{} attested {} for CI ({})",
            crate::ui::valid_sign(),
            crate::ui::highlight(&gates.join(" ")),
            NOTES_REF,
        );
    }
}

/// `amont attest covered` — the verifying side, as CI's one-liner.
///
/// Answers "which gates does a VALID attestation cover for the tree checked
/// out here?", doing everything the workflow snippet used to spell out in
/// sh: freshen the notes ref (best-effort), look for a note on `HEAD` and —
/// for a PR's merge commit — on `HEAD^2`, insist on the format version,
/// insist the attested tree is byte-for-byte `HEAD^{tree}`, and verify the
/// signature against `allowed_signers`. Thirty lines of workflow copied into
/// every repository is exactly the drift this binary exists to end.
///
/// `None` for every failure, and the CLI prints nothing and exits 0 on
/// `None` — fail-open is the caller's contract, not its option. A CI step
/// reading empty output runs its tests, which is always the safe answer.
///
/// This is amont running in CI, which `docs/ci.md` forbids for CHECKS — the
/// line held is narrower than the slogan: CI still never runs a check
/// through amont; this verifies a document about checks that already ran.
/// `require_platform` is the leg asking. `Some("x86_64-linux")` covers only
/// a suite that ran there; `None` is the caller stating that this suite's
/// result does not depend on where it ran (a pure-JS unit run, say) and is
/// spelled `--platform any` in a committed workflow, where it is reviewed
/// like any other line of the repository.
pub fn covered(
    signers: &std::path::Path,
    principal: &str,
    require_platform: Option<&str>,
) -> Option<String> {
    let refspec = format!("+{NOTES_FULL_REF}:{NOTES_FULL_REF}");
    let _ = crate::git::succeeds(&["fetch", "origin", &refspec]);
    let head_tree = crate::git::stdout(&["rev-parse", "HEAD^{tree}"])?;
    // HEAD first: a push event's checkout IS the attested commit. HEAD^2
    // second: a PR checkout is a merge commit git made a moment ago, whose
    // second parent is the pushed tip that carries the note — and the tree
    // comparison below still measures against what is ACTUALLY checked out,
    // so a merge whose tree drifted from the tested tip never skips.
    // The TREE first, which is what the signature covers and therefore the
    // only key that cannot go stale: it finds the attestation after a
    // squash-merge, an amend or a rebase, none of which the commit-shaped
    // candidates below survive when main has moved underneath.
    //
    // `HEAD` and `HEAD^2` stay after it, for notes written by an amont that
    // only ever keyed by commit. They cost one `rev-parse` each and only
    // when the tree lookup found nothing.
    for candidate in [head_tree.as_str(), "HEAD", "HEAD^2"] {
        let Some(object) = crate::git::stdout(&["rev-parse", "--verify", candidate]) else {
            continue;
        };
        let Some(body) = crate::git::stdout(&["notes", "--ref", NOTES_REF, "show", &object]) else {
            continue;
        };
        let Some((payload, sig)) = split_note(&body) else {
            continue;
        };
        // By prefix, not by position: the payload has grown a line once
        // already, and a positional reader silently mis-assigns every field
        // after an insertion rather than failing.
        let mut lines = payload.lines();
        if lines.next() != Some(FORMAT) {
            continue;
        }
        let field = |name: &str| {
            payload
                .lines()
                .find_map(|l| l.strip_prefix(name).and_then(|r| r.strip_prefix(' ')))
                .map(str::trim)
        };
        let (Some(tree), Some(gates), Some(ran_on)) =
            (field("tree"), field("gates"), field("platform"))
        else {
            continue;
        };
        if tree != head_tree || gates.is_empty() {
            continue; // wrong content, or a signed way of saying nothing
        }
        // The leg asking is not the leg that ran: a macOS `cargo test` is no
        // evidence about Windows. `None` means the caller has stated this
        // suite is platform-independent.
        if require_platform.is_some_and(|want| want != ran_on) {
            continue;
        }
        if verify(&payload, &sig, signers, principal) {
            return Some(gates.to_string());
        }
    }
    None
}

/// Where a repository keeps its `allowed_signers` when the caller does not
/// say — the Forgejo location first, the GitHub one second. `None` when
/// neither exists, which the CLI reads as "nothing is covered".
/// Resolved from the REPOSITORY ROOT, not the working directory. A workflow
/// that sets `working-directory` (a monorepo running a matrix inside
/// `packages/<x>`, say) puts the step in a subdirectory, where a relative
/// `.forgejo/allowed_signers` does not exist — and the CLI would then find no
/// signers, print nothing, and fail open FOREVER. Silently: the suite still
/// runs, CI still passes, and nothing anywhere says the gate is dead. That is
/// the worst shape a fail-open can take, so the path is anchored.
pub fn default_signers() -> Option<PathBuf> {
    let root = crate::git::stdout(&["rev-parse", "--show-toplevel"]).map(PathBuf::from);
    [".forgejo/allowed_signers", ".github/allowed_signers"]
        .into_iter()
        .map(|rel| match &root {
            Some(root) => root.join(rel),
            None => PathBuf::from(rel),
        })
        .find(|p| p.exists())
}

/// The first principal an `allowed_signers` file names — the identity to
/// verify against when the caller does not pass `--principal`. One key, one
/// principal is the overwhelmingly common shape of this file; a multi-signer
/// team passes the flag.
pub fn first_principal(signers: &std::path::Path) -> Option<String> {
    let body = std::fs::read_to_string(signers).ok()?;
    body.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .and_then(|l| l.split_whitespace().next())
        .map(str::to_string)
}

/// A note body back into the exact bytes that were signed, plus the
/// signature block. The blank-line split ate the payload's trailing newline;
/// it is part of the signed bytes, so it goes back.
fn split_note(body: &str) -> Option<(String, String)> {
    let (payload, sig) = body.split_once("\n\n")?;
    if !sig.starts_with("-----BEGIN SSH SIGNATURE-----") {
        return None;
    }
    Some((format!("{payload}\n"), sig.to_string()))
}

/// Push the notes ref, marked so the recursive pre-push yields.
///
/// Not `git::succeeds` — that helper cannot set an environment variable, and
/// the guard is the entire point of this wrapper existing.
fn push_notes(remote: &str) -> bool {
    let refspec = format!("{NOTES_FULL_REF}:{NOTES_FULL_REF}");
    Command::new("git")
        .args(["push", remote, &refspec])
        .env(PUSH_GUARD, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
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

    #[test]
    fn the_payload_is_the_documented_contract() {
        let p = payload(
            "abc123",
            &["pre-push-pytest".into(), "pre-push-cargo-test".into()],
        );
        let lines: Vec<&str> = p.lines().collect();
        assert_eq!(lines[0], FORMAT);
        assert_eq!(lines[1], "tree abc123");
        assert_eq!(lines[2], "gates pre-push-pytest pre-push-cargo-test");
        assert_eq!(lines[3], format!("platform {}", platform()));
        assert_eq!(lines[4], format!("amont {}", env!("CARGO_PKG_VERSION")));
        assert!(
            p.ends_with('\n'),
            "CI reconstructs these bytes; the trailing newline is part of them"
        );
    }

    #[test]
    fn sign_verify_roundtrip_and_tamper_rejection() {
        let d = dir("roundtrip");
        let (key, signers) = keypair(&d);
        let p = payload("deadbeef", &["pre-push-pytest".into()]);
        let sig = sign(&p, &key).expect("signing with a real key succeeds");
        assert!(verify(&p, &sig, &signers, "t@t.test"));
        // One byte of the tree changed: the signature must not carry over —
        // this is the entire difference between this module and gate_stamp.
        let tampered = payload("deadbeee", &["pre-push-pytest".into()]);
        assert!(!verify(&tampered, &sig, &signers, "t@t.test"));
        // The right payload under the wrong principal is also no.
        assert!(!verify(&p, &sig, &signers, "someone@else.test"));
        let _ = std::fs::remove_dir_all(&d);
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
            attest_push("origin", &[push_ref], &["pre-push-run-tests-js".into()]);
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
            attest_push("origin", &[push_ref], &["pre-push-run-tests-js".into()]);
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
            attest_push("origin", &[deletion], &["pre-push-pytest".into()]);
            attest_push("origin", &[real], &[]);
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
            attest_push("origin", &[push_ref], &["pre-push-pytest".into()]);
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
                covered(&signers, "t@t.test", Some(&platform())).as_deref(),
                Some("pre-push-pytest"),
                "a fresh clone verifies the attestation and reads the gates"
            );
            assert_eq!(
                covered(&signers, "t@t.test", None).as_deref(),
                Some("pre-push-pytest"),
                "`any` covers a platform-independent suite"
            );
            // The matrix case this exists for: another leg asking about a
            // suite that never ran there.
            assert_eq!(
                covered(&signers, "t@t.test", Some("s390x-aix")),
                None,
                "a pass on one platform is not evidence about another"
            );
            assert_eq!(
                covered(&signers, "someone@else.test", None),
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
            assert_eq!(covered(&signers, "t@t.test", None), None, "drifted tree");
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
        git(&clone, &["reset", "-q", "--hard", &head]);
        in_repo(&clone, || {
            assert_eq!(
                covered(&signers, "t@t.test", None),
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
            assert_eq!(
                first_principal(&found).as_deref(),
                Some("t@t.test"),
                "and readable from there"
            );
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

    /// The attestation survives the commit being rewritten around the same
    /// content — which is how work reaches `main`.
    ///
    /// The verifier used to look on `HEAD` and then `HEAD^2`, and that hunt
    /// has a floor it cannot reach past: a squash-merge onto a main that has
    /// moved produces a commit carrying neither the note nor a parent that
    /// has one, while an attestation for that exact tree sits in the ref.
    /// Signed for the content, findable only by the container.
    ///
    /// The tree key is the same claim the payload already makes — `tree
    /// <sha>` — so this is not a widening: `covered` still refuses unless
    /// the payload's tree equals the checked-out tree AND the signature
    /// verifies. Both are asserted below by the sibling test; this one
    /// asserts only that a legitimate attestation is still FOUND.
    #[test]
    fn an_attestation_survives_a_rewrite_that_keeps_the_tree() {
        let d = dir("rewritten");
        let (key, signers) = keypair(&d);
        let remote = d.join("remote.git");
        std::fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "-q", "--bare", "--template=", "."]);
        git(&remote, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        let work = repo("rewritten-work");
        git(
            &work,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&work, &["config", "amont.attest", "true"]);
        git(&work, &["config", "amont.attestKey", key.to_str().unwrap()]);
        std::fs::write(work.join("a.ts"), "x").unwrap();
        git(&work, &["add", "a.ts"]);
        git(&work, &["commit", "-qm", "feat: on a branch"]);
        git(&work, &["push", "-q", "origin", "HEAD:main"]);
        let branch_tip = git(&work, &["rev-parse", "HEAD"]);
        let push_ref = PushRef {
            local_ref: "refs/heads/main".into(),
            local_oid: branch_tip.clone(),
            remote_ref: "refs/heads/main".into(),
            remote_oid: "0".repeat(40),
        };
        in_repo(&work, || {
            attest_push("origin", &[push_ref], &["pre-push-pytest".into()]);
        });

        // Stand in for the forge's squash: a DIFFERENT commit object with the
        // SAME tree, and — crucially — NOT a parent of anything the verifier
        // would reach, so `HEAD^2` cannot save it.
        git(
            &work,
            &[
                "commit",
                "-q",
                "--amend",
                "-m",
                "feat: squashed by the forge",
            ],
        );
        let rewritten = git(&work, &["rev-parse", "HEAD"]);
        assert_ne!(rewritten, branch_tip, "the fixture must rewrite the commit");
        assert_eq!(
            git(&work, &["rev-parse", "HEAD^{tree}"]),
            git(&work, &["rev-parse", &format!("{branch_tip}^{{tree}}")]),
            "…while preserving the tree, which is the premise"
        );
        git(&work, &["push", "-q", "-f", "origin", "HEAD:main"]);

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
                covered(&signers, "t@t.test", None).as_deref(),
                Some("pre-push-pytest"),
                "the attestation must be found by the tree it signed"
            );
        });
        let _ = std::fs::remove_dir_all(&d);
    }
}
