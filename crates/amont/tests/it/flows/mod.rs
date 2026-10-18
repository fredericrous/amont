//! What a person does with amont, one test per journey (decisions plan
//! 2026-10-17-amont-tests-follow-user-flows, Phase 5).
//!
//! Each flow builds its repository once and walks the journey, asserting
//! what each step leaves on the terminal, in the refs and on disk: amont's
//! public interface. Every assertion names its step, so a failure says
//! where the journey broke, not only that it did. Edge cases stay where they
//! are cheap: tables beside the rule, unit tests beside the mechanism.

/// `assert!` for a flow: the message starts with the step's name.
///
/// ```ignore
/// step!("push", run.passed(), "{}", run.output());
/// ```
macro_rules! step {
    ($step:literal, $cond:expr $(,)?) => {
        assert!($cond, "step `{}`: {}", $step, stringify!($cond))
    };
    ($step:literal, $cond:expr, $($arg:tt)+) => {
        assert!($cond, "step `{}`: {}", $step, format_args!($($arg)+))
    };
}

/// `assert_eq!` for a flow, named the same way.
macro_rules! step_eq {
    ($step:literal, $left:expr, $right:expr $(,)?) => {
        assert_eq!($left, $right, "step `{}`", $step)
    };
    ($step:literal, $left:expr, $right:expr, $($arg:tt)+) => {
        assert_eq!($left, $right, "step `{}`: {}", $step, format_args!($($arg)+))
    };
}

use crate::common::Repo;
use std::process::{Command, Output, Stdio};

/// Whether a command succeeded, and everything it printed: stdout, then
/// stderr, where git puts what its hooks say.
fn said(out: &Output) -> (bool, String) {
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// `amont <args>` from the repository, as a person types it: no hook mode,
/// and their own git config (trust lives there).
fn amont(r: &Repo, args: &[&str]) -> (bool, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_amont"));
    cmd.args(args).current_dir(&r.dir).stdin(Stdio::null());
    Repo::strip_git_env_impl(&mut cmd);
    said(&cmd.output().expect("run amont"))
}

/// `amont trust` then `amont init`: the hooks installed the way a person
/// installs them, so every `git` verb below runs them.
fn install(r: &Repo) {
    for verb in ["trust", "init"] {
        let (ok, out) = amont(r, &[verb]);
        step!("install", ok, "amont {verb}:\n{out}");
    }
}

/// A hooked `git <args>`, with everything git and the hooks said.
fn git(r: &Repo, args: &[&str]) -> (bool, String) {
    said(&r.git(args))
}

/// A hooked `git commit`, with everything the hooks said.
fn git_commit(r: &Repo, args: &[&str]) -> (bool, String) {
    let mut all = vec!["commit", "-q"];
    all.extend_from_slice(args);
    git(r, &all)
}

fn read(r: &Repo, path: &str) -> String {
    std::fs::read_to_string(r.path(path)).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

fn git_out(r: &Repo, args: &[&str]) -> String {
    String::from_utf8_lossy(&r.git(args).stdout)
        .trim_end()
        .to_string()
}

fn head(r: &Repo) -> String {
    git_out(r, &["rev-parse", "HEAD"])
}

mod commit;
mod declared;
mod land;
mod push;
