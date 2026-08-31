//! pre-commit-helm-lint — `helm lint` on the charts a commit touches.

use super::common::{fail, hl, ok, repo_root, staged_files, warn, which};
use crate::check::Outcome;
use std::process::{Command, Stdio};

/// The extensions this check consumes. `.tpl` because a chart's templates are
/// where most of its mistakes live, and editing one without touching a `.yaml`
/// is the ordinary case.
pub const EXTS: &[&str] = &[".yaml", ".yml", ".tpl"];

/// What marks a directory as a chart. `helm lint` wants the chart DIRECTORY,
/// so a staged `templates/deployment.yaml` has to be resolved upward to the
/// directory holding this file.
pub const MARKERS: &[&str] = &["Chart.yaml"];

pub fn run(_args: &[std::ffi::OsString]) -> Outcome {
    let files = staged_files(EXTS);
    if files.is_empty() {
        return Outcome::Passed;
    }
    let root = repo_root();
    // Resolve the staged files to the CHARTS they belong to, before asking
    // whether helm exists: a repository with `.yaml` files and no chart at all
    // must not be told to install anything.
    let charts = super::k8s::marker_roots(&root, &files, MARKERS);
    if charts.is_empty() {
        return Outcome::Passed;
    }
    let Some(bin) = which("helm") else {
        warn(&format!(
            "This repo has charts but helm is not installed. Install {}",
            hl("helm")
        ));
        return Outcome::Unavailable;
    };
    // ONE invocation per chart, not per file. `Perso/charts` holds ten of
    // them, and a commit touching four files in one chart must lint that chart
    // once — `marker_roots` dedupes precisely so this loop can be naive.
    let mut bad: Vec<&str> = Vec::new();
    for chart in &charts {
        // `--` is not available here (helm takes the path positionally), so a
        // chart directory is passed as-is; `marker_roots` only ever returns
        // paths it walked up from a staged file, never anything user-supplied
        // on a command line.
        let ok_now = Command::new(&bin)
            .arg("lint")
            .arg(chart)
            .current_dir(&root)
            .stdin(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok_now {
            bad.push(chart);
        }
    }
    if !bad.is_empty() {
        fail(&format!(
            "helm lint found issues in {}. Please fix",
            hl(&bad.join(", "))
        ));
        return Outcome::Failed;
    }
    ok(&format!(
        "helm lint passed ({} chart{})",
        charts.len(),
        if charts.len() == 1 { "" } else { "s" }
    ));
    Outcome::Passed
}
