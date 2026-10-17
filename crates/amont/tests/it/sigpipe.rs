//! `amont list | head` must die quietly, like every other Unix filter.
//!
//! Rust ignores SIGPIPE at startup, so the first `head` that closed the pipe
//! early turned `list` into a panic with a full backtrace — "failed printing
//! to stdout: Broken pipe" — which reads as a crash in a tool whose whole
//! claim is composure. `main` restores SIGPIPE's default disposition; this
//! test is what stops that line being "simplified" away.
//!
//! Unix-only by nature: Windows has no SIGPIPE, and there the closed-pipe
//! write comes back as an `Err` instead of a signal.
#![cfg(unix)]

use crate::common;
use common::Repo;

/// The manifest is made large enough that `list`'s output cannot fit the
/// kernel's pipe buffer, so the writer is still writing when `head` has
/// exited and the read end is gone — the panic was racy to reproduce with a
/// small repo, and a regression test that only sometimes meets the condition
/// it guards is a coin, not a test.
#[test]
fn a_closed_pipe_kills_list_quietly() {
    let r = Repo::new();
    let mut manifest = String::new();
    for i in 0..4000 {
        manifest.push_str(&format!("pre-commit  check-{i}  *  warn  echo {i}\n"));
    }
    r.write("amont.conf", &manifest);

    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!(
            "'{}' list | head -c 1 > /dev/null",
            env!("CARGO_BIN_EXE_amont")
        ))
        .current_dir(&r.dir)
        .output()
        .expect("run the pipeline");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("panicked"),
        "list panicked on the closed pipe instead of dying quietly:\n{stderr}"
    );
}
