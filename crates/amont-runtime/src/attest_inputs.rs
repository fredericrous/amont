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
    read_spec(tree).ok().map(|(_, spec)| spec)
}

/// Why a spec cannot be used. The hooks collapse all three to "attest by tree
/// alone"; the coverage check must say which one it is, because for a guard
/// "no spec" and "a spec nobody can read" are both a failure to prove anything.
#[derive(Debug, PartialEq)]
pub enum SpecError {
    Missing,
    Ambiguous,
    Invalid(&'static str),
    /// git could not list the tree, or it is empty: nothing to cover.
    Unlisted,
}

/// [`spec_at`] with the reason, and the path the spec was read from.
pub(crate) fn read_spec(tree: &str) -> Result<(&'static str, Spec), SpecError> {
    // Presence comes from a listing that must itself succeed, never from a
    // `cat-file -e` probe: that cannot tell an absent file from a git that
    // failed, and "absent" opens the canonical route while "cannot tell" must
    // not.
    let listing = crate::git::stdout_raw(&[
        "ls-tree",
        "--full-tree",
        tree,
        "--",
        SPEC_PATHS[0],
        SPEC_PATHS[1],
    ])
    .ok_or(SpecError::Unlisted)?;
    let listing = String::from_utf8_lossy(&listing);
    let present: Vec<&'static str> = SPEC_PATHS
        .iter()
        .copied()
        .filter(|p| {
            listing
                .lines()
                .any(|l| l.split_once('\t').is_some_and(|(_, name)| name == *p))
        })
        .collect();
    let path = match present.as_slice() {
        [] => return Err(SpecError::Missing),
        [path] => *path,
        _ => return Err(SpecError::Ambiguous),
    };
    let bytes = crate::git::stdout_raw(&["cat-file", "blob", &format!("{tree}:{path}")])
        .ok_or(SpecError::Invalid(path))?;
    parse_spec(&bytes)
        .map(|spec| (path, spec))
        .ok_or(SpecError::Invalid(path))
}

pub(crate) type Spec = Vec<(String, Vec<PathTok>)>;

/// Paths no gate reads, exempt from the coverage check: the plans this very
/// change keeps rewriting. Everything else a repository tracks is either
/// declared or the check fails.
pub const EXEMPT: [&str; 1] = ["docs/plans/"];

/// What [`check`] found. Empty means every required gate declares every
/// tracked file.
#[derive(Debug, Default, PartialEq)]
pub struct Findings {
    /// Required gates the spec does not declare at all.
    pub undeclared: Vec<String>,
    /// Declared gates that cannot be fingerprinted on the tree (a required
    /// path is missing, or git failed).
    pub unfingerprintable: Vec<String>,
    /// Per gate, the tracked files no declared path is, or is an ancestor of.
    pub uncovered: Vec<(String, Vec<String>)>,
}

/// How many offending paths one gate lists before the rest are counted.
const LIST_CAP: usize = 50;

impl SpecError {
    /// The one line a person reads when the spec itself cannot be used.
    pub fn render(&self, tree: &str) -> String {
        match self {
            SpecError::Missing => format!(
                "check-attest-inputs: no {} or {} on {tree}",
                SPEC_PATHS[0], SPEC_PATHS[1]
            ),
            SpecError::Ambiguous => format!(
                "check-attest-inputs: both {} and {} exist on {tree}; that disables the route",
                SPEC_PATHS[0], SPEC_PATHS[1]
            ),
            SpecError::Invalid(path) => format!(
                "check-attest-inputs: {path} is invalid on {tree} (one violation invalidates the whole spec; see attest SPEC, \"Input fingerprints\")"
            ),
            SpecError::Unlisted => {
                format!("check-attest-inputs: git could not list {tree}, or it is empty")
            }
        }
    }
}

impl Findings {
    /// Everything wrong, for stderr: every gate and path, indented, then the
    /// two ways out.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for g in &self.undeclared {
            out.push_str(&format!(
                "check-attest-inputs: {g}: not declared in the spec\n"
            ));
        }
        for g in &self.unfingerprintable {
            out.push_str(&format!(
                "check-attest-inputs: {g}: cannot be fingerprinted (a required path is missing)\n"
            ));
        }
        for (g, paths) in &self.uncovered {
            out.push_str(&format!("check-attest-inputs: {g}: not covered:\n"));
            for p in paths.iter().take(LIST_CAP) {
                out.push_str(&format!("  {p}\n"));
            }
            if paths.len() > LIST_CAP {
                out.push_str(&format!("  ... and {} more\n", paths.len() - LIST_CAP));
            }
        }
        out.push_str(
            "check-attest-inputs: fix: declare each path above for that gate (a directory \
             covers everything under it); only a path no gate reads may be exempt, and that \
             is a code-review decision. An undeclared path makes CI and the hooks skip \
             wrongly the day it changes.\n",
        );
        out
    }

    pub fn is_empty(&self) -> bool {
        self.undeclared.is_empty() && self.unfingerprintable.is_empty() && self.uncovered.is_empty()
    }
}

/// The files every fingerprint lists whatever a gate declares: both spec
/// locations, and the root `.gitmodules` and `.gitattributes` (see
/// [`implicit_inputs`]). Declaring them again would be noise.
fn is_implicit(file: &[u8]) -> bool {
    SPEC_PATHS.iter().any(|p| p.as_bytes() == file)
        || file == b".gitmodules"
        || file == b".gitattributes"
}

/// Is `file` covered by the declared path `decl`: the file itself, or a file
/// under that directory? Bytes, not text: a name outside printable ASCII can
/// never be declared (the grammar refuses it) and is covered only through an
/// ASCII ancestor.
fn covers(decl: &str, file: &[u8]) -> bool {
    let d = decl.as_bytes();
    file == d || (file.len() > d.len() && file.starts_with(d) && file[d.len()] == b'/')
}

/// Does any declared path in `toks` cover `file`: the file itself, or a
/// directory above it?
pub(crate) fn declares(toks: &[PathTok], file: &str) -> bool {
    toks.iter().any(|t| covers(&t.path, file.as_bytes()))
}

/// The pure half of [`check`]: coverage of `files` by each required gate of
/// `spec`, with the fingerprint question answered by `can_fingerprint`.
pub(crate) fn coverage(
    spec: &Spec,
    required: &[String],
    files: &[Vec<u8>],
    can_fingerprint: impl Fn(&[PathTok]) -> bool,
) -> Findings {
    let mut out = Findings::default();
    for gate in required {
        let Some((_, toks)) = spec.iter().find(|(g, _)| g == gate) else {
            out.undeclared.push(gate.clone());
            continue;
        };
        if !can_fingerprint(toks) {
            out.unfingerprintable.push(gate.clone());
        }
        let missing: Vec<String> = files
            .iter()
            .filter(|f| !EXEMPT.iter().any(|e| f.starts_with(e.as_bytes())))
            .filter(|f| !is_implicit(f))
            .filter(|f| !toks.iter().any(|t| covers(&t.path, f)))
            .map(|f| String::from_utf8_lossy(f).into_owned())
            .collect();
        if !missing.is_empty() {
            out.uncovered.push((gate.clone(), missing));
        }
    }
    out
}

/// Do the required gates declare, completely, everything `tree` tracks?
///
/// Declared inputs are only as good as their completeness: a gate whose spec
/// omits something it reads is skipped wrongly the day that something changes
/// (attest SPEC, "Input fingerprints": the consumer's declaration problem).
/// This is the proof that closes the gap — per gate, by real ancestry, never
/// by union and never by first path component. The spec is read from the tree,
/// not the working copy.
pub fn check(tree: &str, required: &[String]) -> Result<Findings, SpecError> {
    let (_, spec) = read_spec(tree)?;
    let listing =
        crate::git::stdout_raw(&["ls-tree", "-r", "-z", "--full-tree", "--name-only", tree])
            .filter(|l| !l.is_empty())
            .ok_or(SpecError::Unlisted)?;
    let files: Vec<Vec<u8>> = listing
        .split(|&b| b == 0)
        .filter(|f| !f.is_empty())
        .map(<[u8]>::to_vec)
        .collect();
    Ok(coverage(&spec, required, &files, |toks| {
        fingerprint(tree, toks).is_some()
    }))
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

// ---------------------------------------------------------------------------
// The canonical tree (attest 1.5.0). attest's SPEC.md, "Canonical tree": the
// fingerprint of a tree in which the CONTENT of every regular file under the
// top-level `docs/plans/` is not counted but its EXISTENCE is. Nothing is
// declared, so nothing can be under-declared. Ported in behaviour from the
// shell and Rust verifiers there; the vectors below pin the same cases.
// ---------------------------------------------------------------------------

/// Does this `amont.conf` say `set planExempt off`? One tolerant reading,
/// shared with attest: strip CR, trim blanks, split on blanks, compare the
/// three words `set`, `planExempt` and `off` ASCII case-insensitively. A line
/// that is not exactly those three words does not match; any match is off.
pub(crate) fn conf_says_plan_exempt_off(bytes: &[u8]) -> bool {
    bytes.split(|&b| b == b'\n').any(|line| {
        let line: Vec<u8> = line.iter().copied().filter(|&b| b != b'\r').collect();
        let words: Vec<&[u8]> = line
            .split(|&b| b == b' ' || b == b'\t')
            .filter(|w| !w.is_empty())
            .collect();
        words.len() == 3
            && words[0].eq_ignore_ascii_case(b"set")
            && words[1].eq_ignore_ascii_case(b"planExempt")
            && words[2].eq_ignore_ascii_case(b"off")
    })
}

/// Is the canonical route switched off for `tree`? Read from the bytes of
/// `tree:amont.conf` itself, never through the trust-gated settings: `off`
/// only narrows, so it is honoured whoever wrote it. Fails narrow: a failing
/// `ls-tree`, or a listed `amont.conf` whose blob cannot be read, is off; only
/// a successful `ls-tree` that does not list the file means on.
pub(crate) fn plan_exempt_off(tree: &str) -> bool {
    let manifest = crate::manifest::MANIFEST;
    let Some(listing) = crate::git::stdout_raw(&["ls-tree", "--full-tree", tree, "--", manifest])
    else {
        return true;
    };
    if listing.is_empty() {
        return false;
    }
    match crate::git::stdout_raw(&["cat-file", "blob", &format!("{tree}:{manifest}")]) {
        Some(bytes) => conf_says_plan_exempt_off(&bytes),
        None => true,
    }
}

/// The canonical text of an `ls-tree -r --full-tree` listing (paths quoted by
/// `core.quotePath=true`, so one record is one line): the same lines, except
/// that a regular file (`100644`, `100755`) under the top-level `docs/plans/`
/// has its oid replaced by zeros of the same length. Symlinks and gitlinks are
/// never neutralised. `None` for an empty or malformed listing.
pub(crate) fn canon_text(listing: &str) -> Option<String> {
    let mut out = String::with_capacity(listing.len());
    for line in listing.lines() {
        let (head, path) = line.split_once('\t')?;
        let mut f = head.split(' ');
        let (mode, kind, oid) = (f.next()?, f.next()?, f.next()?);
        if f.next().is_some() {
            return None;
        }
        // A quoted path keeps its first characters: `"docs/plans/...`.
        let under_plans = path.starts_with("docs/plans/") || path.starts_with("\"docs/plans/");
        if matches!(mode, "100644" | "100755") && under_plans {
            out.push_str(&format!(
                "{mode} {kind} {}\t{path}\n",
                "0".repeat(oid.len())
            ));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    (!out.is_empty()).then_some(out)
}

/// The canonical fingerprint of `tree`, or `None` when git fails at any step.
pub(crate) fn canon(tree: &str) -> Option<String> {
    let listing = crate::git::stdout_raw(&[
        "-c",
        "core.quotePath=true",
        "ls-tree",
        "-r",
        "--full-tree",
        tree,
    ])?;
    let text = canon_text(std::str::from_utf8(&listing).ok()?)?;
    crate::git::stdout_piped_in(
        std::path::Path::new("."),
        &["hash-object", "--stdin"],
        text.as_bytes(),
    )
    .filter(|s| is_oid(s))
}

fn hashed(preimage: &str) -> Option<String> {
    crate::git::stdout_piped_in(
        std::path::Path::new("."),
        &["hash-object", "--stdin"],
        preimage.as_bytes(),
    )
    .filter(|s| is_oid(s))
}

/// The synthetic key of a canonical fingerprint in attest's inputs ref.
pub(crate) fn canon_key(canon: &str) -> Option<String> {
    hashed(&format!("amont-attest-canon {canon}\n"))
}

/// The key under which the local gate stamps of a canonical fingerprint are
/// filed in amont's own inputs ref: one note per fingerprint, holding the
/// tokens of every gate that passed on it.
pub(crate) fn canon_gate_key(canon: &str) -> Option<String> {
    hashed(&format!("amont-gate-canon {canon}\n"))
}

/// The canonical fingerprint of `tree` when its gates are reused canonically:
/// no spec at all (a successful listing that lists neither location), the
/// repository has not opted out, and git could compute it. An invalid,
/// ambiguous or unlistable spec is today's fail-open: no canonical route, so
/// no canonical stamps.
pub(crate) fn canonical_route(tree: &str) -> Option<String> {
    match read_spec(tree) {
        Err(SpecError::Missing) => {}
        _ => return None,
    }
    canon_unless_off(tree)
}

/// The canonical fingerprint a pushed block may carry: a repository with no
/// spec, or a valid one (which keeps its `input` lines and its precedence for
/// its own gates). Not an invalid, ambiguous or unlistable one.
pub(crate) fn canon_for_attest(tree: &str) -> Option<String> {
    match read_spec(tree) {
        Ok(_) | Err(SpecError::Missing) => {}
        Err(_) => return None,
    }
    canon_unless_off(tree)
}

fn canon_unless_off(tree: &str) -> Option<String> {
    if plan_exempt_off(tree) {
        return None;
    }
    canon(tree)
}

#[cfg(test)]
mod tests {
    use super::*;

    const O1: &str = "1111111111111111111111111111111111111111";
    const O2: &str = "2222222222222222222222222222222222222222";
    const Z: &str = "0000000000000000000000000000000000000000";

    #[test]
    fn the_opt_out_is_one_tolerant_reading() {
        for yes in [
            "set planExempt off\n",
            "SET PLANEXEMPT OFF",
            "  set\tplanExempt   off  \r\n",
            "gate x\nset planExempt off\ngate y\n",
            "set planExempt on\nset planExempt off\n",
            "set planExempt off\nset planExempt on\n",
        ] {
            assert!(conf_says_plan_exempt_off(yes.as_bytes()), "{yes:?}");
        }
        for no in [
            "",
            "set planExempt on\n",
            "set planExempt off # trailing comment\n",
            "# set planExempt off\n",
            "set planExempt\n",
            "set planExempt off off\n",
            "setplanExempt off\n",
            "set planexempt of\n",
        ] {
            assert!(!conf_says_plan_exempt_off(no.as_bytes()), "{no:?}");
        }
    }

    #[test]
    fn canon_text_neutralises_plan_content_and_keeps_everything_else() {
        let a = format!(
            "100644 blob {O1}\tREADME.md\n100644 blob {O1}\tdocs/plans/a.md\n100755 blob {O1}\tdocs/plans/sub/run.sh\n"
        );
        let edited = a.replace(
            &format!("{O1}\tdocs/plans/a.md"),
            &format!("{O2}\tdocs/plans/a.md"),
        );
        assert_eq!(canon_text(&a), canon_text(&edited));
        assert!(canon_text(&a)
            .unwrap()
            .contains(&format!("100644 blob {Z}\tdocs/plans/a.md\n")));
        let other = a.replace(&format!("{O1}\tREADME.md"), &format!("{O2}\tREADME.md"));
        assert_ne!(canon_text(&a), canon_text(&other));
        let added = format!("{a}100644 blob {O1}\tdocs/plans/b.md\n");
        assert_ne!(canon_text(&a), canon_text(&added));
        let renamed = a.replace("docs/plans/a.md", "docs/plans/c.md");
        assert_ne!(canon_text(&a), canon_text(&renamed));
    }

    /// One case of the known answers: a name, a listing, and its canonical
    /// text (`None`: no canon).
    type CanonCase = (String, String, Option<String>);

    /// Parse `tests/vectors/canon.txt`, vendored byte for byte from attest's
    /// `tests/vectors/canon.txt` (its header says the format). Strict: any
    /// line the format does not allow panics, so a damaged copy fails the
    /// test instead of silently testing less.
    fn canon_vectors(text: &str) -> Vec<CanonCase> {
        let mut cases = Vec::new();
        let mut open: Option<(String, String, Option<String>, bool)> = None;
        for (n, line) in text.split('\n').enumerate() {
            let at = n + 1;
            match open.as_mut() {
                None if line.is_empty() || line.starts_with('#') => {}
                None => {
                    let name = line
                        .strip_prefix("case ")
                        .unwrap_or_else(|| panic!("canon.txt:{at}: expected `case`: {line:?}"));
                    open = Some((name.to_string(), String::new(), None, false));
                }
                Some((_, listing, out, none)) => {
                    if line == "end" {
                        let (name, listing, out, none) = open.take().unwrap();
                        assert!(
                            out.is_some() != none,
                            "canon.txt:{at}: `{name}` needs `out` lines or one `none`"
                        );
                        cases.push((name, listing, out));
                    } else if line == "none" {
                        assert!(out.is_none() && !*none, "canon.txt:{at}: misplaced `none`");
                        *none = true;
                    } else if let Some(rec) = line.strip_prefix("in ") {
                        assert!(
                            out.is_none() && !*none,
                            "canon.txt:{at}: `in` after the answer"
                        );
                        listing.push_str(rec);
                        listing.push('\n');
                    } else if let Some(rec) = line.strip_prefix("out ") {
                        assert!(!*none, "canon.txt:{at}: `out` after `none`");
                        let o = out.get_or_insert_with(String::new);
                        o.push_str(rec);
                        o.push('\n');
                    } else {
                        panic!("canon.txt:{at}: not a directive: {line:?}");
                    }
                }
            }
        }
        assert!(open.is_none(), "canon.txt: the last case has no `end`");
        cases
    }

    #[test]
    fn canon_text_gives_every_known_answer() {
        let cases = canon_vectors(include_str!("../tests/vectors/canon.txt"));
        assert!(cases.iter().any(|c| c.2.is_some()) && cases.iter().any(|c| c.2.is_none()));
        for (name, listing, want) in &cases {
            assert_eq!(canon_text(listing).as_ref(), want.as_ref(), "{name}");
        }
    }

    #[test]
    fn the_vectors_reader_refuses_a_damaged_file() {
        for bad in [
            "stray\n",
            "case x\nin a\tb\n",
            "case x\nend\n",
            "case x\nnone\nout y\nend\n",
            "case x\nout y\nin a\tb\nend\n",
            "case x\nwhat\nend\n",
        ] {
            assert!(
                std::panic::catch_unwind(|| canon_vectors(bad)).is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_tree_git_cannot_list_has_no_spec_and_opens_no_route() {
        let bogus = "0123456789012345678901234567890123456789";
        assert_eq!(read_spec(bogus).err(), Some(SpecError::Unlisted));
        assert_eq!(canonical_route(bogus), None);
        assert_eq!(canon_for_attest(bogus), None);
        // A failing ls-tree is not an absent amont.conf: fail narrow.
        assert!(plan_exempt_off(bogus));
    }
}
