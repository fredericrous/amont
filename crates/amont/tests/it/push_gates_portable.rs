//! ADR-0018's refusal on every platform (#414): `push_gates.rs` is Unix
//! only, so Windows had no test that a bare push refuses an unstamped heavy
//! gate. Gates here are node scripts and amont is called directly, with no
//! `sh` anywhere.
//!
//! Also: a declared `heavy` gate is owed before the connection whatever its
//! scope. A `*` declaration can be any command, and one that ran for a minute
//! on the open connection is what #414 saw live.

use crate::common::{self, missing, Repo};
use std::process::Stdio;

fn head(r: &Repo) -> String {
    String::from_utf8_lossy(&r.git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string()
}

/// `amont trust` then `amont init`, as `tree_gates::trust_and_install` does,
/// which is in a Unix-only module.
fn trust_and_install(r: &Repo) {
    for verb in ["trust", "init"] {
        let out = common::amont_cmd()
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

/// The pre-push hook on one ref line: exit code and stderr.
fn hook(r: &Repo, line: &str) -> (i32, String) {
    let mut cmd = common::amont_cmd();
    cmd.arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-push")
        .current_dir(&r.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn");
    {
        use std::io::Write;
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(line.as_bytes())
            .expect("write");
    }
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

/// A repo declaring `decl` (its command `node gate.js`, which logs each run),
/// with one `.txt` commit to push. The repo, the base sha and the log.
fn gated(decl: &str) -> (Repo, String, std::path::PathBuf) {
    let r = Repo::new();
    let log = r.path(".git/gate.log");
    r.stage(
        "gate.js",
        &format!(
            "require('fs').appendFileSync({:?},'x')\n",
            log.to_str().expect("utf8")
        ),
    );
    r.commit("chore: base");
    let base = head(&r);
    r.git(&["update-ref", "refs/remotes/origin/main", &base]);
    r.stage("amont.conf", &format!("{decl}\n"));
    r.commit("chore: the gate");
    trust_and_install(&r);
    r.stage("a.txt", "hello\n");
    r.commit("feat: something to push");
    (r, base, log)
}

fn runs(log: &std::path::Path) -> usize {
    std::fs::read_to_string(log).map(|s| s.len()).unwrap_or(0)
}

fn push_line(from: &str, to: &str) -> String {
    format!("refs/heads/feat/x {to} refs/heads/feat/x {from}\n")
}

#[test]
fn a_bare_push_refuses_an_unstamped_scoped_gate_on_every_platform() {
    if missing("node") {
        return;
    }
    let (r, base, log) = gated("pre-push  suite  *.txt  block  node gate.js");
    let (code, out) = hook(&r, &push_line(&base, &head(&r)));
    assert_ne!(code, 0, "{out}");
    assert_eq!(runs(&log), 0, "no heavy gate starts in a refused push");
    assert!(out.contains("pre-push-suite"), "{out}");
    assert!(
        out.contains(&format!("{} land", amont_runtime::names::BIN)),
        "{out}"
    );
}

#[test]
fn a_declared_heavy_gate_scoped_star_is_owed_before_the_connection() {
    if missing("node") {
        return;
    }
    let (r, base, log) = gated("pre-push  probe  *  warn  heavy node gate.js");
    let (code, out) = hook(&r, &push_line(&base, &head(&r)));
    assert_ne!(
        code, 0,
        "a heavy `*` gate is not run on the connection:\n{out}"
    );
    assert_eq!(runs(&log), 0, "{out}");
    assert!(out.contains("pre-push-probe"), "{out}");
}

#[test]
fn a_light_gate_scoped_star_still_runs_in_the_hook() {
    if missing("node") {
        return;
    }
    let (r, base, log) = gated("pre-push  probe  *  warn  node gate.js");
    let (code, out) = hook(&r, &push_line(&base, &head(&r)));
    assert_eq!(code, 0, "{out}");
    assert_eq!(runs(&log), 1, "{out}");
}
