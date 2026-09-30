//! Running one tree gate's command with a deadline (ADR-0024).
//!
//! A tree gate is optional work beside the commit's real checks, so it must
//! never outlive the time it was given, and never leave anything running when
//! it is stopped:
//!
//! - it runs in its **own process group**, reniced so it yields CPU to the
//!   mandatory checks;
//! - it has a hard **deadline** and a **cancel** flag (the dispatcher sets it
//!   when the slack after the mandatory checks runs out);
//! - on either, the **whole group** is killed (TERM, then KILL) before this
//!   returns, so a lock held for the run is released only once nothing that
//!   could still write the cache is alive.
//!
//! The outcome is richer than `common::run_quiet`'s `bool`. The caller needs to
//! tell a lint failure (withhold the stamp, say how many problems) from a
//! timeout or a cancel (the cache was not proven cold or warm) from a tool that
//! could not start.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How a tree gate's run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeRun {
    /// Exit 0.
    Passed,
    /// Non-zero exit, with a one-line summary (the tool's last non-empty
    /// output line, e.g. eslint's `✖ 3 problems (3 errors, 0 warnings)`).
    Failed(String),
    /// The deadline passed; the group was killed.
    TimedOut,
    /// The cancel flag was set; the group was killed.
    Cancelled,
    /// The command could not be started.
    Spawn(String),
}

/// Lines of output kept for the summary. The tool's output is not shown: a
/// tree gate never decides the commit, so its findings are CI's to print.
const TAIL: usize = 64;

/// How often the runner looks at the child, the deadline and the flag.
const POLL: Duration = Duration::from_millis(20);

/// How long TERM gets before KILL.
const GRACE: Duration = Duration::from_millis(500);

/// Nice increment for a tree gate: below the mandatory checks, not starved.
#[cfg(unix)]
const NICENESS: i32 = 10;

#[cfg(unix)]
extern "C" {
    #[link_name = "setpriority"]
    fn libc_setpriority(which: i32, who: u32, prio: i32) -> i32;
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
    #[link_name = "pipe"]
    fn libc_pipe(fds: *mut i32) -> i32;
}

/// One pipe for BOTH stdout and stderr, as `2>&1` would: the tool's lines
/// arrive in the order it wrote them, so "the last line" means something.
#[cfg(unix)]
fn merged_pipe() -> Option<(File, Stdio, Stdio)> {
    use std::os::unix::io::FromRawFd;
    let mut fds = [0i32; 2];
    // SAFETY: `fds` is two writable ints, which is all `pipe` writes.
    if unsafe { libc_pipe(fds.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: both fds were just created and are owned by nobody else.
    let (read, write) = unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) };
    let write2 = write.try_clone().ok()?;
    Some((read, Stdio::from(write), Stdio::from(write2)))
}

#[cfg(unix)]
const SIGTERM: i32 = 15;
#[cfg(unix)]
const SIGKILL: i32 = 9;

/// Signal every process in `pgid`'s group. `kill(-pgid, sig)`.
#[cfg(unix)]
fn signal_group(pgid: u32, sig: i32) -> bool {
    // SAFETY: plain integers in, an int out; no memory is shared.
    unsafe { libc_kill(-(pgid as i32), sig) == 0 }
}

/// Whether any process of the group is still alive (`kill(-pgid, 0)`).
#[cfg(unix)]
pub fn group_alive(pgid: u32) -> bool {
    signal_group(pgid, 0)
}

fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        let pgid = child.id();
        signal_group(pgid, SIGTERM);
        let until = Instant::now() + GRACE;
        while Instant::now() < until {
            // Reap the leader so its pid cannot linger as a zombie in the
            // group, then ask whether anybody else is left.
            let _ = child.try_wait();
            if !group_alive(pgid) {
                break;
            }
            std::thread::sleep(POLL);
        }
        if group_alive(pgid) {
            signal_group(pgid, SIGKILL);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn drain(
    stream: impl Read + Send + 'static,
    tail: Arc<Mutex<VecDeque<String>>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            if let Ok(mut t) = tail.lock() {
                if t.len() == TAIL {
                    t.pop_front();
                }
                t.push_back(line);
            }
        }
    })
}

fn summary(tail: &Mutex<VecDeque<String>>) -> String {
    tail.lock()
        .ok()
        .and_then(|t| {
            t.iter()
                .rev()
                .map(|l| l.trim())
                .find(|l| !l.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "no output".to_string())
}

/// Run `argv` in `cwd` until it exits, `deadline` passes, or `cancel` is set.
pub fn run(argv: &[String], cwd: &Path, deadline: Instant, cancel: &AtomicBool) -> TreeRun {
    let Some((program, rest)) = argv.split_first() else {
        return TreeRun::Spawn("empty command".into());
    };
    let mut cmd = Command::new(program);
    cmd.args(rest).current_dir(cwd).stdin(Stdio::null());
    #[cfg(unix)]
    let merged = merged_pipe();
    #[cfg(not(unix))]
    let merged: Option<(File, Stdio, Stdio)> = None;
    let reader = match merged {
        Some((read, out, err)) => {
            cmd.stdout(out).stderr(err);
            Some(read)
        }
        None => {
            cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
            None
        }
    };
    crate::hooks::common::strip_git_env(&mut cmd);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
        // SAFETY: `setpriority` is async-signal-safe and touches no memory;
        // it lowers only this child's priority, between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                libc_setpriority(0, 0, NICENESS);
                Ok(())
            });
        }
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return TreeRun::Spawn(format!("{program}: {e}")),
    };
    // The parent's copies of the write end went into `cmd` and die with it,
    // so the reader sees EOF once every process holding the pipe has exited.
    drop(cmd);
    let tail = Arc::new(Mutex::new(VecDeque::with_capacity(TAIL)));
    let mut drains = Vec::new();
    if let Some(read) = reader {
        drains.push(drain(read, Arc::clone(&tail)));
    }
    if let Some(out) = child.stdout.take() {
        drains.push(drain(out, Arc::clone(&tail)));
    }
    if let Some(err) = child.stderr.take() {
        drains.push(drain(err, Arc::clone(&tail)));
    }
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Read to EOF before summarising — bounded, because a
                // grandchild the tool left behind may still hold the pipe.
                let until = Instant::now() + GRACE;
                while Instant::now() < until && drains.iter().any(|d| !d.is_finished()) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                return if status.success() {
                    TreeRun::Passed
                } else {
                    TreeRun::Failed(summary(&tail))
                };
            }
            Ok(None) => {}
            Err(e) => {
                kill_tree(&mut child);
                return TreeRun::Spawn(format!("{program}: {e}"));
            }
        }
        if cancel.load(Ordering::SeqCst) {
            kill_tree(&mut child);
            return TreeRun::Cancelled;
        }
        if Instant::now() >= deadline {
            kill_tree(&mut child);
            return TreeRun::TimedOut;
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn sh(script: &str) -> Vec<String> {
        vec!["sh".into(), "-c".into(), script.into()]
    }

    fn soon(ms: u64) -> Instant {
        Instant::now() + Duration::from_millis(ms)
    }

    #[test]
    fn tree_run_passed() {
        let r = run(
            &sh("exit 0"),
            Path::new("."),
            soon(5_000),
            &AtomicBool::new(false),
        );
        assert_eq!(r, TreeRun::Passed);
    }

    #[test]
    fn tree_run_failed_summarises_the_last_line() {
        let r = run(
            &sh("echo 'a.ts: bad'; echo '3 problems' >&2; exit 1"),
            Path::new("."),
            soon(5_000),
            &AtomicBool::new(false),
        );
        assert_eq!(r, TreeRun::Failed("3 problems".into()));
    }

    #[test]
    fn tree_run_spawn_error() {
        let r = run(
            &["amont-no-such-program-xyz".to_string()],
            Path::new("."),
            soon(5_000),
            &AtomicBool::new(false),
        );
        assert!(matches!(r, TreeRun::Spawn(_)), "{r:?}");
    }

    /// The deadline kills the WHOLE group: a grandchild the tool forked must
    /// not survive it (it could still be writing the cache).
    #[test]
    fn tree_run_timed_out_leaves_no_child_alive() {
        let dir = std::env::temp_dir().join(format!("amont-tree-run-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pidfile = dir.join("grandchild.pid");
        let script = format!("sleep 30 & echo $! > {}; wait", pidfile.display());
        let r = run(&sh(&script), &dir, soon(400), &AtomicBool::new(false));
        assert_eq!(r, TreeRun::TimedOut);
        let pid: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        // SAFETY: probing a pid with signal 0.
        let alive = unsafe { libc_kill(pid, 0) == 0 };
        assert!(!alive, "grandchild {pid} survived the deadline");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tree_run_cancelled() {
        let flag = Arc::new(AtomicBool::new(false));
        let setter = Arc::clone(&flag);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            setter.store(true, Ordering::SeqCst);
        });
        let r = run(&sh("sleep 30"), Path::new("."), soon(10_000), &flag);
        assert_eq!(r, TreeRun::Cancelled);
    }
}
