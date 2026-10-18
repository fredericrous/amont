//! The tool-driven pre-commit hooks: lint-json-yaml, yamllint, lint-js,
//! prettier, ruff, pyright.
//!
//! Every one is SCOPED — it fires only when the repo opts in via a config —
//! and SOFT — a missing tool warns and skips rather than blocking a commit.
//! Those two properties are what keep a Python repo from pulling eslint, so
//! they are tested first and without needing the tool present.

use crate::common;
use common::{missing, Repo};

/// A Helm template body. `a: {{ .Values.x }}` is NOT usable here — it happens
/// to be valid flow-mapping YAML, so yq accepts it and the "outside a chart
/// still fails" case would pass for the wrong reason. A conditional BLOCK is
/// what yq genuinely rejects, which is why the zsh suite used one.
/// The one rule the yamllint cases opt into.
const YAMLLINT: &str = "rules:\n  trailing-spaces: enable\n";

const HELM_TMPL: &str =
    "{{- if .Values.enabled }}\nkind: Deployment\nmetadata:\n  name: x\n{{- end }}\n";

// ---- lint-json-yaml -----------------------------------------------------

#[test]
fn invalid_json_is_rejected_and_valid_json_passes() {
    if missing("node") {
        return;
    }
    let r = Repo::new();
    r.stage("bad.json", "{\"a\": 1,,}\n");
    assert!(!r.hook("pre-commit-lint-json-yaml", &[]).passed());

    let r = Repo::new();
    r.stage("ok.json", "{\"a\": 1}\n");
    assert!(r.hook("pre-commit-lint-json-yaml", &[]).passed());
}

#[test]
fn invalid_yaml_is_rejected_and_valid_yaml_passes() {
    if missing("yq") {
        return;
    }
    let r = Repo::new();
    r.stage("bad.yaml", "a:\n\tb: 1\n"); // tab indentation
    assert!(!r.hook("pre-commit-lint-json-yaml", &[]).passed());

    let r = Repo::new();
    r.stage("ok.yaml", "a:\n  b: 1\n");
    assert!(r.hook("pre-commit-lint-json-yaml", &[]).passed());
}

/// `.yml` is the OTHER spelling, and it was never linted at all.
///
/// The registry declared `[".json", ".yaml", ".yml"]` while the check asked
/// `staged_files` for `[".yaml"]`. `amont list` and the fleet dashboard both
/// reported the check as covering `.yml`, and a staged, broken `x.yml` returned
/// `Outcome::Passed` with no output whatsoever. Both lists now come from
/// `lint_json_yaml::EXTS`.
#[test]
fn a_broken_yml_is_rejected_like_a_broken_yaml() {
    if missing("yq") {
        return;
    }
    let r = Repo::new();
    r.stage("bad.yml", "a:\n\tb: 1\n"); // tab indentation
    assert!(
        !r.hook("pre-commit-lint-json-yaml", &[]).passed(),
        ".yml was declared in scope but never actually parsed"
    );

    let r = Repo::new();
    r.stage("ok.yml", "a:\n  b: 1\n");
    assert!(r.hook("pre-commit-lint-json-yaml", &[]).passed());
}

/// …and the Helm carve-out has to reach the newly-linted extension too, or
/// fixing the scope would start failing valid chart commits that spell their
/// templates `.yml`.
#[test]
fn a_helm_chart_template_named_yml_is_skipped() {
    if missing("yq") {
        return;
    }
    let r = Repo::new();
    r.write("chart/Chart.yaml", "name: c\n");
    r.stage("chart/templates/deploy.yml", HELM_TMPL);
    assert!(r.hook("pre-commit-lint-json-yaml", &[]).passed());
}

/// Helm chart templates carry Go templating and are not valid YAML until Helm
/// renders them. Without this carve-out every valid chart commit would need
/// --no-verify.
#[test]
fn a_helm_chart_template_is_skipped() {
    if missing("yq") {
        return;
    }
    let r = Repo::new();
    r.write("chart/Chart.yaml", "name: c\n");
    r.stage("chart/templates/deploy.yaml", HELM_TMPL);
    assert!(r.hook("pre-commit-lint-json-yaml", &[]).passed());
}

/// …but the same Go-template YAML OUTSIDE a chart is still invalid.
#[test]
fn go_template_yaml_outside_a_chart_still_fails() {
    if missing("yq") {
        return;
    }
    let r = Repo::new();
    r.stage("k/deploy.yaml", HELM_TMPL);
    assert!(!r.hook("pre-commit-lint-json-yaml", &[]).passed());
}

// ---- yamllint -----------------------------------------------------------

/// Stock yamllint rules are too noisy to enforce generically, so a repo-local
/// config is the opt-in signal. Without one the hook must do nothing.
///
/// SILENT, not merely passing, and that holds whether or not yamllint is
/// installed: the opt-in is tested before the binary, so a repo that never
/// asked for yamllint is never told to install it. One repo in the fleet has
/// this config; the nag used to reach the other ninety-five.
#[test]
fn yamllint_does_nothing_without_a_repo_config() {
    let r = Repo::new();
    r.stage("a.yaml", "a:   1\n");
    let run = r.hook("pre-commit-yamllint", &[]);
    assert!(run.silent(), "expected silence, got:\n{}", run.output());
}

#[test]
fn yamllint_runs_when_the_repo_opts_in() {
    if missing("yamllint") {
        return;
    }
    let r = Repo::new();
    r.write(".yamllint", "rules:\n  trailing-spaces: enable\n");
    r.stage("a.yaml", "a: 1   \n"); // trailing spaces
    assert!(!r.hook("pre-commit-yamllint", &[]).passed());
}

// ---- lint-js ------------------------------------------------------------

/// eslint 9+ ERRORS when it finds no config instead of no-op'ing, so the
/// config is the opt-in signal — without it a repo that does not lint JS
/// would fail every commit.
#[test]
fn lint_js_skips_a_repo_with_no_eslint_config() {
    let r = Repo::new();
    r.stage("a.ts", "const x = 1\n");
    let run = r.hook("pre-commit-lint-js", &[]);
    assert!(run.passed());
    assert!(run.says("no eslint config"));
}

#[test]
fn lint_js_reports_a_real_error() {
    if missing("eslint") {
        return;
    }
    let r = Repo::new();
    r.write(
        "eslint.config.js",
        "module.exports = [{rules:{'no-undef':'error'}}];\n",
    );
    r.stage("a.js", "undefinedFunction();\n");
    assert!(!r.hook("pre-commit-lint-js", &[]).passed());
}

// ---- prettier -----------------------------------------------------------

#[test]
fn prettier_does_nothing_without_config_or_a_local_binary() {
    let r = Repo::new();
    r.stage("a.ts", "const  x   =1\n");
    assert!(r.hook("pre-commit-prettier", &[]).passed());
}

#[test]
fn prettier_flags_an_unformatted_file_when_the_repo_opts_in() {
    if missing("prettier") {
        return;
    }
    let r = Repo::new();
    r.write(".prettierrc", "{}\n");
    r.stage("a.ts", "const  x   =1\n");
    assert!(!r.hook("pre-commit-prettier", &[]).passed());

    let r2 = Repo::new();
    r2.write(".prettierrc", "{}\n");
    r2.stage("b.ts", "const x = 1;\n");
    assert!(r2.hook("pre-commit-prettier", &[]).passed());
}

// ---- ruff / pyright -----------------------------------------------------

#[test]
fn ruff_skips_a_repo_with_no_ruff_config() {
    let r = Repo::new();
    r.stage("a.py", "import os\n");
    assert!(r.hook("pre-commit-ruff", &[]).passed());
}

#[test]
fn ruff_reports_lint_and_format_problems_when_the_repo_opts_in() {
    if missing("ruff") && missing("uvx") {
        return;
    }
    let r = Repo::new();
    r.write("pyproject.toml", "[tool.ruff]\n");
    r.stage("a.py", "import os\n"); // unused import
    assert!(!r.hook("pre-commit-ruff", &[]).passed());
}

/// `pre-commit-ruff` declared `Fix::Rewrite` with no fixing code at all — only
/// prettier and the manifest's externals ever called `restage` — so
/// `amont list --json` reported `"fix":"rewrite"`, which `agents_md` tells
/// agents to trust, for a check that could never repair anything.
///
/// Unlike `cargo fmt`, ruff legitimately leaves findings it cannot fix, so
/// the repair pass's exit code decides nothing: the verdict comes from a fresh
/// pair of check passes afterwards.
#[test]
fn ruff_fixes_what_it_can_and_still_blocks_on_the_rest() {
    if missing("ruff") && missing("uvx") {
        return;
    }
    let r = Repo::new();
    r.write("pyproject.toml", "[tool.ruff]\n");
    r.git(&["config", "amont.fix", "true"]);
    // `import os` is auto-fixable (F401); the undefined name is not.
    r.stage("a.py", "import os\nprint( undefined_name )\n");

    let run = r.hook("pre-commit-ruff", &[]);
    assert!(
        !run.passed(),
        "an unfixable finding must still block:\n{}",
        run.output()
    );
    let on_disk = std::fs::read_to_string(r.path("a.py")).expect("read");
    assert!(
        !on_disk.contains("import os"),
        "the fixable finding should have been repaired: {on_disk:?}"
    );
    let staged = r.git(&["show", ":a.py"]);
    assert_eq!(
        String::from_utf8_lossy(&staged.stdout),
        on_disk,
        "whatever ruff did fix must be staged, so the next attempt starts from there"
    );
}

/// With fixing OFF the file must come back byte for byte as it was written.
#[test]
fn ruff_leaves_files_alone_when_fixing_is_off() {
    if missing("ruff") && missing("uvx") {
        return;
    }
    let r = Repo::new();
    r.write("pyproject.toml", "[tool.ruff]\n");
    r.stage("a.py", "import os\n");

    assert!(!r.hook("pre-commit-ruff", &[]).passed());
    assert_eq!(
        std::fs::read_to_string(r.path("a.py")).expect("read"),
        "import os\n",
        "it rewrote a file nobody asked it to rewrite"
    );
}

/// `pre-commit-pyright` is `Severity::Block` — it stops commits — and for a
/// long time this was its ONLY test: an assertion that it does nothing. Pyright
/// was installed on every CI runner and nothing exercised it, so a check with
/// the power to reject a commit had no coverage of the path where it does.
/// The two cases below are that coverage; this one keeps the scoping honest.
///
/// SILENT, not merely passing: a repo with no pyright config has not opted in,
/// and must not be told to install a type checker it never asked for — the same
/// rule yamllint and kube-linter were fixed for.
///
/// Needs no tool at all. `opts_in` is tested before the binary is resolved, so
/// this holds on a machine with no pyright, which is most of them.
#[test]
fn pyright_skips_a_repo_with_no_pyright_config() {
    let r = Repo::new();
    r.stage("a.py", "x: int = 'nope'\n");
    let run = r.hook("pre-commit-pyright", &[]);
    assert!(run.passed());
    assert!(
        run.silent(),
        "a repo that never opted in must hear nothing:\n{}",
        run.output()
    );
}

/// The blocking path, which nothing asserted until now.
///
/// `pyrightconfig.json` is the opt-in. `x: int = 'nope'` is a plain
/// assignment-type error that needs no imports and no inference across files,
/// so it fails on any pyright version without pinning the test to one.
///
/// The second assertion is not padding. When this case was first run the check
/// was passing `--` before the file list, pyright read that as a path, and the
/// hook failed with `File or directory ".../--" does not exist` — a blocked
/// commit, exit 4, and nothing to do with the staged code. This case went GREEN
/// on that. "It blocked" is far too weak a claim for a `Severity::Block` check;
/// what it must do is block for the reason the author can act on.
#[test]
fn pyright_reports_a_type_error() {
    if missing("pyright") {
        return;
    }
    let r = Repo::new();
    r.write("pyrightconfig.json", "{}\n");
    r.stage("a.py", "x: int = 'nope'\n");
    let run = r.hook("pre-commit-pyright", &[]);
    assert!(
        !run.passed(),
        "a type error must block a Severity::Block check:\n{}",
        run.output()
    );
    assert!(
        !run.says("does not exist"),
        "it blocked over an argument it could not resolve, not over the code:\n{}",
        run.output()
    );
    assert!(
        run.says("a.py"),
        "the report must name the offending file:\n{}",
        run.output()
    );
}

/// …and the other half, which is what stops the case above from passing for the
/// wrong reason. A check that fails on everything satisfies "reports a type
/// error" perfectly well; only a clean file passing proves it is reading the
/// code rather than tripping over its own invocation — a wrong `--`, a bad
/// working directory, a binary it could not resolve.
///
/// This is the case that actually caught the `--` bug, on its first run.
#[test]
fn pyright_passes_clean_code() {
    if missing("pyright") {
        return;
    }
    let r = Repo::new();
    r.write("pyrightconfig.json", "{}\n");
    r.stage("a.py", "x: int = 1\n");
    let run = r.hook("pre-commit-pyright", &[]);
    assert!(run.passed(), "clean code must not block:\n{}", run.output());
}

// ---- every tool ---------------------------------------------------------

/// hook, tool, opt-in config (name, body), file, content, passes.
type FlagNamedCase = (
    &'static str,
    &'static str,
    Option<(&'static str, &'static str)>,
    &'static str,
    &'static str,
    bool,
);

/// A staged file named like a flag is still judged on its CONTENT, by every
/// tool: clean passes and broken fails. Without a `--` before the file list
/// each tool reads `-weird.<ext>` as an option before it opens the file:
/// node says `bad option`, yq `unknown shorthand flag`, yamllint's argparse
/// loses its required positional, and prettier only WARNS and exits 0 having
/// checked nothing. Pyright has no `--`, so each file is handed over as
/// `./<path>`. The failing row of each pair is what proves the file was
/// read: a tool that silently checks nothing passes the clean row too.
#[test]
fn a_file_named_like_a_flag_is_judged_on_its_content() {
    let cases: &[FlagNamedCase] = &[
        (
            "lint-json-yaml",
            "node",
            None,
            "-weird.json",
            "{\"a\": 1}\n",
            true,
        ),
        (
            "lint-json-yaml",
            "node",
            None,
            "-weird.json",
            "{\"a\": \n",
            false,
        ),
        ("lint-json-yaml", "yq", None, "-weird.yaml", "a: 1\n", true),
        (
            "lint-json-yaml",
            "yq",
            None,
            "-weird.yaml",
            HELM_TMPL,
            false,
        ),
        (
            "yamllint",
            "yamllint",
            Some((".yamllint", YAMLLINT)),
            "-weird.yaml",
            "a: 1\n",
            true,
        ),
        (
            "yamllint",
            "yamllint",
            Some((".yamllint", YAMLLINT)),
            "-weird.yaml",
            "a: 1   \n",
            false,
        ),
        (
            "prettier",
            "prettier",
            Some((".prettierrc", "{}\n")),
            "-weird.ts",
            "const x = 1;\n",
            true,
        ),
        (
            "prettier",
            "prettier",
            Some((".prettierrc", "{}\n")),
            "-weird.ts",
            "const  x   =1\n",
            false,
        ),
        (
            "pyright",
            "pyright",
            Some(("pyrightconfig.json", "{}\n")),
            "-weird.py",
            "x: int = 1\n",
            true,
        ),
        (
            "pyright",
            "pyright",
            Some(("pyrightconfig.json", "{}\n")),
            "-weird.py",
            "x: int = 'nope'\n",
            false,
        ),
    ];
    for &(hook, tool, config, file, content, passes) in cases {
        if missing(tool) {
            continue;
        }
        let r = Repo::new();
        if let Some((name, body)) = config {
            r.write(name, body);
        }
        r.stage(file, content);
        let run = r.hook(&format!("pre-commit-{hook}"), &[]);
        assert_eq!(
            run.passed(),
            passes,
            "{hook} on {file} holding {content:?}:\n{}",
            run.output()
        );
    }
}
