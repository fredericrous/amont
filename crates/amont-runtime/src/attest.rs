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
//! The consumer copy here is therefore **frozen: bug fixes only**. New
//! verifying work — better diagnostics, other forges — happens in that
//! repository, and its `tests/conformance.sh` is the contract both sides
//! answer to. `amont attest covered` keeps working for anyone already calling
//! it. The producer follows that repository's `SPEC.md` as it grows: since
//! attest 1.3.0 a committed `.github/attest-inputs` names the paths each gate
//! reads, and the block carries an `input <gate> <fingerprint>` line per
//! declared gate and is filed under that fingerprint in a second notes ref,
//! `refs/notes/amont-attest-inputs`, so an unrelated change on main no longer
//! voids it.
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

/// The second ref, keyed by input fingerprint (attest 1.3.0). Every key there
/// is an oid that is not an object — `git notes` accepts that — which is why
/// it is its own ref: `git notes prune` on it would drop everything.
pub const INPUTS_REF: &str = "amont-attest-inputs";
pub const INPUTS_FULL_REF: &str = "refs/notes/amont-attest-inputs";

/// Where a repository declares what each gate reads, in order of precedence.
const SPEC_PATHS: [&str; 2] = [".forgejo/attest-inputs", ".github/attest-inputs"];

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
///
/// `inputs` are the `input <gate> <fingerprint>` lines (attest 1.3.0), after
/// `platform` and before `amont`, in spec order — additive, so a verifier that
/// predates them reads the block exactly as before.
pub fn payload(tree: &str, gates: &[String], inputs: &[(String, String)]) -> String {
    let mut p = format!(
        "{FORMAT}\ntree {tree}\ngates {}\nplatform {}\n",
        gates.join(" "),
        platform()
    );
    for (gate, fp) in inputs {
        p.push_str(&format!("input {gate} {fp}\n"));
    }
    p.push_str(&format!("amont {}\n", env!("CARGO_PKG_VERSION")));
    p
}

// ---------------------------------------------------------------------------
// Input fingerprints. attest's SPEC.md, "Input fingerprints": the grammar and
// the hashing are that repository's contract, ported here verbatim in
// behaviour — a fingerprint this hook writes must be the one its verifiers
// compute, byte for byte.
// ---------------------------------------------------------------------------

const MAX_SPEC_BYTES: usize = 65536;
const MAX_SPEC_GATES: usize = 64;
const MAX_SPEC_PATHS: usize = 64;

/// A gate name as the spec allows it: `[A-Za-z0-9][A-Za-z0-9._-]{0,63}`.
fn valid_gate(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    name.len() <= 64 && chars.all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

/// One declared path. `?build.rs` in the spec is `optional` (attest 1.4.0):
/// it may be absent, and its absence is then part of the fingerprint. The
/// marker is stripped here, once, so nothing downstream can hand `?x` to git
/// — where it would match nothing and silently drop out of the listing.
#[derive(Debug, Clone, PartialEq)]
struct PathTok {
    path: String,
    optional: bool,
}

/// A path token that is not a literal, root-relative file or directory.
/// `git ls-tree` does not glob, so a wildcard would fingerprint nothing. One
/// leading `?` is the optional marker, not a wildcard; the rest obeys every
/// rule, so `??x` and `?/x` are bad, and so is a lone `?`.
fn bad_path(tok: &str) -> bool {
    let tok = match tok.strip_prefix('?') {
        Some("") => return true,
        Some(rest) => rest,
        None => tok,
    };
    tok.starts_with(':')
        || tok.starts_with('/')
        || tok.starts_with("./")
        || tok.starts_with("../")
        || tok.ends_with('/')
        || tok
            .chars()
            .any(|c| matches!(c, '*' | '?' | '[' | ']' | '\\'))
        || tok
            .split('/')
            .any(|c| c.is_empty() || c == "." || c == "..")
}

/// The spec, byte-strict; any violation invalidates the WHOLE spec.
fn parse_spec(bytes: &[u8]) -> Option<Vec<(String, Vec<PathTok>)>> {
    if bytes.len() > MAX_SPEC_BYTES
        || bytes
            .iter()
            .any(|&b| !(b == b' ' || b == b'\t' || b == b'\n' || (0x21..=0x7e).contains(&b)))
    {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let mut gates: Vec<(String, Vec<PathTok>)> = Vec::new();
    for line in text.split('\n') {
        let mut toks = line.split([' ', '\t']).filter(|t| !t.is_empty());
        let Some(gate) = toks.next() else { continue };
        if gate.starts_with('#') {
            continue;
        }
        if !valid_gate(gate) || gates.iter().any(|(g, _)| g == gate) {
            return None;
        }
        let paths: Vec<String> = toks.map(String::from).collect();
        if paths.is_empty() || paths.len() > MAX_SPEC_PATHS || paths.iter().any(|p| bad_path(p)) {
            return None;
        }
        let paths = paths
            .into_iter()
            .map(|p| match p.strip_prefix('?') {
                Some(rest) => PathTok {
                    path: rest.to_string(),
                    optional: true,
                },
                None => PathTok {
                    path: p,
                    optional: false,
                },
            })
            .collect();
        gates.push((gate.to_string(), paths));
        if gates.len() > MAX_SPEC_GATES {
            return None;
        }
    }
    Some(gates)
}

/// The spec at `tree`, read from the tree (never the working copy). `None`
/// when there is none, when both locations exist, or when it is invalid —
/// the hook then attests by tree alone, as before.
fn spec_at(tree: &str) -> Option<Vec<(String, Vec<PathTok>)>> {
    let present: Vec<&str> = SPEC_PATHS
        .iter()
        .copied()
        .filter(|p| crate::git::succeeds(&["cat-file", "-e", &format!("{tree}:{p}")]))
        .collect();
    let [path] = present.as_slice() else {
        return None;
    };
    let bytes = crate::git::stdout_raw(&["cat-file", "blob", &format!("{tree}:{path}")])?;
    parse_spec(&bytes)
}

/// The paths every fingerprint lists besides the gate's own: both spec
/// locations, `.gitmodules`, and `.gitattributes` at the root and at every
/// ancestor directory of every declared path. Deduplicated.
fn implicit_inputs(tokens: &[String]) -> Vec<String> {
    let mut attrs: Vec<String> = vec![".gitattributes".to_string()];
    for t in tokens {
        let comps: Vec<&str> = t.split('/').collect();
        let mut prefix = String::new();
        for c in &comps[..comps.len().saturating_sub(1)] {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(c);
            attrs.push(format!("{prefix}/.gitattributes"));
        }
    }
    attrs.sort();
    attrs.dedup();
    let mut out: Vec<String> = SPEC_PATHS.iter().map(|s| s.to_string()).collect();
    out.push(".gitmodules".to_string());
    out.extend(attrs);
    out
}

fn is_oid(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64)
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// The fingerprint of `gate` on `tree`: `git hash-object` over the
/// `ls-tree -r -z --full-tree` listing of the implicit paths and the gate's
/// declared ones. `None` when a REQUIRED declared path does not resolve on
/// that tree, when git fails at any step, or when the listing is empty. An
/// optional path is listed when it exists and bound by its absence when not.
fn fingerprint(tree: &str, toks: &[PathTok]) -> Option<String> {
    let required: Vec<&str> = toks
        .iter()
        .filter(|t| !t.optional)
        .map(|t| t.path.as_str())
        .collect();
    if !required.is_empty() {
        let names: String = required.iter().map(|p| format!("{tree}:{p}\n")).collect();
        let answers = crate::git::stdout_piped(&["cat-file", "--batch-check"], &names)?;
        if answers.lines().count() != required.len()
            || answers.lines().any(|l| l.ends_with(" missing"))
        {
            return None;
        }
    }
    let paths: Vec<String> = toks.iter().map(|t| t.path.clone()).collect();
    let mut args: Vec<String> = vec![
        "ls-tree".into(),
        "-r".into(),
        "-z".into(),
        "--full-tree".into(),
        tree.to_string(),
        "--".into(),
    ];
    args.extend(implicit_inputs(&paths));
    args.extend(paths.iter().cloned());
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let listing = crate::git::stdout_raw(&argv)?;
    if listing.is_empty() {
        return None;
    }
    crate::git::stdout_piped_in(
        std::path::Path::new("."),
        &["hash-object", "--stdin"],
        &listing,
    )
    .filter(|s| is_oid(s))
}

/// The `input` lines for the gates being attested that the spec at `tree`
/// declares, in spec order.
fn inputs_for(tree: &str, gates: &[String]) -> Vec<(String, String)> {
    let Some(spec) = spec_at(tree) else {
        return Vec::new();
    };
    spec.iter()
        .filter(|(g, _)| gates.iter().any(|x| x == g))
        .filter_map(|(g, paths)| fingerprint(tree, paths).map(|fp| (g.clone(), fp)))
        .collect()
}

/// The synthetic note key for (gate, fingerprint).
fn input_key(gate: &str, fp: &str) -> Option<String> {
    let pre = format!("amont-attest-input {gate} {fp}\n");
    crate::git::stdout_piped_in(
        std::path::Path::new("."),
        &["hash-object", "--stdin"],
        pre.as_bytes(),
    )
    .filter(|s| is_oid(s))
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
/// Create `path` for writing, refusing anything already there.
///
/// `std::fs::write` is `O_CREAT|O_TRUNC` and FOLLOWS a symlink, which on a
/// shared `/tmp` is the whole bug: anyone who guesses the name and pre-creates
/// it as a link to a file we can write gets that file truncated and
/// overwritten on our behalf. The sticky bit does not help — it stops you
/// replacing someone else's file, not creating a name nobody has taken.
///
/// `create_new` is `O_CREAT|O_EXCL`, which refuses an existing path of any
/// kind, symlink included, and never follows it. The attack becomes a failed
/// verification, which is the safe direction for a signature check to fail.
fn create_exclusive(path: &std::path::Path) -> Option<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .ok()
}

pub fn verify(
    payload: &str,
    sig: &str,
    allowed_signers: &std::path::Path,
    principal: &str,
) -> bool {
    use std::io::Write;
    // -Y verify takes the signature as a FILE; the payload rides stdin.
    //
    // The nanosecond is not decoration. The name was pid + a pointer, which
    // a squatter on a shared /tmp can sit on: `create_exclusive` then refuses
    // every call and verification fails permanently. Varying the name per
    // call makes that a race to lose rather than a door to hold shut.
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let sig_file = std::env::temp_dir().join(format!(
        "amont-attest-verify-{}-{:p}-{nonce}.sig",
        std::process::id(),
        &sig
    ));
    let Some(mut f) = create_exclusive(&sig_file) else {
        return false;
    };
    if f.write_all(format!("{sig}\n").as_bytes()).is_err() {
        let _ = std::fs::remove_file(&sig_file);
        return false;
    }
    // Closed before ssh-keygen opens it.
    drop(f);
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
    let mut blocks: Vec<(String, String)> = Vec::new();
    let mut input_blocks: Vec<(String, String)> = Vec::new();
    for r in refs {
        if is_zero(&r.local_oid) {
            continue; // deleting a ref pushes no code
        }
        let spec = format!("{}^{{tree}}", r.local_oid);
        let Some(tree) = crate::git::stdout(&["rev-parse", &spec]) else {
            continue;
        };
        // Input fingerprints, from the spec in THIS tip's tree — the tree
        // the gates ran against — for the gates that passed.
        let inputs = inputs_for(&tree, gates);
        let p = payload(&tree, gates, &inputs);
        let body = match sign(&p, &key) {
            Some(sig) => format!("{p}\n{sig}"),
            None => continue,
        };
        for (gate, fp) in &inputs {
            if let Some(k) = input_key(gate, fp) {
                input_blocks.push((k, body.clone()));
            }
        }
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
        blocks.push((tree, body.clone()));
        blocks.push((r.local_oid.clone(), body));
    }
    if blocks.is_empty() {
        return;
    }
    // Each ref on its own: a main ref that is already up to date never
    // short-circuits the inputs ref, which is how a push whose inputs ref
    // failed earlier repairs the missing keys.
    let main_ok = publish(remote, NOTES_FULL_REF, &blocks);
    let inputs_ok = input_blocks.is_empty() || publish(remote, INPUTS_FULL_REF, &input_blocks);
    if main_ok {
        crate::say!(
            "{} attested {} for CI ({}{})",
            crate::ui::valid_sign(),
            crate::ui::highlight(&gates.join(" ")),
            NOTES_REF,
            if input_blocks.is_empty() {
                String::new()
            } else if inputs_ok {
                format!(", {} input fingerprints", input_blocks.len())
            } else {
                ", input fingerprints not published".to_string()
            },
        );
    }
}

/// How many times a push that lost a race is retried before giving up.
const PUSH_ATTEMPTS: u32 = 4;

/// Attach each block to its object on the REMOTE's copy of the notes ref, and
/// push the result. Never `notes add -f`, never a blind push of the local ref.
///
/// A note is one per object and may already hold blocks by other producers —
/// since attest 1.2.0, CI signs the same tree on its own platform, and a
/// teammate's push may have attested it too. `add -f` erased them all; a push
/// of the local ref, which never fetched, was rejected as non-fast-forward the
/// moment anyone else had written the ref, and the failure was silent. So:
///
/// 1. fetch the remote's ref into a TEMPORARY ref, leaving the local
///    `refs/notes/amont-attest` alone whatever happens next;
/// 2. `notes append` each block there, unless that exact block is already
///    present (ed25519 signatures are deterministic, so a re-push of the
///    same tree produces the same bytes);
/// 3. push the temporary ref to the remote's ref. A non-fast-forward
///    rejection means another producer wrote meanwhile: go back to 1.
///
/// On success the local ref follows what was published. The temporary ref is
/// removed on every path. `notes_ref` is the fully qualified ref to publish
/// to — the main ref or the inputs ref.
fn publish(remote: &str, notes_ref: &str, blocks: &[(String, String)]) -> bool {
    let short = notes_ref.trim_start_matches("refs/notes/");
    let tmp = format!("amont-attest-push-{}-{short}", std::process::id());
    let tmp_full = format!("refs/notes/{tmp}");
    let fetch_spec = format!("+{notes_ref}:{tmp_full}");
    let push_spec = format!("{tmp_full}:{notes_ref}");
    let mut published = false;
    for _ in 0..PUSH_ATTEMPTS {
        let _ = crate::git::succeeds(&["update-ref", "-d", &tmp_full]);
        // A remote with no such ref yet fails the fetch, harmlessly: append
        // then creates the note from nothing.
        let _ = crate::git::succeeds(&["fetch", "--quiet", remote, &fetch_spec]);
        let mut appended = false;
        for (object, body) in blocks {
            let existing =
                crate::git::stdout(&["notes", "--ref", &tmp, "show", object]).unwrap_or_default();
            if existing.contains(body.trim_end()) {
                continue;
            }
            if crate::git::succeeds(&["notes", "--ref", &tmp, "append", "-m", body, object]) {
                appended = true;
            }
        }
        if !appended {
            // Everything is already there — a re-push of an attested tree.
            published = true;
            break;
        }
        match push_notes(remote, &push_spec) {
            Push::Done => {
                published = true;
                break;
            }
            Push::Raced => continue,
            Push::Refused => break,
        }
    }
    if published {
        let _ = crate::git::succeeds(&["update-ref", notes_ref, &tmp_full]);
    }
    let _ = crate::git::succeeds(&["update-ref", "-d", &tmp_full]);
    published
}

/// What a notes push came back with.
enum Push {
    Done,
    /// Rejected as non-fast-forward: another producer wrote the ref between
    /// our fetch and our push. Worth another round.
    Raced,
    /// Anything else — no network, no permission, no remote. Not worth one.
    Refused,
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
    covered_within(signers, principal, require_platform, REMOTE_BUDGET_SECS)
}

/// Seconds each remote call of `covered` may take, as attest's verifier.
const REMOTE_BUDGET_SECS: u64 = 15;

/// What syncing the local notes ref with origin allows.
#[derive(Debug, PartialEq)]
enum Sync {
    /// Judge the local ref: it is origin's, or there is no origin at all.
    Judge,
    /// Judge nothing, for the reason given (already reported on stderr).
    Skip,
}

/// The local `refs/notes/amont-attest` as ORIGIN'S MIRROR (attest 1.4.0,
/// SPEC.md "Which refs are read"). Origin is the only place an attestation
/// can be revoked, so a ref origin no longer has is deleted here, and a copy
/// nobody could refresh is not judged: a stale mirror on a persistent runner
/// must not outlive a revocation.
///
/// The fetch lands in a THROWAWAY ref and the main ref moves by a local
/// compare-and-swap: the deadline kills git with SIGKILL, and a fetch killed
/// while writing the main ref leaves `amont-attest.lock`, which would fail
/// every later fetch and delete — coverage lost for good, silently. The
/// throwaway lives outside `refs/notes/`, so no notes push or pruning fetch
/// ever touches it.
fn sync_mirror(budget: u64) -> Sync {
    if !crate::git::succeeds(&["remote", "get-url", "origin"]) {
        // For fixtures and local use; CI always has an origin.
        return Sync::Judge;
    }
    sweep_sync_refs();
    let old = crate::git::stdout(&["rev-parse", "--verify", "--quiet", NOTES_FULL_REF]);
    let tmp = format!("{SYNC_NAMESPACE}{}/amont-attest", std::process::id());
    let fetched = crate::git::probe_remote(
        &[
            "fetch",
            "--quiet",
            "origin",
            &format!("+{NOTES_FULL_REF}:{tmp}"),
        ],
        budget,
    );
    let verdict = match fetched {
        crate::git::Probe::Exit(0) => {
            let new = crate::git::stdout(&["rev-parse", "--verify", "--quiet", &tmp]);
            // Compare-and-swap against what was there before the fetch: a
            // producer that published meanwhile is not overwritten.
            let expected = old.clone().unwrap_or_default();
            match new {
                None => {
                    // The throwaway vanished under us: nothing was fetched.
                    say_skip(&format!(
                        "the fetch of {NOTES_FULL_REF} left nothing to read; local mirror not judged"
                    ));
                    Sync::Skip
                }
                Some(new)
                    if crate::git::succeeds(&["update-ref", NOTES_FULL_REF, &new, &expected]) =>
                {
                    Sync::Judge
                }
                Some(new) => {
                    // The swap failed. Judge only if the ref now holds EXACTLY
                    // what origin just gave us (a concurrent fetch or publish
                    // got there first); anything else — a stale lock, a ref
                    // nobody refreshed — is not origin's, and judging it would
                    // honour a revoked attestation.
                    let now =
                        crate::git::stdout(&["rev-parse", "--verify", "--quiet", NOTES_FULL_REF]);
                    if now.as_deref() == Some(new.as_str()) {
                        Sync::Judge
                    } else {
                        say_skip(&format!(
                            "cannot update {NOTES_FULL_REF} to origin's copy; local mirror not judged"
                        ));
                        Sync::Skip
                    }
                }
            }
        }
        crate::git::Probe::TimedOut(secs) => {
            // Origin did not answer; asking it again would only double the wait.
            say_skip(&format!(
                "origin did not answer within {secs} s for {NOTES_FULL_REF}; local mirror not judged, running everything"
            ));
            Sync::Skip
        }
        _ => match crate::git::probe_remote(
            &["ls-remote", "--exit-code", "origin", NOTES_FULL_REF],
            budget,
        ) {
            crate::git::Probe::Exit(2) => drop_mirror(old.as_deref()),
            crate::git::Probe::Exit(0) => {
                say_skip(&format!(
                    "origin has {NOTES_FULL_REF} but fetching it failed; local mirror not judged, running everything"
                ));
                Sync::Skip
            }
            crate::git::Probe::Exit(code) => {
                say_skip(&format!(
                    "cannot fetch {NOTES_FULL_REF} from origin (exit {code}; no credentials? persist-credentials: false?); local mirror not judged, running everything"
                ));
                Sync::Skip
            }
            crate::git::Probe::TimedOut(secs) => {
                say_skip(&format!(
                    "origin did not answer within {secs} s for {NOTES_FULL_REF}; local mirror not judged, running everything"
                ));
                Sync::Skip
            }
            crate::git::Probe::Failed => {
                say_skip(&format!(
                    "cannot fetch {NOTES_FULL_REF} from origin (git did not run); local mirror not judged, running everything"
                ));
                Sync::Skip
            }
        },
    };
    let _ = crate::git::succeeds(&["update-ref", "-d", &tmp]);
    report_stale_lock();
    verdict
}

/// Origin answered and has no such ref: delete the mirror, if there is one,
/// and say how to undo it. Compare-and-delete against the oid read before
/// the fetch, so a ref a concurrent publish just wrote is left alone.
fn drop_mirror(old: Option<&str>) -> Sync {
    let Some(oid) = old else {
        return Sync::Skip; // never attested here: nothing to delete, nothing to say
    };
    if crate::git::succeeds(&["update-ref", "-d", NOTES_FULL_REF, oid]) {
        say(format!(
            "amont: origin has no {NOTES_FULL_REF}; deleted the local mirror (was {oid})"
        ));
        say(format!(
            "amont:   undo locally: git update-ref {NOTES_FULL_REF} {oid}   (undo the revocation: git push origin {oid}:{NOTES_FULL_REF})"
        ));
    } else {
        say_skip(&format!(
            "origin has no {NOTES_FULL_REF} but the local mirror could not be deleted (read-only .git?); local mirror not judged"
        ));
    }
    Sync::Skip
}

/// Where the throwaway sync refs live: one per process id.
const SYNC_NAMESPACE: &str = "refs/amont-tmp/";

/// Remove the throwaways of runs that were killed before their cleanup —
/// only those whose process is gone, so a concurrent run in the same clone
/// keeps its own. Where liveness cannot be asked (no `kill`), nothing is
/// swept: a leftover outside `refs/notes/` is inert, never pushed or read.
fn sweep_sync_refs() {
    let Some(refs) = crate::git::stdout(&["for-each-ref", "--format=%(refname)", SYNC_NAMESPACE])
    else {
        return;
    };
    for r in refs.lines() {
        let Some(pid) = r
            .strip_prefix(SYNC_NAMESPACE)
            .and_then(|rest| rest.split('/').next())
            .filter(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        else {
            continue;
        };
        if pid == std::process::id().to_string() || process_alive(pid) != Some(false) {
            continue;
        }
        let _ = crate::git::succeeds(&["update-ref", "-d", r]);
    }
}

/// `Some(false)` only when the system says no such process exists.
fn process_alive(pid: &str) -> Option<bool> {
    if !cfg!(unix) {
        return None;
    }
    // LC_ALL=C: the answer is read from kill's English message below.
    let out = Command::new("kill")
        .args(["-0", pid])
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .ok()?;
    if out.status.success() {
        return Some(true);
    }
    // Gone only on a positive "no such process"; anything else (EPERM: it
    // exists, we may not signal it) counts as alive, so nothing live is swept.
    let err = String::from_utf8_lossy(&out.stderr).to_lowercase();
    Some(!err.contains("no such process"))
}

/// A lock a killed git left on the MAIN ref fails every later fetch and
/// delete; say so, with the command, rather than lose coverage in silence.
fn report_stale_lock() {
    let lock = format!("{NOTES_FULL_REF}.lock");
    if let Some(path) = crate::git::stdout(&["rev-parse", "--git-path", &lock]) {
        if std::path::Path::new(&path).exists() {
            say_skip(&format!(
                "a git that was killed left a lock on {NOTES_FULL_REF}; if no git is running: rm {path}"
            ));
        }
    }
}

/// Why nothing is covered, on stderr. Stdout stays the gates or nothing.
fn say_skip(why: &str) {
    say(format!("amont: {why}"));
}

thread_local! {
    /// Lines `covered` wrote to stderr, recorded when a test asks — so the
    /// exact wording, and the undo command, are asserted rather than trusted.
    static CAPTURE: std::cell::RefCell<Option<Vec<String>>> = const { std::cell::RefCell::new(None) };
}

/// One line on stderr (and into [`CAPTURE`] when a test is recording).
fn say(line: String) {
    CAPTURE.with(|c| {
        if let Some(v) = c.borrow_mut().as_mut() {
            v.push(line.clone());
        }
    });
    eprintln!("{line}");
}

/// [`covered`] with the per-call budget as a parameter, for tests.
fn covered_within(
    signers: &std::path::Path,
    principal: &str,
    require_platform: Option<&str>,
    budget: u64,
) -> Option<String> {
    if sync_mirror(budget) == Sync::Skip {
        return None;
    }
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

/// Push `refspec`, marked so the recursive pre-push yields.
///
/// Not `git::succeeds` — that helper cannot set an environment variable, and
/// the guard is the entire point of this wrapper existing. stderr is read
/// only to tell a lost race from a real refusal.
fn push_notes(remote: &str, refspec: &str) -> Push {
    let out = Command::new("git")
        .args(["push", "--quiet", remote, refspec])
        .env(PUSH_GUARD, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .output();
    let Ok(out) = out else { return Push::Refused };
    if out.status.success() {
        return Push::Done;
    }
    let err = String::from_utf8_lossy(&out.stderr);
    if [
        "non-fast-forward",
        "fetch first",
        "stale info",
        "cannot lock ref",
        "failed to lock",
    ]
    .iter()
    .any(|m| err.contains(m))
    {
        Push::Raced
    } else {
        Push::Refused
    }
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

    /// `amont.attest` and the key path come from git config, which each test
    /// sets on its own repo; no policy is in play.
    fn test_settings() -> crate::config::Settings {
        crate::config::Settings::default()
    }

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

    /// The signature file must never be written THROUGH something already at
    /// its path. `std::fs::write` would follow a symlink and truncate whatever
    /// it points at — on a shared `/tmp`, a file chosen by whoever guessed the
    /// name first.
    #[test]
    fn the_signature_file_refuses_to_follow_what_is_already_there() {
        let dir = std::env::temp_dir().join(format!("amont-excl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");

        // A plain file already there: refused, and left untouched.
        let taken = dir.join("taken");
        std::fs::write(&taken, "original").expect("seed");
        assert!(create_exclusive(&taken).is_none());
        assert_eq!(std::fs::read_to_string(&taken).unwrap(), "original");

        // The real attack: the path is a symlink pointing somewhere valuable.
        #[cfg(unix)]
        {
            let victim = dir.join("victim");
            std::fs::write(&victim, "precious").expect("seed");
            let link = dir.join("link");
            std::os::unix::fs::symlink(&victim, &link).expect("symlink");
            assert!(
                create_exclusive(&link).is_none(),
                "a symlink must be refused, not followed"
            );
            assert_eq!(
                std::fs::read_to_string(&victim).unwrap(),
                "precious",
                "the link target was written through"
            );
        }

        // And a free name still works, or the fix would break verification.
        let fresh = dir.join("fresh");
        assert!(create_exclusive(&fresh).is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_payload_is_the_documented_contract() {
        let p = payload(
            "abc123",
            &["pre-push-pytest".into(), "pre-push-cargo-test".into()],
            &[],
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
        let p = payload("deadbeef", &["pre-push-pytest".into()], &[]);
        let sig = sign(&p, &key).expect("signing with a real key succeeds");
        assert!(verify(&p, &sig, &signers, "t@t.test"));
        // One byte of the tree changed: the signature must not carry over —
        // this is the entire difference between this module and gate_stamp.
        let tampered = payload("deadbeee", &["pre-push-pytest".into()], &[]);
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

    fn blocks_in(body: &str) -> usize {
        body.matches("-----BEGIN SSH SIGNATURE-----").count()
    }

    /// The remote already holds a block on this tree — CI's, or a teammate's.
    /// Ours is APPENDED beside it; nothing is erased, and the local notes ref
    /// was never consulted for what the remote has.
    #[test]
    fn a_block_already_on_the_remote_survives_and_ours_is_appended() {
        let (d, work, remote, _) = remote_and_work("append");
        let tree = git(&work, &["rev-parse", "HEAD^{tree}"]);
        // Someone else's block, written straight into the remote.
        let seed = repo("append-seed");
        git(
            &seed,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&seed, &["fetch", "-q", "origin"]);
        std::fs::write(seed.join("a.ts"), "x").unwrap();
        git(&seed, &["add", "a.ts"]);
        git(&seed, &["commit", "-qm", "chore: a"]);
        let foreign = "amont-attest-v2\ntree x\ngates ci-fmt\nplatform s390x-aix\namont other\n\n-----BEGIN SSH SIGNATURE-----\nnope\n-----END SSH SIGNATURE-----";
        git(
            &seed,
            &["notes", "--ref", NOTES_REF, "add", "-m", foreign, &tree],
        );
        git(
            &seed,
            &[
                "push",
                "-q",
                "origin",
                &format!("{NOTES_FULL_REF}:{NOTES_FULL_REF}"),
            ],
        );
        in_repo(&work, || {
            attest_push(
                &test_settings(),
                "origin",
                &[push_ref_for(&work)],
                &["pre-push-run-tests-js".into()],
            );
        });
        let body = git(&remote, &["notes", "--ref", NOTES_REF, "show", &tree]);
        assert_eq!(blocks_in(&body), 2, "both blocks on the remote:\n{body}");
        assert!(body.contains("gates ci-fmt"), "the foreign block survives");
        assert!(
            body.contains("gates pre-push-run-tests-js"),
            "ours was appended"
        );
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&work);
        let _ = std::fs::remove_dir_all(&seed);
    }

    /// A local notes ref that is behind the remote — the shape after CI has
    /// signed since our last push — used to make the push non-fast-forward
    /// and silently lose the attestation. The remote's copy is fetched first,
    /// so the push lands, and the local ref then follows it.
    #[test]
    fn a_stale_local_notes_ref_no_longer_loses_the_push() {
        let (d, work, remote, _) = remote_and_work("stale");
        let head = git(&work, &["rev-parse", "HEAD"]);
        // An unrelated local note, never pushed: the local ref exists and has
        // nothing in common with what the remote will hold.
        git(
            &work,
            &[
                "notes",
                "--ref",
                NOTES_REF,
                "add",
                "-m",
                "stale local",
                &head,
            ],
        );
        let stale = git(&work, &["rev-parse", NOTES_FULL_REF]);
        // Meanwhile the remote got a note from someone else on another object.
        let seed = repo("stale-seed");
        std::fs::write(seed.join("b.ts"), "y").unwrap();
        git(&seed, &["add", "b.ts"]);
        git(&seed, &["commit", "-qm", "chore: b"]);
        git(
            &seed,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(
            &seed,
            &[
                "notes",
                "--ref",
                NOTES_REF,
                "add",
                "-m",
                "remote first",
                "HEAD",
            ],
        );
        git(&seed, &["push", "-q", "origin", "HEAD:refs/heads/other"]);
        git(
            &seed,
            &[
                "push",
                "-q",
                "origin",
                &format!("{NOTES_FULL_REF}:{NOTES_FULL_REF}"),
            ],
        );
        in_repo(&work, || {
            attest_push(
                &test_settings(),
                "origin",
                &[push_ref_for(&work)],
                &["pre-push-run-tests-js".into()],
            );
        });
        let body = git(&remote, &["notes", "--ref", NOTES_REF, "show", &head]);
        assert!(
            body.contains("gates pre-push-run-tests-js"),
            "the push landed:\n{body}"
        );
        assert_ne!(
            git(&work, &["rev-parse", NOTES_FULL_REF]),
            stale,
            "the local ref followed the published state"
        );
        assert!(
            git(&work, &["for-each-ref", "refs/notes/amont-attest-push-*"]).is_empty(),
            "no temporary ref left behind"
        );
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&work);
        let _ = std::fs::remove_dir_all(&seed);
    }

    /// Pushing the same tree twice appends nothing the second time: the block
    /// is byte-identical (ed25519 is deterministic) and already there.
    #[test]
    fn a_second_push_of_the_same_tree_appends_nothing() {
        let (d, work, remote, _) = remote_and_work("twice");
        let tree = git(&work, &["rev-parse", "HEAD^{tree}"]);
        for _ in 0..2 {
            in_repo(&work, || {
                attest_push(
                    &test_settings(),
                    "origin",
                    &[push_ref_for(&work)],
                    &["pre-push-run-tests-js".into()],
                );
            });
        }
        let body = git(&remote, &["notes", "--ref", NOTES_REF, "show", &tree]);
        assert_eq!(blocks_in(&body), 1, "one block, not two:\n{body}");
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&work);
    }

    /// A committed spec: the block carries an `input` line per declared gate
    /// and is filed under the fingerprint key in the inputs ref — the
    /// fingerprint being exactly what attest's SPEC.md computes.
    #[test]
    fn a_spec_yields_input_lines_and_fingerprint_keyed_notes() {
        let (d, work, remote, _) = remote_and_work("inputs");
        std::fs::create_dir_all(work.join(".github")).unwrap();
        std::fs::write(
            work.join(".github/attest-inputs"),
            "# what each gate reads\npre-push-run-tests-js a.ts\nother nope\n",
        )
        .unwrap();
        git(&work, &["add", ".github/attest-inputs"]);
        git(&work, &["commit", "-qm", "chore: spec"]);
        let tree = git(&work, &["rev-parse", "HEAD^{tree}"]);
        // The reference fingerprint, by hand: implicit paths then the token.
        let listing = std::process::Command::new("git")
            .args([
                "-C",
                work.to_str().unwrap(),
                "ls-tree",
                "-r",
                "-z",
                "--full-tree",
                &tree,
                "--",
                ".forgejo/attest-inputs",
                ".github/attest-inputs",
                ".gitmodules",
                ".gitattributes",
                "a.ts",
            ])
            .output()
            .unwrap()
            .stdout;
        let expected =
            crate::git::stdout_piped_in(&work, &["hash-object", "--stdin"], &listing).unwrap();
        in_repo(&work, || {
            attest_push(
                &test_settings(),
                "origin",
                &[push_ref_for(&work)],
                &["pre-push-run-tests-js".into(), "other".into()],
            );
        });
        let body = git(&remote, &["notes", "--ref", NOTES_REF, "show", &tree]);
        assert!(
            body.contains(&format!("input pre-push-run-tests-js {expected}\n")),
            "the input line carries the reference fingerprint:\n{body}"
        );
        assert!(
            !body.contains("input other "),
            "a gate whose path does not exist gets no line"
        );
        let key = crate::git::stdout_piped_in(
            &work,
            &["hash-object", "--stdin"],
            format!("amont-attest-input pre-push-run-tests-js {expected}\n").as_bytes(),
        )
        .unwrap();
        let under_key = git(&remote, &["notes", "--ref", INPUTS_REF, "show", &key]);
        assert_eq!(
            under_key, body,
            "the same block is filed under the fingerprint key"
        );
        // A second push appends nothing anywhere.
        in_repo(&work, || {
            attest_push(
                &test_settings(),
                "origin",
                &[push_ref_for(&work)],
                &["pre-push-run-tests-js".into(), "other".into()],
            );
        });
        assert_eq!(
            git(&remote, &["notes", "--ref", INPUTS_REF, "show", &key])
                .matches("BEGIN SSH SIGNATURE")
                .count(),
            1
        );
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&work);
    }

    /// No spec, or an invalid one: no `input` line, no inputs ref, the tree
    /// note exactly as before.
    #[test]
    fn without_a_valid_spec_the_block_is_tree_keyed_only() {
        let (d, work, remote, _) = remote_and_work("nospec");
        std::fs::create_dir_all(work.join(".github")).unwrap();
        std::fs::write(
            work.join(".github/attest-inputs"),
            "pre-push-run-tests-js src/*.ts\n",
        )
        .unwrap();
        git(&work, &["add", ".github/attest-inputs"]);
        git(&work, &["commit", "-qm", "chore: bad spec"]);
        let tree = git(&work, &["rev-parse", "HEAD^{tree}"]);
        in_repo(&work, || {
            attest_push(
                &test_settings(),
                "origin",
                &[push_ref_for(&work)],
                &["pre-push-run-tests-js".into()],
            );
        });
        let body = git(&remote, &["notes", "--ref", NOTES_REF, "show", &tree]);
        assert!(!body.contains("\ninput "), "no input line:\n{body}");
        assert!(
            !crate::git::succeeds_in(
                &remote,
                &["rev-parse", "--verify", "--quiet", INPUTS_FULL_REF]
            ),
            "no inputs ref was created"
        );
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&work);
    }

    #[test]
    fn the_spec_grammar_is_attests() {
        assert!(parse_spec(b"# c\ng src Cargo.toml\n").is_some());
        for bad in [
            b"g src/*.rs\n".as_slice(),
            b"g :!x\n",
            b"g\n",
            b"g src\ng x\n",
            b"g src\r\n",
            b"g sr\xc3\xa9\n",
            b"-g src\n",
            b"g ./src\n",
            b"g src/\n",
            b"g a/../b\n",
        ] {
            assert!(parse_spec(bad).is_none(), "{bad:?}");
        }
        assert_eq!(
            implicit_inputs(&["crates/foo/src".into()]),
            [
                ".forgejo/attest-inputs",
                ".github/attest-inputs",
                ".gitmodules",
                ".gitattributes",
                "crates/.gitattributes",
                "crates/foo/.gitattributes"
            ]
        );
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
            attest_push(
                &test_settings(),
                "origin",
                &[push_ref],
                &["pre-push-pytest".into()],
            );
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

    // --- attest 1.4.0 parity: optional paths --------------------------------

    #[test]
    fn optional_paths_are_marked_once_and_stripped() {
        let spec = parse_spec(b"g src ?build.rs ?a/b/c\n").expect("valid");
        assert_eq!(
            spec[0].1,
            [
                PathTok {
                    path: "src".into(),
                    optional: false
                },
                PathTok {
                    path: "build.rs".into(),
                    optional: true
                },
                PathTok {
                    path: "a/b/c".into(),
                    optional: true
                },
            ]
        );
        assert!(
            parse_spec(b"g ?nope\n").is_some(),
            "an all-optional gate is a gate"
        );
        for bad in [
            "?", "??x", "?/x", "?./x", "?x/", "?:x", "x?", "?a//b", "?a/../b",
        ] {
            assert!(
                parse_spec(format!("g {bad}\n").as_bytes()).is_none(),
                "{bad} should be invalid"
            );
        }
    }

    /// The fingerprint of `g src ?nope` equals the hash of a listing built BY
    /// HAND from attest's SPEC — the optional path is absent, so the listing
    /// is the spec and `src` — and changes the moment the path appears.
    #[test]
    fn an_absent_optional_path_is_bound_by_its_absence() {
        let work = repo("optional-fp");
        std::fs::create_dir_all(work.join("src")).unwrap();
        std::fs::create_dir_all(work.join(".github")).unwrap();
        std::fs::write(work.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(work.join(".github/attest-inputs"), "g src ?nope\n").unwrap();
        git(&work, &["add", "-A"]);
        git(&work, &["commit", "-qm", "chore: spec"]);
        let tree = git(&work, &["rev-parse", "HEAD^{tree}"]);
        let spec_oid = git(
            &work,
            &["rev-parse", &format!("{tree}:.github/attest-inputs")],
        );
        let main_oid = git(&work, &["rev-parse", &format!("{tree}:src/main.rs")]);
        let listing = format!(
            "100644 blob {spec_oid}\t.github/attest-inputs\x00100644 blob {main_oid}\tsrc/main.rs\x00"
        );
        let hand =
            crate::git::stdout_piped_in(&work, &["hash-object", "--stdin"], listing.as_bytes())
                .unwrap();
        let before = in_repo(&work, || {
            let spec = spec_at(&tree).expect("the spec parses");
            fingerprint(&tree, &spec[0].1)
        });
        assert_eq!(before.as_deref(), Some(hand.as_str()));
        std::fs::write(work.join("nope"), "now it exists\n").unwrap();
        git(&work, &["add", "nope"]);
        git(&work, &["commit", "-qm", "chore: nope"]);
        let tree2 = git(&work, &["rev-parse", "HEAD^{tree}"]);
        let after = in_repo(&work, || {
            let spec = spec_at(&tree2).expect("the spec parses");
            fingerprint(&tree2, &spec[0].1)
        });
        assert!(
            after.is_some() && after != before,
            "its appearance changes the fingerprint"
        );
        let _ = std::fs::remove_dir_all(&work);
    }

    // --- attest 1.4.0 parity: the notes ref is origin's mirror --------------

    /// A work repo that pushed an attestation, and a fresh clone of its remote
    /// that `covered` answers for.
    fn attested_clone(name: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let (d, work, remote, signers) = remote_and_work(name);
        git(&remote, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        git(&work, &["push", "-q", "origin", "HEAD:main"]);
        in_repo(&work, || {
            attest_push(
                &test_settings(),
                "origin",
                &[push_ref_for(&work)],
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
        (d, remote, clone, signers)
    }

    fn covers(clone: &Path, signers: &Path, budget: u64) -> Option<String> {
        covers_logged(clone, signers, budget).0
    }

    /// [`covers`], with the stderr lines it wrote.
    fn covers_logged(clone: &Path, signers: &Path, budget: u64) -> (Option<String>, Vec<String>) {
        in_repo(clone, || {
            CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
            let got = covered_within(signers, "t@t.test", None, budget);
            let lines = CAPTURE.with(|c| c.borrow_mut().take()).unwrap_or_default();
            (got, lines)
        })
    }

    #[test]
    fn a_ref_revoked_on_origin_stops_covering_and_the_mirror_goes() {
        let (d, remote, clone, signers) = attested_clone("revoked");
        assert_eq!(
            covers(&clone, &signers, 15).as_deref(),
            Some("pre-push-pytest")
        );
        let oid = git(&clone, &["rev-parse", NOTES_FULL_REF]);
        git(&remote, &["update-ref", "-d", NOTES_FULL_REF]);
        let (got, lines) = covers_logged(&clone, &signers, 15);
        assert_eq!(got, None, "revoked on origin");
        assert_eq!(
            lines[0],
            format!("amont: origin has no {NOTES_FULL_REF}; deleted the local mirror (was {oid})")
        );
        assert!(
            in_repo(&clone, || crate::git::stdout(&[
                "rev-parse",
                "--verify",
                "--quiet",
                NOTES_FULL_REF
            ]))
            .is_none(),
            "the local mirror was deleted"
        );
        // The undo command, run exactly as printed.
        let undo = lines[1]
            .split("undo locally: git ")
            .nth(1)
            .and_then(|r| r.split("   (").next())
            .expect("an undo command");
        git(&clone, &undo.split(' ').collect::<Vec<_>>());
        assert_eq!(git(&clone, &["rev-parse", NOTES_FULL_REF]), oid);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_unreachable_origin_covers_nothing_and_keeps_the_mirror() {
        let (d, _remote, clone, signers) = attested_clone("unreachable");
        assert_eq!(
            covers(&clone, &signers, 15).as_deref(),
            Some("pre-push-pytest")
        );
        git(
            &clone,
            &[
                "remote",
                "set-url",
                "origin",
                "file:///nonexistent/amont-origin.git",
            ],
        );
        let (got, lines) = covers_logged(&clone, &signers, 15);
        assert_eq!(got, None, "a stale mirror is not judged");
        assert!(
            lines.iter().any(|l| l.starts_with(&format!(
                "amont: cannot fetch {NOTES_FULL_REF} from origin (exit "
            ))),
            "the reason is on stderr: {lines:?}"
        );
        assert!(
            !git(&clone, &["rev-parse", NOTES_FULL_REF]).is_empty(),
            "and it is kept"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A guard, not a reproduction: without an origin the local ref was
    /// always judged, and still is — fixtures and local use rely on it.
    #[test]
    fn without_an_origin_the_local_ref_is_judged() {
        let (d, _remote, clone, signers) = attested_clone("no-origin");
        assert_eq!(
            covers(&clone, &signers, 15).as_deref(),
            Some("pre-push-pytest")
        );
        git(&clone, &["remote", "remove", "origin"]);
        assert_eq!(
            covers(&clone, &signers, 15).as_deref(),
            Some("pre-push-pytest")
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A remote that never answers is cut off by the deadline, leaves no lock
    /// on the main ref, and does not stop the next good fetch.
    #[cfg(unix)]
    #[test]
    fn a_silent_origin_is_cut_off_and_leaves_nothing_behind() {
        use std::os::unix::fs::PermissionsExt;
        let (d, remote, clone, signers) = attested_clone("silent");
        let hang = d.join("hang-ssh");
        std::fs::write(&hang, "#!/bin/sh\nexec sleep 60\n").unwrap();
        std::fs::set_permissions(&hang, std::fs::Permissions::from_mode(0o755)).unwrap();
        // The user's own ssh command is honoured, which is how this one hangs.
        git(
            &clone,
            &["config", "core.sshCommand", hang.to_str().unwrap()],
        );
        git(
            &clone,
            &["remote", "set-url", "origin", "ssh://example.invalid/x.git"],
        );
        // Timed INSIDE the working-directory lock these tests share, so the
        // wait for other tests is not counted.
        let (got, took) = in_repo(&clone, || {
            let t0 = std::time::Instant::now();
            let got = covered_within(&signers, "t@t.test", None, 2);
            (got, t0.elapsed())
        });
        assert_eq!(got, None);
        // The 2 s budget plus process overhead — far from the remote's 60 s.
        assert!(took.as_secs() < 6, "took {took:?}");
        let lock = in_repo(&clone, || {
            crate::git::stdout(&["rev-parse", "--git-path", &format!("{NOTES_FULL_REF}.lock")])
        })
        .unwrap();
        assert!(
            !clone.join(&lock).exists() && !Path::new(&lock).exists(),
            "no lock left"
        );
        git(&clone, &["config", "--unset", "core.sshCommand"]);
        git(
            &clone,
            &["remote", "set-url", "origin", remote.to_str().unwrap()],
        );
        assert_eq!(
            covers(&clone, &signers, 15).as_deref(),
            Some("pre-push-pytest")
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_stale_lock_on_the_mirror_means_nothing_is_judged() {
        let (d, remote, clone, signers) = attested_clone("stale-lock");
        assert_eq!(
            covers(&clone, &signers, 15).as_deref(),
            Some("pre-push-pytest")
        );
        let lock = in_repo(&clone, || {
            crate::git::stdout(&["rev-parse", "--git-path", &format!("{NOTES_FULL_REF}.lock")])
        })
        .unwrap();
        let lock = if Path::new(&lock).is_absolute() {
            PathBuf::from(lock)
        } else {
            clone.join(lock)
        };
        std::fs::write(&lock, "").unwrap();
        // Origin unchanged: the locked copy IS origin's, so it may be judged.
        assert_eq!(
            covers(&clone, &signers, 15).as_deref(),
            Some("pre-push-pytest"),
            "an unchanged origin: the copy is origin's"
        );
        // The case that matters: origin moved on (a new attestation, or a
        // revocation rewrite) while the lock pins the stale copy.
        let main = git(&remote, &["rev-parse", "main"]);
        // A bare remote has no committer identity on a fresh runner.
        git(
            &remote,
            &[
                "-c",
                "user.email=t@t.test",
                "-c",
                "user.name=t",
                "notes",
                "--ref",
                NOTES_REF,
                "add",
                "-f",
                "-m",
                "rewritten",
                &main,
            ],
        );
        let (got, lines) = covers_logged(&clone, &signers, 15);
        assert_eq!(got, None, "a pinned stale copy is never judged");
        assert!(
            lines.iter().any(|l| l.contains(&format!(
                "left a lock on {NOTES_FULL_REF}; if no git is running: rm "
            ))),
            "the lock is reported with its rm: {lines:?}"
        );
        std::fs::remove_file(&lock).unwrap();
        assert_eq!(
            covers(&clone, &signers, 15).as_deref(),
            Some("pre-push-pytest")
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Unix only: elsewhere liveness cannot be asked, so nothing is swept —
    /// by design, since a leftover outside `refs/notes/` is inert.
    #[cfg(unix)]
    #[test]
    fn leftover_sync_refs_are_swept() {
        let (d, _remote, clone, signers) = attested_clone("sweep");
        let head = git(&clone, &["rev-parse", "HEAD"]);
        git(
            &clone,
            // A pid no process has: 2^22 + 1 exceeds pid_max on Linux and macOS.
            &["update-ref", "refs/amont-tmp/4194305/amont-attest", &head],
        );
        assert_eq!(
            covers(&clone, &signers, 15).as_deref(),
            Some("pre-push-pytest")
        );
        assert_eq!(git(&clone, &["for-each-ref", "refs/amont-tmp/"]), "");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_remote_environment_never_prompts() {
        let (args, env) = crate::git::remote_env(crate::git::Ssh::Batch);
        assert_eq!(
            args,
            [
                "-c",
                "http.lowSpeedLimit=1",
                "-c",
                "http.lowSpeedTime=10",
                "-c",
                "credential.interactive=never"
            ]
        );
        assert!(env.contains(&("GIT_TERMINAL_PROMPT", "0")));
        assert!(
            env.contains(&("GIT_ASKPASS", "")),
            "present and EMPTY disables every askpass"
        );
        assert!(env.contains(&("GCM_INTERACTIVE", "never")));
        assert!(env.contains(&(
            "GIT_SSH_COMMAND",
            "ssh -o BatchMode=yes -o ConnectTimeout=10"
        )));
        let (_, env) = crate::git::remote_env(crate::git::Ssh::User);
        assert!(
            !env.iter().any(|(k, _)| *k == "GIT_SSH_COMMAND"),
            "the user's own ssh command is left alone"
        );
    }

    /// An http origin that demands credentials: through the remote
    /// environment no askpass runs; without it the same askpass does — the
    /// negative control that keeps this test from passing vacuously.
    #[cfg(unix)]
    #[test]
    fn no_askpass_runs_against_an_origin_that_wants_credentials() {
        use std::io::{Read, Write};
        use std::os::unix::fs::PermissionsExt;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut s = stream;
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let _ = s.write_all(
                    b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"x\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        });
        let work = repo("askpass");
        let marker = work.join("asked");
        let askpass = work.join("askpass.sh");
        std::fs::write(
            &askpass,
            format!("#!/bin/sh\ntouch '{}'\necho x\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&askpass, std::fs::Permissions::from_mode(0o755)).unwrap();
        git(
            &work,
            &["config", "core.askPass", askpass.to_str().unwrap()],
        );
        git(&work, &["config", "credential.helper", ""]);
        let url = format!("http://127.0.0.1:{port}/x.git");
        in_repo(&work, || {
            let _ = crate::git::probe_remote(&["ls-remote", &url], 10);
        });
        assert!(
            !marker.exists(),
            "an askpass ran through the remote environment"
        );
        // Plain git, prompts off, the ambient askpass variables removed: the
        // repo's core.askPass is then what git runs — the very thing the
        // remote environment must stop.
        let _ = std::process::Command::new("git")
            .current_dir(&work)
            .args(["ls-remote", &url])
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_ASKPASS")
            .env_remove("SSH_ASKPASS")
            .stdin(std::process::Stdio::null())
            .output();
        assert!(
            marker.exists(),
            "negative control: without it the askpass does run"
        );
        let _ = std::fs::remove_dir_all(&work);
    }
}
