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

/// GATE entries this repository already runs at COMMIT time, and so must not
/// run again here.
///
/// A repository moves one earlier by declaring it in `amont.conf` under the
/// name of the script:
///
/// ```text
/// pre-commit  typecheck  .ts .tsx  block  npm run typecheck
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
/// a per-ref question `run` answers with [`scope_covers`].
///
/// Get any of these wrong and the result is the failure this project is
/// arranged against — a push that reports a green gate having run nothing.
/// One gap remains open by design: a commit created with `--no-verify` never
/// ran the check, and nothing here can see that. `docs/checks.md` says so out
/// loud instead of this code pretending otherwise.
fn gated_at_commit() -> Vec<(&'static str, crate::check::Scope)> {
    let skips = crate::configured_skips();
    let severities = crate::registry::Overrides::read();
    let declared = crate::manifest::externals();
    GATE.iter()
        .filter_map(|script| {
            declared.iter().find_map(|ext| {
                let crate::manifest::Kind::Runnable { scope, .. } = &ext.kind else {
                    return None;
                };
                (ext.stage == crate::check::Stage::PreCommit
                    && ext.short_name == *script
                    && severities.of(ext) == crate::check::Severity::Block
                    && !skips.iter().any(|s| crate::skip_suppresses(&ext.id, s)))
                .then_some((*script, *scope))
            })
        })
        .collect()
}

/// Did the commit-time declaration see EVERYTHING this push changes?
///
/// The commit-time check fires when ANY staged file matches its scope, so a
/// push whose JS changes fall only partly inside that scope carries commits
/// the check never judged — a `*.ts,*.tsx` declaration says nothing about a
/// `.js` change. All-match, not any-match, because the question here is
/// coverage, and under-approximating is the safe direction: an uncovered file
/// re-runs a script that would have passed, a wrongly-covered one skips the
/// suite.
pub fn scope_covers(scope_exts: &[&str], changed_js: &[String]) -> bool {
    scope_exts.is_empty()
        || changed_js
            .iter()
            .all(|f| scope_exts.iter().any(|ext| f.ends_with(ext)))
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
/// this migration. Restricted to the "scripts" object so a same-named key
/// elsewhere (a dependency called `test`, say) cannot answer for it.
pub fn defines_script(pkg_json: &str, script: &str) -> bool {
    let Some(k) = pkg_json.find("\"scripts\"") else {
        return false;
    };
    let Some(open_rel) = pkg_json[k..].find('{') else {
        return false;
    };
    let open = k + open_rel;
    let bytes = pkg_json.as_bytes();
    let mut depth = 0usize;
    let mut end = open;
    let mut in_str = false;
    let mut escaped = false;
    for (i, &c) in bytes.iter().enumerate().skip(open) {
        if in_str {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_str = false;
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = i;
                    break;
                }
            }
            _ => {}
        }
    }
    if end <= open {
        return false;
    }
    let body = &pkg_json[open..=end];
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
fn run_gate(root: &str, folder: &str, already: &[&str]) -> bool {
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
        let status = cmd.status();
        match status {
            Ok(status) if status.success() => {}
            // The gate's own exit code is not propagated: git only
            // distinguishes zero from non-zero, and npm's codes said nothing
            // the message above has not already said.
            Ok(_) | Err(_) => return false,
        }
    }
    true
}

pub fn run(refs: &[crate::pushrefs::PushRef]) -> Outcome {
    // An all-zero oid, of whatever length this repo's hash is (sha1 or sha256).
    let zero = git::stdout(&["hash-object", "--stdin"])
        .map(|h| "0".repeat(h.len()))
        .unwrap_or_else(|| "0".repeat(40));

    let Some(root) = git::stdout(&["rev-parse", "--show-toplevel"]) else {
        return Outcome::Passed;
    };
    let pkg_dirs = git::stdout_paths(&["ls-files"])
        .map(|f| package_dirs(&f))
        .unwrap_or_default();

    // The DECLARATIONS are a property of the repository, resolved once; whether
    // one covers a given push is a property of that ref's changed files, judged
    // inside the loop.
    let declared_gate = gated_at_commit();
    let mut announced: Vec<&'static str> = Vec::new();

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
        let already: Vec<&'static str> = declared_gate
            .iter()
            .filter(|(_, scope)| scope_covers(scope.files, &js_changed))
            .map(|(script, _)| *script)
            .collect();

        // Same question as cargo-test: the suite should be answering about
        // the commits being pushed, not about whatever is open in the
        // editor — and about THIS ref's commits, not some other ref in the
        // same push. A single worktree shared across every ref would run a
        // second ref's tests against a first ref's tree.
        let (run_in, _guard) = crate::pushed_tree::where_to_run(local_oid, &root);
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
            let newly: Vec<&'static str> = already
                .iter()
                .copied()
                .filter(|s| !announced.contains(s))
                .collect();
            if !newly.is_empty() {
                println!(
                    "{} {} gated at commit instead — not repeating {} here",
                    crate::ui::valid_sign(),
                    newly.join(", "),
                    if newly.len() == 1 { "it" } else { "them" },
                );
                announced.extend(newly);
            }
        }

        for folder in folders {
            // Root only: the declared command runs at the repo root.
            let already_here: &[&str] = if folder.is_empty() { &already } else { &[] };
            if !run_gate(&where_, &folder, already_here) {
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
        let ts_only = &[".ts", ".tsx"];
        let all_ts: Vec<String> = vec!["src/a.ts".into(), "src/b.tsx".into()];
        let mixed: Vec<String> = vec!["src/a.ts".into(), "src/legacy.js".into()];
        let js_only: Vec<String> = vec!["src/legacy.js".into()];
        assert!(scope_covers(ts_only, &all_ts));
        assert!(!scope_covers(ts_only, &mixed));
        assert!(!scope_covers(ts_only, &js_only));
        // An empty scope is `*` — it saw everything.
        assert!(scope_covers(&[], &mixed));
        // Nothing relevant changed: nothing was missed.
        assert!(scope_covers(ts_only, &[]));
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
}
