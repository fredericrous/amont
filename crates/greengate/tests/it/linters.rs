//! What one tool hook does that the others do not: lint-json-yaml's Helm
//! carve-out, ruff's partial fix, and the flag-named file every hook must
//! judge on its content. The promises they all keep (scoping, a broken and a
//! clean input, a missing tool, fixing on and off) are rows of the tables in
//! `tool_tables`.

use crate::common;
use common::{missing, Repo};

/// The one rule the yamllint cases opt into.
const YAMLLINT: &str = "rules:\n  trailing-spaces: enable\n";

/// A Helm template body. `a: {{ .Values.x }}` is NOT usable here — it happens
/// to be valid flow-mapping YAML, so yq accepts it and the "outside a chart
/// still fails" case would pass for the wrong reason. A conditional BLOCK is
/// what yq genuinely rejects, which is why the zsh suite used one.
const HELM_TMPL: &str =
    "{{- if .Values.enabled }}\nkind: Deployment\nmetadata:\n  name: x\n{{- end }}\n";

// ---- lint-json-yaml: the Helm carve-out ----------------------------------

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

/// …and the carve-out has to reach `.yml`, the spelling that was once never
/// linted at all, or fixing that scope would start failing valid chart
/// commits that spell their templates `.yml`.
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

// ---- ruff ---------------------------------------------------------------

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
