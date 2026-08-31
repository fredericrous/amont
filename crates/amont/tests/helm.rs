//! pre-commit-helm-lint.

mod common;
use common::Repo;

fn chart(r: &Repo, dir: &str, name: &str) {
    r.stage(
        &format!("{dir}/Chart.yaml"),
        &format!("apiVersion: v2\nname: {name}\nversion: 0.1.0\n"),
    );
    r.stage(&format!("{dir}/values.yaml"), "replicas: 1\n");
    r.stage(
        &format!("{dir}/templates/cm.yaml"),
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: x\n",
    );
}

/// A repository with `.yaml` and no `Chart.yaml` must not hear about helm —
/// that marker is the whole opt-in, and most YAML in the world is not a chart.
#[test]
fn yaml_without_a_chart_is_silent() {
    let r = Repo::new();
    r.stage("config.yaml", "a: 1\n");
    let run = r.hook("pre-commit", &[]);
    assert!(!run.says("helm"), "{}", run.output());
    assert!(run.passed(), "{}", run.output());
}

#[test]
fn a_valid_chart_passes() {
    if common::missing("helm") {
        return;
    }
    let r = Repo::new();
    chart(&r, "charts/app", "app");
    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "a valid chart must pass: {}", run.output());
    assert!(run.says("helm lint passed"), "{}", run.output());
}

/// THE property worth pinning: `Perso/charts` holds ten charts, and a commit
/// touching several files across two of them must lint each chart ONCE — not
/// once per file, and not the repository root once.
#[test]
fn each_chart_is_linted_once_however_many_files_it_staged() {
    if common::missing("helm") {
        return;
    }
    let r = Repo::new();
    chart(&r, "charts/one", "one");
    chart(&r, "charts/two", "two");
    // Three more files inside one chart: still one invocation for it.
    r.stage(
        "charts/one/templates/a.yaml",
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: a\n",
    );
    r.stage(
        "charts/one/templates/b.yaml",
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: b\n",
    );
    r.stage(
        "charts/one/templates/_helpers.tpl",
        "{{- define \"x\" -}}y{{- end -}}\n",
    );

    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.output());
    assert!(
        run.says("2 charts"),
        "two charts, counted once each: {}",
        run.output()
    );
}

#[test]
fn a_broken_chart_blocks_and_names_itself() {
    if common::missing("helm") {
        return;
    }
    let r = Repo::new();
    // No `name:` — helm lint refuses a chart without one.
    r.stage("charts/bad/Chart.yaml", "apiVersion: v2\nversion: 0.1.0\n");
    r.stage("charts/bad/values.yaml", "x: 1\n");
    let run = r.hook("pre-commit", &[]);
    assert!(!run.passed(), "a broken chart must block: {}", run.output());
    assert!(
        run.says("charts/bad"),
        "and say WHICH chart: {}",
        run.output()
    );
}
