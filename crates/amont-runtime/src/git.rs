//! Thin wrappers over the `git` calls the hooks make.

use std::process::{Command, Stdio};

/// Run `cmd`, retrying the transient SPAWN failures a loaded machine
/// produces: EINTR, EAGAIN (fork pressure), ETXTBSY (another thread's
/// fork-to-exec window still holding a write descriptor on the executable).
/// A NON-ZERO EXIT IS NEVER RETRIED — that is git answering; this covers
/// only "git could not be asked".
///
/// The failure this ends: `gate_stamp`'s tests — and, invisibly, real
/// hooks on a loaded machine — watched a single failed fork turn
/// `bind_to_head` into "nothing to stamp". The hooks' fail-open reading of
/// `None` is right for a git that is genuinely absent; three attempts over
/// ~130ms is the difference between that and a scheduler hiccup.
fn retrying<T>(mut attempt: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut delay = std::time::Duration::from_millis(10);
    for tries_left in [2u8, 1, 0] {
        match attempt() {
            Err(e) if tries_left > 0 && transient(&e) => {
                std::thread::sleep(delay);
                delay *= 3;
            }
            other => return other,
        }
    }
    unreachable!("the zero-tries arm returns")
}

/// The retryable kinds, matched on raw OS codes because the precise
/// `io::ErrorKind` variants (`ExecutableFileBusy`, `ResourceBusy`) are not
/// stable at this crate's MSRV: EINTR(4), EAGAIN(11 linux / 35 mac),
/// ETXTBSY(26).
fn transient(e: &std::io::Error) -> bool {
    if matches!(
        e.kind(),
        std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
    ) {
        return true;
    }
    matches!(e.raw_os_error(), Some(4 | 11 | 26 | 35))
}

/// stdout of a git command, trimmed. `None` when git itself failed — which the
/// hooks treat as "cannot tell, do not block", never as "empty".
pub fn stdout(args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.args(args).stderr(Stdio::null());
    let out = retrying(|| cmd.output()).ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The same, run inside `dir`.
///
/// The dashboard asks about repositories it is not standing in, and must get
/// the answer git would give THERE — config is per-repository, so asking from
/// the wrong directory returns the wrong severity.
pub fn stdout_in(dir: &std::path::Path, args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args).stderr(Stdio::null());
    let out = retrying(|| cmd.output()).ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// [`succeeds`] for a repository this process is not standing in — the shape
/// the fleet needs to UNDO something (delete a ref it wrote) rather than ask
/// about it. Output discarded; `false` covers "git could not run" too.
pub fn succeeds_in(dir: &std::path::Path, args: &[&str]) -> bool {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    retrying(|| cmd.status())
        .map(|s| s.success())
        .unwrap_or(false)
}

/// stdout of a git command that itself reads a list from stdin — `diff-tree
/// --stdin`, fed a list of commits, is the only caller today. Lossy but
/// untrimmed: every line is a path, and the caller trims those itself.
pub fn stdout_piped(args: &[&str], stdin: &str) -> Option<String> {
    use std::io::Write;
    let mut cmd = Command::new("git");
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = retrying(|| cmd.spawn()).ok()?;
    child.stdin.take()?.write_all(stdin.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// As `stdout_piped`, but returning the RAW bytes.
///
/// Needed by the one caller that must both feed git a list on stdin and read a
/// `-z` path list back — `diff-tree --stdin -z`. `stdout_paths` cannot serve it
/// (no stdin) and `stdout_piped` cannot either (lossy `String`, and the NUL
/// separators are the whole point).
pub fn stdout_piped_raw(args: &[&str], stdin: &str) -> Option<Vec<u8>> {
    use std::io::Write;
    let mut cmd = Command::new("git");
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = retrying(|| cmd.spawn()).ok()?;
    child.stdin.take()?.write_all(stdin.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    out.status.success().then_some(out.stdout)
}

/// As `stdout_piped`, but inside `dir` and taking raw bytes.
///
/// `-C dir` matters for `hash-object`: a repository configured for SHA-256
/// computes a different id than the default, so the identity has to be asked
/// of THAT repository. Bytes rather than `&str` because the input is a file we
/// have already read and must not re-encode.
pub fn stdout_piped_in(dir: &std::path::Path, args: &[&str], stdin: &[u8]) -> Option<String> {
    use std::io::Write;
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = retrying(|| cmd.spawn()).ok()?;
    child.stdin.take()?.write_all(stdin).ok()?;
    let out = child.wait_with_output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Raw stdout, untrimmed and not lossy — for a patch, where a trailing newline
/// and any byte in a binary hunk are load-bearing.
pub fn stdout_raw(args: &[&str]) -> Option<Vec<u8>> {
    let mut cmd = Command::new("git");
    cmd.args(args).stderr(Stdio::null());
    let out = retrying(|| cmd.output()).ok()?;
    out.status.success().then_some(out.stdout)
}

/// Everything a git command said: its exit code, its stdout and its stderr.
pub struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// A git command's full result, for the caller that must tell one kind of
/// failure from another.
///
/// [`stdout`] collapses every non-zero exit to `None` and discards stderr,
/// which is the right shape for "cannot tell, do not block". It is the wrong
/// shape for reading configuration: `git config --get` exits **1** for a key
/// nobody set and **128** for a key set to something git itself refuses to
/// parse, and those two must not become the same answer — one is a default,
/// the other is a mistake somebody needs to be told about. See `config`.
pub fn output(args: &[&str]) -> Option<Output> {
    let mut cmd = Command::new("git");
    cmd.args(args).stdin(Stdio::null());
    let out = retrying(|| cmd.output()).ok()?;
    Some(Output {
        // A process killed by a signal has no code. Treat that as "git did not
        // answer" rather than inventing one; the caller falls back.
        code: out.status.code()?,
        stdout: String::from_utf8_lossy(&out.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
    })
}

/// How a bounded network probe ended. Exit codes stay visible because the
/// callers need to keep git's answers apart: `ls-remote --exit-code` exits
/// **2** for "connected, no such ref" and **128** for "could not connect",
/// and reading those as one boolean is how offline got reported as
/// "upstream deleted".
pub enum Probe {
    /// Ran to completion with this exit code.
    Exit(i32),
    /// Killed at the deadline; carries the budget it exceeded, in seconds.
    TimedOut(u64),
    /// Could not spawn, or died to a signal — "git did not answer".
    Failed,
}

/// A git command that TALKS TO THE NETWORK, killed at `budget_secs`.
///
/// Every other runner in this module waits forever, which is correct for
/// local plumbing — a `rev-parse` that hangs means the machine is already
/// lost. A network verb hanging is Tuesday: captive portal, VPN split
/// brain, a remote that accepts the TCP connect and then says nothing.
/// Unbounded, that held the push hostage inside the index hold with no
/// deadline anywhere; the learned response is `--no-verify`, permanently.
/// `budget_secs == 0` means no deadline (the same opt-out `amont.timeout`
/// honours). Output is discarded — network callers decide on exit codes.
pub fn probe(args: &[&str], budget_secs: u64) -> Probe {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if budget_secs == 0 {
        return match retrying(|| cmd.status()) {
            Ok(s) => s.code().map(Probe::Exit).unwrap_or(Probe::Failed),
            Err(_) => Probe::Failed,
        };
    }
    let Ok(mut child) = retrying(|| cmd.spawn()) else {
        return Probe::Failed;
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(budget_secs);
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return s.code().map(Probe::Exit).unwrap_or(Probe::Failed),
            Ok(None) => {}
            Err(_) => return Probe::Failed,
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Probe::TimedOut(budget_secs);
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

/// True when the command exits 0. Output discarded.
pub fn succeeds(args: &[&str]) -> bool {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    retrying(|| cmd.status())
        .map(|s| s.success())
        .unwrap_or(false)
}

/// A path list from `diff --name-only`, `diff-tree --name-only` or
/// `ls-files` — commands whose output is meant to be split into individual
/// paths, never just read as one blob.
///
/// By default git QUOTES any "unusual" byte in a path, non-ASCII included:
/// `é.json` prints as `"\303\251.json"`. Reading that line as a literal path
/// looks up a file that does not exist — the caller then treats real,
/// unstaged content as absent, which is how `StagedOnly` used to lose it.
/// `-z` disables quoting entirely and NUL-terminates each entry instead, so
/// there is no escaping left to get wrong. Inserted right after the
/// subcommand (`args[0]`), which is always a valid position for it on every
/// command this is used for.
pub fn stdout_paths(args: &[&str]) -> Option<Vec<String>> {
    let (first, rest) = args.split_first()?;
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(*first);
    argv.push("-z");
    argv.extend_from_slice(rest);
    stdout_raw(&argv).map(|raw| split_nul_paths(&raw))
}

/// The parsing half of [`stdout_paths`], split out so it can be tested on
/// literal bytes rather than a real git process — including the byte
/// sequence a QUOTED path would have produced under the old line-splitting
/// approach, to prove `-z` output is never reinterpreted that way.
pub(crate) fn split_nul_paths(raw: &[u8]) -> Vec<String> {
    raw.split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

#[cfg(test)]
mod retry_tests {
    use super::*;

    /// The classifier: scheduler hiccups retry, real answers do not.
    #[test]
    fn transient_covers_the_fork_pressure_kinds_and_nothing_else() {
        for code in [4, 11, 26, 35] {
            assert!(
                transient(&std::io::Error::from_raw_os_error(code)),
                "raw {code} is a loaded-machine hiccup"
            );
        }
        assert!(transient(&std::io::Error::from(
            std::io::ErrorKind::Interrupted
        )));
        assert!(!transient(&std::io::Error::from(
            std::io::ErrorKind::NotFound
        )));
        assert!(!transient(&std::io::Error::from_raw_os_error(13))); // EACCES
    }

    /// Three attempts, then the error is the caller's: a git that is
    /// genuinely absent must not cost more than ~130ms of patience.
    #[test]
    fn retrying_gives_up_after_three_transient_failures() {
        let mut calls = 0;
        let r: std::io::Result<()> = retrying(|| {
            calls += 1;
            Err(std::io::Error::from_raw_os_error(11))
        });
        assert!(r.is_err());
        assert_eq!(calls, 3);
    }

    /// A non-transient error returns immediately — a missing git is an
    /// answer, not a hiccup.
    #[test]
    fn a_hard_error_is_not_retried() {
        let mut calls = 0;
        let r: std::io::Result<()> = retrying(|| {
            calls += 1;
            Err(std::io::Error::from(std::io::ErrorKind::NotFound))
        });
        assert!(r.is_err());
        assert_eq!(calls, 1);
    }

    /// A success after a hiccup is a success.
    #[test]
    fn one_hiccup_then_an_answer_is_an_answer() {
        let mut calls = 0;
        let r = retrying(|| {
            calls += 1;
            if calls == 1 {
                Err(std::io::Error::from_raw_os_error(4))
            } else {
                Ok(42)
            }
        });
        assert_eq!(r.unwrap(), 42);
        assert_eq!(calls, 2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_nul_and_drops_the_trailing_empty_segment() {
        assert_eq!(
            split_nul_paths(b"src/main.rs\0Cargo.toml\0"),
            vec!["src/main.rs", "Cargo.toml"]
        );
    }

    #[test]
    fn empty_input_is_no_paths() {
        assert_eq!(split_nul_paths(b""), Vec::<String>::new());
    }

    /// The exact bug this exists to prevent: under `--name-only` without
    /// `-z`, git would have printed `é.json` as the quoted, LINE-oriented
    /// text `"\303\251.json"` — literal backslashes, digits and quotes, nine
    /// bytes standing in for the original two-byte UTF-8 sequence. `-z`
    /// output carries the real UTF-8 bytes of the path with no such
    /// reinterpretation, so splitting on NUL must hand them back unchanged.
    #[test]
    fn a_non_ascii_path_is_not_reinterpreted_as_its_quoted_form() {
        let mut raw = "é.json".as_bytes().to_vec();
        raw.push(0);
        let got = split_nul_paths(&raw);
        assert_eq!(got, vec!["é.json".to_string()]);
        assert_ne!(got[0], "\"\\303\\251.json\"", "must not be the quoted form");
    }
}

/// The branch `HEAD` names, or `None` on a detached head — asked of git ONCE
/// per process and lent to every check that wants it.
///
/// Two always-on pre-commit checks (`branch-pattern`, `branch-protect`) open
/// with this same question. Asked twice it is two spawns on every commit,
/// which is exactly the o(checks) growth `tests/spawn_budget.rs` exists to
/// refuse; asked once it is the price of one check, however many share it.
pub fn current_branch() -> Option<&'static str> {
    static BRANCH: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BRANCH
        .get_or_init(|| stdout(&["symbolic-ref", "--quiet", "--short", "HEAD"]))
        .as_deref()
}

/// Whether any remote is configured. Same device, same reason: a contract
/// about pushing has nothing to gate in a repository nothing is pushed from,
/// and more than one check asks before speaking.
pub fn has_remote() -> bool {
    static REMOTE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *REMOTE.get_or_init(|| stdout(&["remote"]).is_some_and(|r| !r.is_empty()))
}
