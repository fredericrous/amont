//! pre-push-run-tests-js — run each touched JS package's gate before pushing.
//!
//! git feeds pre-push one line per ref on stdin. That list is parsed ONCE by
//! the dispatcher (see `pushrefs`) and lent here, because stdin can only be
//! consumed once and more than one check needs it:
//!     <local ref> <local oid> <remote ref> <remote oid>
//! An all-zero local oid is a deletion; an all-zero remote oid means the branch
//! is new, so everything it carries is in range.

use super::common::program;
use crate::check::Outcome;
use crate::git;
use std::io::IsTerminal;
use std::process::{Command, Stdio};

/// Whichever of these the package defines, cheapest first, stopping at the
/// first failure — a type error costs seconds, not a full suite.
///
/// `lint` is deliberately absent: pre-commit-lint-js already lints staged files
/// with the repo's pinned eslint, so repeating it here costs time and catches
/// nothing new.
///
/// That argument is not special to `lint`. Any entry here can be moved earlier
/// by a repository — see [`gated_at_commit`] — and once it runs on every commit,
/// running it again on push is the same pure repetition. `typecheck` is the one
/// people move: push is too late to hear about a type error you introduced an
/// hour ago.
const GATE: [&str; 3] = ["typecheck", "test:unit", "test"];

/// Whether `name` is one of the scripts the npm push gate runs ([`GATE`]).
pub(crate) fn is_gate_script(name: &str) -> bool {
    GATE.contains(&name)
}

/// GATE entries this repository already runs at COMMIT time, and so must not
/// run again here.
///
/// A repository moves one earlier by declaring it in `amont.conf` under the
/// name of the script:
///
/// ```text
/// pre-commit  typecheck  *.ts,*.tsx  block  npm run typecheck
/// ```
///
/// The name is the contract, deliberately — matching on the command would have
/// to guess at `npm` vs `pnpm` vs `yarn`, at `--silent`, at a wrapper script,
/// and would answer "no" to things that plainly are the same check.
///
/// **Only a declaration that would actually run, and actually block, counts**:
///
///   * `Kind::Runnable` excludes an unusable line and — through
///     [`crate::manifest::gate`] — an UNTRUSTED manifest. A repository could
///     otherwise declare `pre-commit typecheck`, never be trusted, and silently
///     have types checked at neither end.
///   * `hook.skip` is honoured for the same reason, one layer up: a declaration
///     the author has switched off is not a check.
///   * The EFFECTIVE severity must be `block`, overrides included. A `warn`
///     declaration lets the commit through on failure, so treating it as
///     commit-time coverage would replace a blocking push check with a
///     non-blocking commit one.
///
/// The declaration's scope rides along rather than being judged here: whether
/// it covered a given push depends on which files that push changed, which is
/// a per-ref question `run` answers with [`crate::check::Scope::covers_all`].
///
/// Get any of these wrong and the result is the failure this project is
/// arranged against — a push that reports a green gate having run nothing.
/// A declaration passing this filter is still only a PROMISE: whether the
/// check executed for the commits actually being pushed is what
/// [`crate::gate_stamp`]'s per-commit stamps answer, in `run`.
pub(crate) fn gated_at_commit(
    settings: &crate::config::Settings,
    declared: &[crate::manifest::External],
) -> Vec<GateDecl> {
    let mut decls = blocking_commit_decls(settings, declared);
    decls.retain(|d| GATE.contains(&d.script.as_str()));
    decls
}

/// EVERY blocking commit-time declaration, whatever its name — the general
/// form [`gated_at_commit`] narrows to the npm GATE vocabulary. This is what
/// earns stamps and what the bypass ledger counts: a `pre-commit test *.rs
/// block cargo test` line gates exactly the way `typecheck` always has, and
/// a same-named pre-push declaration defers to its stamps (see
/// [`pair_verdict`]). The npm push gate was the first consumer of this
/// machinery, not its definition.
pub(crate) fn blocking_commit_decls(
    settings: &crate::config::Settings,
    declared: &[crate::manifest::External],
) -> Vec<GateDecl> {
    // The fast path: pre-commit calls this on every commit to know what to
    // stamp, and most repositories declare nothing — no pre-commit
    // declaration means no git spawns for skips and severity overrides.
    if gate_names_declared(declared).is_empty() {
        return Vec::new();
    }
    let skips = crate::configured_skips(settings);
    let severities = crate::registry::Overrides::read(settings);
    declared
        .iter()
        .filter_map(|ext| {
            let crate::manifest::Kind::Runnable { files, .. } = &ext.kind else {
                return None;
            };
            ext.blocks_at_commit(&severities, &skips).then(|| GateDecl {
                script: ext.short_name.clone(),
                id: ext.id.clone(),
                lineno: ext.lineno,
                // What it fires on and is judged on: the resolved trigger
                // (its own scope united with its push twin's), else its own.
                scope: ext.effective_scope(),
                files: *files,
            })
        })
        .collect()
}

/// Names this manifest declares at pre-commit, before any config is read —
/// the zero-spawn question "could this repository have a commit-time gate at
/// all". [`blocking_commit_decls`] and the bypass ledger's fast path both
/// start here, from the same predicate, so they cannot drift.
pub(crate) fn gate_names_declared(declared: &[crate::manifest::External]) -> Vec<String> {
    declared
        .iter()
        .filter(|ext| ext.stage == crate::check::Stage::PreCommit)
        .map(|ext| ext.short_name.clone())
        .collect()
}

/// How a `Gated` push was covered, so its line can say what the skip rests on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Coverage {
    /// Every pushed commit in the pair's scope carries its stamp.
    EveryCommit,
    /// At least one ref had a commit with no stamp, and was covered by its
    /// TIP alone: these tips' own stamp keys hold the pair token (ADR-0019).
    /// Short shas, one per such ref.
    Tips(Vec<String>),
}

/// Why a same-named commit-time declaration did not vouch for a push. Each of
/// these used to be a silent `NotPaired`, and a silent rerun of the whole suite
/// is how a land held for two hours (2026-11-10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WhyNot {
    /// Its scope does not cover these pushed files.
    ScopeGap {
        /// What the declaration covers, as written (`*.rs`).
        scope: String,
        uncovered: Vec<String>,
        cause: GapCause,
        /// The 1-based `amont.conf` line to edit, when it is known.
        at: Option<usize>,
    },
    /// It is a warn check, which vouches for nothing.
    Warn,
    /// `hook.skip` switches it off here.
    Skipped,
    /// The repository lacks the marker that turns it on.
    NotOptedIn(Vec<String>),
    /// Its program was not found, so its stamps vouch for nothing.
    Unidentified,
}

/// What left a declaration narrower than the files its push twin acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GapCause {
    /// A `files` declaration is handed the matched paths, so it is never
    /// widened.
    Files,
    /// The twin acts on every change, which widening would not follow.
    UnscopedTwin,
    /// Nothing explains it: the trigger was not resolved (a missed
    /// `resolve_triggers` call), which is a bug here and not in the repository.
    Unresolved,
}

/// The reason a declaration did not vouch because the repository lacks its
/// opt-in marker.
fn not_opted_in(scope: &crate::check::Scope) -> WhyNot {
    WhyNot::NotOptedIn(scope.opt_in.iter().map(|m| m.to_string()).collect())
}

/// The reason a declaration did not vouch because its scope misses some of the
/// files `relevant` to its push twin. One derivation for every gate that pairs
/// (`pair_verdict` and the npm push gate), so the same gap gets the same
/// words on both paths. `twin_line` is the `amont.conf` line of a DECLARED
/// push twin that acts on every change, the line to scope.
fn scope_gap(
    pair: &GateDecl,
    push_scope: &crate::check::Scope,
    relevant: &[String],
    twin_line: Option<usize>,
) -> WhyNot {
    let (cause, at) = if pair.files {
        (GapCause::Files, pair.lineno)
    } else if push_scope.is_unscoped() {
        (GapCause::UnscopedTwin, twin_line)
    } else {
        (GapCause::Unresolved, pair.lineno)
    };
    WhyNot::ScopeGap {
        scope: scope_label(&pair.scope),
        uncovered: relevant
            .iter()
            .filter(|file| !pair.scope.covers(file))
            .cloned()
            .collect(),
        cause,
        at,
    }
}

/// The `amont.conf` line of the `pre-push` declaration named `name`, if there
/// is one: the line to edit when its twin acts on every change.
fn declared_twin_line(manifest: &crate::manifest::Manifest, name: &str) -> Option<usize> {
    manifest
        .externals
        .iter()
        .find(|e| e.stage == crate::check::Stage::PrePush && e.short_name == name)
        .and_then(|e| e.lineno)
}

/// Whether a reason's first line is cut to fit 80 columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineWidth {
    /// On a terminal: cut the variable part, keep the ending.
    Cut,
    /// Piped, in a CI log: every path in full.
    Whole,
}

/// The lines that say why a same-named commit-time declaration did not vouch:
/// the reason, then for a scope gap an indented line naming the `amont.conf`
/// line to edit. Pure, so the words and their width are tested.
///
/// On a terminal the first line is cut to 80 columns (the sign and its space
/// stand for two of them), and only its variable part is cut, so its ending,
/// `— running it here`, always survives. Piped, nothing is cut: a CI log keeps
/// every path. Every value from the manifest or the repository is sanitised.
pub(crate) fn reason_lines(name: &str, why: &WhyNot, width: LineWidth) -> Vec<String> {
    let name = crate::ui::sanitize(name);
    let first = |prefix: String, variable: String, suffix: &str| -> String {
        if width == LineWidth::Cut {
            let line = crate::tree_lint::fit_line(&format!("  {prefix}"), &variable, suffix);
            line.strip_prefix("  ").unwrap_or(&line).to_string()
        } else {
            format!("{prefix}{variable}{suffix}")
        }
    };
    match why {
        WhyNot::ScopeGap {
            scope,
            uncovered,
            cause,
            at,
        } => {
            let shown: Vec<String> = uncovered
                .iter()
                .take(3)
                .map(|path| crate::ui::sanitize(path))
                .collect();
            let paths = match uncovered.len().saturating_sub(3) {
                0 => shown.join(", "),
                more => format!("{} and {more} more", shown.join(", ")),
            };
            let head = first(
                format!(
                    "{name} at commit covers {}, not ",
                    crate::ui::sanitize(scope)
                ),
                paths,
                " — running it here",
            );
            let at_known = at.is_some();
            let at = match at {
                Some(line) => format!("{} line {line}", crate::manifest::MANIFEST),
                None => crate::manifest::MANIFEST.to_string(),
            };
            let first_path = shown.first().map(String::as_str).unwrap_or("it");
            let fix = match cause {
                GapCause::Files => {
                    format!("    a files declaration is not widened: add {first_path} to {at}")
                }
                GapCause::UnscopedTwin if at_known => {
                    format!("    its push twin runs on every change: scope that line in {at}")
                }
                GapCause::UnscopedTwin => format!(
                    "    its push twin runs on every change: declare a scoped pre-push {name} line in {at}"
                ),
                GapCause::Unresolved => format!(
                    "    its push twin's files were not applied: a {} bug, please report it",
                    crate::names::BIN
                ),
            };
            vec![head, fix]
        }
        WhyNot::Warn => vec![first(
            String::new(),
            name,
            " at commit is warn-only, so it vouches for nothing — running it here",
        )],
        WhyNot::Skipped => vec![first(
            String::new(),
            name,
            " at commit is skipped here (hook.skip) — running it here",
        )],
        WhyNot::NotOptedIn(markers) => {
            let markers: Vec<String> = markers.iter().map(|m| crate::ui::sanitize(m)).collect();
            vec![first(
                String::new(),
                name,
                &format!(
                    " at commit is not opted in (needs {}) — running it here",
                    markers.join(" | ")
                ),
            )]
        }
        WhyNot::Unidentified => vec![first(
            String::new(),
            name,
            " at commit: its program was not found — running it here",
        )],
    }
}

/// Say why a same-named commit-time declaration did not vouch, before the push
/// runs the gate. Exit codes are untouched: this is a line, not a verdict.
pub(crate) fn say_why_not(name: &str, why: &WhyNot) {
    let width = if std::io::stdout().is_terminal() {
        LineWidth::Cut
    } else {
        LineWidth::Whole
    };
    let mut lines = reason_lines(name, why, width).into_iter();
    if let Some(head) = lines.next() {
        crate::say!("{} {head}", crate::ui::warning_sign());
    }
    for rest in lines {
        crate::say!("{rest}");
    }
}

/// What a declared PRE-PUSH external should do about a same-named
/// commit-time declaration — the general form of the npm gate's stamp
/// suppression, for names the GATE list never heard of. Declaring `test`
/// (or `lint`, or anything) at BOTH stages is the contract: the pre-commit
/// side earns per-commit stamps, and the pre-push side runs only for pushes
/// carrying commits with no record of it, unless a ref's tip itself passed.
pub(crate) enum PairVerdict {
    /// No blocking commit-time pair covers this push — run normally. With the
    /// reason, when a same-named commit-time declaration exists and did not
    /// vouch: the push says so, then runs the gate. `None` when there is no
    /// such declaration, or nothing for it to vouch for.
    NotPaired(Option<WhyNot>),
    /// Every judged ref is covered — skip, saying so, and how.
    Gated(Coverage),
    /// A pair exists but this many pushed commits carry no record of it —
    /// run, saying why the declaration's word was not enough.
    Unstamped(usize),
    /// No pushed commit is unstamped except this many, whose gate the busy
    /// policy deferred at commit (ADR-0013): defer the twin to CI as well.
    /// Never `Gated`, which would attest a suite that ran nowhere.
    Deferred(usize),
}

/// Judge a pre-push gate against its commit-time pair, with exactly the npm
/// gate's rules: the pair must be BLOCKING (a warn check vouches for
/// nothing), its scope must have seen every file this push changes that the
/// push-side check would fire on, and every relevant commit must carry its
/// stamp. Any git refusal reads as "no stamp", which runs the check — the
/// only safe direction.
///
/// Takes the push side as `(name, scope)` rather than an `&External`,
/// because those two are all it ever used and a BUILT-IN has both. That is
/// the whole reason built-ins can be paired now: the logic never needed a
/// declaration, only a name to match and a scope to judge against.
///
/// `name` is the SHORT name — `cargo-test`, not `pre-push-cargo-test`. It is
/// compared against [`GateDecl::script`], which is what the author wrote in
/// `amont.conf`. Passing an id here matches nothing and silently pairs
/// nothing; see `Check::pairing_name`.
pub(crate) fn pair_verdict(
    settings: &crate::config::Settings,
    name: &str,
    push_scope: &crate::check::Scope,
    manifest: &crate::manifest::Manifest,
    push: &crate::pushrefs::PushRefs,
) -> PairVerdict {
    // Cheap name test before any config read or git spawn: most pre-push
    // gates have no commit-time namesake.
    let Some(commit_side) = manifest
        .externals
        .iter()
        .find(|e| e.stage == crate::check::Stage::PreCommit && e.short_name == name)
    else {
        return PairVerdict::NotPaired(None);
    };
    let Some(pair) = blocking_commit_decls(settings, &manifest.externals)
        .into_iter()
        .find(|d| d.script == name)
    else {
        return PairVerdict::NotPaired(why_not_blocking(settings, commit_side));
    };
    // A declaration this repository has not opted into never ran at commit
    // time, so it has nothing to vouch with — the push side runs on its own
    // word. Judged against the REPOSITORY, once, and never against a commit's
    // changed files: see `repo_opted_in`.
    if !repo_opted_in(&pair.scope) {
        return PairVerdict::NotPaired(Some(not_opted_in(&pair.scope)));
    }
    // The complete token of the commit-time declaration. Unknown identity (its
    // program cannot be found) means its stamps vouch for nothing: the push
    // says so, then runs on its own word.
    let Some(pair_token) = manifest
        .externals
        .iter()
        .find(|e| e.id == pair.id)
        .and_then(|e| crate::gate_identity::external_token(e, &pair.script))
    else {
        return PairVerdict::NotPaired(Some(WhyNot::Unidentified));
    };
    let zero = git::stdout(&["hash-object", "--stdin"])
        .map(|h| "0".repeat(h.len()))
        .unwrap_or_else(|| "0".repeat(40));
    let mut unstamped = 0usize;
    let mut deferred = 0usize;
    let mut judged_any = false;
    // The refs covered by their tip alone, as short shas for the line.
    let mut tips: Vec<String> = Vec::new();
    // Under `onBusy defer`, a commit whose gate the busy policy deferred is
    // not "unstamped": its twin defers too (ADR-0013). `AMONT_ON_BUSY=wait`
    // turns this off and the twin runs.
    let defer = matches!(
        crate::host_slots::busy_policy(settings),
        crate::host_slots::BusyPolicy::Defer { .. }
    );
    for r in push.get() {
        let changed = crate::pushrefs::changed_files_for(r, &zero);
        // `covers`, NOT `matches`, and the difference is the whole reason
        // built-ins can be paired at all.
        //
        // `matches` also asks the OPT-IN question — "is there a Cargo.toml
        // here" — and asking it of a single path can only ever answer no.
        // `pre-push-cargo-test` opts in on `Cargo.toml`, so every changed
        // `.rs` file would be judged irrelevant, `judged_any` would stay
        // false, and the verdict would be `NotPaired` for every push in
        // every Rust repository. Silently: a gate that never pairs looks
        // exactly like a gate with no pair declared.
        //
        // Opt-in is a fact about the REPOSITORY, settled once by the
        // dispatcher when it decided this check runs here at all. What this
        // loop needs is the per-file question, which is `covers`.
        let relevant: Vec<String> = changed
            .iter()
            .filter(|f| push_scope.is_unscoped() || push_scope.covers(f))
            .cloned()
            .collect();
        if relevant.is_empty() {
            continue; // this ref never fires the push-side check
        }
        judged_any = true;
        if !pair.scope.covers_all(&relevant) {
            // The pair has not judged everything this push changes, so its
            // word covers nothing here. Said out loud, as "running it here"
            // and never as partial coverage, which would read as
            // reassurance: it names what the declaration misses.
            return PairVerdict::NotPaired(Some(scope_gap(
                &pair,
                push_scope,
                &relevant,
                declared_twin_line(manifest, name),
            )));
        }
        let per_commit = crate::pushrefs::commits_and_files_for(r, &zero);
        let ids: Vec<String> = per_commit.iter().map(|(c, _)| c.clone()).collect();
        let stamps = crate::gate_stamp::stamps_for(&ids);
        // `touches`, NOT `matches`, for the same reason as above and with a
        // worse consequence. `matches` asks the opt-in question of ONE
        // COMMIT's changed files: a `*.txt+marker` pair judged a commit that
        // changed `a.txt` and left `marker` alone as "would never fire", so
        // the commit was not counted, `unstamped` stayed 0, and the verdict
        // was `Gated` — the push-side twin skipped on the strength of a
        // stamp that did not exist. Opt-in was settled for the repository
        // above; per commit the only question is whether it touched the
        // scope.
        let missing: Vec<String> = per_commit
            .iter()
            .filter(|(_, files)| pair.scope.touches(files))
            .filter(|(commit, _)| !stamps.get(commit).is_some_and(|s| s.contains(&pair_token)))
            .map(|(commit, _)| commit.clone())
            .collect();
        // The tip is what ships. When its OWN stamp key — the tree's note, its
        // declared-input fingerprints, its canonical tree (ADR-0011, ADR-0012),
        // exactly what `stamps_for` read above — holds the pair token, the
        // code being pushed passed the commit-time gate, and an older commit
        // that never earned a stamp (relais's hookless `commit-tree`, a
        // deferral) changes nothing about it (ADR-0019). A changed declared
        // input or gate script gives the tip a different key, so it vouches
        // for nothing and the per-commit count stands.
        //
        // Asked only when some commit is missing its stamp: a fully stamped
        // ref is covered the old way, and its line keeps meaning so.
        //
        // Never for a `files` pair: its command saw only that commit's
        // matched paths, so its stamp says nothing about the rest of the tree.
        // holds-until: every pair without `files` judges the whole tree. A
        // command that picks the staged paths itself (`lint-staged`,
        // `--cached`, `--staged`) and has a pre-push twin breaks that; none
        // existed in the fleet on 2026-11-06 (docs/custom-checks.md).
        //
        // AFTER the coverage guards above, never before: a ref whose changes
        // the pair's scope does not cover is `NotPaired`, whatever its tip
        // says.
        if !missing.is_empty()
            && !pair.files
            && stamps
                .get(&r.local_oid)
                .is_some_and(|s| s.contains(&pair_token))
        {
            tips.push(crate::gate_evidence::short(&r.local_oid).to_string());
            continue;
        }
        let excused = if defer {
            crate::gate_stamp::deferred_for(&missing, &pair.script)
        } else {
            Default::default()
        };
        deferred += excused.len();
        unstamped += missing.len() - excused.len();
    }
    if !judged_any {
        return PairVerdict::NotPaired(None);
    }
    // Never `Gated` when a deferral covers a commit: `Gated` puts the twin
    // in `passed`, which the signed attestation names, and CI would then skip
    // a suite that ran nowhere.
    if unstamped > 0 {
        PairVerdict::Unstamped(unstamped)
    } else if deferred > 0 {
        PairVerdict::Deferred(deferred)
    } else {
        PairVerdict::Gated(if tips.is_empty() {
            Coverage::EveryCommit
        } else {
            Coverage::Tips(tips)
        })
    }
}

/// A scope as a person writes it: `*.rs`, `Cargo.toml`, `dir/**/*.md`.
fn scope_label(scope: &crate::check::Scope) -> String {
    scope
        .files
        .iter()
        .chain(scope.names.iter())
        .chain(scope.dirs.iter())
        .map(|t| {
            if t.starts_with('.') {
                format!("*{t}")
            } else {
                (*t).to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Why a runnable commit-time declaration that is not among the blocking ones
/// did not vouch: `hook.skip` switches it off, or it is a warn check. `None`
/// for one that is not runnable at all (an untrusted manifest, a broken line),
/// which the dispatchers already say once per stage.
fn why_not_blocking(
    settings: &crate::config::Settings,
    ext: &crate::manifest::External,
) -> Option<WhyNot> {
    if !matches!(ext.kind, crate::manifest::Kind::Runnable { .. }) {
        return None;
    }
    let skipped = crate::configured_skips(settings)
        .iter()
        .any(|s| crate::skip_suppresses(&ext.id, s));
    Some(if skipped {
        WhyNot::Skipped
    } else {
        WhyNot::Warn
    })
}

/// Has this repository opted into `scope`? The index is the only honest
/// source — the same question `manifest.rs` asks before running the
/// declaration at commit time, so the stamp check and the run agree on
/// whether it could have run at all.
///
/// `true` when git will not list the files: an unverified answer must not
/// silently promote a declaration to "never ran" — that direction re-runs
/// the push-side gate, which is the safe one. `tracked_files` is cached
/// per process, so the git spawn is paid once however many pairs ask.
fn repo_opted_in(scope: &crate::check::Scope) -> bool {
    scope.opt_in.is_empty()
        || crate::hooks::common::tracked_files().map_or(true, |tracked| scope.opted_in(&tracked))
}

/// One gate entry a commit-time declaration stands in for.
pub(crate) struct GateDecl {
    /// The declared name — a GATE script for the npm push gate, any name
    /// for a declared pre-commit/pre-push pair.
    pub script: String,
    /// `<stage>-<name>` — what dispatch outcomes and `hook.skip` key on.
    /// Carried so pre-commit can match "this declaration" to "that outcome"
    /// without re-deriving the id and drifting.
    pub id: String,
    /// The declaration's scope, judged per ref by
    /// [`crate::check::Scope::covers_all`].
    pub scope: crate::check::Scope,
    /// The `files` marker: the command is handed only the commit's matched
    /// paths, so its stamp vouches for that commit's files and never for the
    /// tree. Such a pair cannot cover a ref by its tip ([`pair_verdict`]).
    pub files: bool,
    /// The 1-based `amont.conf` line that declares it, for a message that
    /// sends a person to the line to edit.
    pub lineno: Option<usize>,
}

/// The extensions this check treats as "JS worth testing". Exported so
/// `registry.rs` declares the scope from the same constant — see
/// `lint_json_yaml::EXTS` for the drift this prevents.
pub const JS_EXTS: &[&str] = &[".js", ".jsx", ".ts", ".tsx", ".vue"];

fn is_js(file: &str) -> bool {
    JS_EXTS.iter().any(|e| file.ends_with(e))
}

fn parent_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

/// Does `package.json` define `script` under "scripts"?
///
/// A brace-matched scan rather than a JSON parser: the only question is whether
/// one key exists in one object, and a dependency-free binary is the point of
/// this migration. Restricted to the TOP-LEVEL "scripts" object so a
/// same-named key elsewhere (a dependency called `test`, say) cannot answer
/// for it.
pub fn defines_script(pkg_json: &str, script: &str) -> bool {
    let Some(body) = scripts_object(pkg_json) else {
        return false;
    };
    let needle = format!("\"{script}\"");
    let mut from = 0;
    while let Some(i) = body[from..].find(&needle) {
        let at = from + i;
        let after = &body[at + needle.len()..];
        if after.trim_start().starts_with(':') {
            return true;
        }
        from = at + needle.len();
    }
    false
}

/// The text of the top-level `"scripts"` object, braces included.
///
/// A depth-tracking scan, not `find("\"scripts\"")`: anchoring on the FIRST
/// occurrence of the substring anywhere meant `{"files":["scripts"],…}`
/// handed the brace-matcher whichever object came next — so a dependency
/// named `test` answered as a script while the real scripts object was never
/// read, which is exactly the false positive `defines_script`'s doc promises
/// away. A string only counts as a key when it sits at depth 1 (inside the
/// document object, outside any array) AND its next non-space byte is `:` —
/// `{"name": "scripts"}` is a value, not the key.
fn scripts_object(pkg_json: &str) -> Option<&str> {
    let bytes = pkg_json.as_bytes();
    let mut depth = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                let start = i + 1;
                let close = string_end(bytes, start)?;
                let content = &pkg_json[start..close];
                i = close + 1;
                if depth != 1 {
                    continue;
                }
                let mut j = i;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j >= bytes.len() || bytes[j] != b':' {
                    continue; // a value, not a key
                }
                if content != "scripts" {
                    continue;
                }
                let mut k = j + 1;
                while k < bytes.len() && bytes[k].is_ascii_whitespace() {
                    k += 1;
                }
                // The one top-level "scripts" key exists but is not an
                // object: there are no scripts, and no later impostor may
                // answer instead.
                if k >= bytes.len() || bytes[k] != b'{' {
                    return None;
                }
                let end = object_end(bytes, k)?;
                return Some(&pkg_json[k..=end]);
            }
            b'{' | b'[' => {
                depth += 1;
                i += 1;
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

/// Index of the closing quote of the string whose content starts at `from`.
fn string_end(bytes: &[u8], from: usize) -> Option<usize> {
    let mut escaped = false;
    for (i, &c) in bytes.iter().enumerate().skip(from) {
        if escaped {
            escaped = false;
        } else if c == b'\\' {
            escaped = true;
        } else if c == b'"' {
            return Some(i);
        }
    }
    None
}

/// Index of the `}` matching the `{` at `open`, string-aware.
fn object_end(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => i = string_end(bytes, i + 1)?,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Tracked `package.json` paths, as directories relative to the repo root.
///
/// `git ls-files` instead of the shell version's `fd package.json`: it drops
/// the `fd` dependency (undeclared, and one of two binaries the hooks silently
/// required), and it is the more correct set anyway — only TRACKED packages can
/// be part of a push, and node_modules is excluded by construction rather than
/// by fd happening to honour .gitignore.
pub fn package_dirs(ls_files: &[String]) -> Vec<String> {
    let mut dirs: Vec<String> = ls_files
        .iter()
        .map(String::as_str)
        .filter(|f| *f == "package.json" || f.ends_with("/package.json"))
        .map(|f| parent_of(f).to_string())
        .collect();
    dirs.sort();
    dirs.dedup();
    dirs
}

/// Packages that actually contain one of the changed files.
///
/// `.iter().any()`, not the shell version's original `.filter()` — an empty
/// array is truthy in JS, so that selected EVERY package regardless of what
/// changed. Invisible in a single-package repo, quadratic noise in a monorepo.
pub fn packages_to_test(pkg_dirs: &[String], changed_dirs: &[String]) -> Vec<String> {
    pkg_dirs
        .iter()
        .filter(|pkg| {
            changed_dirs.iter().any(|dir| {
                if pkg.is_empty() {
                    true
                } else {
                    dir == *pkg || dir.starts_with(&format!("{pkg}/"))
                }
            })
        })
        .cloned()
        .collect()
}

/// The GATE, minus what a `pre-commit` declaration already covers.
///
/// Split from `run_gate` so the rule can be tested without a package on disk
/// and without spawning npm — `run_gate` reaches for both.
pub fn gate_for(pkg_json: &str, already: &[&str]) -> Vec<&'static str> {
    GATE.iter()
        .copied()
        .filter(|s| defines_script(pkg_json, s) && !already.contains(s))
        .collect()
}

/// True when the gate passed (or there was none to run).
fn run_gate(
    settings: &crate::config::Settings,
    root: &str,
    folder: &str,
    already: &[&str],
) -> bool {
    let dir = if folder.is_empty() {
        root.to_string()
    } else {
        format!("{root}/{folder}")
    };
    let Ok(pkg) = std::fs::read_to_string(format!("{dir}/package.json")) else {
        return true;
    };
    for script in gate_for(&pkg, already) {
        // Same hazard as cargo test: git exports GIT_DIR to hooks, and a JS
        // test that shells out to git would then operate on this repo rather
        // than its own fixture.
        let mut cmd = Command::new(program("npm"));
        cmd.args(["run", script])
            .current_dir(&dir)
            .stdin(Stdio::null());
        super::common::strip_git_env(&mut cmd);
        match super::common::status_streamed(settings, &mut cmd, super::common::Retry::Once) {
            Ok(super::common::Ran::Status(status)) if status.success() => {}
            Ok(super::common::Ran::TimedOut(budget)) => {
                super::common::say_timed_out(script, budget);
                return false;
            }
            // The gate's own exit code is not propagated: git only
            // distinguishes zero from non-zero, and npm's codes said nothing
            // the message above has not already said.
            Ok(_) | Err(_) => return false,
        }
    }
    true
}

pub fn run(
    settings: &crate::config::Settings,
    refs: &[crate::pushrefs::PushRef],
    declared: &[crate::manifest::External],
    gate: &str,
) -> Outcome {
    // An all-zero oid, of whatever length this repo's hash is (sha1 or sha256).
    let zero = git::stdout(&["hash-object", "--stdin"])
        .map(|h| "0".repeat(h.len()))
        .unwrap_or_else(|| "0".repeat(40));

    // `Unavailable`, never `Passed`, when git itself will not answer: a push
    // gate that reports green having asked nothing is the exact conflation
    // `Outcome::Unavailable` exists to prevent (see its doc in check.rs), and
    // the same split `install::repo_hooks` was rewritten for. The dispatcher
    // says "could not run" and does not block — loud, and honest.
    let Some(root) = git::stdout(&["rev-parse", "--show-toplevel"]) else {
        super::common::warn("run-tests-js: git would not answer — the gate did NOT run");
        return Outcome::Unavailable;
    };
    let Some(tracked) = git::stdout_paths(&["ls-files"]) else {
        super::common::warn("run-tests-js: git would not answer — the gate did NOT run");
        return Outcome::Unavailable;
    };
    let pkg_dirs = package_dirs(&tracked);

    // The DECLARATIONS are a property of the repository, resolved once; whether
    // one covers a given push is a property of that ref's changed files and of
    // its commits' stamps, judged inside the loop.
    let declared_gate = gated_at_commit(settings, declared);
    let mut announced: Vec<String> = Vec::new();
    let mut warned: Vec<String> = Vec::new();
    let mut said_why: Vec<String> = Vec::new();

    for r in refs {
        let local_oid = r.local_oid.as_str();
        if local_oid == zero {
            continue; // deleting a ref pushes no code
        }
        // `pushrefs::changed_files_for` exists for exactly this question, and
        // this check used to recompute it inline with all three bugs that
        // function's doc comment records fixing:
        //
        //   - a brand-new branch (`remote_oid == zero`) diffed only its TIP,
        //     so on a multi-commit push an earlier commit's `.ts` change was
        //     invisible and the suite never ran;
        //   - a two-dot range handed to `diff-tree` is a two-TREE compare, not
        //     a commit walk, so a file changed and reverted later in the same
        //     push netted to nothing;
        //   - merge commits show NOTHING without `-m`, so a file touched only
        //     to resolve a conflict selected no package.
        //
        // Every one of those let a push proceed GREEN with the suite never
        // having run. `rust_tools::test` was already the model.
        let changed = crate::pushrefs::changed_files_for(r, &zero);
        let js_changed: Vec<String> = changed.iter().filter(|f| is_js(f)).cloned().collect();
        let changed_dirs: Vec<String> = js_changed
            .iter()
            .map(|f| parent_of(f).to_string())
            .collect();
        if changed_dirs.is_empty() {
            continue;
        }

        // A declaration only stands in for this push if its scope saw every JS
        // file the push changes — and only for the ROOT package, because that
        // is where the declared command runs. A sub-package's gate is a
        // different command in a different directory, and no root declaration
        // has judged it.
        //
        // And the declaration must have EXECUTED, which is the stamps'
        // question: every commit in this push the declaration would have
        // fired on must carry a `gate_stamp` note naming the script. A commit
        // made with `--no-verify`, from a client that runs no hooks, on a
        // machine without amont, or with a rewritten hash has no stamp — and
        // an unstamped commit is one the check never judged, so the gate runs
        // rather than trusting the declaration's word for it.
        //
        // And the REPOSITORY must have opted into it — judged against the
        // index, not per commit: see `pair_verdict` for the skipped-gate bug
        // that asking it of a commit's changed files produced.
        let candidates: Vec<&GateDecl> = declared_gate
            .iter()
            .filter(|d| d.scope.covers_all(&js_changed) && repo_opted_in(&d.scope))
            .collect();
        // A declaration that stood in for a script and did not cover this push
        // used to be dropped without a word, and the script ran again. Said
        // once per script, in the pair gates' words.
        for d in &declared_gate {
            if said_why.contains(&d.script) {
                continue;
            }
            let why = if !repo_opted_in(&d.scope) {
                Some(not_opted_in(&d.scope))
            } else if !d.scope.covers_all(&js_changed) {
                let js_scope = crate::check::Scope::files(JS_EXTS);
                Some(scope_gap(d, &js_scope, &js_changed, None))
            } else {
                None
            };
            if let Some(why) = why {
                say_why_not(&d.script, &why);
                said_why.push(d.script.clone());
            }
        }
        let mut already: Vec<String> = Vec::new();
        if !candidates.is_empty() {
            let per_commit = crate::pushrefs::commits_and_files_for(r, &zero);
            let ids: Vec<String> = per_commit.iter().map(|(c, _)| c.clone()).collect();
            let stamps = crate::gate_stamp::stamps_for(&ids);
            for d in candidates {
                // Complete token or no vouching: a declaration whose program
                // cannot be identified vouches for nothing, so it says so and
                // the script runs.
                let token = declared
                    .iter()
                    .find(|e| e.id == d.id)
                    .and_then(|e| crate::gate_identity::external_token(e, &d.script));
                let Some(token) = token else {
                    // Nothing to vouch with: say so once, and the script runs.
                    if !said_why.contains(&d.script) {
                        say_why_not(&d.script, &WhyNot::Unidentified);
                        said_why.push(d.script.clone());
                    }
                    continue;
                };
                let unstamped = per_commit
                    .iter()
                    .filter(|(_, files)| d.scope.touches(files))
                    .filter(|(commit, _)| {
                        !stamps
                            .get(commit)
                            .is_some_and(|stamped| stamped.contains(&token))
                    })
                    .count();
                if unstamped == 0 {
                    already.push(d.script.clone());
                } else if !warned.contains(&d.script) {
                    crate::say!(
                        "{} {} is declared at commit time, but {unstamped} pushed \
                         commit{} carr{} no record of it — running it here",
                        crate::ui::warning_sign(),
                        d.script,
                        if unstamped == 1 { "" } else { "s" },
                        if unstamped == 1 { "ies" } else { "y" },
                    );
                    warned.push(d.script.clone());
                }
            }
        }

        // Same question as cargo-test: the suite should be answering about
        // the commits being pushed, not about whatever is open in the
        // editor — and about THIS ref's commits, not some other ref in the
        // same push. A single worktree shared across every ref would run a
        // second ref's tests against a first ref's tree.
        let (run_in, _guard) = crate::pushed_tree::where_to_run(settings, local_oid, &root, gate);
        let where_ = run_in.to_string_lossy().into_owned();
        let folders = packages_to_test(&pkg_dirs, &changed_dirs);

        // Said out loud, and not folded into the pass line. A check that stops
        // running is exactly what this project refuses to let happen quietly —
        // the reader has to be able to see that `typecheck` moved rather than
        // discover later that nothing ran it. Announced only when the skip is
        // actually applied to this ref: claiming suppression that severity,
        // scope or a missing root package disqualified would be the same lie
        // in the other direction.
        if folders.iter().any(|f| f.is_empty()) {
            let newly: Vec<String> = already
                .iter()
                .filter(|s| !announced.contains(s))
                .cloned()
                .collect();
            if !newly.is_empty() {
                crate::hooks::common::ok(
                    settings,
                    &format!(
                        "{} gated at commit instead — not repeating {} here",
                        newly.join(", "),
                        if newly.len() == 1 { "it" } else { "them" },
                    ),
                );
                announced.extend(newly);
            }
        }

        let already_strs: Vec<&str> = already.iter().map(String::as_str).collect();
        for folder in folders {
            // Root only: the declared command runs at the repo root.
            let already_here: &[&str] = if folder.is_empty() {
                &already_strs
            } else {
                &[]
            };
            if !run_gate(settings, &where_, &folder, already_here) {
                return Outcome::Failed;
            }
        }
    }
    Outcome::Passed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The filter itself, without a package on disk or an npm to spawn.
    ///
    /// Order is part of the contract and is asserted: GATE is cheapest-first so
    /// a type error costs seconds rather than a full suite, and removing an
    /// entry must not disturb what is left.
    #[test]
    fn the_gate_drops_only_what_commit_already_covers() {
        let pkg = r#"{"scripts":{"typecheck":"tsc","test":"vitest run"}}"#;
        assert_eq!(gate_for(pkg, &[]), vec!["typecheck", "test"]);
        assert_eq!(gate_for(pkg, &["typecheck"]), vec!["test"]);
        assert_eq!(gate_for(pkg, &["test"]), vec!["typecheck"]);
        assert!(gate_for(pkg, &["typecheck", "test"]).is_empty());
        // A name the package does not define is not a script to skip, and
        // naming one must not disturb the rest.
        assert_eq!(gate_for(pkg, &["test:unit"]), vec!["typecheck", "test"]);
    }

    /// Coverage is all-match: one changed file outside the declaration's scope
    /// means commits this push carries were never judged by it.
    #[test]
    fn a_scope_covers_only_when_every_changed_file_matches() {
        use crate::check::Scope;
        const TS_ONLY: Scope = Scope::files(&[".ts", ".tsx"]);
        let all_ts: Vec<String> = vec!["src/a.ts".into(), "src/b.tsx".into()];
        let mixed: Vec<String> = vec!["src/a.ts".into(), "src/legacy.js".into()];
        let js_only: Vec<String> = vec!["src/legacy.js".into()];
        assert!(TS_ONLY.covers_all(&all_ts));
        assert!(!TS_ONLY.covers_all(&mixed));
        assert!(!TS_ONLY.covers_all(&js_only));
        // An empty scope is `*` — it saw everything.
        assert!(Scope::ALWAYS.covers_all(&mixed));
        // Nothing relevant changed: nothing was missed.
        assert!(TS_ONLY.covers_all(&[]));

        // A bare-filename scope covers by BASENAME, never by suffix.
        const BY_NAME: Scope = Scope {
            files: &[],
            names: &["package.json"],
            dirs: &[],
            opt_in: &[],
            not_during: &[],
        };
        assert!(BY_NAME.covers_all(&["apps/web/package.json".into()]));
        assert!(!BY_NAME.covers_all(&["not-package.json".into()]));
    }

    #[test]
    fn finds_scripts_only_inside_the_scripts_object() {
        let pkg = r#"{"name":"x","scripts":{"test":"vitest","typecheck":"tsc"},"devDependencies":{"lint":"1"}}"#;
        assert!(defines_script(pkg, "test"));
        assert!(defines_script(pkg, "typecheck"));
        assert!(!defines_script(pkg, "test:unit"));
        // present, but as a DEPENDENCY — must not answer for the scripts object
        assert!(!defines_script(pkg, "lint"));
    }

    #[test]
    fn survives_nested_objects_and_escaped_quotes() {
        let pkg = r#"{"scripts":{"test":"echo \"hi\"","build":"x"},"other":{"test":"no"}}"#;
        assert!(defines_script(pkg, "test"));
        assert!(defines_script(pkg, "build"));
        assert!(!defines_script(pkg, "other"));
    }

    #[test]
    fn no_scripts_object_means_no_scripts() {
        assert!(!defines_script(r#"{"name":"x"}"#, "test"));
    }

    /// The misanchor this scan replaced: `find("\"scripts\"")` hit the string
    /// inside the `files` ARRAY, brace-matched the dependencies object that
    /// followed, and a dependency named `test` answered as a script — while
    /// the real scripts object was never read.
    #[test]
    fn a_scripts_string_elsewhere_cannot_anchor_the_scan() {
        let pkg = r#"{"files":["scripts"],"dependencies":{"test":"1.0"},"scripts":{"build":"x"}}"#;
        assert!(
            !defines_script(pkg, "test"),
            "a dependency answered as a script"
        );
        assert!(
            defines_script(pkg, "build"),
            "the real scripts object was skipped"
        );

        // The same shape via a string VALUE rather than an array element.
        let pkg =
            r#"{"description":"scripts","dependencies":{"test":"1.0"},"scripts":{"build":"x"}}"#;
        assert!(!defines_script(pkg, "test"));
        assert!(defines_script(pkg, "build"));

        // A NESTED "scripts" key (inside another object) is not the top-level
        // one.
        let pkg = r#"{"config":{"scripts":{"test":"inner"}},"scripts":{"build":"x"}}"#;
        assert!(!defines_script(pkg, "test"));
        assert!(defines_script(pkg, "build"));
    }

    /// `"scripts"` as a top-level key whose value is not an object defines
    /// nothing — and no later impostor may answer instead.
    #[test]
    fn a_non_object_scripts_value_defines_nothing() {
        assert!(!defines_script(r#"{"scripts":"echo hi"}"#, "test"));
        assert!(!defines_script(
            r#"{"scripts":"x","other":{"test":"y"}}"#,
            "test"
        ));
    }

    #[test]
    fn collects_package_directories() {
        let ls: Vec<String> = [
            "package.json",
            "apps/web/package.json",
            "apps/web/src/a.ts",
            "README.md",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert_eq!(
            package_dirs(&ls),
            vec!["".to_string(), "apps/web".to_string()]
        );
    }

    /// The JS bug: `.filter()` returns an array, `[]` is truthy, so every
    /// package was selected whatever changed.
    #[test]
    fn selects_only_packages_containing_a_change() {
        let pkgs = vec!["apps/web".to_string(), "apps/api".to_string()];
        let changed = vec!["apps/web/src".to_string()];
        assert_eq!(
            packages_to_test(&pkgs, &changed),
            vec!["apps/web".to_string()]
        );
    }

    #[test]
    fn the_repo_root_package_matches_any_change() {
        let pkgs = vec!["".to_string()];
        assert_eq!(
            packages_to_test(&pkgs, &["src".to_string()]),
            vec!["".to_string()]
        );
    }

    fn every_reason(path: &str) -> Vec<WhyNot> {
        vec![
            WhyNot::ScopeGap {
                scope: "*.rs".into(),
                uncovered: vec![path.into()],
                cause: GapCause::Files,
                at: Some(3),
            },
            WhyNot::Warn,
            WhyNot::Skipped,
            WhyNot::NotOptedIn(vec!["marker".into()]),
            WhyNot::Unidentified,
        ]
    }

    /// The first line of every reason fits 80 columns with the sign and its
    /// space in front, for an ordinary name, and ends in `running it here`:
    /// the part a reader needs must never be the part that gets cut.
    #[test]
    fn every_reason_fits_its_line_and_keeps_its_ending() {
        for why in every_reason("rustfmt.toml") {
            let head = &reason_lines("cargo-test", &why, LineWidth::Whole)[0];
            assert!(head.chars().count() + 2 <= 80, "{head}");
            assert!(head.ends_with("— running it here"), "{head}");
        }
    }

    /// On a terminal a long path, or a long name, is cut, never the ending.
    #[test]
    fn on_a_terminal_only_the_variable_part_is_cut() {
        let long = "crates/a/very/deep/package/directory/src/components/widgets/Button.stories.tsx";
        for why in every_reason(long) {
            let head = &reason_lines("cargo-test", &why, LineWidth::Cut)[0];
            assert!(head.chars().count() + 2 <= 80, "{head}");
            assert!(head.ends_with("— running it here"), "{head}");
        }
        let name = "a-rather-long-check-name-indeed-for-a-gate";
        for why in [
            WhyNot::Warn,
            WhyNot::Skipped,
            WhyNot::NotOptedIn(vec!["marker".into()]),
            WhyNot::Unidentified,
        ] {
            let head = &reason_lines(name, &why, LineWidth::Cut)[0];
            assert!(head.chars().count() + 2 <= 80, "{head}");
            assert!(head.ends_with("— running it here"), "{head}");
        }
    }

    /// Piped, nothing is cut: a CI log keeps every path.
    #[test]
    fn piped_nothing_is_cut() {
        let long = "crates/a/very/deep/package/directory/src/components/widgets/Button.stories.tsx";
        let why = WhyNot::ScopeGap {
            scope: "*.rs".into(),
            uncovered: vec![long.into()],
            cause: GapCause::Files,
            at: None,
        };
        assert!(reason_lines("cargo-test", &why, LineWidth::Whole)[0].contains(long));
    }

    /// Everything the manifest or the repository supplies is sanitised, the
    /// scope and the markers included.
    #[test]
    fn values_from_the_manifest_are_sanitised() {
        let why = WhyNot::NotOptedIn(vec!["mark\u{1b}[31mer".into()]);
        let head = &reason_lines("cargo-test", &why, LineWidth::Whole)[0];
        assert!(!head.contains('\u{1b}'), "{head:?}");
    }
}
