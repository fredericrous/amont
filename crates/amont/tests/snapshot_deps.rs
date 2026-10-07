//! amont prepares a snapshot's JavaScript dependencies itself, and carries
//! the untracked files a suite needs — see `snapshot_prep.rs`.
//!
//! Two tiers. The logic tier drives STUB `npm`/`pnpm` scripts first on PATH:
//! each logs its argv, and control files decide its exit code, its output
//! and whether it blocks — so every branch is reachable without a network or
//! a real registry. The last tests use the REAL managers on an offline
//! fixture (a local tarball dependency), skipped when a manager is missing.
#![cfg(unix)]

mod common;
use common::{missing, Repo};

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------- helpers

fn stubs_dir(r: &Repo) -> PathBuf {
    r.dir.join(".stubs")
}

fn ctl(r: &Repo) -> PathBuf {
    stubs_dir(r).join("ctl")
}

/// Set a control file: `<op>.exit`, `<op>.out`, `<op>.block`.
fn set(r: &Repo, name: &str, body: &str) {
    std::fs::write(ctl(r).join(name), body).unwrap();
}

fn stub_log(r: &Repo) -> String {
    std::fs::read_to_string(ctl(r).join("log")).unwrap_or_default()
}

/// The stub both managers share. The OPERATION is what the control files
/// are keyed by: `install`, `verify` (the clone check), `lockcheck` (npm's
/// manifest check), `members`.
const STUB: &str = r#"#!/bin/sh
ctl="__CTL__"
tool=$(basename "$0")
echo "$tool $* @ $(pwd)" >> "$ctl/log"
op=other
case "$tool $*" in
  "npm ci"*) op=install ;;
  "npm ls"*--package-lock-only*) op=lockcheck ;;
  "npm ls"*) op=verify ;;
  "npm query"*) op=members ;;
  "pnpm ls"*) op=members ;;
  "pnpm install"*--offline*) op=verify ;;
  "pnpm install"*) op=install ;;
esac
if [ -e "$ctl/$op.block" ]; then
  echo $$ > "$ctl/$op.pid"
  n=0
  while [ ! -e "$ctl/release" ] && [ $n -lt 600 ]; do sleep 0.1; n=$((n+1)); done
fi
if [ "$op" = install ]; then
  if find . -path '*node_modules*' -name marker-source 2>/dev/null | grep -q .; then
    echo "STALE $(pwd)" >> "$ctl/log"
  fi
  [ -e .env ] && echo "ENV-VISIBLE $(pwd)" >> "$ctl/log"
  mkdir -p node_modules && echo x > node_modules/marker-installed
fi
if [ "$op" = install ] && [ -e "$ctl/init-bin" ]; then
  # What a real package's `prepare: amont init` does: run an amont that
  # lives UNDER the snapshot, so its current_exe() is a snapshot path.
  mkdir -p node_modules/.bin && cp "$(cat "$ctl/init-bin")" node_modules/.bin/amont
  echo "SNAPSHOT-ENV=${AMONT_SNAPSHOT:-unset}" >> "$ctl/log"
  node_modules/.bin/amont init >> "$ctl/init.out" 2>&1
  echo "INIT-EXIT=$?" >> "$ctl/log"
fi
if [ "$op" = verify ] && [ -e node_modules/.pnpm-workspace-state-v1.json ]; then
  echo "STATE-FILE-KEPT" >> "$ctl/log"
fi
if [ "$op" = verify ] && [ -e packages/m/node_modules/.marker/marker-source ]; then
  echo "MEMBER-CLONED" >> "$ctl/log"
fi
if [ -e "$ctl/$op.out" ]; then
  if [ "$op" = members ] && [ "$tool" = pnpm ]; then
    pwd
    while read -r rel; do [ -n "$rel" ] && echo "$(pwd)/$rel"; done < "$ctl/$op.out"
  else
    cat "$ctl/$op.out"
  fi
fi
exit $(cat "$ctl/$op.exit" 2>/dev/null || echo 0)
"#;

fn install_stubs(r: &Repo) {
    let bin = stubs_dir(r).join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(ctl(r)).unwrap();
    let body = STUB.replace("__CTL__", &ctl(r).display().to_string());
    for tool in ["npm", "pnpm"] {
        let p = bin.join(tool);
        std::fs::write(&p, &body).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn path_with(prefix: Option<&Path>) -> std::ffi::OsString {
    let base = std::env::var_os("PATH").unwrap_or_default();
    match prefix {
        Some(p) => {
            let mut v = p.as_os_str().to_os_string();
            v.push(":");
            v.push(&base);
            v
        }
        None => base,
    }
}

/// `amont <args>` in the repo, with the stubs first on PATH when `stubbed`.
fn amont(r: &Repo, args: &[&str], stubbed: bool) -> (i32, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_amont"));
    cmd.args(args).current_dir(&r.dir).stdin(Stdio::null());
    Repo::strip_git_env_impl(&mut cmd);
    cmd.env("GIT_CONFIG_GLOBAL", r.dir.join("fake-gitconfig"));
    let bin = stubs_dir(r).join("bin");
    cmd.env("PATH", path_with(stubbed.then_some(bin.as_path())));
    let out = cmd.output().expect("run amont");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn rehearse(r: &Repo, flags: &[&str]) -> (i32, String) {
    let mut args = vec!["rehearse"];
    args.extend_from_slice(flags);
    amont(r, &args, true)
}

fn push_out(r: &Repo, from: &str, to: &str) -> (i32, String) {
    let line = format!("refs/heads/feat/x {to} refs/heads/feat/x {from}\n");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_amont"));
    cmd.arg("--hooks-dir")
        .arg(r.dir.join(".git/hooks"))
        .arg("pre-push")
        .current_dir(&r.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    Repo::strip_git_env_impl(&mut cmd);
    let bin = stubs_dir(r).join("bin");
    cmd.env("PATH", path_with(Some(&bin)));
    let mut child = cmd.spawn().expect("spawn");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(line.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("wait");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn head(r: &Repo) -> String {
    String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string()
}

fn note(r: &Repo, rev: &str) -> String {
    String::from_utf8_lossy(&r.git(&["notes", "--ref", "amont-gate", "show", rev]).stdout)
        .to_string()
}

fn state(r: &Repo) -> String {
    std::fs::read_to_string(r.dir.join(".git/amont-rehearsal")).unwrap_or_default()
}

fn runs(r: &Repo) -> usize {
    std::fs::read_to_string(r.dir.join("gate.log"))
        .map(|s| s.len())
        .unwrap_or(0)
}

fn worktrees(r: &Repo) -> usize {
    String::from_utf8_lossy(&r.git(&["worktree", "list", "--porcelain"]).stdout)
        .lines()
        .filter(|l| l.starts_with("worktree "))
        .count()
}

fn wait_for(what: &str, mut pred: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !pred() {
        assert!(Instant::now() < deadline, "waited 30s for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn trust_and_install(r: &Repo) {
    for verb in ["trust", "init"] {
        let out = Command::new(env!("CARGO_BIN_EXE_amont"))
            .arg(verb)
            .current_dir(&r.dir)
            .stdin(Stdio::null())
            .output()
            .expect("amont");
        assert!(
            out.status.success(),
            "amont {verb}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// A repo on `feat/x` with one pre-push gate that FAILS without a
/// `node_modules` in its cwd, `origin/main` at the base, and one commit to
/// push. `layout` stages the package files into the base commit.
fn js_repo(layout: impl FnOnce(&Repo)) -> (Repo, String) {
    let r = Repo::new();
    install_stubs(&r);
    let log = r.dir.join("gate.log").display().to_string();
    r.stage(
        "gate.js",
        &format!(
            "const fs=require('fs');\nfs.appendFileSync({log:?},'x');\n\
             if (!fs.existsSync('node_modules')) process.exit(1);\n"
        ),
    );
    r.stage(
        ".gitignore",
        "node_modules/\n.env\n.stubs/\ngate.log\nfake-gitconfig\n",
    );
    layout(&r);
    r.commit("chore: base");
    let base = head(&r);
    r.stage(
        "amont.conf",
        "pre-push    suite  *.txt  block  node gate.js\n",
    );
    r.commit("chore: the gate");
    trust_and_install(&r);
    r.git(&["checkout", "-q", "--no-track", "-b", "feat/x"]);
    r.git(&["update-ref", "refs/remotes/origin/main", &base]);
    r.stage("a.txt", "hello\n");
    r.commit("feat: something to push");
    (r, base)
}

fn npm_root(r: &Repo) {
    r.stage(
        "package.json",
        "{\"name\":\"root\",\"version\":\"1.0.0\"}\n",
    );
    r.stage("package-lock.json", "lock v1\n");
}

/// The working tree's own install, as a developer's checkout would have it.
fn installed(r: &Repo, dir: &str) {
    let nm = r.dir.join(dir).join("node_modules");
    std::fs::create_dir_all(&nm).unwrap();
    std::fs::write(nm.join("marker-source"), "from the working tree\n").unwrap();
    std::fs::write(nm.join(".package-lock.json"), "hidden\n").unwrap();
}

fn logged(r: &Repo, needle: &str) -> bool {
    stub_log(r).lines().any(|l| l.contains(needle))
}

// ------------------------------------------------------------ install mode

/// The default is what CI does: `npm ci` in the snapshot, never a clone.
#[test]
fn the_default_installs_in_the_snapshot() {
    if missing("node") {
        return;
    }
    let (r, _) = js_repo(npm_root);
    installed(&r, "");
    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 0, "{out}\n{}", stub_log(&r));
    assert!(out.contains("npm ci"), "{out}");
    assert!(
        logged(&r, "npm ci --no-audit --no-fund --prefer-offline"),
        "{}",
        stub_log(&r)
    );
    assert!(
        !logged(&r, "STALE"),
        "no clone under install: {}",
        stub_log(&r)
    );
    assert!(!logged(&r, "npm ls"), "install mode asks nothing else");
    assert_eq!(runs(&r), 1);
    assert!(note(&r, "HEAD").contains("pre-push-suite"));
    assert!(
        !r.dir.join("node_modules/marker-installed").exists(),
        "the install happened in the snapshot, not the working tree"
    );
}

/// `off` prepares nothing — today's behaviour: the gate cannot start.
#[test]
fn off_prepares_nothing() {
    if missing("node") {
        return;
    }
    let (r, _) = js_repo(npm_root);
    r.git(&["config", "amont.snapshotDeps", "off"]);
    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 1, "{out}");
    assert!(stub_log(&r).is_empty(), "{}", stub_log(&r));
}

/// A `snapshotPrepare` command owns the dependencies: the built-in install
/// stands down; carry still runs, first.
#[test]
fn a_prepare_command_owns_the_dependencies() {
    if missing("node") {
        return;
    }
    let (r, _) = js_repo(npm_root);
    r.write(".env", "SECRET=1\n");
    r.git(&["config", "amont.snapshotCarry", ".env"]);
    r.git(&[
        "config",
        "amont.snapshotPrepare",
        "test -e .env && mkdir node_modules",
    ]);
    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 0, "{out}");
    assert!(!logged(&r, "npm"), "{}", stub_log(&r));
    assert!(out.contains("carried .env"), "{out}");
}

// ------------------------------------------------- the shared hooks dir

/// Every amont shim's `BAKED=` line, by hook name.
fn baked(r: &Repo) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = std::fs::read_dir(r.dir.join(".git/hooks"))
        .unwrap()
        .filter_map(|e| {
            let e = e.ok()?;
            let text = std::fs::read_to_string(e.path()).ok()?;
            let line = text.lines().find(|l| l.starts_with("BAKED="))?.to_string();
            Some((e.file_name().to_string_lossy().into_owned(), line))
        })
        .collect();
    v.sort();
    v
}

const SENTINEL: &str = "/sentinel/A/amont";

/// Re-point every shim at a path no build will ever have, so a re-bake —
/// by ANY binary, this test's included — shows as a changed line.
fn seed_sentinel(r: &Repo) {
    for entry in std::fs::read_dir(r.dir.join(".git/hooks")).unwrap() {
        let p = entry.unwrap().path();
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        if text.contains(env!("CARGO_BIN_EXE_amont")) {
            std::fs::write(&p, text.replace(env!("CARGO_BIN_EXE_amont"), SENTINEL)).unwrap();
        }
    }
    let seeded = baked(r);
    assert!(!seeded.is_empty(), "no amont shims to seed");
    assert!(
        seeded.iter().all(|(_, l)| l.contains(SENTINEL)),
        "{seeded:?}"
    );
}

/// The bug this guards: the snapshot is a linked worktree, so it shares
/// `.git/hooks`, and its install runs `prepare` — an `amont init` whose
/// binary lives UNDER the snapshot. That baked every shim to a temp path
/// deleted minutes later. The snapshot is marked; `init` stands down.
#[test]
fn a_snapshot_install_never_rebakes_the_shared_hooks() {
    if missing("node") {
        return;
    }
    let (r, _) = js_repo(npm_root);
    seed_sentinel(&r);
    let before = baked(&r);
    set(&r, "init-bin", env!("CARGO_BIN_EXE_amont"));

    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 0, "{out}\n{}", stub_log(&r));
    assert!(logged(&r, "INIT-EXIT=0"), "{}", stub_log(&r));
    assert!(logged(&r, "SNAPSHOT-ENV=1"), "{}", stub_log(&r));
    let init_out = std::fs::read_to_string(ctl(&r).join("init.out")).unwrap_or_default();
    assert!(
        init_out.contains("inside an amont push snapshot"),
        "{init_out}"
    );
    let after = baked(&r);
    assert!(
        after.iter().all(|(_, l)| !l.contains("amont-push-")),
        "a shim was baked into the snapshot: {after:?}"
    );
    assert_eq!(before, after, "the shared hooks changed");
}

/// `snapshotPrepare` runs inside the snapshot too, and gets
/// `AMONT_SNAPSHOT=1` to know it. The guard does not depend on that
/// variable: the second `init` runs WITHOUT it — the shape of an install a
/// gate runs itself, whose environment carries no snapshot variable — and
/// still stands down, because the marker is in the snapshot's own git dir.
#[test]
fn a_prepare_command_in_the_snapshot_never_rebakes_the_shared_hooks() {
    if missing("node") {
        return;
    }
    let (r, _) = js_repo(npm_root);
    seed_sentinel(&r);
    let before = baked(&r);
    let log = ctl(&r).join("prepare.log").display().to_string();
    let bin = env!("CARGO_BIN_EXE_amont");
    r.git(&[
        "config",
        "amont.snapshotPrepare",
        &format!(
            "mkdir -p node_modules/.bin && cp {bin:?} node_modules/.bin/amont && \
             echo \"env=${{AMONT_SNAPSHOT:-unset}}\" >> {log:?} && \
             node_modules/.bin/amont init >> {log:?} 2>&1 && \
             env -u AMONT_SNAPSHOT node_modules/.bin/amont init >> {log:?} 2>&1"
        ),
    ]);
    let (code, out) = rehearse(&r, &["--wait"]);
    let prepared = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(code, 0, "{out}\n{prepared}");
    assert!(prepared.contains("env=1"), "{prepared}");
    assert_eq!(
        prepared.matches("inside an amont push snapshot").count(),
        2,
        "{prepared}"
    );
    assert_eq!(before, baked(&r), "the shared hooks changed");
}

/// A failing install fails the preparation; the rehearsal records WHY.
#[test]
fn a_failed_install_fails_the_rehearsal_with_its_reason() {
    if missing("node") {
        return;
    }
    let (r, _) = js_repo(npm_root);
    set(&r, "install.exit", "1");
    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("npm ci failed in the root"), "{out}");
    assert!(state(&r).contains("phase=failed"), "{}", state(&r));
    assert!(state(&r).contains("reason=could not prepare the snapshot: npm ci failed"));
    assert_eq!(runs(&r), 0);
    assert_eq!(worktrees(&r), 1);
}

/// At push time (`testPushedTree`) a snapshot that could not be prepared
/// falls back to the working tree, says so, and stamps nothing.
#[test]
fn a_push_whose_snapshot_cannot_be_prepared_falls_back_and_stamps_nothing() {
    if missing("node") {
        return;
    }
    let (r, base) = js_repo(npm_root);
    installed(&r, "");
    r.git(&["config", "amont.testPushedTree", "true"]);
    set(&r, "install.exit", "1");
    // Dirty, so the working tree is NOT the tip: a clean one legitimately
    // vouches for it, which is `stamp_tips`' existing rule.
    r.write("a.txt", "uncommitted\n");
    let (code, out) = push_out(&r, &base, &head(&r));
    assert_eq!(code, 0, "the working tree has node_modules: {out}");
    assert!(out.contains("could not prepare the snapshot"), "{out}");
    assert!(out.contains("testing the working tree instead"), "{out}");
    assert!(!note(&r, "HEAD").contains("pre-push-suite"), "no stamp");
}

// --------------------------------------------------------------- reuse mode

/// npm is never reused: `npm ls` fails fresh installs of real graphs with
/// peer-range conflicts, so it can vouch for nothing. `reuse` says so and
/// installs.
#[test]
fn npm_reuse_always_installs() {
    if missing("node") {
        return;
    }
    let (r, _) = js_repo(npm_root);
    installed(&r, "");
    r.git(&["config", "amont.snapshotDeps", "reuse"]);
    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("npm cannot check an installed tree"), "{out}");
    assert!(logged(&r, "npm ci"), "{}", stub_log(&r));
    assert!(!logged(&r, "npm ls"), "{}", stub_log(&r));
    assert!(!logged(&r, "STALE"), "{}", stub_log(&r));
}

fn pnpm_root(r: &Repo) {
    r.stage("package.json", "{\"name\":\"root\"}\n");
    r.stage("pnpm-lock.yaml", "lockfileVersion: 9\n");
    r.stage("pnpm-workspace.yaml", "packages:\n  - packages/*\n");
}

/// A working tree installed the way pnpm's isolated linker leaves it: every
/// package a link into `.pnpm`, `@scope` dirs of links, and a marker the
/// stub installer reports as STALE if a clone survives into an install.
fn pnpm_installed(r: &Repo, dir: &str) {
    let nm = r.dir.join(dir).join("node_modules");
    std::fs::create_dir_all(&nm).unwrap();
    if dir.is_empty() {
        let pkg = nm.join(".pnpm/a@1.0.0/node_modules/a");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("package.json"), "{\"name\":\"a\"}").unwrap();
        std::fs::write(nm.join(".pnpm/marker-source"), "x").unwrap();
        std::fs::write(nm.join(".modules.yaml"), "nodeLinker: isolated\n").unwrap();
        std::os::unix::fs::symlink(".pnpm/a@1.0.0/node_modules/a", nm.join("a")).unwrap();
        std::fs::create_dir_all(nm.join("@scope")).unwrap();
        std::os::unix::fs::symlink("../.pnpm/a@1.0.0/node_modules/a", nm.join("@scope/a")).unwrap();
    } else {
        let up = "../".repeat(dir.split('/').count() + 1);
        std::os::unix::fs::symlink(
            format!("{up}node_modules/.pnpm/a@1.0.0/node_modules/a"),
            nm.join("a"),
        )
        .unwrap();
        std::fs::create_dir_all(nm.join(".marker")).unwrap();
        std::fs::write(nm.join(".marker/marker-source"), "x").unwrap();
    }
}

/// pnpm writes its install record LAST; a fixture that wrote a member after
/// it would look like an in-place edit.
fn pnpm_record(r: &Repo) {
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(
        r.dir.join("node_modules/.modules.yaml"),
        "nodeLinker: isolated\n",
    )
    .unwrap();
}

fn reuse_repo() -> (Repo, String) {
    let (r, base) = js_repo(|r| {
        pnpm_root(r);
        r.stage("packages/m/package.json", "{\"name\":\"m\"}\n");
    });
    pnpm_installed(&r, "");
    pnpm_installed(&r, "packages/m");
    pnpm_record(&r);
    r.git(&["config", "amont.snapshotDeps", "reuse"]);
    set(&r, "members.out", "packages/m\n");
    (r, base)
}

const PNPM_INSTALL: &str = "pnpm install --frozen-lockfile --prefer-offline";

/// Accepted: root and member clones stay, pnpm verified them, nothing was
/// installed, and the source's recorded paths did not travel.
#[test]
fn pnpm_reuse_keeps_a_clone_pnpm_accepts() {
    if missing("node") {
        return;
    }
    let (r, _) = reuse_repo();
    std::fs::write(
        r.dir.join("node_modules/.pnpm-workspace-state-v1.json"),
        "{\"projects\":{}}\n",
    )
    .unwrap();
    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 0, "{out}\n{}", stub_log(&r));
    assert!(
        out.contains("reused from the working tree — pnpm accepted it"),
        "{out}"
    );
    assert!(
        logged(
            &r,
            "pnpm install --frozen-lockfile --offline --config.confirmModulesPurge=false"
        ),
        "{}",
        stub_log(&r)
    );
    assert!(
        logged(&r, "pnpm ls -r --depth -1 --parseable"),
        "{}",
        stub_log(&r)
    );
    assert!(!logged(&r, "STATE-FILE-KEPT"), "{}", stub_log(&r));
    assert!(
        logged(&r, "MEMBER-CLONED"),
        "the member's node_modules came too: {}",
        stub_log(&r)
    );
    assert!(
        !logged(&r, PNPM_INSTALL),
        "nothing installed: {}",
        stub_log(&r)
    );
    assert!(note(&r, "HEAD").contains("pre-push-suite"));
}

/// Rejected by pnpm: every clone of the unit — root AND member — is removed,
/// THEN the install runs; never an install on top of a clone.
#[test]
fn a_rejected_clone_is_rolled_back_whole_before_the_install() {
    if missing("node") {
        return;
    }
    let (r, _) = reuse_repo();
    set(&r, "verify.exit", "1");
    set(
        &r,
        "verify.out",
        "ERR_PNPM_OUTDATED_LOCKFILE  Cannot install with frozen-lockfile\n",
    );
    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("pnpm did not accept the clone"), "{out}");
    assert!(
        out.contains("ERR_PNPM_OUTDATED_LOCKFILE"),
        "the reason is quoted: {out}"
    );
    assert!(logged(&r, PNPM_INSTALL), "{}", stub_log(&r));
    assert!(
        !logged(&r, "STALE"),
        "a clone survived the rollback: {}",
        stub_log(&r)
    );
}

/// A directory pnpm did not make is importable and pnpm's check ignores it,
/// so the layout is checked here.
#[test]
fn a_stray_directory_rejects_the_clone() {
    if missing("node") {
        return;
    }
    let (r, _) = reuse_repo();
    std::fs::create_dir_all(r.dir.join("node_modules/zzz")).unwrap();
    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("zzz is not a link pnpm made"), "{out}");
    assert!(!logged(&r, "--offline"), "never verified: {}", stub_log(&r));
    assert!(logged(&r, PNPM_INSTALL));
    assert!(!logged(&r, "STALE"), "{}", stub_log(&r));
}

/// The layout rule is the isolated linker's; any other layout is not reused.
#[test]
fn a_hoisted_install_is_not_reused() {
    if missing("node") {
        return;
    }
    let (r, _) = reuse_repo();
    std::fs::write(
        r.dir.join("node_modules/.modules.yaml"),
        "nodeLinker: hoisted\n",
    )
    .unwrap();
    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("not installed with pnpm's isolated linker"),
        "{out}"
    );
    assert!(logged(&r, PNPM_INSTALL));
}

/// A working-tree lockfile that is not the commit's cannot vouch for it.
#[test]
fn a_different_lockfile_in_the_working_tree_means_install() {
    if missing("node") {
        return;
    }
    let (r, _) = reuse_repo();
    r.write("pnpm-lock.yaml", "lockfileVersion: 9\n# uncommitted\n");
    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("lockfile differs from the commit's"), "{out}");
    assert!(!logged(&r, "pnpm ls"), "{}", stub_log(&r));
    assert!(logged(&r, PNPM_INSTALL));
}

/// The case a frozen install refuses: a committed manifest the lockfile
/// does not satisfy. The clone is rejected AND the install fails — so the
/// preparation fails, as CI would.
#[test]
fn a_manifest_the_lockfile_does_not_satisfy_fails_preparation() {
    if missing("node") {
        return;
    }
    let (r, _) = reuse_repo();
    set(&r, "verify.exit", "1");
    set(
        &r,
        "verify.out",
        "specifiers in the lockfile don't match specifiers in package.json\n",
    );
    set(&r, "install.exit", "1");
    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 2, "{out}");
    assert!(
        out.contains("specifiers in the lockfile don't match"),
        "{out}"
    );
    assert!(state(&r).contains("phase=failed"));
    assert!(!logged(&r, "STALE"));
}

/// An edit made in the installed tree after pnpm finished is invisible to
/// pnpm's own check; the install record's mtime is not.
#[test]
fn a_file_edited_after_the_install_rejects_the_clone() {
    if missing("node") {
        return;
    }
    let (r, _) = reuse_repo();
    std::thread::sleep(Duration::from_millis(20));
    let edited = r
        .dir
        .join("node_modules/.pnpm/a@1.0.0/node_modules/a/package.json");
    std::fs::write(&edited, "{\"name\":\"a\",\"patched\":true}").unwrap();
    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("changed after the install"), "{out}");
    assert!(out.contains("a/package.json"), "the file is named: {out}");
    assert!(
        !logged(&r, "--offline"),
        "never cloned or verified: {}",
        stub_log(&r)
    );
    assert!(logged(&r, PNPM_INSTALL));
    assert!(!logged(&r, "STALE"), "{}", stub_log(&r));
}

/// A clone still reading the working tree through a link is not isolated.
#[test]
fn a_link_back_into_the_working_tree_rejects_the_clone() {
    if missing("node") {
        return;
    }
    let (r, _) = reuse_repo();
    std::os::unix::fs::symlink(&r.dir, r.dir.join("node_modules/evil")).unwrap();
    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("links back into the working tree"), "{out}");
    assert!(!logged(&r, "--offline"), "{}", stub_log(&r));
    assert!(logged(&r, PNPM_INSTALL));
    assert!(!logged(&r, "STALE"));
}

/// A nested project with its own lockfile is its own unit: never cloned as
/// the outer unit's member, prepared on its own.
#[test]
fn a_nested_lockfile_is_its_own_unit() {
    if missing("node") {
        return;
    }
    let (r, _) = js_repo(|r| {
        pnpm_root(r);
        r.stage("tools/package.json", "{\"name\":\"tools\"}\n");
        r.stage("tools/pnpm-lock.yaml", "tools lock\n");
    });
    pnpm_installed(&r, "");
    pnpm_installed(&r, "tools");
    pnpm_record(&r);
    // tools' working-tree lockfile is not the commit's: it must install.
    r.write("tools/pnpm-lock.yaml", "tools lock, edited\n");
    r.git(&["config", "amont.snapshotDeps", "reuse"]);
    set(&r, "members.out", "tools\n");
    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 0, "{out}\n{}", stub_log(&r));
    assert!(out.contains("in the root reused"), "{out}");
    assert!(out.contains("in tools/"), "{out}");
    assert!(
        stub_log(&r).lines().any(|l| l.starts_with(PNPM_INSTALL)
            && l.contains("--ignore-workspace")
            && l.ends_with("/tools")),
        "a standalone lockfile installs with --ignore-workspace: {}",
        stub_log(&r)
    );
    assert!(!logged(&r, "STALE"), "{}", stub_log(&r));
}

/// Only the units the push touches are prepared: with an upstream, a unit
/// holding none of the changed files is named and skipped — and prepared as
/// soon as a push touches it.
#[test]
fn an_untouched_unit_is_not_prepared() {
    if missing("node") {
        return;
    }
    let (r, _) = js_repo(|r| {
        npm_root(r);
        r.stage("spikes/s/package.json", "{\"name\":\"s\"}\n");
        r.stage("spikes/s/package-lock.json", "spike lock\n");
    });
    // An upstream to measure the push against: the local `main`.
    r.git(&["config", "branch.feat/x.remote", "."]);
    r.git(&["config", "branch.feat/x.merge", "refs/heads/main"]);
    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 0, "{out}\n{}", stub_log(&r));
    assert!(out.contains("not preparing spikes/s/"), "{out}");
    assert!(
        !stub_log(&r).lines().any(|l| l.ends_with("/spikes/s")),
        "{}",
        stub_log(&r)
    );
    assert!(
        logged(&r, "npm ci"),
        "the root is always prepared: {}",
        stub_log(&r)
    );

    r.stage("spikes/s/notes.txt", "touched\n");
    r.commit("feat: touch the spike");
    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("not preparing"), "{out}");
    assert!(
        stub_log(&r)
            .lines()
            .any(|l| l.starts_with("npm ci") && l.ends_with("/spikes/s")),
        "{}",
        stub_log(&r)
    );
}

/// Both lockfiles: `packageManager` decides, or preparation is refused.
#[test]
fn two_lockfiles_need_a_package_manager_field() {
    if missing("node") {
        return;
    }
    let (r, _) = js_repo(|r| {
        npm_root(r);
        r.stage("pnpm-lock.yaml", "lockfileVersion: 9\n");
    });
    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 2, "{out}");
    assert!(
        out.contains("both package-lock.json and pnpm-lock.yaml"),
        "{out}"
    );
    assert!(stub_log(&r).is_empty());

    r.stage(
        "package.json",
        "{\"name\":\"root\",\"packageManager\":\"pnpm@10.30.2\"}\n",
    );
    r.stage("a.txt", "again\n");
    r.commit("feat: say which manager");
    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 0, "{out}");
    assert!(logged(&r, PNPM_INSTALL), "{}", stub_log(&r));
}

// ---------------------------------------------------------------- discovery

/// The pushed tip decides the units, not HEAD: a push of an older commit is
/// prepared from THAT commit's lockfiles.
#[test]
fn units_come_from_the_pushed_tip_not_head() {
    if missing("node") {
        return;
    }
    let (r, base) = js_repo(npm_root);
    installed(&r, "");
    let older = head(&r);
    r.stage("tools/package.json", "{\"name\":\"tools\"}\n");
    r.stage("tools/package-lock.json", "tools lock\n");
    r.stage("a.txt", "newer\n");
    r.commit("feat: a second project");
    r.git(&["config", "amont.testPushedTree", "true"]);
    let (code, out) = push_out(&r, &base, &older);
    assert_eq!(code, 0, "{out}\n{}", stub_log(&r));
    assert!(!out.contains("could not prepare"), "{out}");
    assert!(logged(&r, "npm ci"), "{}", stub_log(&r));
    assert!(
        !stub_log(&r)
            .lines()
            .any(|l| l.starts_with("npm ci") && l.ends_with("/tools")),
        "HEAD's second project is not in the pushed tip: {}",
        stub_log(&r)
    );
}

/// A lockfile staged in the developer's index is not in the commit, so it
/// is not a unit of the snapshot.
#[test]
fn a_staged_lockfile_is_not_a_unit() {
    if missing("node") {
        return;
    }
    let (r, _) = js_repo(npm_root);
    r.stage("web/package.json", "{\"name\":\"web\"}\n");
    r.stage("web/package-lock.json", "web lock\n");
    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("in web/"), "{out}");
    assert!(
        !stub_log(&r).lines().any(|l| l.ends_with("/web")),
        "{}",
        stub_log(&r)
    );
}

// -------------------------------------------------------------------- carry

/// Carried BEFORE the install, so an install-time file is there for it.
#[test]
fn carry_copies_an_untracked_file_before_the_install() {
    if missing("node") {
        return;
    }
    let (r, _) = js_repo(npm_root);
    r.write(".env", "SECRET=1\n");
    r.git(&["config", "amont.snapshotCarry", ".env, .npmrc"]);
    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("carried .env"), "{out}");
    assert!(
        out.contains("not carrying .npmrc"),
        "a missing entry is said, not fatal: {out}"
    );
    assert!(logged(&r, "ENV-VISIBLE"), "{}", stub_log(&r));
}

/// Committed: `set snapshotCarry` in a trusted amont.conf, so every clone
/// carries the same files without a local `git config`.
#[test]
fn a_committed_carry_line_is_honoured_once_trusted() {
    if missing("node") {
        return;
    }
    let (r, _) = js_repo(npm_root);
    r.write(".env", "SECRET=1\n");
    r.stage(
        "amont.conf",
        "pre-push    suite  *.txt  block  node gate.js\nset snapshotCarry .env\n",
    );
    r.stage("a.txt", "carry\n");
    r.commit("chore: carry the env");
    trust_and_install(&r);
    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("carried .env"), "{out}");
    assert!(logged(&r, "ENV-VISIBLE"), "{}", stub_log(&r));
}

/// Every refusal, in one message; nothing copied; nothing tested.
#[test]
fn carry_refuses_every_bad_entry_in_one_message() {
    if missing("node") {
        return;
    }
    let (r, _) = js_repo(|r| {
        npm_root(r);
        r.stage("src/app.txt", "tracked\n");
    });
    r.write(".env", "SECRET=1\n");
    std::fs::create_dir_all(r.dir.join("real")).unwrap();
    std::fs::write(r.dir.join("real/x"), "x").unwrap();
    std::os::unix::fs::symlink(r.dir.join("real"), r.dir.join("linked")).unwrap();
    r.git(&[
        "config",
        "amont.snapshotCarry",
        ".env src/app.txt src . .. .git/config /etc/hosts linked/x",
    ]);
    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 2, "{out}");
    for needle in [
        "refused 7 entries",
        "src/app.txt: tracked content",
        "src: tracked content",
        ".: has a `.` or `..` component",
        "..: has a `.` or `..` component",
        ".git/config: is inside .git",
        "/etc/hosts: not a repo-relative path",
        "linked/x:",
    ] {
        assert!(out.contains(needle), "missing {needle:?} in:\n{out}");
    }
    assert!(
        !out.contains("carried .env"),
        "nothing is copied when anything is refused"
    );
    assert!(stub_log(&r).is_empty(), "{}", stub_log(&r));
    assert_eq!(runs(&r), 0);
}

/// Present but unreadable is a failure, not a skip.
#[test]
fn an_unreadable_carry_entry_fails_preparation() {
    if missing("node") {
        return;
    }
    let (r, _) = js_repo(npm_root);
    let env = r.dir.join(".env");
    std::fs::write(&env, "SECRET=1\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&env, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&env).is_ok() {
        return; // running as root: permissions do not bind
    }
    r.git(&["config", "amont.snapshotCarry", ".env"]);
    let (code, out) = rehearse(&r, &["--wait"]);
    std::fs::set_permissions(&env, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(code, 2, "{out}");
    assert!(out.contains(".env could not be copied"), "{out}");
}

// ------------------------------------------------------------- lifecycle

/// A long install is visible: `--status` says `preparing`, a push waits for
/// it (bounded), and a newer commit cancels it — installer included.
#[test]
fn a_rehearsal_is_registered_while_it_prepares() {
    if missing("node") {
        return;
    }
    let (r, base) = js_repo(npm_root);
    set(&r, "install.block", "");
    let (code, out) = rehearse(&r, &[]);
    assert_eq!(code, 0, "{out}");
    wait_for("the worker to register", || {
        state(&r).contains("step=preparing")
    });
    wait_for("the installer to start", || {
        ctl(&r).join("install.pid").exists()
    });
    let (_, status) = rehearse(&r, &["--status"]);
    assert!(status.contains("preparing"), "{status}");

    // A push meanwhile waits rather than starting over — within its budget.
    r.git(&["config", "amont.rehearsalWait", "2"]);
    let (_, out) = push_out(&r, &base, &head(&r));
    assert!(out.contains("still running — waiting up to"), "{out}");

    // A newer commit cancels it, the installer too.
    let installer: u32 = std::fs::read_to_string(ctl(&r).join("install.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    std::fs::remove_file(ctl(&r).join("install.block")).unwrap();
    r.stage("a.txt", "newer\n");
    r.commit("feat: newer");
    let (code, out) = rehearse(&r, &["--wait"]);
    assert_eq!(code, 0, "{out}");
    let alive = |pid: u32| {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    wait_for("the cancelled installer to exit", || !alive(installer));
    std::fs::write(ctl(&r).join("release"), "").unwrap();
    assert_eq!(worktrees(&r), 1, "no snapshot left behind");
    assert!(note(&r, "HEAD").contains("pre-push-suite"));
}

// ---------------------------------------------------------- real managers

fn tarball(dir: &Path, name: &str) -> String {
    let pkg = dir.join(format!("src-{name}"));
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(
        pkg.join("package.json"),
        format!("{{\"name\":\"{name}\",\"version\":\"1.0.0\"}}"),
    )
    .unwrap();
    let out = Command::new("npm")
        .args(["pack", "--pack-destination"])
        .arg(dir)
        .current_dir(&pkg)
        .output()
        .expect("npm pack");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::remove_dir_all(&pkg).unwrap();
    format!("{name}-1.0.0.tgz")
}

fn real_repo(manager: &str) -> Option<(Repo, String)> {
    if missing("node") || missing("npm") || missing(manager) {
        return None;
    }
    let (r, base) = js_repo(|r| {
        let a = tarball(&r.dir, "a");
        r.git(&["add", &a]);
        r.stage(
            "package.json",
            &format!("{{\"name\":\"root\",\"version\":\"1.0.0\",\"dependencies\":{{\"a\":\"file:./{a}\"}}}}"),
        );
        let args: &[&str] = if manager == "npm" {
            &["install", "--offline", "--no-audit", "--no-fund"]
        } else {
            &["install", "--offline"]
        };
        let out = Command::new(manager)
            .args(args)
            .current_dir(&r.dir)
            .output()
            .expect("install");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let lock = if manager == "npm" {
            "package-lock.json"
        } else {
            "pnpm-lock.yaml"
        };
        r.git(&["add", lock]);
    });
    r.git(&["config", "amont.snapshotDeps", "reuse"]);
    Some((r, base))
}

fn real(r: &Repo, flags: &[&str]) -> (i32, String) {
    let mut args = vec!["rehearse"];
    args.extend_from_slice(flags);
    amont(r, &args, false)
}

#[test]
fn real_npm_installs_with_npm_ci() {
    let Some((r, _)) = real_repo("npm") else {
        return;
    };
    let (code, out) = real(&r, &["--wait"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("npm cannot check an installed tree"), "{out}");
    assert!(out.contains("rehearsal of"), "{out}");
}

#[test]
fn real_npm_refuses_a_manifest_its_lockfile_does_not_satisfy() {
    let Some((r, _)) = real_repo("npm") else {
        return;
    };
    r.stage(
        "package.json",
        "{\"name\":\"root\",\"version\":\"1.0.0\",\"dependencies\":{\"a\":\"file:./a-1.0.0.tgz\",\"b\":\"^1.0.0\"}}",
    );
    r.stage("a.txt", "mismatch\n");
    r.commit("feat: a dependency the lockfile never heard of");
    let (code, out) = real(&r, &["--wait"]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("npm ci failed"), "{out}");
    assert!(state(&r).contains("phase=failed"));
}

#[test]
fn real_pnpm_accepts_a_matching_clone_and_refuses_a_mismatch() {
    let Some((r, _)) = real_repo("pnpm") else {
        return;
    };
    let (code, out) = real(&r, &["--wait"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("reused from the working tree — pnpm accepted it"),
        "{out}"
    );

    r.stage(
        "package.json",
        "{\"name\":\"root\",\"version\":\"1.0.0\",\"dependencies\":{\"a\":\"file:./a-1.0.0.tgz\",\"b\":\"^1.0.0\"}}",
    );
    r.stage("a.txt", "mismatch\n");
    r.commit("feat: a dependency the lockfile never heard of");
    let (code, out) = real(&r, &["--wait"]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("pnpm did not accept the clone"), "{out}");
    assert!(out.contains("pnpm install failed"), "{out}");
}
