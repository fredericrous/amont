//! The active-run entry: while a quiet check runs, one small JSON file says
//! so, for relais's pane to tail its log (relais plan 2026-10-11 "a quiet run
//! is live in relais's pane"; the contract is relais `docs/quiet-runs.md`).
//!
//! `<dir>/<pid>-<n>.json`, `<dir>` being `$QUIET_ACTIVE_DIR`, else
//! `${XDG_STATE_HOME:-~/.local/state}/quiet/active` (`%LOCALAPPDATA%` on
//! Windows). Written when the capture opens; rewritten with `exit` and
//! `ended` when it ends, and LEFT: relais's watcher emits it and deletes it,
//! or a later sweep does once it is 60 s old, so a run that ends between two
//! of the watcher's polls is still seen with its exit.
//!
//! Nothing here may change a check's verdict or output: every failure is
//! silence, and the capture works without an entry.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::json;

/// An ended entry nobody emitted is removed after this long.
const ENDED_KEEP_MS: u128 = 60_000;

/// Disambiguates the concurrent pre-commit checks of one process.
static SEQ: AtomicU32 = AtomicU32::new(0);

/// Where entries live, or `None` when no location can be named.
pub fn dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("QUIET_ACTIVE_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    // A unit test never writes into the person's real state directory,
    // where a live relais pane would draw it.
    if cfg!(test) {
        return None;
    }
    #[cfg(windows)]
    {
        let base = std::env::var_os("LOCALAPPDATA")?;
        Some(PathBuf::from(base).join("quiet").join("active"))
    }
    #[cfg(not(windows))]
    {
        let base = match std::env::var_os("XDG_STATE_HOME").filter(|d| !d.is_empty()) {
            Some(d) => PathBuf::from(d),
            None => PathBuf::from(std::env::var_os("HOME")?)
                .join(".local")
                .join("state"),
        };
        Some(base.join("quiet").join("active"))
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// One live run's entry. Ending it twice (`end`, then `Drop`) writes once.
pub struct Entry {
    path: PathBuf,
    /// The fields written at start, `log` apart: the log is renamed when the
    /// run ends, and the ended entry names where it went.
    fields: Vec<String>,
    log: PathBuf,
    done: bool,
}

impl Entry {
    /// Sweep stale entries, then write this run's. `None` on any failure.
    pub fn start(dir: &Path, check: &str, repo: &str, log: &Path) -> Option<Entry> {
        // Only a run inside a Claude Code session is ever drawn: without
        // one, an entry would be litter (and a test suite's fixtures).
        let session = std::env::var("CLAUDE_CODE_SESSION_ID")
            .ok()
            .filter(|s| !s.is_empty())?;
        Entry::start_for(dir, check, repo, log, &session)
    }

    fn start_for(dir: &Path, check: &str, repo: &str, log: &Path, session: &str) -> Option<Entry> {
        create_dir(dir).ok()?;
        sweep(dir);
        let pid = std::process::id();
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let fields = vec![
            "\"v\":1".to_string(),
            json::string_field("tool", "amont"),
            json::string_field("session", session),
            json::string_field("check", check),
            json::string_field("repo", repo),
            json::int_field("pid", i64::from(pid)),
            format!("\"started\":{}", now_ms()),
        ];
        let entry = Entry {
            path: dir.join(format!("{pid}-{n}.json")),
            fields,
            log: log.to_path_buf(),
            done: false,
        };
        write_atomic(&entry.path, &entry.body(None)).ok()?;
        Some(entry)
    }

    fn body(&self, end: Option<(Option<i32>, u128)>) -> String {
        let mut fields = self.fields.clone();
        fields.push(json::string_field("log", &self.log.to_string_lossy()));
        if let Some((exit, ended)) = end {
            fields.push(json::opt_int_field("exit", exit.map(i64::from)));
            fields.push(format!("\"ended\":{ended}"));
        }
        json::object(&fields)
    }

    /// The run is over: add `exit` (`None`: killed, or no code) and `ended`,
    /// name the log where it now is (`final_log`, after the rename), and
    /// leave the file for the watcher or a later sweep.
    pub fn end(&mut self, exit: Option<i32>, final_log: Option<&Path>) {
        if self.done {
            return;
        }
        self.done = true;
        if let Some(p) = final_log {
            self.log = p.to_path_buf();
        }
        // A failed rewrite leaves the entry live-looking; the watcher then
        // ends it when the pid dies (relais plan: storage never changes a
        // check's verdict).
        let _ = write_atomic(&self.path, &self.body(Some((exit, now_ms()))));
    }
}

impl Drop for Entry {
    /// A capture dropped without `end` (an early return, a panic) must not
    /// leave an entry that looks live: it ends with no exit, which the pane
    /// draws as failed.
    fn drop(&mut self) {
        self.end(None, None);
    }
}

/// The entry directory, private. A directory that cannot be made 0700 gets
/// no entry: its contents would be readable by other users.
fn create_dir(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Write `<path>.tmp` (0600) and rename it over `path`: the watcher never
/// reads half a file.
fn write_atomic(path: &Path, body: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let mut open = OpenOptions::new();
    open.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open.mode(0o600);
    }
    let mut f = open.open(&tmp)?;
    f.write_all(body.as_bytes())?;
    drop(f);
    fs::rename(&tmp, path)
}

/// Remove this tool's entries whose run is gone without ending, and any
/// ended entry the watcher has not taken within [`ENDED_KEEP_MS`]. Best-
/// effort: a file that cannot be read or removed is left.
fn sweep(dir: &Path) {
    let Ok(read) = fs::read_dir(dir) else { return };
    let now = now_ms();
    for e in read.filter_map(|e| e.ok()) {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Some(v) = fs::read_to_string(&path)
            .ok()
            .and_then(|t| crate::json_read::parse(&t))
        else {
            continue;
        };
        let num = |k: &str| match v.get(k) {
            Some(crate::json_read::Value::Num(n)) => n.parse::<u128>().ok(),
            _ => None,
        };
        let stale = match num("ended") {
            Some(ended) => now.saturating_sub(ended) > ENDED_KEEP_MS,
            None => {
                v.get("tool").and_then(|t| t.as_str()) == Some("amont")
                    // A pid that does not fit is no process.
                    && num("pid").is_some_and(|pid| u32::try_from(pid).map_or(true, |p| !alive(p)))
            }
        };
        if stale {
            // Best-effort, like the rest of the sweep: the next one retries.
            let _ = fs::remove_file(&path);
        }
    }
}

/// Whether `pid` is a running process. `EPERM` means it exists. Off unix
/// the answer is "alive": the entry then waits for its own `ended`.
fn alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        // SAFETY: signal 0 sends nothing; it only asks whether the pid exists.
        if unsafe { kill(pid, 0) } == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(1) // EPERM
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("amont-active-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn read(path: &Path) -> crate::json_read::Value {
        crate::json_read::parse(&fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn an_entry_carries_the_documented_fields_and_ends_in_place() {
        let dir = scratch("fields");
        let log = dir.join("a \"quoted\" \\ path.log.tmp");
        let mut e = Entry::start_for(&dir, "clippy", "/r/repo", &log, "s-test").unwrap();
        let v = read(&e.path);
        for k in [
            "v", "tool", "session", "check", "repo", "log", "pid", "started",
        ] {
            assert!(v.get(k).is_some(), "missing {k}");
        }
        assert_eq!(v.get("tool").and_then(|t| t.as_str()), Some("amont"));
        assert_eq!(
            v.get("log").and_then(|t| t.as_str()),
            Some(&*log.to_string_lossy())
        );
        assert!(v.get("ended").is_none());
        let done = dir.join("final.log");
        e.end(Some(101), Some(&done));
        let v = read(&e.path);
        assert_eq!(
            v.get("log").and_then(|t| t.as_str()),
            Some(&*done.to_string_lossy())
        );
        assert!(matches!(v.get("exit"), Some(crate::json_read::Value::Num(n)) if n == "101"));
        assert!(v.get("ended").is_some());
        assert!(e.path.exists(), "an ended entry is left for the watcher");
    }

    #[test]
    fn two_entries_in_one_process_get_two_files() {
        let dir = scratch("two");
        let a = Entry::start_for(&dir, "a", "/r", &dir.join("a.log"), "s-test").unwrap();
        let b = Entry::start_for(&dir, "b", "/r", &dir.join("b.log"), "s-test").unwrap();
        assert_ne!(a.path, b.path);
        assert!(a.path.exists() && b.path.exists());
    }

    #[test]
    fn dropping_without_end_records_no_exit() {
        let dir = scratch("drop");
        let path = {
            let e = Entry::start_for(&dir, "x", "/r", &dir.join("x.log"), "s-test").unwrap();
            e.path.clone()
        };
        let v = read(&path);
        assert!(matches!(v.get("exit"), Some(crate::json_read::Value::Null)));
        assert!(v.get("ended").is_some());
    }

    #[test]
    fn the_sweep_takes_dead_and_old_ended_entries_only() {
        let dir = scratch("sweep");
        fs::create_dir_all(&dir).unwrap();
        let old = now_ms() - 120_000;
        let recent = now_ms() - 1_000;
        // pid 0x7fff_fff0 is not a process here.
        fs::write(
            dir.join("dead.json"),
            r#"{"tool":"amont","pid":2147483632}"#,
        )
        .unwrap();
        fs::write(
            dir.join("other-tool-dead.json"),
            r#"{"tool":"amont-agent","pid":2147483632}"#,
        )
        .unwrap();
        fs::write(
            dir.join("old.json"),
            format!(r#"{{"tool":"amont-agent","ended":{old}}}"#),
        )
        .unwrap();
        fs::write(
            dir.join("recent.json"),
            format!(r#"{{"tool":"amont","ended":{recent}}}"#),
        )
        .unwrap();
        let _live = Entry::start_for(&dir, "live", "/r", &dir.join("l.log"), "s-test").unwrap();
        assert!(!dir.join("dead.json").exists());
        assert!(
            dir.join("other-tool-dead.json").exists(),
            "another tool's live-looking entry is its own"
        );
        assert!(!dir.join("old.json").exists());
        assert!(dir.join("recent.json").exists());
    }

    #[test]
    fn quiet_entries_never_reach_the_real_state_dir() {
        // .cargo/config.toml: an agent running this suite must not put its
        // fixtures in the person's live relais pane.
        assert_eq!(std::env::var("CLAUDE_CODE_SESSION_ID").as_deref(), Ok(""));
        assert!(std::env::var("QUIET_ACTIVE_DIR").is_ok_and(|d| d.contains("target")));
        assert!(Entry::start(&scratch("nosession"), "x", "/r", Path::new("/l")).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn the_entry_and_its_dir_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("mode");
        let e = Entry::start_for(&dir, "x", "/r", &dir.join("x.log"), "s-test").unwrap();
        assert_eq!(
            fs::metadata(&e.path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn an_unwritable_dir_gives_no_entry_and_no_error() {
        let file = scratch("blocked");
        fs::write(&file, "not a dir").unwrap();
        assert!(
            Entry::start_for(&file.join("sub"), "x", "/r", Path::new("/l"), "s-test").is_none()
        );
    }
}
