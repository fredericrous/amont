//! pre-commit-helm-lint: chart discovery. Scoping, a valid and a broken chart
//! and a missing helm are rows of the tables in `tool_tables`.

use crate::common;
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
