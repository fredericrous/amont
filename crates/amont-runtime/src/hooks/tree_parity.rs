//! pre-commit-tree-parity — a CI step skipped on `tree-<name>` must run exactly
//! that gate's command (ADR-0024, `ci.skip-needs-the-same-command`).
//!
//! An attestation names a gate, and a gate is worth only the command behind
//! it. If a workflow's gated `run:` drifts from the `tree` line in
//! `amont.conf`, CI keeps skipping a step whose command nobody proved. This
//! check compares the two on every commit that touches either, and CI runs it
//! too (`amont tree-parity`, never skipped), so a `--no-verify` drift still
//! fails.
//!
//! amont has no dependencies, so this is not a YAML parser. It reads the one
//! shape workflow files take — `jobs:` → a job → `steps:` → `- ` items — line
//! by line, and **fails closed**: anything it cannot read with certainty on a
//! gated step is a problem, never a guess. That covers:
//!
//! - a block scalar;
//! - an anchor or alias;
//! - a quoted form with escapes;
//! - `${{ }}` interpolation;
//! - a step-level `env:` or `shell:`, or a `working-directory:` the gate does
//!   not declare as `cwd=`;
//! - any `env:` or `defaults:` a gated step would inherit from its job or its
//!   workflow.
//!
//! A gate that needs one of these does not attest.

use std::path::Path;

use crate::check::Outcome;
use crate::hooks::common::{fail, hl, ok, repo_root};
use crate::manifest::{normalize_command, tree_gates, TreeGate, MANIFEST};

/// Where workflows live, per forge.
pub const WORKFLOW_DIRS: &[&str] = &[".github/workflows", ".forgejo/workflows"];

/// One thing wrong, where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub file: String,
    pub line: usize,
    pub what: String,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.line == 0 {
            write!(f, "{}: {}", self.file, self.what)
        } else {
            write!(f, "{}:{}: {}", self.file, self.line, self.what)
        }
    }
}

/// A step, as far as parity needs to see one.
#[derive(Debug, Default)]
struct Step {
    line: usize,
    /// `(key, raw value after the colon, line)` for keys at the step's own
    /// indentation — never nested ones.
    keys: Vec<(String, String, usize)>,
    /// Every line of the step, for finding `tree-` references wherever the
    /// `if:` puts them (a block scalar included).
    text: String,
}

#[derive(Debug, Default)]
struct Job {
    /// `env:` or `defaults:` at the job's own level.
    inherits: Option<usize>,
    steps: Vec<Step>,
}

#[derive(Debug, Default)]
struct Workflow {
    /// `env:` or `defaults:` at the top level.
    inherits: Option<usize>,
    jobs: Vec<Job>,
    /// `tree-` references on lines that belong to no step.
    stray: Vec<usize>,
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

/// `key: value` → `(key, value)`; `None` when the line is not a mapping key.
fn key_of(s: &str) -> Option<(&str, &str)> {
    let (k, v) = s.split_once(':')?;
    let ok = !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    (ok && (v.is_empty() || v.starts_with(' '))).then(|| (k, v.trim()))
}

fn mentions_tree(line: &str) -> bool {
    line.contains("'tree-") || line.contains("\"tree-")
}

fn read_workflow(text: &str) -> Workflow {
    let mut wf = Workflow::default();
    let mut in_jobs = false;
    let mut job_indent: Option<usize> = None;
    let mut job_key_indent: Option<usize> = None;
    let mut steps_indent: Option<usize> = None;
    let mut dash_indent: Option<usize> = None;
    for (i, raw) in text.lines().enumerate() {
        let lineno = i + 1;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let ind = indent_of(raw);
        if ind == 0 {
            in_jobs = false;
            job_indent = None;
            job_key_indent = None;
            steps_indent = None;
            dash_indent = None;
            if let Some((k, _)) = key_of(trimmed) {
                if k == "jobs" {
                    in_jobs = true;
                } else if k == "env" || k == "defaults" {
                    wf.inherits.get_or_insert(lineno);
                }
            }
            if mentions_tree(raw) {
                wf.stray.push(lineno);
            }
            continue;
        }
        if !in_jobs {
            if mentions_tree(raw) {
                wf.stray.push(lineno);
            }
            continue;
        }
        let ji = *job_indent.get_or_insert(ind);
        if ind <= ji {
            // A new job header (or something at the jobs level).
            job_key_indent = None;
            steps_indent = None;
            dash_indent = None;
            wf.jobs.push(Job::default());
            if mentions_tree(raw) {
                wf.stray.push(lineno);
            }
            continue;
        }
        let Some(job) = wf.jobs.last_mut() else {
            if mentions_tree(raw) {
                wf.stray.push(lineno);
            }
            continue;
        };
        let jki = *job_key_indent.get_or_insert(ind);
        if ind <= jki {
            // A job-level key ends any steps list.
            steps_indent = None;
            dash_indent = None;
            if let Some((k, _)) = key_of(trimmed) {
                if k == "steps" {
                    steps_indent = Some(ind);
                } else if k == "env" || k == "defaults" {
                    job.inherits.get_or_insert(lineno);
                }
            }
            if mentions_tree(raw) {
                wf.stray.push(lineno);
            }
            continue;
        }
        if steps_indent.is_none() {
            if mentions_tree(raw) {
                wf.stray.push(lineno);
            }
            continue;
        }
        if let Some(item) = trimmed
            .strip_prefix("- ")
            .or((trimmed == "-").then_some(""))
        {
            let di = *dash_indent.get_or_insert(ind);
            if ind == di {
                let mut step = Step {
                    line: lineno,
                    ..Step::default()
                };
                step.text.push_str(raw);
                step.text.push('\n');
                if let Some((k, v)) = key_of(item) {
                    step.keys.push((k.to_string(), v.to_string(), lineno));
                }
                job.steps.push(step);
                continue;
            }
        }
        let Some(step) = job.steps.last_mut() else {
            if mentions_tree(raw) {
                wf.stray.push(lineno);
            }
            continue;
        };
        step.text.push_str(raw);
        step.text.push('\n');
        // A key at the step's own indentation (dash + 2).
        if Some(ind) == dash_indent.map(|d| d + 2) {
            if let Some((k, v)) = key_of(trimmed) {
                step.keys.push((k.to_string(), v.to_string(), lineno));
            }
        }
    }
    wf
}

/// The `tree-<name>` tokens a step's text references.
fn referenced(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for quote in ['\'', '"'] {
        let needle = format!("{quote}tree-");
        let mut rest = text;
        while let Some(i) = rest.find(&needle) {
            let after = &rest[i + needle.len()..];
            let end = after.find(quote).unwrap_or(after.len());
            let name = &after[..end];
            if !name.is_empty() && !out.iter().any(|n| n == name) {
                out.push(name.to_string());
            }
            rest = &after[end..];
        }
    }
    out
}

/// A single-line scalar's value, or why it cannot be read with certainty.
fn scalar(raw: &str) -> Result<String, &'static str> {
    let v = raw.trim();
    if v.is_empty() || v.starts_with('|') || v.starts_with('>') {
        return Err("a block scalar — write the command on one line");
    }
    if v.starts_with('&') || v.starts_with('*') {
        return Err("an anchor or alias");
    }
    let value = if let Some(inner) = v.strip_prefix('"') {
        let Some(inner) = inner.strip_suffix('"') else {
            return Err("an unterminated quoted scalar");
        };
        if inner.contains(['\\', '"']) {
            return Err("a quoted scalar with escapes");
        }
        inner.to_string()
    } else if let Some(inner) = v.strip_prefix('\'') {
        let Some(inner) = inner.strip_suffix('\'') else {
            return Err("an unterminated quoted scalar");
        };
        if inner.contains('\'') {
            return Err("a quoted scalar with escapes");
        }
        inner.to_string()
    } else {
        if v.contains(" #") || v.contains(": ") || v.starts_with(['{', '[', '!', '%', '@', '`']) {
            return Err("a plain scalar YAML may read differently — quote it");
        }
        v.to_string()
    };
    if value.contains("${{") {
        return Err("`${{ }}` interpolation");
    }
    Ok(value)
}

/// Every problem in `workflows` (path, text) against `gates`.
pub fn check_texts(gates: &[TreeGate], workflows: &[(String, String)]) -> Vec<Problem> {
    let mut problems = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for (file, text) in workflows {
        let wf = read_workflow(text);
        for line in &wf.stray {
            problems.push(Problem {
                file: file.clone(),
                line: *line,
                what: "a tree gate referenced outside a step — only a step may be skipped on one"
                    .into(),
            });
        }
        for job in &wf.jobs {
            for step in &job.steps {
                let names = referenced(&step.text);
                if names.is_empty() {
                    continue;
                }
                let at = |what: String| Problem {
                    file: file.clone(),
                    line: step.line,
                    what,
                };
                if names.len() > 1 {
                    problems.push(at(format!(
                        "one step is gated on several tree gates ({}) — a gate proves one command",
                        names.join(", ")
                    )));
                    continue;
                }
                let name = &names[0];
                let Some(gate) = gates.iter().find(|g| &g.name == name) else {
                    problems.push(at(format!("`tree-{name}` is not declared in {MANIFEST}")));
                    continue;
                };
                seen.push(&gate.name);
                if let Some(l) = job.inherits.or(wf.inherits) {
                    problems.push(Problem {
                        file: file.clone(),
                        line: l,
                        what: format!(
                            "`tree-{name}` is skipped in a job that inherits `env:` or \
                             `defaults:` — they change what `run:` does; move them onto \
                             the steps that need them"
                        ),
                    });
                    continue;
                }
                let mut run = None;
                let mut bad = None;
                for (k, v, l) in &step.keys {
                    match k.as_str() {
                        "run" => run = Some((v, *l)),
                        "env" | "shell" => {
                            bad = Some(format!("a step-level `{k}:` changes what `run:` does"))
                        }
                        "working-directory" => match (scalar(v), &gate.cwd) {
                            (Ok(dir), Some(cwd))
                                if dir.trim_end_matches('/').trim_start_matches("./")
                                    == cwd.as_str() => {}
                            _ => {
                                bad = Some(
                                    "`working-directory:` differs from the gate's `cwd=`".into(),
                                )
                            }
                        },
                        _ => {}
                    }
                }
                if bad.is_none() && gate.cwd.is_some() {
                    let has_wd = step.keys.iter().any(|(k, _, _)| k == "working-directory");
                    if !has_wd {
                        bad = Some(format!(
                            "the gate runs in `{}` (cwd=) but the step has no \
                             `working-directory:`",
                            gate.cwd.as_deref().unwrap_or("")
                        ));
                    }
                }
                if let Some(why) = bad {
                    problems.push(at(format!("`tree-{name}`: {why}")));
                    continue;
                }
                let Some((raw, line)) = run else {
                    problems.push(at(format!("`tree-{name}` gates a step with no `run:`")));
                    continue;
                };
                match scalar(raw) {
                    Err(why) => problems.push(Problem {
                        file: file.clone(),
                        line,
                        what: format!("`tree-{name}`: `run:` is {why}"),
                    }),
                    Ok(value) => {
                        let got = normalize_command(&value);
                        let want = gate.normalized();
                        if got != want {
                            problems.push(Problem {
                                file: file.clone(),
                                line,
                                what: format!(
                                    "`tree-{name}` runs `{got}` here but `{want}` in {MANIFEST}:{}",
                                    gate.lineno
                                ),
                            });
                        }
                    }
                }
            }
        }
    }
    for gate in gates {
        if !seen.contains(&gate.name.as_str()) {
            problems.push(Problem {
                file: MANIFEST.into(),
                line: gate.lineno,
                what: format!(
                    "`tree-{}` is declared but no workflow step is skipped on it",
                    gate.name
                ),
            });
        }
    }
    problems
}

/// Read `root`'s manifest and workflows, and compare them.
pub fn check_repo(root: &Path) -> Vec<Problem> {
    let Ok(manifest) = std::fs::read_to_string(root.join(MANIFEST)) else {
        return Vec::new();
    };
    let gates = tree_gates(&manifest);
    if gates.is_empty() {
        return Vec::new();
    }
    let mut workflows = Vec::new();
    for dir in WORKFLOW_DIRS {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
            continue;
        };
        let mut paths: Vec<_> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("yml" | "yaml")))
            .collect();
        paths.sort();
        for p in paths {
            if let Ok(text) = std::fs::read_to_string(&p) {
                let rel = p.strip_prefix(root).unwrap_or(&p).display().to_string();
                workflows.push((rel, text));
            }
        }
    }
    check_texts(&gates, &workflows)
}

/// The pre-commit check. `Inert` when the repository declares no tree gate.
pub fn run(settings: &crate::config::Settings) -> Outcome {
    let root = repo_root();
    let root = Path::new(&root);
    let declares = std::fs::read_to_string(root.join(MANIFEST))
        .map(|t| !tree_gates(&t).is_empty())
        .unwrap_or(false);
    if !declares {
        return Outcome::Inert;
    }
    let problems = check_repo(root);
    if problems.is_empty() {
        ok(settings, "tree gates match their CI steps");
        return Outcome::Passed;
    }
    for p in &problems {
        eprintln!("  {}", crate::ui::sanitize(&p.to_string()));
    }
    fail(&format!(
        "{} tree-gate parity problem(s) — fix the workflow or the {} line; run {} to re-check",
        problems.len(),
        MANIFEST,
        hl("amont tree-parity")
    ));
    Outcome::Failed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gates(text: &str) -> Vec<TreeGate> {
        let g = tree_gates(text);
        assert!(!g.is_empty(), "no gate parsed from {text:?}");
        g
    }

    fn check(manifest: &str, workflow: &str) -> Vec<Problem> {
        check_texts(
            &gates(manifest),
            &[(
                ".forgejo/workflows/ci.yaml".to_string(),
                workflow.to_string(),
            )],
        )
    }

    const ESLINT: &str = "tree eslint eslint * attest npm run lint -- {cache}";

    fn wf(step_run: &str) -> String {
        format!(
            "name: ci\non:\n  push:\njobs:\n  checks:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n      - id: attest\n        uses: fredericrous/attest@v1\n      - name: Lint\n        if: ${{{{ !contains(fromJSON(steps.attest.outputs.gates || '[]'), 'tree-eslint') }}}}\n        {step_run}\n"
        )
    }

    #[test]
    fn parity_matching_run_passes() {
        assert_eq!(check(ESLINT, &wf("run: npm run lint")), vec![]);
    }

    #[test]
    fn parity_quoted_run_passes() {
        assert_eq!(check(ESLINT, &wf("run: 'npm run lint'")), vec![]);
    }

    #[test]
    fn parity_drifted_run_fails() {
        let p = check(ESLINT, &wf("run: npm run lint -- --max-warnings 0"));
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0]
            .what
            .contains("runs `npm run lint -- --max-warnings 0` here"));
    }

    #[test]
    fn parity_block_scalar_rejected() {
        let p = check(ESLINT, &wf("run: |\n          npm run lint"));
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].what.contains("block scalar"));
    }

    #[test]
    fn parity_anchor_rejected() {
        let p = check(ESLINT, &wf("run: *lint"));
        assert!(p[0].what.contains("anchor"), "{p:?}");
    }

    #[test]
    fn parity_interpolation_rejected() {
        let p = check(ESLINT, &wf("run: npm run ${{ matrix.task }}"));
        assert!(p[0].what.contains("interpolation"), "{p:?}");
    }

    #[test]
    fn parity_step_env_rejected() {
        let p = check(
            ESLINT,
            &wf("env:\n          CI: '1'\n        run: npm run lint"),
        );
        assert!(p[0].what.contains("step-level `env:`"), "{p:?}");
    }

    #[test]
    fn parity_step_shell_rejected() {
        let p = check(ESLINT, &wf("shell: bash\n        run: npm run lint"));
        assert!(p[0].what.contains("step-level `shell:`"), "{p:?}");
    }

    #[test]
    fn parity_undeclared_working_directory_rejected() {
        let p = check(
            ESLINT,
            &wf("working-directory: web\n        run: npm run lint"),
        );
        assert!(p[0].what.contains("working-directory"), "{p:?}");
    }

    #[test]
    fn parity_declared_cwd_passes() {
        let p = check(
            "tree eslint eslint * attest cwd=web npm run lint",
            &wf("working-directory: web\n        run: npm run lint"),
        );
        assert_eq!(p, vec![]);
    }

    #[test]
    fn parity_cwd_without_working_directory_rejected() {
        let p = check(
            "tree eslint eslint * attest cwd=web npm run lint",
            &wf("run: npm run lint"),
        );
        assert!(p[0].what.contains("no `working-directory:`"), "{p:?}");
    }

    #[test]
    fn parity_job_env_rejected() {
        let w = wf("run: npm run lint").replace(
            "    runs-on: ubuntu-latest\n",
            "    runs-on: ubuntu-latest\n    env:\n      NODE_ENV: test\n",
        );
        let p = check(ESLINT, &w);
        assert!(p[0].what.contains("inherits"), "{p:?}");
    }

    #[test]
    fn parity_job_defaults_rejected() {
        let w = wf("run: npm run lint").replace(
            "    runs-on: ubuntu-latest\n",
            "    runs-on: ubuntu-latest\n    defaults:\n      run:\n        working-directory: web\n",
        );
        let p = check(ESLINT, &w);
        assert!(p[0].what.contains("inherits"), "{p:?}");
    }

    #[test]
    fn parity_workflow_defaults_rejected() {
        let w = wf("run: npm run lint")
            .replace("jobs:\n", "defaults:\n  run:\n    shell: bash\njobs:\n");
        let p = check(ESLINT, &w);
        assert!(p[0].what.contains("inherits"), "{p:?}");
    }

    #[test]
    fn parity_undeclared_gate_rejected() {
        let p = check(
            "tree prettier prettier * attest npx prettier --check .",
            &wf("run: npm run lint"),
        );
        assert!(
            p.iter()
                .any(|p| p.what.contains("`tree-eslint` is not declared")),
            "{p:?}"
        );
        assert!(
            p.iter().any(|p| p.what.contains("no workflow step")),
            "{p:?}"
        );
    }

    #[test]
    fn parity_declared_gate_without_step_rejected() {
        let p = check(
            ESLINT,
            "name: ci\njobs:\n  a:\n    steps:\n      - run: true\n",
        );
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].what.contains("no workflow step is skipped on it"));
    }

    #[test]
    fn parity_job_level_reference_rejected() {
        let w = "name: ci\njobs:\n  lint:\n    if: ${{ !contains(fromJSON(needs.a.outputs.gates), 'tree-eslint') }}\n    steps:\n      - run: npm run lint\n";
        let p = check(ESLINT, w);
        assert!(p.iter().any(|p| p.what.contains("outside a step")), "{p:?}");
    }

    #[test]
    fn parity_two_gates_on_one_step_rejected() {
        let w = wf("run: npm run lint").replace(
            "'tree-eslint') }}",
            "'tree-eslint') && !contains(fromJSON(steps.attest.outputs.gates), 'tree-prettier') }}",
        );
        let p = check(
            "tree eslint eslint * attest npm run lint\ntree prettier prettier * attest npx prettier --check .",
            &w,
        );
        assert!(
            p.iter().any(|p| p.what.contains("several tree gates")),
            "{p:?}"
        );
    }
}
