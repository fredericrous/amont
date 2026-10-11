//! A quiet check's output: the whole of it on disk, a bounded excerpt in
//! memory (fleet plan 2026-10-11 "test output keeps its status", shared
//! rules).
//!
//! A captured check used to grow `slot.buf` with every byte its tool
//! printed and pour it all out at the end — a passing clippy was 3.4 KB of
//! download lines in an agent's context. Under quiet, the bytes go to
//! `<git-dir>/amont/logs/<check>.<pid>.log.tmp` as they arrive (tailable
//! while the check runs), and through the excerpt accumulator, which holds
//! at most 64 KB whatever the tool prints. When the check ends the file is
//! renamed to `.log`, and the slot prints one line on a pass or the
//! failure blocks on anything else.
//!
//! Storage is never allowed to change the verdict: a log that cannot be
//! created, written or renamed stops being written, the accumulator keeps
//! being fed, and the result says where the log is not.
//!
//! holds-until: amont installs no SIGINT/SIGTERM handler of its own for
//! this. `staged_only` already does, to bring back the user's parked
//! unstaged changes during the index-fidelity hold, and a second handler on
//! the commit path would compete with it. So an interrupted run leaves its
//! `<check>.<pid>.log.tmp` — complete up to the interrupt, and named by the
//! running line — and pruning never removes a `.tmp`. Revisit when amont
//! has one signal path that both can register with.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::excerpt::Accumulator;

/// Finished logs kept per check. `*.tmp` (a run in flight, or one a killed
/// process left) are never pruned.
const KEEP: usize = 10;

/// Where a check's log ended up.
pub enum Stored {
    Saved(PathBuf),
    /// The run is complete in the `.tmp`; only the rename failed.
    RenameFailed(PathBuf),
    NotWritten(String),
}

impl Stored {
    /// The line to print after the result, when the log is not where the
    /// result says it is.
    pub fn note(&self) -> Option<String> {
        match self {
            Stored::Saved(_) => None,
            Stored::RenameFailed(tmp) => Some(format!("log left at {}", tmp.display())),
            Stored::NotWritten(why) => Some(format!("log not written: {why}")),
        }
    }

    /// The path a reader can open, if any.
    pub fn path(&self) -> Option<&Path> {
        match self {
            Stored::Saved(p) | Stored::RenameFailed(p) => Some(p),
            Stored::NotWritten(_) => None,
        }
    }
}

pub struct Capture {
    pub tmp: PathBuf,
    done: PathBuf,
    file: Option<File>,
    err: Option<String>,
    written: u64,
    /// Bytes the tool printed, whether or not the file kept them.
    pub seen: u64,
    #[cfg(feature = "test-hooks")]
    fail_after: Option<u64>,
    pub acc: Accumulator,
}

/// `<git-dir>/amont/logs`, created 0700. `None` outside a repository.
pub fn log_dir() -> Option<PathBuf> {
    let git_dir = crate::git::stdout(&["rev-parse", "--absolute-git-dir"])?;
    Some(PathBuf::from(git_dir).join("amont").join("logs"))
}

impl Capture {
    /// Open `<dir>/<check>.<pid>.log.tmp`. A failure is remembered, not
    /// returned: the excerpt still works without a file.
    pub fn open(dir: Option<PathBuf>, check: &str) -> Capture {
        #[cfg(feature = "test-hooks")]
        let fail_after = std::env::var("AMONT_TEST_LOG_FAIL_AFTER")
            .ok()
            .and_then(|v| v.parse().ok());
        #[cfg(not(feature = "test-hooks"))]
        let fail_after = None;
        Capture::open_failing_after(dir, check, fail_after)
    }

    /// [`Capture::open`], with the write failure the tests inject passed in
    /// (only honoured under the `test-hooks` feature).
    pub fn open_failing_after(
        dir: Option<PathBuf>,
        check: &str,
        #[cfg_attr(not(feature = "test-hooks"), allow(unused_variables))] fail_after: Option<u64>,
    ) -> Capture {
        let name = file_name(check);
        let pid = std::process::id();
        let (tmp, done, file, err) = match dir {
            None => (
                PathBuf::new(),
                PathBuf::new(),
                None,
                Some("not inside a git repository".to_string()),
            ),
            Some(dir) => {
                let done = dir.join(format!("{name}.{pid}.log"));
                let tmp = dir.join(format!("{name}.{pid}.log.tmp"));
                match create(&dir, &tmp) {
                    Ok(f) => (tmp, done, Some(f), None),
                    Err(e) => (tmp, done, None, Some(format!("cannot create the log: {e}"))),
                }
            }
        };
        Capture {
            tmp,
            done,
            file,
            err,
            written: 0,
            seen: 0,
            #[cfg(feature = "test-hooks")]
            fail_after,
            acc: Accumulator::new(crate::excerpt::DEFAULT_BOUND),
        }
    }

    /// Whether a log file is being written.
    pub fn writing(&self) -> bool {
        self.file.is_some()
    }

    pub fn take(&mut self, bytes: &[u8]) {
        self.seen += bytes.len() as u64;
        self.acc.feed(bytes);
        let (chunk, injected) = self.clip(bytes);
        let Some(file) = self.file.as_mut() else {
            return;
        };
        match file.write_all(chunk) {
            Ok(()) => self.written += chunk.len() as u64,
            Err(e) => self.err = Some(format!("write failed: {e}")),
        }
        if injected && self.err.is_none() {
            self.err = Some(format!(
                "injected write failure after {} bytes",
                self.written
            ));
        }
        if self.err.is_some() {
            // Stop writing; the accumulator keeps being fed.
            self.file = None;
        }
    }

    #[cfg(feature = "test-hooks")]
    fn clip<'a>(&self, bytes: &'a [u8]) -> (&'a [u8], bool) {
        match self.fail_after {
            Some(limit) => {
                let left = limit.saturating_sub(self.written) as usize;
                if left < bytes.len() {
                    (&bytes[..left], true)
                } else {
                    (bytes, false)
                }
            }
            None => (bytes, false),
        }
    }

    #[cfg(not(feature = "test-hooks"))]
    fn clip<'a>(&self, bytes: &'a [u8]) -> (&'a [u8], bool) {
        (bytes, false)
    }

    /// Close the file and move it into place.
    pub fn finish(&mut self) -> Stored {
        let file = self.file.take();
        drop(file);
        if let Some(why) = self.err.take() {
            // A prefix of the run is not the run: do not leave it looking
            // like a log.
            if !self.tmp.as_os_str().is_empty() {
                let _ = fs::remove_file(&self.tmp);
            }
            return Stored::NotWritten(why);
        }
        match fs::rename(&self.tmp, &self.done) {
            Ok(()) => {
                if let Some(dir) = self.done.parent() {
                    prune(dir, &self.done);
                }
                Stored::Saved(self.done.clone())
            }
            Err(_) => Stored::RenameFailed(self.tmp.clone()),
        }
    }
}

fn create(dir: &Path, tmp: &Path) -> std::io::Result<File> {
    fs::create_dir_all(dir)?;
    let mut open = OpenOptions::new();
    open.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
        open.mode(0o600);
    }
    open.open(tmp)
}

/// A check name as a file name: `[A-Za-z0-9._-]`, everything else `_`.
fn file_name(check: &str) -> String {
    let clean: String = check
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(60)
        .collect();
    if clean.is_empty() {
        "check".into()
    } else {
        clean
    }
}

/// Keep the newest [`KEEP`] finished logs of the check `done` belongs to.
/// Best-effort: a log that cannot be listed or removed is left for the next
/// run, and nothing this run prints depends on it.
fn prune(dir: &Path, done: &Path) {
    let Some(name) = done.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    // `<check>.<pid>.log` → `<check>.`
    let Some(stem) = name.strip_suffix(".log").and_then(|s| s.rsplit_once('.')) else {
        return;
    };
    let prefix = format!("{}.", stem.0);
    let Ok(read) = fs::read_dir(dir) else { return };
    let mut logs: Vec<(std::time::SystemTime, PathBuf)> = read
        .filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.starts_with(&prefix)
                && n.ends_with(".log")
                && n[prefix.len()..n.len() - 4]
                    .bytes()
                    .all(|b| b.is_ascii_digit())
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    logs.sort();
    let excess = logs.len().saturating_sub(KEEP);
    for (_, old) in logs.into_iter().take(excess) {
        let _ = fs::remove_file(old);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("amont-capture-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_tmp_is_renamed_on_finish() {
        let dir = scratch("rename");
        let mut c = Capture::open(Some(dir.clone()), "pre-push-cargo-test");
        assert!(c.tmp.exists());
        c.take(b"hello\n");
        let stored = c.finish();
        let path = stored.path().unwrap().to_path_buf();
        assert!(path.exists() && !c.tmp.exists());
        assert_eq!(fs::read_to_string(path).unwrap(), "hello\n");
        assert!(stored.note().is_none());
    }

    #[test]
    fn no_directory_still_gives_an_excerpt() {
        let mut c = Capture::open(None, "x");
        c.take(b"thread 'a' panicked at src/a.rs:1:1:\nboom\n");
        let stored = c.finish();
        assert!(stored.note().unwrap().starts_with("log not written:"));
        assert!(c.acc.finish("-").contains("panicked at"));
    }

    #[test]
    fn pruning_keeps_ten_and_never_a_tmp_or_another_check() {
        let dir = scratch("prune");
        fs::create_dir_all(&dir).unwrap();
        for i in 0..15 {
            fs::write(dir.join(format!("clippy.{}.log", 1000 + i)), "x").unwrap();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        fs::write(dir.join("clippy.1.log.tmp"), "x").unwrap();
        fs::write(dir.join("clippy-extra.5.log"), "x").unwrap();
        let mut c = Capture::open(Some(dir.clone()), "clippy");
        c.finish();
        let names: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        let clippy = names
            .iter()
            .filter(|n| n.starts_with("clippy.") && n.ends_with(".log"))
            .count();
        assert_eq!(clippy, KEEP, "{names:?}");
        assert!(names.iter().any(|n| n == "clippy.1.log.tmp"));
        assert!(names.iter().any(|n| n == "clippy-extra.5.log"));
    }

    #[cfg(unix)]
    #[test]
    fn the_log_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("mode");
        let c = Capture::open(Some(dir), "x");
        let mode = fs::metadata(&c.tmp).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(feature = "test-hooks")]
    #[test]
    fn a_failed_write_keeps_the_excerpt() {
        let dir = scratch("inject");
        let mut c = Capture::open_failing_after(Some(dir), "x", Some(10));
        c.take(b"0123456789abcdef\nthread 'a' panicked at src/a.rs:1:1:\n");
        let stored = c.finish();
        assert!(stored.note().unwrap().contains("injected write failure"));
        assert!(!c.tmp.exists());
        assert!(c.acc.finish("-").contains("panicked at"));
    }

    #[test]
    fn fifty_megabytes_keep_the_accumulator_bounded() {
        let mut c = Capture::open(None, "x");
        let chunk: String = (0..1000).map(|i| format!("line {i} ok\n")).collect();
        let mut fed = 0u64;
        while fed < 50 * 1024 * 1024 {
            c.take(chunk.as_bytes());
            fed += chunk.len() as u64;
        }
        assert!(c.acc.held() <= 64 * 1024, "{}", c.acc.held());
        assert_eq!(c.seen, fed);
    }
}
