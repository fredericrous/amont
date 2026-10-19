//! The per-language checks, one table per property they share (decisions plan
//! 2026-10-17-amont-tests-follow-user-flows, Phase 7).
//!
//! Every tool hook makes the same promises: out of scope it passes and says
//! nothing; opted in, a broken input is refused and a clean one passes; a
//! missing tool warns and never blocks; with fixing off a file is left as it
//! was written, and with fixing on the repair reaches the index. Each promise
//! is one test here, with a row per tool, so a tool without a row for a
//! promise the others keep is visible in the table rather than in a file
//! nobody opens. What only one tool does stays beside it: ruff's partial fix,
//! cargo's workspaces, the npm gate's order, chart discovery, the toolchain
//! pin.
//!
//! A row is a repository built from scratch: `written` files (an opt-in
//! config, not staged), `base` files committed first, then `staged`. A
//! `pre-push-*` hook gets `staged` as a second commit and the range between
//! the two on stdin, as git feeds it. Every row runs, and every failing row is
//! named with its tool, hook and files, so one broken tool does not hide the
//! others' verdicts.
//!
//! The clean and broken inputs of a check are one [`Pair`]: a check that
//! judges nothing passes its clean input too, and only the broken half of the
//! same pair shows that it read the file.
//!
//! Slower than a second, on purpose: a table runs its rows one after the
//! other, so it costs the sum of the tests it replaced (the two content
//! tables about 5 s each, most of it clippy and pyright). Running rows in
//! threads would buy that back by loading the machine the clock-bound tests
//! in this binary share; libtest already runs the tables beside each other.

use crate::common;
use common::{missing, HookRun, Repo};
use std::io::Write;
use std::process::{Command, Stdio};

/// When a row runs.
#[derive(Clone, Copy)]
enum Gate {
    /// Always: the row needs no tool.
    Always,
    /// Only when every one of these is on PATH.
    Tools(&'static [&'static str]),
    /// Only when at least one of these is on PATH.
    AnyOf(&'static [&'static str]),
    /// Only when this tool is ABSENT: the row asserts the soft-fail branch.
    /// The present path prints a marker of its own, because `missing()`
    /// prints only on the absent one, and CI's skip reporter looks for
    /// `argo PRESENT — skipping` to know the argo row did not vanish.
    Absent(&'static str),
}

impl Gate {
    fn skips(self) -> bool {
        match self {
            Gate::Always => false,
            Gate::Tools(all) => all.iter().any(|t| missing(t)),
            Gate::AnyOf(any) => any.iter().all(|t| missing(t)),
            Gate::Absent(t) => {
                if missing(t) {
                    false
                } else {
                    println!("  ! {t} PRESENT — skipping (this case asserts the ABSENT branch)");
                    true
                }
            }
        }
    }
}

/// The PATH the hook runs with.
#[derive(Clone, Copy)]
#[cfg_attr(not(unix), allow(dead_code))] // the shim rows skip off unix
enum PathKind {
    /// The machine's own.
    Host,
    /// Fake tools (name, `/bin/sh` body) first, the machine's PATH after.
    /// Unix only, as the files these rows came from were.
    Shims(&'static [(&'static str, &'static str)]),
    /// A directory holding only git: every tool is absent, whether or not
    /// this machine has it. Unix only (a symlink to git).
    Bare,
}

/// What a row leaves on disk or in the index, checked after the hook.
#[derive(Clone, Copy)]
enum After {
    /// The working-tree file holds exactly this.
    Disk(&'static str, &'static str),
    /// The staged blob holds exactly this.
    Index(&'static str, &'static str),
}

type Files = &'static [(&'static str, &'static str)];

#[derive(Clone, Copy)]
struct Row {
    /// The tool the row is about, as its failure names it.
    tool: &'static str,
    hook: &'static str,
    gate: Gate,
    path: PathKind,
    /// `git config` set before anything else (`amont.fix`).
    config: Files,
    /// Written and NOT staged: the repository's opt-in.
    written: Files,
    /// Committed before `staged`.
    base: Files,
    staged: Files,
    passes: bool,
    /// No output at all.
    silent: bool,
    says: &'static [&'static str],
    never_says: &'static [&'static str],
    after: &'static [After],
}

const ROW: Row = Row {
    tool: "",
    hook: "",
    gate: Gate::Always,
    path: PathKind::Host,
    config: &[],
    written: &[],
    base: &[],
    staged: &[],
    passes: true,
    silent: false,
    says: &[],
    never_says: &[],
    after: &[],
};

/// One check's clean input and its broken one.
type Pair = (Row, Row);

impl Row {
    fn name(&self) -> String {
        let files: Vec<&str> = self.staged.iter().map(|(p, _)| *p).collect();
        format!("{} · {} · {}", self.tool, self.hook, files.join(" "))
    }
}

fn head(r: &Repo) -> String {
    String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string()
}

#[cfg(unix)]
fn shim_dir(r: &Repo) -> std::path::PathBuf {
    let dir = r.path(".git/toolshims");
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// The PATH value for `kind`, or `None` for the machine's own.
#[cfg(unix)]
fn path_for(r: &Repo, kind: PathKind) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    match kind {
        PathKind::Host => None,
        PathKind::Shims(shims) => {
            let dir = shim_dir(r);
            for (name, body) in shims {
                let p = dir.join(name);
                std::fs::write(&p, format!("#!/bin/sh\n{body}")).expect("write");
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod");
            }
            Some(format!(
                "{}:{}",
                dir.display(),
                std::env::var("PATH").unwrap_or_default()
            ))
        }
        PathKind::Bare => {
            let dir = shim_dir(r);
            let git = std::env::split_paths(&std::env::var_os("PATH").expect("PATH"))
                .map(|d| d.join("git"))
                .find(|p| p.is_file())
                .expect("git on PATH");
            std::os::unix::fs::symlink(git, dir.join("git")).expect("symlink git");
            Some(dir.display().to_string())
        }
    }
}

/// Build the row's repository and run its hook. `None` when the row skips.
fn run(row: &Row) -> Option<(Repo, HookRun)> {
    if row.gate.skips() {
        return None;
    }
    #[cfg(not(unix))]
    if !matches!(row.path, PathKind::Host) {
        println!("  ! {} needs a unix PATH shim — skipping", row.name());
        return None;
    }
    let r = Repo::new();
    for (k, v) in row.config {
        r.git(&["config", k, v]);
    }
    for (p, c) in row.written {
        r.write(p, c);
    }
    let push = row.hook.starts_with("pre-push");
    let mut from = String::new();
    if !row.base.is_empty() {
        for (p, c) in row.base {
            r.stage(p, c);
        }
        r.commit("chore: base");
        from = head(&r);
    }
    for (p, c) in row.staged {
        r.stage(p, c);
    }
    #[cfg(unix)]
    let path = path_for(&r, row.path);
    #[cfg(not(unix))]
    let path: Option<String> = None;

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_amont"));
    cmd.arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg(row.hook)
        .current_dir(&r.dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    Repo::strip_git_env_impl(&mut cmd);
    Repo::pin_host_env(&mut cmd);
    if let Some(p) = path {
        cmd.env("PATH", p);
    }
    let out = if push {
        assert!(!from.is_empty(), "{}: a push row needs a base", row.name());
        r.commit("feat: change");
        let line = format!("refs/heads/feat/x {} refs/heads/feat/x {from}\n", head(&r));
        let mut child = cmd.stdin(Stdio::piped()).spawn().expect("spawn amont");
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(line.as_bytes())
            .expect("write stdin");
        child.wait_with_output().expect("wait amont")
    } else {
        cmd.stdin(Stdio::null()).output().expect("run amont")
    };
    let run = HookRun {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    };
    Some((r, run))
}

/// Everything the row expected and did not get.
fn problems(row: &Row, r: &Repo, run: &HookRun) -> Vec<String> {
    let mut p = Vec::new();
    if run.passed() != row.passes {
        p.push(format!(
            "expected it to {}, exit {}",
            if row.passes { "pass" } else { "refuse" },
            run.code
        ));
    }
    if row.silent && !run.silent() {
        p.push("expected silence".into());
    }
    for s in row.says {
        if !run.says(s) {
            p.push(format!("expected it to say {s:?}"));
        }
    }
    for s in row.never_says {
        if run.says(s) {
            p.push(format!("expected it never to say {s:?}"));
        }
    }
    for a in row.after {
        match *a {
            After::Disk(path, want) => {
                let got = std::fs::read_to_string(r.path(path)).unwrap_or_default();
                if got != want {
                    p.push(format!("{path} on disk is {got:?}, expected {want:?}"));
                }
            }
            After::Index(path, want) => {
                let staged = r.git(&["show", &format!(":{path}")]);
                let got = String::from_utf8_lossy(&staged.stdout);
                if got != want {
                    p.push(format!("{path} in the index is {got:?}, expected {want:?}"));
                }
            }
        }
    }
    p
}

/// Run every row; fail once, naming each row that broke its promise.
fn table<'a>(rows: impl IntoIterator<Item = &'a Row>) {
    let mut failed = Vec::new();
    let mut ran = 0;
    for row in rows {
        let Some((r, run)) = run(row) else {
            continue;
        };
        ran += 1;
        let p = problems(row, &r, &run);
        if !p.is_empty() {
            failed.push(format!(
                "{}: {}\n{}",
                row.name(),
                p.join("; "),
                run.output()
            ));
        }
    }
    assert!(
        failed.is_empty(),
        "{} of {ran} rows failed:\n\n{}",
        failed.len(),
        failed.join("\n\n")
    );
}

// ---- inputs -----------------------------------------------------------------

const YAMLLINT: Files = &[(".yamllint", "rules:\n  trailing-spaces: enable\n")];
const ESLINT: Files = &[(
    "eslint.config.js",
    "module.exports = [{rules:{'no-undef':'error'}}];\n",
)];
const PRETTIERRC: Files = &[(".prettierrc", "{}\n")];
const RUFF: Files = &[("pyproject.toml", "[tool.ruff]\n")];
const PYRIGHT: Files = &[("pyrightconfig.json", "{}\n")];
const FIX: Files = &[("amont.fix", "true")];

const CARGO_TOML: &str =
    "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n";
const UNFORMATTED_RS: &str = "fn main(){let _x=1;}\n";

const GO_MOD: &str = "module example.com/x\n";
const GO_MODULE: Files = &[("go.mod", GO_MOD), ("main.go", "package main\n")];
/// `-l` lists the file; `-w` writes the formatted content.
const GOFMT_FIXES: &str = "case \"$1\" in\n-l) echo main.go ;;\n-w) printf 'package main\\n\\nfunc main() {}\\n' > main.go ;;\nesac\nexit 0";

const DEPLOY: &str = "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: d\n";
const CLEAN_DEPLOY: &str = "\
apiVersion: apps/v1
kind: Deployment
metadata:
  name: d
spec:
  selector:
    matchLabels:
      app: d
  template:
    metadata:
      labels:
        app: d
    spec:
      containers:
        - name: c
          image: nginx:1.25
";
/// `CLEAN_DEPLOY` on `nginx:latest`.
const LATEST_DEPLOY: &str = "\
apiVersion: apps/v1
kind: Deployment
metadata:
  name: d
spec:
  selector:
    matchLabels:
      app: d
  template:
    metadata:
      labels:
        app: d
    spec:
      containers:
        - name: c
          image: nginx:latest
";
/// `CLEAN_DEPLOY` with a string `replicas`. kustomize builds it happily — it
/// does not schema-check — so a refusal is specifically kubeconform's.
const STRING_REPLICAS_DEPLOY: &str = "\
apiVersion: apps/v1
kind: Deployment
metadata:
  name: d
spec:
  replicas: three
  selector:
    matchLabels:
      app: d
  template:
    metadata:
      labels:
        app: d
    spec:
      containers:
        - name: c
          image: nginx:1.25
";
/// One deterministic rule, chosen in the config rather than inherited from
/// the tool's defaults: an upstream default-set change must not move a row.
const LATEST_TAG_ONLY: &str = "\
checks:
  addAllBuiltIn: false
  doNotAutoAddDefaults: true
  include:
    - latest-tag
";
const KUSTOMIZATION: (&str, &str) = (
    "kubernetes/kustomization.yaml",
    "resources:\n  - deploy.yaml\n",
);

const CHART: Files = &[
    (
        "charts/app/Chart.yaml",
        "apiVersion: v2\nname: app\nversion: 0.1.0\n",
    ),
    ("charts/app/values.yaml", "replicas: 1\n"),
    (
        "charts/app/templates/cm.yaml",
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: x\n",
    ),
];

// ---- out of scope -----------------------------------------------------------

/// Out of scope, a check passes and says nothing about itself: no opt-in
/// config, no project marker, no file it reads, a path outside its prefixes,
/// a push that touches nothing its suite tests.
///
/// SILENT, not merely passing, wherever the hook runs alone: the opt-in is
/// tested before the binary, so a repository that never asked for a tool is
/// never told to install it (one repository in the fleet has a yamllint
/// config; the nag used to reach the other ninety-five). kube-linter once
/// printed a skip notice here, and yamllint's silence is the precedent it
/// was changed to. The rows that run the whole `pre-commit` dispatcher can
/// only promise not to mention their tool. lint-js alone says why it
/// skipped: eslint 9+ ERRORS without a config instead of no-op'ing, so the
/// config is the opt-in and the skip is reported.
///
/// The `SHOULD-NOT-RUN` shims prove the tool was never invoked, not only that
/// it was quiet. The rows whose tool is installed hold whether or not it
/// is, so none of them is gated.
#[test]
fn out_of_scope_a_check_passes_and_says_nothing() {
    const NOT_RUN: &str = "echo SHOULD-NOT-RUN\nexit 1";
    let rows = [
        // No opt-in.
        Row {
            tool: "yamllint",
            hook: "pre-commit-yamllint",
            staged: &[("a.yaml", "a:   1\n")],
            silent: true,
            ..ROW
        },
        Row {
            tool: "eslint",
            hook: "pre-commit-lint-js",
            staged: &[("a.ts", "const x = 1\n")],
            says: &["no eslint config"],
            ..ROW
        },
        Row {
            tool: "prettier",
            hook: "pre-commit-prettier",
            staged: &[("a.ts", "const  x   =1\n")],
            silent: true,
            ..ROW
        },
        Row {
            tool: "ruff",
            hook: "pre-commit-ruff",
            staged: &[("a.py", "import os\n")],
            silent: true,
            ..ROW
        },
        Row {
            tool: "pyright",
            hook: "pre-commit-pyright",
            staged: &[("a.py", "x: int = 'nope'\n")],
            silent: true,
            ..ROW
        },
        Row {
            tool: "kube-linter",
            hook: "pre-commit-kube-linter",
            staged: &[("kubernetes/app/deploy.yaml", DEPLOY)],
            silent: true,
            ..ROW
        },
        // No project marker: a kustomization, a Chart.yaml, a Cargo.toml,
        // a go.mod above the file.
        Row {
            tool: "kubeconform",
            hook: "pre-commit-kubeconform",
            staged: &[("kubernetes/loose/deploy.yaml", DEPLOY)],
            silent: true,
            ..ROW
        },
        Row {
            tool: "helm",
            hook: "pre-commit",
            staged: &[("config.yaml", "a: 1\n")],
            never_says: &["helm"],
            ..ROW
        },
        Row {
            tool: "cargo",
            hook: "pre-commit-cargo-fmt",
            staged: &[("scripts/loose.rs", "fn main() {}\n")],
            silent: true,
            never_says: &["Rust formatting"],
            ..ROW
        },
        Row {
            tool: "gofmt",
            hook: "pre-commit-gofmt",
            path: PathKind::Shims(&[("gofmt", NOT_RUN)]),
            staged: &[("scripts/loose.go", "package main\n")],
            silent: true,
            never_says: &["SHOULD-NOT-RUN"],
            ..ROW
        },
        Row {
            tool: "go",
            hook: "pre-commit-go-vet",
            path: PathKind::Shims(&[("go", NOT_RUN)]),
            staged: &[("scripts/loose.go", "package main\n")],
            silent: true,
            never_says: &["SHOULD-NOT-RUN"],
            ..ROW
        },
        // Opted in, but nothing staged the check reads.
        Row {
            tool: "node/yq",
            hook: "pre-commit-lint-json-yaml",
            staged: &[("notes.md", "no json here\n")],
            silent: true,
            ..ROW
        },
        Row {
            tool: "yamllint",
            hook: "pre-commit-yamllint",
            written: YAMLLINT,
            staged: &[("notes.md", "no yaml here\n")],
            silent: true,
            ..ROW
        },
        Row {
            tool: "eslint",
            hook: "pre-commit-lint-js",
            written: ESLINT,
            staged: &[("notes.md", "no js here\n")],
            silent: true,
            ..ROW
        },
        Row {
            tool: "prettier",
            hook: "pre-commit-prettier",
            written: PRETTIERRC,
            staged: &[("a.py", "x = 1\n")], // prettier reads Markdown

            silent: true,
            ..ROW
        },
        Row {
            tool: "ruff",
            hook: "pre-commit-ruff",
            written: RUFF,
            staged: &[("notes.md", "no python here\n")],
            silent: true,
            ..ROW
        },
        Row {
            tool: "pyright",
            hook: "pre-commit-pyright",
            written: PYRIGHT,
            staged: &[("notes.md", "no python here\n")],
            silent: true,
            ..ROW
        },
        Row {
            tool: "cargo",
            hook: "pre-commit-cargo-fmt",
            staged: &[("a.py", "x = 1\n"), ("README.md", "hi\n")],
            silent: true,
            ..ROW
        },
        Row {
            tool: "cargo",
            hook: "pre-commit-clippy",
            staged: &[("a.py", "x = 1\n"), ("README.md", "hi\n")],
            silent: true,
            ..ROW
        },
        Row {
            tool: "hadolint",
            hook: "pre-commit",
            staged: &[("notes.md", "no docker here\n")],
            never_says: &["hadolint"],
            ..ROW
        },
        // The documented limit, pinned so it stays a decision rather than a
        // surprise: scope name tokens are exact basenames, so a suffixed
        // Dockerfile is NOT matched (docs/checks.md). Bad enough to fail
        // hadolint if it were ever handed to it.
        Row {
            tool: "hadolint",
            hook: "pre-commit",
            staged: &[("Dockerfile.dev", "FROM alpine\nRUN cd /tmp\n")],
            never_says: &["hadolint"],
            ..ROW
        },
        Row {
            tool: "shellcheck",
            hook: "pre-commit",
            staged: &[("notes.md", "no shell here\n")],
            never_says: &["shellcheck"],
            ..ROW
        },
        // Outside the k8s prefixes, or not a kind the check reads.
        Row {
            tool: "argo",
            hook: "pre-commit-argo-lint",
            staged: &[("src/config.yaml", "kind: Workflow\n")],
            silent: true,
            ..ROW
        },
        Row {
            tool: "argo",
            hook: "pre-commit-argo-lint",
            staged: &[("kubernetes/app/deploy.yaml", DEPLOY)],
            silent: true,
            ..ROW
        },
        Row {
            tool: "kube-linter",
            hook: "pre-commit-kube-linter",
            staged: &[("src/app.yaml", DEPLOY)],
            silent: true,
            ..ROW
        },
        Row {
            tool: "kubeconform",
            hook: "pre-commit-kubeconform",
            staged: &[("src/app.yaml", DEPLOY)],
            silent: true,
            ..ROW
        },
        // A push touching nothing the suite tests: the suite is not a toll.
        Row {
            tool: "cargo",
            hook: "pre-push-cargo-test",
            base: &[
                ("Cargo.toml", CARGO_TOML),
                ("src/main.rs", "fn main() {}\n"),
            ],
            staged: &[("README.md", "docs\n")],
            ..ROW
        },
        Row {
            tool: "go",
            hook: "pre-push-go-test",
            path: PathKind::Shims(&[("go", NOT_RUN)]),
            base: &[("go.mod", GO_MOD)],
            staged: &[("README.md", "docs only\n")],
            never_says: &["SHOULD-NOT-RUN"],
            ..ROW
        },
        Row {
            tool: "pytest",
            hook: "pre-push-pytest",
            path: PathKind::Shims(&[("pytest", NOT_RUN)]),
            base: &[("conftest.py", "# setup\n")],
            staged: &[("README.md", "docs only\n")],
            never_says: &["SHOULD-NOT-RUN"],
            ..ROW
        },
        // `"test": "exit 1"`: if the gate ran, the push would fail.
        Row {
            tool: "npm",
            hook: "pre-push-run-tests-js",
            base: &[(
                "package.json",
                r#"{"name":"t","scripts":{"test":"exit 1"}}"#,
            )],
            staged: &[("README.md", "docs\n")],
            ..ROW
        },
    ];
    table(&rows);
}

// ---- opted in: judged on the content ----------------------------------------

/// Every check, opted in, with a clean input and a broken one. The broken
/// half is what proves the check reads the file: one that fails on its own
/// invocation (a wrong `--`, a bad working directory, a binary it could not
/// resolve) passes nothing, and one that judges nothing passes both.
fn pairs() -> Vec<Pair> {
    let node = Row {
        tool: "node",
        hook: "pre-commit-lint-json-yaml",
        gate: Gate::Tools(&["node"]),
        ..ROW
    };
    let yq = Row {
        tool: "yq",
        gate: Gate::Tools(&["yq"]),
        ..node
    };
    let yamllint = Row {
        tool: "yamllint",
        hook: "pre-commit-yamllint",
        gate: Gate::Tools(&["yamllint"]),
        written: YAMLLINT,
        ..ROW
    };
    let eslint = Row {
        tool: "eslint",
        hook: "pre-commit-lint-js",
        gate: Gate::Tools(&["eslint"]),
        written: ESLINT,
        ..ROW
    };
    let prettier = Row {
        tool: "prettier",
        hook: "pre-commit-prettier",
        gate: Gate::Tools(&["prettier"]),
        written: PRETTIERRC,
        ..ROW
    };
    let ruff = Row {
        tool: "ruff",
        hook: "pre-commit-ruff",
        gate: Gate::AnyOf(&["ruff", "uvx"]),
        written: RUFF,
        ..ROW
    };
    let pyright = Row {
        tool: "pyright",
        hook: "pre-commit-pyright",
        gate: Gate::Tools(&["pyright"]),
        written: PYRIGHT,
        ..ROW
    };
    let fmt = Row {
        tool: "cargo",
        hook: "pre-commit-cargo-fmt",
        gate: Gate::Tools(&["cargo"]),
        ..ROW
    };
    let clippy = Row {
        hook: "pre-commit-clippy",
        ..fmt
    };
    let cargo_test = Row {
        hook: "pre-push-cargo-test",
        base: &[
            ("Cargo.toml", CARGO_TOML),
            (
                "src/main.rs",
                "fn main() {}\n\n#[test]\nfn passes() {\n    assert_eq!(1, 1);\n}\n",
            ),
        ],
        ..fmt
    };
    let gofmt = Row {
        tool: "gofmt",
        hook: "pre-commit-gofmt",
        staged: GO_MODULE,
        ..ROW
    };
    let vet = Row {
        tool: "go",
        hook: "pre-commit-go-vet",
        staged: GO_MODULE,
        ..ROW
    };
    let go_test = Row {
        tool: "go",
        hook: "pre-push-go-test",
        base: &[("go.mod", GO_MOD)],
        staged: &[("main.go", "package main\n")],
        ..ROW
    };
    let pytest = Row {
        tool: "pytest",
        hook: "pre-push-pytest",
        base: &[("conftest.py", "# pytest setup\n")],
        staged: &[("test_x.py", "def test_x():\n    assert True\n")],
        ..ROW
    };
    let npm = Row {
        tool: "npm",
        hook: "pre-push-run-tests-js",
        gate: Gate::Tools(&["npm"]),
        staged: &[("src/a.js", "const x = 1;\n")],
        ..ROW
    };
    let kube_linter = Row {
        tool: "kube-linter",
        hook: "pre-commit-kube-linter",
        gate: Gate::Tools(&["kube-linter"]),
        ..ROW
    };
    let kubeconform = Row {
        tool: "kubeconform",
        hook: "pre-commit-kubeconform",
        gate: Gate::Tools(&["kubeconform", "kustomize"]),
        ..ROW
    };
    let helm = Row {
        tool: "helm",
        hook: "pre-commit",
        gate: Gate::Tools(&["helm"]),
        ..ROW
    };
    let hadolint = Row {
        tool: "hadolint",
        hook: "pre-commit",
        gate: Gate::Tools(&["hadolint"]),
        ..ROW
    };
    let shellcheck = Row {
        tool: "shellcheck",
        hook: "pre-commit",
        gate: Gate::Tools(&["shellcheck"]),
        ..ROW
    };
    vec![
        (
            Row {
                staged: &[("ok.json", "{\"a\": 1}\n")],
                ..node
            },
            Row {
                staged: &[("bad.json", "{\"a\": 1,,}\n")],
                passes: false,
                ..node
            },
        ),
        (
            Row {
                staged: &[("ok.yaml", "a:\n  b: 1\n")],
                ..yq
            },
            Row {
                staged: &[("bad.yaml", "a:\n\tb: 1\n")], // tab indentation
                passes: false,
                ..yq
            },
        ),
        // `.yml` is the OTHER spelling, and it was never linted at all: the
        // registry declared `[".json", ".yaml", ".yml"]` while the check
        // asked `staged_files` for `[".yaml"]`, so a broken `x.yml` passed
        // with no output whatsoever. Both lists now come from
        // `lint_json_yaml::EXTS`.
        (
            Row {
                staged: &[("ok.yml", "a:\n  b: 1\n")],
                ..yq
            },
            Row {
                staged: &[("bad.yml", "a:\n\tb: 1\n")],
                passes: false,
                ..yq
            },
        ),
        (
            Row {
                staged: &[("a.yaml", "a: 1\n")],
                ..yamllint
            },
            Row {
                staged: &[("a.yaml", "a: 1   \n")], // trailing spaces
                passes: false,
                ..yamllint
            },
        ),
        (
            Row {
                staged: &[("a.js", "const x = 1;\nx;\n")],
                says: &["ESLint passed"],
                ..eslint
            },
            Row {
                staged: &[("a.js", "undefinedFunction();\n")],
                passes: false,
                ..eslint
            },
        ),
        (
            Row {
                staged: &[("b.ts", "const x = 1;\n")],
                ..prettier
            },
            Row {
                staged: &[("a.ts", "const  x   =1\n")],
                passes: false,
                ..prettier
            },
        ),
        (
            Row {
                staged: &[("a.py", "x = 1\n")],
                says: &["Ruff passed"],
                ..ruff
            },
            Row {
                staged: &[("a.py", "import os\n")], // unused import
                passes: false,
                ..ruff
            },
        ),
        // `pre-commit-pyright` is `Severity::Block`, and for a long time its
        // only test said it did nothing. `x: int = 'nope'` fails on any
        // pyright version without imports or inference across files. When
        // the broken row was first run the check passed `--` before the
        // files, pyright read it as a path and blocked with `File or
        // directory ".../--" does not exist`: "it blocked" is too weak for a
        // blocking check, so the row also asks that it blocked over the code.
        // The clean row is the one that caught that bug, on its first run.
        (
            Row {
                staged: &[("a.py", "x: int = 1\n")],
                says: &["pyright passed"],
                ..pyright
            },
            Row {
                staged: &[("a.py", "x: int = 'nope'\n")],
                passes: false,
                says: &["a.py"],
                never_says: &["does not exist"],
                ..pyright
            },
        ),
        (
            Row {
                staged: &[
                    ("Cargo.toml", CARGO_TOML),
                    ("src/main.rs", "fn main() {\n    let _x = 1;\n}\n"),
                ],
                says: &["Rust formatting is clean"],
                ..fmt
            },
            Row {
                staged: &[("Cargo.toml", CARGO_TOML), ("src/main.rs", UNFORMATTED_RS)],
                passes: false,
                ..fmt
            },
        ),
        // `-D warnings` is the point: an ordinary rustc warning has to fail
        // the commit, not merely print.
        (
            Row {
                staged: &[
                    ("Cargo.toml", CARGO_TOML),
                    ("src/main.rs", "fn main() {\n    println!(\"ok\");\n}\n"),
                ],
                says: &["Clippy passed"],
                ..clippy
            },
            Row {
                staged: &[
                    ("Cargo.toml", CARGO_TOML),
                    ("src/main.rs", "fn main() {\n    let unused = 1;\n}\n"),
                ],
                passes: false,
                ..clippy
            },
        ),
        (
            Row {
                staged: &[(
                    "src/main.rs",
                    "fn main() {}\n\n#[test]\nfn passes() {\n    assert_eq!(2, 2);\n}\n",
                )],
                says: &["Rust tests passed"],
                ..cargo_test
            },
            Row {
                staged: &[(
                    "src/main.rs",
                    "fn main() {}\n\n#[test]\nfn fails() {\n    assert_eq!(1, 2);\n}\n",
                )],
                passes: false,
                ..cargo_test
            },
        ),
        // gofmt exits 0 either way: the LISTING decides.
        (
            Row {
                path: PathKind::Shims(&[("gofmt", "exit 0")]),
                says: &["Go formatting is clean"],
                ..gofmt
            },
            Row {
                path: PathKind::Shims(&[("gofmt", "echo main.go\nexit 0")]),
                passes: false,
                says: &["Unformatted Go", "main.go"],
                ..gofmt
            },
        ),
        (
            Row {
                path: PathKind::Shims(&[("go", "exit 0")]),
                says: &["go vet passed"],
                ..vet
            },
            Row {
                path: PathKind::Shims(&[(
                    "go",
                    "echo 'main.go:1:1: unreachable code' >&2\nexit 1",
                )]),
                passes: false,
                says: &["go vet"],
                ..vet
            },
        ),
        (
            Row {
                path: PathKind::Shims(&[("go", "exit 0")]),
                says: &["Go tests passed"],
                ..go_test
            },
            Row {
                path: PathKind::Shims(&[("go", "echo '--- FAIL: TestX' \nexit 1")]),
                passes: false,
                says: &["Go tests failed"],
                ..go_test
            },
        ),
        (
            Row {
                path: PathKind::Shims(&[("pytest", "exit 0")]),
                says: &["Python tests passed"],
                ..pytest
            },
            Row {
                path: PathKind::Shims(&[("pytest", "echo 'FAILED test_x.py::test_x'\nexit 1")]),
                passes: false,
                says: &["Python tests failed"],
                ..pytest
            },
        ),
        (
            Row {
                base: &[(
                    "package.json",
                    r#"{"name":"t","scripts":{"test":"exit 0"}}"#,
                )],
                ..npm
            },
            Row {
                base: &[(
                    "package.json",
                    r#"{"name":"t","scripts":{"test":"exit 1"}}"#,
                )],
                passes: false,
                ..npm
            },
        ),
        (
            Row {
                staged: &[
                    (".kube-linter.yaml", LATEST_TAG_ONLY),
                    ("kubernetes/deploy.yaml", CLEAN_DEPLOY),
                ],
                says: &["kube-linter passed"],
                ..kube_linter
            },
            Row {
                staged: &[
                    (".kube-linter.yaml", LATEST_TAG_ONLY),
                    ("kubernetes/deploy.yaml", LATEST_DEPLOY),
                ],
                passes: false,
                says: &["kube-linter"],
                ..kube_linter
            },
        ),
        (
            Row {
                staged: &[KUSTOMIZATION, ("kubernetes/deploy.yaml", CLEAN_DEPLOY)],
                says: &["kubeconform passed"],
                ..kubeconform
            },
            Row {
                staged: &[
                    KUSTOMIZATION,
                    ("kubernetes/deploy.yaml", STRING_REPLICAS_DEPLOY),
                ],
                passes: false,
                says: &["kubeconform failed"],
                ..kubeconform
            },
        ),
        // A chart without `name:` is one helm refuses, and the refusal says
        // WHICH chart.
        (
            Row {
                staged: CHART,
                says: &["helm lint passed"],
                ..helm
            },
            Row {
                staged: &[
                    ("charts/bad/Chart.yaml", "apiVersion: v2\nversion: 0.1.0\n"),
                    ("charts/bad/values.yaml", "x: 1\n"),
                ],
                passes: false,
                says: &["charts/bad"],
                ..helm
            },
        ),
        // `FROM alpine` unpinned is DL3006 and `RUN cd` is DL3003; hadolint's
        // default threshold is `info`, so either fails. The clean one pins
        // the tag and does nothing else.
        (
            Row {
                staged: &[("Dockerfile", "FROM alpine:3.19\nRUN echo hi\n")],
                ..hadolint
            },
            Row {
                staged: &[("Dockerfile", "FROM alpine\nRUN cd /tmp && echo hi\n")],
                passes: false,
                says: &["hadolint"],
                ..hadolint
            },
        ),
        // SC2086: an unquoted `$1`, caught by shellcheck's default rules.
        (
            Row {
                staged: &[("good.sh", "#!/bin/sh\nrm -rf \"$1\"\n")],
                ..shellcheck
            },
            Row {
                staged: &[("bad.sh", "#!/bin/sh\nrm -rf $1\n")],
                passes: false,
                says: &["shellcheck"],
                ..shellcheck
            },
        ),
    ]
}

#[test]
fn opted_in_a_broken_input_is_refused() {
    table(pairs().iter().map(|(_, broken)| broken));
}

#[test]
fn opted_in_a_clean_input_passes() {
    table(pairs().iter().map(|(clean, _)| clean));
}

// ---- a missing tool ---------------------------------------------------------

/// A missing tool warns and never blocks: `Unavailable`, not `Failed`. A
/// check that could not run has found nothing, and blocking a commit over
/// amont's own plumbing is the failure mode `staged_files` documents.
///
/// The `Bare` rows run with a PATH holding only git, so the tool is absent
/// whatever this machine has. The `Absent` rows run only where the tool
/// really is absent, through the dispatcher a commit runs. argo is never
/// installed in CI for that reason: its row is the only argo case, so
/// installing argo would disable it, not enable it.
///
/// `pre-push-run-tests-js` has no row: a missing npm fails the push (the
/// gate cannot tell it from a failing script), which is not this promise.
#[test]
fn a_missing_tool_warns_and_never_blocks() {
    let bare = Row {
        path: PathKind::Bare,
        ..ROW
    };
    let rows = [
        Row {
            tool: "argo",
            hook: "pre-commit-argo-lint",
            gate: Gate::Absent("argo"),
            staged: &[("kubernetes/wf/w.yaml", "kind: Workflow\n")],
            says: &["install"],
            ..ROW
        },
        Row {
            tool: "hadolint",
            hook: "pre-commit",
            gate: Gate::Absent("hadolint"),
            staged: &[("Dockerfile", "FROM alpine\nRUN cd /tmp\n")],
            says: &["not installed"],
            ..ROW
        },
        Row {
            tool: "shellcheck",
            hook: "pre-commit",
            gate: Gate::Absent("shellcheck"),
            staged: &[("bad.sh", "#!/bin/sh\nrm -rf $1\n")],
            says: &["not installed"],
            ..ROW
        },
        Row {
            tool: "gofmt",
            hook: "pre-commit-gofmt",
            staged: GO_MODULE,
            says: &["gofmt is not installed"],
            ..bare
        },
        Row {
            tool: "go",
            hook: "pre-commit-go-vet",
            staged: GO_MODULE,
            says: &["go toolchain is not installed"],
            ..bare
        },
        Row {
            tool: "go",
            hook: "pre-push-go-test",
            base: &[("go.mod", GO_MOD)],
            staged: &[("main.go", "package main\n")],
            says: &["go toolchain is not installed"],
            ..bare
        },
        Row {
            tool: "node",
            hook: "pre-commit-lint-json-yaml",
            staged: &[("ok.json", "{\"a\": 1}\n")],
            says: &["JSON files detected"],
            ..bare
        },
        Row {
            tool: "yq",
            hook: "pre-commit-lint-json-yaml",
            staged: &[("ok.yaml", "a:\n  b: 1\n")],
            says: &["YAML files detected"],
            ..bare
        },
        Row {
            tool: "yamllint",
            hook: "pre-commit-yamllint",
            written: YAMLLINT,
            staged: &[("a.yaml", "a: 1\n")],
            says: &["yamllint but it is not installed"],
            ..bare
        },
        Row {
            tool: "eslint",
            hook: "pre-commit-lint-js",
            written: ESLINT,
            staged: &[("a.js", "const x = 1;\nx;\n")],
            says: &["no eslint binary"],
            ..bare
        },
        Row {
            tool: "prettier",
            hook: "pre-commit-prettier",
            written: PRETTIERRC,
            staged: &[("a.ts", "const x = 1;\n")],
            says: &["no prettier binary"],
            ..bare
        },
        Row {
            tool: "ruff",
            hook: "pre-commit-ruff",
            written: RUFF,
            staged: &[("a.py", "x = 1\n")],
            says: &["no ruff/uvx binary"],
            ..bare
        },
        Row {
            tool: "pyright",
            hook: "pre-commit-pyright",
            written: PYRIGHT,
            staged: &[("a.py", "x: int = 1\n")],
            says: &["no pyright binary"],
            ..bare
        },
        Row {
            tool: "pytest",
            hook: "pre-push-pytest",
            base: &[("conftest.py", "# pytest setup\n")],
            staged: &[("test_x.py", "def test_x():\n    assert True\n")],
            says: &["pytest is not installed"],
            ..bare
        },
        Row {
            tool: "cargo",
            hook: "pre-commit-cargo-fmt",
            staged: &[("Cargo.toml", CARGO_TOML), ("src/main.rs", UNFORMATTED_RS)],
            says: &["rustfmt is not installed"],
            ..bare
        },
        Row {
            tool: "cargo",
            hook: "pre-commit-clippy",
            staged: &[("Cargo.toml", CARGO_TOML), ("src/main.rs", UNFORMATTED_RS)],
            says: &["clippy is not installed"],
            ..bare
        },
        Row {
            tool: "cargo",
            hook: "pre-push-cargo-test",
            base: &[("Cargo.toml", CARGO_TOML)],
            staged: &[("src/main.rs", "fn main() {}\n")],
            says: &["cargo is not installed"],
            ..bare
        },
        Row {
            tool: "kube-linter",
            hook: "pre-commit-kube-linter",
            staged: &[
                (".kube-linter.yaml", LATEST_TAG_ONLY),
                ("kubernetes/deploy.yaml", CLEAN_DEPLOY),
            ],
            says: &["kube-linter but it is not installed"],
            ..bare
        },
        Row {
            tool: "kubeconform",
            hook: "pre-commit-kubeconform",
            staged: &[KUSTOMIZATION, ("kubernetes/deploy.yaml", CLEAN_DEPLOY)],
            says: &["Skipping kubeconform"],
            ..bare
        },
        Row {
            tool: "helm",
            hook: "pre-commit-helm-lint",
            staged: CHART,
            says: &["helm is not installed"],
            ..bare
        },
        Row {
            tool: "hadolint",
            hook: "pre-commit-hadolint",
            staged: &[("Dockerfile", "FROM alpine\nRUN cd /tmp\n")],
            says: &["hadolint is not installed"],
            ..bare
        },
        Row {
            tool: "shellcheck",
            hook: "pre-commit-shellcheck",
            staged: &[("bad.sh", "#!/bin/sh\nrm -rf $1\n")],
            says: &["shellcheck is not installed"],
            ..bare
        },
    ];
    table(&rows);
}

// ---- fixing -----------------------------------------------------------------

/// With fixing OFF a refused file comes back byte for byte as it was written.
#[test]
fn with_fixing_off_a_refused_file_is_left_as_written() {
    let rows = [
        Row {
            tool: "cargo",
            hook: "pre-commit-cargo-fmt",
            gate: Gate::Tools(&["cargo"]),
            staged: &[("Cargo.toml", CARGO_TOML), ("src/main.rs", UNFORMATTED_RS)],
            passes: false,
            after: &[After::Disk("src/main.rs", UNFORMATTED_RS)],
            ..ROW
        },
        Row {
            tool: "ruff",
            hook: "pre-commit-ruff",
            gate: Gate::AnyOf(&["ruff", "uvx"]),
            written: RUFF,
            staged: &[("a.py", "import os\n")],
            passes: false,
            after: &[After::Disk("a.py", "import os\n")],
            ..ROW
        },
        Row {
            tool: "prettier",
            hook: "pre-commit-prettier",
            gate: Gate::Tools(&["prettier"]),
            written: PRETTIERRC,
            staged: &[("a.ts", "const  x   =1\n")],
            passes: false,
            after: &[After::Disk("a.ts", "const  x   =1\n")],
            ..ROW
        },
        // The shim WOULD rewrite on `-w`: only the check's choice not to ask
        // keeps the file as it was.
        Row {
            tool: "gofmt",
            hook: "pre-commit-gofmt",
            path: PathKind::Shims(&[("gofmt", GOFMT_FIXES)]),
            staged: GO_MODULE,
            passes: false,
            after: &[After::Disk("main.go", "package main\n")],
            ..ROW
        },
    ];
    table(&rows);
}

/// With `amont.fix true` a fixable finding is repaired, the repair is
/// re-staged, and the commit goes through. The registry declared
/// `Fix::Rewrite` for cargo-fmt, and `amont list --json` reported
/// `"fix":"rewrite"` (which `agents_md` tells agents to trust), long before
/// any fixing code existed: an agent could set `amont.fix` and wait forever.
/// ruff's partial fix is its own case in `linters`.
#[test]
fn with_fixing_on_the_repair_reaches_the_index() {
    let rows = [
        Row {
            tool: "cargo",
            hook: "pre-commit-cargo-fmt",
            gate: Gate::Tools(&["cargo"]),
            config: FIX,
            staged: &[("Cargo.toml", CARGO_TOML), ("src/main.rs", UNFORMATTED_RS)],
            says: &["reformatted and re-staged"],
            after: &[After::Index(
                "src/main.rs",
                "fn main() {\n    let _x = 1;\n}\n",
            )],
            ..ROW
        },
        Row {
            tool: "gofmt",
            hook: "pre-commit-gofmt",
            path: PathKind::Shims(&[("gofmt", GOFMT_FIXES)]),
            config: FIX,
            staged: GO_MODULE,
            says: &["Go reformatted and re-staged"],
            after: &[After::Index("main.go", "package main\n\nfunc main() {}\n")],
            ..ROW
        },
        Row {
            tool: "prettier",
            hook: "pre-commit-prettier",
            gate: Gate::Tools(&["prettier"]),
            config: FIX,
            written: PRETTIERRC,
            staged: &[("a.ts", "const  x   =1\n")],
            after: &[After::Index("a.ts", "const x = 1;\n")],
            ..ROW
        },
    ];
    table(&rows);
}

// ---- a manifest alone -------------------------------------------------------

/// A dependency bump changes what compiles without touching a single source
/// file, which is exactly when the check is worth running.
#[test]
fn a_manifest_only_change_still_runs_the_check() {
    let rows = [
        Row {
            tool: "cargo",
            hook: "pre-commit-clippy",
            gate: Gate::Tools(&["cargo"]),
            base: &[
                ("Cargo.toml", CARGO_TOML),
                ("src/main.rs", "fn main() {\n    println!(\"ok\");\n}\n"),
            ],
            staged: &[(
                "Cargo.toml",
                "[package]\nname = \"t\"\nversion = \"0.2.0\"\nedition = \"2021\"\n\n[dependencies]\n",
            )],
            says: &["Clippy passed"],
            ..ROW
        },
        Row {
            tool: "go",
            hook: "pre-commit-go-vet",
            path: PathKind::Shims(&[("go", "echo VET-RAN >&2\nexit 0")]),
            base: &[("main.go", "package main\n")],
            staged: &[(
                "go.mod",
                "module example.com/x\n\nrequire example.com/dep v1.2.3\n",
            )],
            says: &["VET-RAN"],
            ..ROW
        },
    ];
    table(&rows);
}
