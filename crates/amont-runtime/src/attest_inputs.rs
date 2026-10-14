//! The attest input-fingerprint grammar and hashing, shared by whoever needs to
//! say "this gate's inputs are byte-identical to the ones it passed on".
//!
//! attest's SPEC.md, "Input fingerprints", is the contract: the grammar and
//! the hashing are that repository's, ported here in behaviour — a fingerprint
//! this crate writes must be the one its verifiers compute, byte for byte.
//! Moved out of `attest.rs` so the gate-stamp cache can key on the same
//! fingerprint without a second implementation to drift.

/// Where a repository declares what each gate reads, in order of precedence.
pub(crate) const SPEC_PATHS: [&str; 2] = [".forgejo/attest-inputs", ".github/attest-inputs"];

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
pub(crate) struct PathTok {
    pub(crate) path: String,
    pub(crate) optional: bool,
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
pub(crate) fn parse_spec(bytes: &[u8]) -> Option<Vec<(String, Vec<PathTok>)>> {
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
pub(crate) fn spec_at(tree: &str) -> Option<Vec<(String, Vec<PathTok>)>> {
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
pub(crate) fn implicit_inputs(tokens: &[String]) -> Vec<String> {
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

pub(crate) fn is_oid(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64)
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// The fingerprint of `gate` on `tree`: `git hash-object` over the
/// `ls-tree -r -z --full-tree` listing of the implicit paths and the gate's
/// declared ones. `None` when a REQUIRED declared path does not resolve on
/// that tree, when git fails at any step, or when the listing is empty. An
/// optional path is listed when it exists and bound by its absence when not.
pub(crate) fn fingerprint(tree: &str, toks: &[PathTok]) -> Option<String> {
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

/// The synthetic note key for (gate, fingerprint).
pub(crate) fn input_key(gate: &str, fp: &str) -> Option<String> {
    let pre = format!("amont-attest-input {gate} {fp}\n");
    crate::git::stdout_piped_in(
        std::path::Path::new("."),
        &["hash-object", "--stdin"],
        pre.as_bytes(),
    )
    .filter(|s| is_oid(s))
}
