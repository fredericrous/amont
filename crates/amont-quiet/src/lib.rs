//! The quiet-run active entry: while a quiet check runs, one small JSON file
//! says so, for a pane (relais's) to tail its log. amont owns the contract,
//! `docs/quiet-runs.md` (decisions ADR-0039), and this crate is its one
//! implementation: amont and amont-agent write through it, relais finds the
//! directory through it.
//!
//! `<dir>/<pid>-<n>.json`, `<dir>` being `$QUIET_ACTIVE_DIR`, else
//! `${XDG_STATE_HOME:-$HOME/.local/state}/quiet/active` (`%LOCALAPPDATA%` on
//! Windows). Written when the capture opens; rewritten with `exit` and
//! `ended` when it ends, and LEFT: a reader may delete it by its own
//! protocol, or a later sweep does once it is 60 s old, so a run that ends
//! between two of a reader's polls is still seen with its exit.
//!
//! Nothing here may change a check's verdict or output: every failure is
//! silence, and the caller works without an entry.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// An ended entry nobody emitted is removed after this long.
const ENDED_KEEP_MS: u128 = 60_000;

/// A temp file older than this was left by a writer that died mid-write.
const TEMP_KEEP_MS: u128 = 3_600_000;

/// Disambiguates the concurrent checks of one process.
static SEQ: AtomicU32 = AtomicU32::new(0);

/// Where entries live, read from `env`, or `None` when no location can be
/// named. An empty variable counts as unset: an empty `HOME` names no
/// directory, never a relative one under the working directory.
pub fn dir(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    dir_for(env, cfg!(windows))
}

/// [`dir`] from the process environment.
pub fn dir_from_env() -> Option<PathBuf> {
    dir(|k| std::env::var_os(k))
}

fn dir_for(env: impl Fn(&str) -> Option<OsString>, windows: bool) -> Option<PathBuf> {
    let get = |k: &str| env(k).filter(|v| !v.is_empty());
    if let Some(d) = get("QUIET_ACTIVE_DIR") {
        return Some(PathBuf::from(d));
    }
    if windows {
        return Some(
            PathBuf::from(get("LOCALAPPDATA")?)
                .join("quiet")
                .join("active"),
        );
    }
    let base = match get("XDG_STATE_HOME") {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(get("HOME")?).join(".local").join("state"),
    };
    Some(base.join("quiet").join("active"))
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
    /// The fields written at start, `log` apart: the log may be renamed when
    /// the run ends, and the ended entry names where it went.
    fields: Vec<String>,
    log: PathBuf,
    done: bool,
}

impl Entry {
    /// Sweep stale entries, then write this run's. `None` on any failure, and
    /// for an empty `session`: only a run inside a Claude Code session is
    /// ever drawn, so without one an entry would be litter. `repo` is asked
    /// only when an entry is written, so a caller outside a session pays
    /// nothing, not even the git call behind it.
    pub fn start<R: AsRef<str>>(
        dir: &Path,
        tool: &str,
        session: &str,
        check: &str,
        repo: impl FnOnce() -> R,
        log: &Path,
    ) -> Option<Entry> {
        if session.is_empty() {
            return None;
        }
        create_dir(dir).ok()?;
        sweep(dir, tool, now_ms());
        let pid = std::process::id();
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let fields = vec![
            "\"v\":1".to_string(),
            string_field("tool", tool),
            string_field("session", session),
            string_field("check", check),
            string_field("repo", repo().as_ref()),
            format!("\"pid\":{pid}"),
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

    /// The entry file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn body(&self, end: Option<(Option<i32>, u128)>) -> String {
        let mut fields = self.fields.clone();
        fields.push(string_field("log", &self.log.to_string_lossy()));
        if let Some((exit, ended)) = end {
            fields.push(match exit {
                Some(code) => format!("\"exit\":{code}"),
                None => "\"exit\":null".to_string(),
            });
            fields.push(format!("\"ended\":{ended}"));
        }
        format!("{{{}}}", fields.join(","))
    }

    /// The run is over: add `exit` (`None`: killed, or no code) and `ended`,
    /// name the log where it now is (`final_log`, after a rename), and leave
    /// the file for a reader or a later sweep. Only the first call writes.
    pub fn end(&mut self, exit: Option<i32>, final_log: Option<&Path>) {
        if self.done {
            return;
        }
        self.done = true;
        if let Some(p) = final_log {
            self.log = p.to_path_buf();
        }
        // A failed rewrite leaves the entry live-looking; a reader then ends
        // it when the pid dies. Storage never changes a check's verdict.
        let _ = write_atomic(&self.path, &self.body(Some((exit, now_ms())))); // best-effort: see above
    }
}

impl Drop for Entry {
    /// An entry dropped without `end` (an early return, a panic) must not
    /// look live: it ends with no exit, which a pane draws as failed.
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

/// The temp a write of `path` goes through: `.<name>.tmp`, which every
/// reader skips (a dotfile, and not `.json`).
fn temp_of(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.tmp"))
}

/// Write the temp (0600) and rename it over `path`, so a reader never reads
/// half a file. The temp is created new: an existing one, a symlink
/// included, fails the write instead of being followed or truncated. A temp
/// this call created and could not finish is removed; one it did not create
/// is never touched.
fn write_atomic(path: &Path, body: &str) -> std::io::Result<()> {
    let tmp = temp_of(path);
    let mut open = OpenOptions::new();
    open.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open.mode(0o600);
    }
    let mut f = open.open(&tmp)?;
    let written = f.write_all(body.as_bytes()).and_then(|()| f.sync_data());
    drop(f);
    let done = written.and_then(|()| fs::rename(&tmp, path));
    if done.is_err() {
        let _ = fs::remove_file(&tmp); // best-effort: the write failed anyway, and a sweep takes a stale temp
    }
    done
}

/// Whether `name` is a temp some writer leaves while it writes: this crate's
/// `.<id>.json.tmp`, and the two forms writers used before it, amont's
/// `<id>.json.tmp` and amont-agent's `.amont-agent-tmp-*.json`.
fn is_temp(name: &str) -> bool {
    name.ends_with(".json.tmp")
        || (name.starts_with(".amont-agent-tmp-") && name.ends_with(".json"))
}

/// Remove `tool`'s entries whose run is gone without ending, any ended entry
/// a reader has not taken within 60 s, and temps older than an hour. Another
/// tool's live-looking entry is its own to sweep. Best-effort: a file that
/// cannot be read or removed is left for the next sweep.
pub fn sweep(dir: &Path, tool: &str, now_ms: u128) {
    let Ok(read) = fs::read_dir(dir) else { return };
    for e in read.filter_map(|e| e.ok()) {
        let path = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if is_temp(&name) {
            let old = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .is_some_and(|m| now_ms.saturating_sub(m.as_millis()) > TEMP_KEEP_MS);
            if old {
                let _ = fs::remove_file(&path); // best-effort: the next sweep retries
            }
            continue;
        }
        if name.starts_with('.') || !name.ends_with(".json") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let stale = match field(&text, "ended").and_then(|n| n.parse::<u128>().ok()) {
            Some(ended) => now_ms.saturating_sub(ended) > ENDED_KEEP_MS,
            None => {
                field(&text, "tool") == Some(tool)
                    && field(&text, "pid")
                        .and_then(|p| p.parse::<i64>().ok())
                        .is_some_and(|pid| !alive(pid))
            }
        };
        if stale {
            let _ = fs::remove_file(&path); // best-effort: the next sweep retries
        }
    }
}

/// Whether `pid` may be a running process. A pid that is not one (zero,
/// negative, wider than an `i32`) is not alive, and is never passed to
/// `kill`, where zero and negative values name process groups. On unix a
/// pid is gone only when `kill(pid, 0)` says `ESRCH`; any other answer keeps
/// the entry, which a reader removes after an hour. Off unix every real pid
/// counts as alive, for the same reason.
pub fn alive(pid: i64) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    #[cfg(unix)]
    {
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        // SAFETY: signal 0 sends nothing; it only asks whether the pid exists.
        if unsafe { kill(pid, 0) } == 0 {
            return true;
        }
        const ESRCH: i32 = 3;
        std::io::Error::last_os_error().raw_os_error() != Some(ESRCH)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Escape `s` per the JSON spec: `"`, `\`, and control characters.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn string_field(key: &str, value: &str) -> String {
    format!("\"{}\":\"{}\"", escape(key), escape(value))
}

/// The raw value of top-level `key` in a flat JSON object: a number or
/// `null` as written, a string without its quotes (escapes left as they are,
/// enough for the plain names and numbers the sweep compares). A key is
/// matched only right after `{` or `,`, so the same text inside a string
/// value, where its quotes are escaped, never matches.
fn field<'t>(text: &'t str, key: &str) -> Option<&'t str> {
    let needle = format!("\"{key}\"");
    let mut from = 0;
    while let Some(i) = text[from..].find(&needle).map(|i| i + from) {
        from = i + needle.len();
        let before = text[..i].trim_end();
        if !(before.ends_with('{') || before.ends_with(',')) {
            continue;
        }
        let rest = text[from..].trim_start().strip_prefix(':')?.trim_start();
        if let Some(s) = rest.strip_prefix('"') {
            let mut esc = false;
            for (j, c) in s.char_indices() {
                match c {
                    '\\' if !esc => esc = true,
                    '"' if !esc => return Some(&s[..j]),
                    _ => esc = false,
                }
            }
            return None;
        }
        let end = rest
            .find(|c: char| c == ',' || c == '}' || c.is_whitespace())
            .unwrap_or(rest.len());
        return Some(&rest[..end]);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path under the temp dir, removed when the test ends (a directory
    /// or a file, whichever the test made of it).
    struct Scratch(PathBuf);

    impl std::ops::Deref for Scratch {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl AsRef<Path> for Scratch {
        fn as_ref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            if fs::remove_dir_all(&self.0).is_err() {
                let _ = fs::remove_file(&self.0); // test cleanup: a leftover is harmless
            }
        }
    }

    fn scratch(tag: &str) -> Scratch {
        let d = std::env::temp_dir().join(format!("amont-quiet-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d); // absent unless an earlier run was killed
        Scratch(d)
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[test]
    fn the_directory_follows_the_documented_precedence() {
        let unix = |p: &[(&str, &str)]| dir_for(env(p), false);
        let windows = |p: &[(&str, &str)]| dir_for(env(p), true);
        assert_eq!(
            unix(&[("QUIET_ACTIVE_DIR", "/q"), ("HOME", "/h")]),
            Some(PathBuf::from("/q"))
        );
        assert_eq!(
            unix(&[("XDG_STATE_HOME", "/s"), ("HOME", "/h")]),
            Some(PathBuf::from("/s/quiet/active"))
        );
        assert_eq!(
            unix(&[("HOME", "/h")]),
            Some(PathBuf::from("/h/.local/state/quiet/active"))
        );
        // An empty value is unset: an empty HOME names no directory at all,
        // never `.local/state` under the working directory.
        assert_eq!(unix(&[("HOME", ""), ("XDG_STATE_HOME", "")]), None);
        assert_eq!(
            unix(&[("QUIET_ACTIVE_DIR", ""), ("HOME", "/h")]),
            Some(PathBuf::from("/h/.local/state/quiet/active"))
        );
        assert_eq!(unix(&[]), None);
        assert_eq!(
            windows(&[("LOCALAPPDATA", "C:\\L"), ("HOME", "/h")]),
            Some(PathBuf::from("C:\\L").join("quiet").join("active"))
        );
        assert_eq!(windows(&[("LOCALAPPDATA", ""), ("HOME", "/h")]), None);
        assert_eq!(
            windows(&[("QUIET_ACTIVE_DIR", "/q")]),
            Some(PathBuf::from("/q"))
        );
    }

    #[test]
    fn an_entry_carries_the_documented_fields_and_ends_in_place() {
        let dir = scratch("fields");
        let log = dir.join("a \"quoted\" \\ path.log.tmp");
        let mut e = Entry::start(&dir, "amont", "s-test", "clippy", || "/r/repo", &log).unwrap();
        let t = read(e.path());
        assert!(t.starts_with("{\"v\":1,\"tool\":\"amont\",\"session\":\"s-test\",\"check\":\"clippy\",\"repo\":\"/r/repo\",\"pid\":"));
        assert!(t.contains(r#""log":"#) && t.contains(r#"a \"quoted\" \\ path.log.tmp"#));
        assert!(field(&t, "started").is_some());
        assert!(field(&t, "ended").is_none());
        let done = dir.join("final.log");
        e.end(Some(101), Some(&done));
        let t = read(e.path());
        assert_eq!(field(&t, "log"), Some(&*done.to_string_lossy()));
        assert_eq!(field(&t, "exit"), Some("101"));
        assert!(field(&t, "ended").is_some());
        e.end(Some(7), None);
        assert_eq!(
            field(&read(e.path()), "exit"),
            Some("101"),
            "only the first end writes"
        );
        assert!(e.path().exists(), "an ended entry is left for a reader");
    }

    #[test]
    fn the_tool_name_is_the_callers() {
        let dir = scratch("tool");
        let e = Entry::start(
            &dir,
            "amont-agent",
            "s",
            "true",
            String::new,
            &dir.join("l"),
        )
        .unwrap();
        assert_eq!(field(&read(e.path()), "tool"), Some("amont-agent"));
    }

    #[test]
    fn two_entries_in_one_process_get_two_files() {
        let dir = scratch("two");
        let a = Entry::start(&dir, "amont", "s", "a", || "/r", &dir.join("a.log")).unwrap();
        let b = Entry::start(&dir, "amont", "s", "b", || "/r", &dir.join("b.log")).unwrap();
        assert_ne!(a.path(), b.path());
        assert!(a.path().exists() && b.path().exists());
    }

    #[test]
    fn dropping_without_end_records_no_exit() {
        let dir = scratch("drop");
        let path = {
            let e = Entry::start(&dir, "amont", "s", "x", || "/r", &dir.join("x.log")).unwrap();
            e.path().to_path_buf()
        };
        let t = read(&path);
        assert_eq!(field(&t, "exit"), Some("null"));
        assert!(field(&t, "ended").is_some());
    }

    #[test]
    fn no_session_writes_nothing_and_never_asks_for_the_repo() {
        let dir = scratch("nosession");
        let e = Entry::start(
            &dir,
            "amont",
            "",
            "x",
            || -> &str { panic!("repo asked") },
            &dir.join("l"),
        );
        assert!(e.is_none());
        assert!(!dir.exists());
    }

    #[test]
    fn the_sweep_takes_dead_old_ended_and_stale_temp_files_only() {
        let dir = scratch("sweep");
        fs::create_dir_all(&dir).unwrap();
        let now = now_ms();
        let w = |n: &str, body: &str| fs::write(dir.join(n), body).unwrap();
        // pid 0x7fff_fff0 is not a process here.
        w("dead.json", r#"{"tool":"amont","pid":2147483632}"#);
        w("too-wide.json", r#"{"tool":"amont","pid":4294967296}"#);
        w(
            "other-tool-dead.json",
            r#"{"tool":"amont-agent","pid":2147483632}"#,
        );
        w(
            "self.json",
            &format!(r#"{{"tool":"amont","pid":{}}}"#, std::process::id()),
        );
        w(
            "old.json",
            &format!(r#"{{"tool":"amont-agent","ended":{}}}"#, now - 120_000),
        );
        w(
            "recent.json",
            &format!(r#"{{"tool":"amont","ended":{}}}"#, now - 1_000),
        );
        let temps = [".1-0.json.tmp", "1-1.json.tmp", ".amont-agent-tmp-1-2.json"];
        for t in temps {
            w(t, "");
        }
        sweep(&dir, "amont", now);
        for t in temps {
            assert!(dir.join(t).exists(), "a fresh temp {t} stays");
        }
        // Two hours on, the same temps are stale; the entries are judged
        // again too, so check them against the first sweep only.
        let gone = |n: &str| !dir.join(n).exists();
        assert_eq!(
            gone("dead.json"),
            cfg!(unix),
            "off unix every real pid is alive"
        );
        assert!(
            gone("too-wide.json"),
            "a pid wider than an i32 is no process"
        );
        assert!(
            !gone("other-tool-dead.json"),
            "another tool's live-looking entry is its own"
        );
        assert!(!gone("self.json"), "a live pid keeps its entry");
        assert!(gone("old.json"));
        assert!(!gone("recent.json"));
        sweep(&dir, "amont", now + 2 * TEMP_KEEP_MS);
        for t in temps {
            assert!(gone(t), "a stale temp {t} goes");
        }
    }

    #[test]
    fn a_pid_that_is_no_process_is_never_alive() {
        assert!(!alive(0));
        assert!(!alive(-1));
        assert!(!alive(i64::from(i32::MAX) + 1));
        assert!(alive(i64::from(std::process::id())));
    }

    #[test]
    fn a_field_inside_a_string_value_never_matches() {
        let t = r#"{"repo":"a,\"pid\":1","pid": 42 ,"tool":"amont"}"#;
        assert_eq!(field(t, "pid"), Some("42"));
        assert_eq!(field(t, "tool"), Some("amont"));
        assert_eq!(field(t, "ended"), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_entry_and_its_dir_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("mode");
        let e = Entry::start(&dir, "amont", "s", "x", || "/r", &dir.join("x.log")).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(e.path()), 0o600);
        assert_eq!(mode(&dir), 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn a_planted_temp_symlink_is_never_followed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("symlink");
        fs::create_dir_all(&dir).unwrap();
        let sentinel = scratch("sentinel");
        fs::write(&sentinel, "keep me").unwrap();
        fs::set_permissions(&sentinel, fs::Permissions::from_mode(0o644)).unwrap();
        let target = dir.join("9-9.json");
        std::os::unix::fs::symlink(&sentinel, temp_of(&target)).unwrap();
        assert!(write_atomic(&target, "{\"v\":1}").is_err());
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "keep me");
        assert_eq!(
            fs::metadata(&sentinel).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert!(!target.exists());
        assert!(
            fs::symlink_metadata(temp_of(&target)).is_ok(),
            "a temp this call did not create is left"
        );
    }

    #[test]
    fn a_write_that_cannot_rename_removes_its_own_temp() {
        let dir = scratch("rename");
        fs::create_dir_all(dir.join("9-8.json")).unwrap();
        let target = dir.join("9-8.json");
        // A directory in the way: the rename fails after the temp was made.
        fs::create_dir(target.join("x")).unwrap();
        assert!(write_atomic(&target, "{}").is_err());
        assert!(!temp_of(&target).exists(), "the temp it created is gone");
    }

    #[test]
    fn an_unwritable_dir_gives_no_entry_and_no_error() {
        let file = scratch("blocked");
        fs::write(&file, "not a dir").unwrap();
        assert!(Entry::start(
            &file.join("sub"),
            "amont",
            "s",
            "x",
            || "/r",
            Path::new("/l")
        )
        .is_none());
    }
}
