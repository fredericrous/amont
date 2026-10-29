//! Host slots: one machine-wide queue for every heavy job on the host
//! (amont ADR-0014, decisions ADR-0049). The contract is `docs/host-slots.md`.
//!
//! A heavy job — a test suite, a build, a type checker — already uses every
//! core. Several of them at once, from several worktrees, sessions and
//! agents, thrash the same cores until their own timeouts fire. So each takes
//! a slot first: a `flock` on `slot-N` under one fixed per-user directory.
//!
//! What lives in the directory:
//!
//! ```text
//! slot-0 … slot-{cap-1}   the slots; held = flocked
//! admission               the one admission lock (taken without blocking)
//! admitted-at             when the last holder was admitted, for the ramp
//! queue/<nanos>-<pid>     a waiter's ticket, flocked for as long as it waits
//! holders/slot-N.who      who holds slot N (JSON), while it does
//! waits.jsonl             one line per acquisition, for measurement
//! ```
//!
//! Only the process that took a slot holds it: every descriptor here is
//! opened close-on-exec (Rust's default), so a daemon a build leaves behind
//! never keeps a slot.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Exported to the command a slot holder runs; an amont (or a wrapper)
/// started under it does not queue again for the slot its parent holds.
pub const HELD_ENV: &str = "AMONT_HOST_SLOT";

/// A diagnostic, and the seam tests use: the slot directory.
pub const DIR_ENV: &str = "AMONT_SLOT_DIR";

/// At most one new holder per this long under automatic sizing: the
/// one-minute load average lags, and after 10 s shows only ~15% of a new
/// holder's load, so a burst of waiters on an idle host would all pile in.
pub const RAMP: Duration = Duration::from_secs(60);

/// How many slots THIS process holds, across its threads.
static HELD: AtomicUsize = AtomicUsize::new(0);

/// Slots this process holds right now.
pub fn held_in_process() -> usize {
    HELD.load(Ordering::SeqCst)
}

/// How many slots there are, and whether the load decides admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sizing {
    /// `amont.hostSlots` unset: at most `max(1, cores/2)`; a free slot is
    /// taken only while the one-minute load is below the core count, at most
    /// one new holder per [`RAMP`].
    Auto,
    /// `amont.hostSlots N`: exactly N, no load test, no ramp.
    Fixed(u32),
}

impl Sizing {
    /// How many slot files count on this machine.
    pub fn cap(self) -> u32 {
        self.cap_for(cores())
    }

    /// How many slot files count on a machine with `cores` cores: the one
    /// admission reads, so the cap and the load test see the same machine.
    pub fn cap_for(self, cores: u32) -> u32 {
        match self {
            Sizing::Auto => (cores / 2).max(1),
            Sizing::Fixed(n) => n.max(1),
        }
    }
}

/// Logical cores this process may run on.
pub fn cores() -> u32 {
    std::thread::available_parallelism()
        .map(|n| u32::try_from(n.get()).unwrap_or(u32::MAX))
        .unwrap_or(1)
}

/// The one-minute load average, where it can be read.
/// Copied from amont-runtime's `load.rs` (a leaf reading, ADR-0039 allows
/// the copy): a crate with no dependencies cannot reach it.
pub fn load1() -> Option<f64> {
    platform::load1()
}

/// The slot directory: `AMONT_SLOT_DIR`, else `$XDG_RUNTIME_DIR/amont-slots`
/// on Linux, else `/tmp/amont-slots-<uid>`. A FIXED path, never
/// `std::env::temp_dir()`, which follows a `$TMPDIR` that macOS and agent
/// sandboxes set per session — every session would get a queue of its own.
pub fn dir() -> PathBuf {
    if let Some(d) = std::env::var_os(DIR_ENV).filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    #[cfg(target_os = "linux")]
    if let Some(d) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(d).join("amont-slots");
    }
    PathBuf::from(format!("/tmp/amont-slots-{}", platform::uid()))
}

/// Make `dir` (and its `queue/` and `holders/`) if needed, then refuse it
/// unless it is a real directory this user owns with mode 0700: on a shared
/// `/tmp`, another user could create it first and hold every slot.
pub fn safe_dir(dir: &Path) -> Result<(), String> {
    platform::make_private(dir)?;
    platform::check_private(dir)?;
    for sub in ["queue", "holders"] {
        platform::make_private(&dir.join(sub))?;
    }
    Ok(())
}

/// Whether slots work on this platform (`flock`): not on Windows.
pub const SUPPORTED: bool = platform::SUPPORTED;

/// Who holds a slot, written to `holders/slot-N.who` while it does.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Holder {
    pub pid: u32,
    /// `amont`, `relais`, `amont slot run`, …
    pub tool: String,
    /// The repository or directory the job runs in.
    pub repo: String,
    /// What it runs, short: `cargo test`, `pre-commit-clippy`.
    pub command: String,
    /// The agent session, when one is known.
    pub session: String,
    pub started_ms: u128,
}

impl Holder {
    /// This process, running `command` for `tool`, in the current directory.
    pub fn here(tool: &str, command: &str) -> Holder {
        Holder {
            pid: std::process::id(),
            tool: tool.to_string(),
            repo: std::env::current_dir()
                .map(|d| d.display().to_string())
                .unwrap_or_default(),
            command: command.to_string(),
            session: std::env::var("CLAUDE_SESSION_ID").unwrap_or_default(),
            started_ms: now_ms(),
        }
    }

    /// `relais: cargo test (amont-wt-x, 4m)`, for a waiter's line.
    pub fn describe(&self, now_ms: u128) -> String {
        let repo = Path::new(&self.repo)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.repo.clone());
        let mins = now_ms.saturating_sub(self.started_ms) / 60_000;
        format!("{}: {} ({repo}, {mins}m)", self.tool, self.command)
    }

    /// The record as one JSON object: what `holders/slot-N.who` holds, and
    /// what `amont slots --json` lists.
    pub fn to_json(&self) -> String {
        format!(
            "{{\"pid\":{},\"tool\":\"{}\",\"repo\":\"{}\",\"command\":\"{}\",\"session\":\"{}\",\"started_ms\":{}}}",
            self.pid,
            escape(&self.tool),
            escape(&self.repo),
            escape(&self.command),
            escape(&self.session),
            self.started_ms
        )
    }

    fn from_json(s: &str) -> Option<Holder> {
        Some(Holder {
            pid: raw(s, "pid")?.parse().ok()?,
            tool: unescape(&raw(s, "tool")?),
            repo: unescape(&raw(s, "repo")?),
            command: unescape(&raw(s, "command")?),
            session: unescape(&raw(s, "session").unwrap_or_default()),
            started_ms: raw(s, "started_ms")?.parse().ok()?,
        })
    }
}

/// Held slots: released when dropped, and by the kernel if the process dies.
#[derive(Debug)]
pub struct Lease {
    files: Vec<File>,
    who: Vec<PathBuf>,
}

impl Lease {
    /// How many slots this lease holds.
    pub fn weight(&self) -> usize {
        self.files.len()
    }
}

impl Drop for Lease {
    /// The holder record first, then the locks, then the count: a releasing
    /// holder never deletes its successor's record, and the count never reads
    /// zero while this process still holds a flock.
    fn drop(&mut self) {
        for w in &self.who {
            let _ = fs::remove_file(w); // best-effort: a stale record is ignored by pid
        }
        let n = self.files.len();
        self.files.clear();
        HELD.fetch_sub(n, Ordering::SeqCst);
    }
}

/// What a waiter sees while it waits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Queue {
    /// This waiter's place: 0 is the head.
    pub position: usize,
    /// Waiters in line, this one included.
    pub waiting: usize,
    /// Who holds the slots now, where they said so.
    pub holders: Vec<Holder>,
    /// Slots held by a client that writes no record (amont ≤ 1.61, old
    /// relais).
    pub unknown: usize,
}

/// The answer to one [`Waiter::poll`].
#[derive(Debug)]
pub enum Poll {
    Granted(Lease),
    Waiting(Queue),
}

/// What the world looks like to one admission decision: the seam the tests
/// drive instead of the real load and clock.
#[derive(Debug, Clone, Copy)]
pub struct Probe {
    pub load1: Option<f64>,
    pub cores: u32,
    pub now_ms: u128,
}

impl Probe {
    pub fn now() -> Probe {
        Probe {
            load1: load1(),
            cores: cores(),
            now_ms: now_ms(),
        }
    }
}

/// A place in the queue, held across polls. Dropping it leaves the queue.
#[derive(Debug)]
pub struct Waiter {
    dir: PathBuf,
    sizing: Sizing,
    weight: u32,
    holder: Holder,
    ticket: Option<(PathBuf, File)>,
}

impl Waiter {
    /// Join the queue in [`dir`].
    pub fn new(sizing: Sizing, weight: u32, holder: Holder) -> io::Result<Waiter> {
        Waiter::new_in(&dir(), sizing, weight, holder)
    }

    /// Join the queue in `dir`. The ticket is created under a hidden name,
    /// locked, and only then renamed into the queue — a rename keeps the
    /// lock, so no visible ticket is ever unlocked, and cleanup (which removes
    /// visible tickets whose lock is free) can never take a live one.
    pub fn new_in(dir: &Path, sizing: Sizing, weight: u32, holder: Holder) -> io::Result<Waiter> {
        safe_dir(dir).map_err(io::Error::other)?;
        let queue = dir.join("queue");
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        let hidden = queue.join(format!(".new-{pid}-{seq}"));
        let file = platform::try_lock(&hidden)?
            .ok_or_else(|| io::Error::other("a fresh ticket was already locked"))?;
        let visible = queue.join(format!("{nanos:024}-{pid}-{seq}"));
        fs::rename(&hidden, &visible)?;
        Ok(Waiter {
            dir: dir.to_path_buf(),
            sizing,
            weight: weight.max(1),
            holder,
            ticket: Some((visible, file)),
        })
    }

    /// Ask once, without blocking.
    pub fn poll(&mut self) -> io::Result<Poll> {
        self.poll_with(Probe::now())
    }

    /// [`Waiter::poll`] against a given load and clock.
    pub fn poll_with(&mut self, probe: Probe) -> io::Result<Poll> {
        // The admission lock, without blocking: a stopped or stuck holder of
        // it must never hold a hook past its own deadline.
        let Some(_admission) = platform::try_lock(&self.dir.join("admission"))? else {
            return Ok(Poll::Waiting(Queue::default()));
        };
        let mine = self.ticket.as_ref().map(|(p, _)| p.clone());
        let live = live_tickets(&self.dir, mine.as_deref())?;
        let position = mine
            .as_ref()
            .and_then(|m| live.iter().position(|t| t == m))
            .unwrap_or(0);
        let cap = self.sizing.cap_for(probe.cores);
        let Probed { free, held } = probe_slots(&self.dir, cap)?;
        let weight = self.weight.min(cap) as usize;
        let admit = position == 0 && free.len() >= weight && {
            let floor = held.is_empty();
            match self.sizing {
                Sizing::Fixed(_) => true,
                Sizing::Auto => {
                    floor || {
                        let below = probe.load1.map_or(true, |l| l < f64::from(probe.cores));
                        let since = probe.now_ms.saturating_sub(admitted_at(&self.dir));
                        below && since >= RAMP.as_millis()
                    }
                }
            }
        };
        if !admit {
            let (holders, unknown) = read_holders(&self.dir, &held);
            return Ok(Poll::Waiting(Queue {
                position,
                waiting: live.len(),
                holders,
                unknown,
            }));
        }
        let mut files = Vec::new();
        let mut who = Vec::new();
        for (index, file) in free.into_iter().take(weight) {
            let path = self.dir.join("holders").join(format!("slot-{index}.who"));
            let _ = fs::write(&path, self.holder.to_json()); // best-effort: a missing record shows "unknown"
            who.push(path);
            files.push(file);
        }
        HELD.fetch_add(files.len(), Ordering::SeqCst);
        let _ = fs::write(self.dir.join("admitted-at"), probe.now_ms.to_string()); // best-effort: a lost stamp only shortens one ramp
        self.leave();
        Ok(Poll::Granted(Lease { files, who }))
    }

    fn leave(&mut self) {
        if let Some((path, file)) = self.ticket.take() {
            let _ = fs::remove_file(&path); // best-effort: cleanup removes an unlocked ticket
            drop(file);
        }
    }
}

impl Drop for Waiter {
    fn drop(&mut self) {
        self.leave();
    }
}

/// Wait up to `budget` for `weight` slots, polling. `Ok(None)` when the
/// budget ran out. `on_wait` sees each answer while waiting.
pub fn acquire(
    sizing: Sizing,
    weight: u32,
    holder: Holder,
    budget: Duration,
    mut on_wait: impl FnMut(&Queue),
) -> io::Result<Option<Lease>> {
    let started = Instant::now();
    let mut waiter = Waiter::new(sizing, weight, holder)?;
    loop {
        match waiter.poll()? {
            Poll::Granted(lease) => return Ok(Some(lease)),
            Poll::Waiting(queue) => on_wait(&queue),
        }
        if started.elapsed() >= budget {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// The host's slots right now, for `amont slots`.
#[derive(Debug, Clone, Default)]
pub struct Status {
    pub cap: u32,
    pub holders: Vec<Holder>,
    pub unknown: usize,
    pub waiting: usize,
}

/// Read the slots without taking any: under the admission lock when it is
/// free (so slot probes cannot race an admission), else from the records
/// alone.
pub fn status(sizing: Sizing) -> io::Result<Status> {
    status_in(&dir(), sizing)
}

pub fn status_in(dir: &Path, sizing: Sizing) -> io::Result<Status> {
    safe_dir(dir).map_err(io::Error::other)?;
    let cap = sizing.cap();
    let waiting = live_tickets_readonly(dir)?;
    let Some(_admission) = platform::try_lock(&dir.join("admission"))? else {
        let held: Vec<u32> = (0..cap).collect();
        let (holders, _) = read_holders(dir, &held);
        return Ok(Status {
            cap,
            holders,
            unknown: 0,
            waiting,
        });
    };
    let Probed { free, held } = probe_slots(dir, cap)?;
    drop(free);
    let (holders, unknown) = read_holders(dir, &held);
    Ok(Status {
        cap,
        holders,
        unknown,
        waiting,
    })
}

/// Append one acquisition to `waits.jsonl`: the source of the wait
/// percentiles the plan measures. Best-effort.
pub fn record_wait(dir: &Path, tool: &str, weight: u32, waited_ms: u128, outcome: &str) {
    let load = load1().map_or("null".to_string(), |l| format!("{l:.2}"));
    let line = format!(
        "{{\"at_ms\":{},\"tool\":\"{}\",\"weight\":{weight},\"waited_ms\":{waited_ms},\"load1\":{load},\"outcome\":\"{}\"}}\n",
        now_ms(),
        escape(tool),
        escape(outcome)
    );
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("waits.jsonl"))
    {
        let _ = f.write_all(line.as_bytes()); // best-effort: a lost line costs one sample
    } // else best-effort: an unopenable log costs one sample, never a slot
}

/// Milliseconds since the epoch.
pub fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn admitted_at(dir: &Path) -> u128 {
    fs::read_to_string(dir.join("admitted-at"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// Visible tickets in order, removing the ones whose waiter died (their
/// lock is free). `mine` is never probed: this process holds it.
fn live_tickets(dir: &Path, mine: Option<&Path>) -> io::Result<Vec<PathBuf>> {
    let mut live = Vec::new();
    for entry in fs::read_dir(dir.join("queue"))? {
        let path = entry?.path();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.starts_with('.') {
            continue;
        }
        if Some(path.as_path()) == mine {
            live.push(path);
            continue;
        }
        match platform::try_lock(&path) {
            Ok(Some(_dead)) => {
                let _ = fs::remove_file(&path); // best-effort: a dead waiter's ticket
            }
            _ => live.push(path),
        }
    }
    live.sort();
    Ok(live)
}

/// How many visible tickets there are, without touching any.
fn live_tickets_readonly(dir: &Path) -> io::Result<usize> {
    Ok(fs::read_dir(dir.join("queue"))?
        .filter_map(Result::ok)
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .count())
}

/// The slots below the cap, as one probe found them.
struct Probed {
    /// The free ones, locked by the probe: the caller keeps or drops them.
    free: Vec<(u32, File)>,
    /// The indices of the held ones.
    held: Vec<u32>,
}

/// Lock every free slot below `cap`, and list the held ones. Called only
/// under the admission lock.
fn probe_slots(dir: &Path, cap: u32) -> io::Result<Probed> {
    let mut probed = Probed {
        free: Vec::new(),
        held: Vec::new(),
    };
    for index in 0..cap {
        match platform::try_lock(&dir.join(format!("slot-{index}")))? {
            Some(file) => probed.free.push((index, file)),
            None => probed.held.push(index),
        }
    }
    Ok(probed)
}

/// The records of the held slots whose writer is still alive; a held slot
/// with none is an old client's.
fn read_holders(dir: &Path, held: &[u32]) -> (Vec<Holder>, usize) {
    let mut holders = Vec::new();
    let mut unknown = 0;
    for index in held {
        let path = dir.join("holders").join(format!("slot-{index}.who"));
        match fs::read_to_string(&path)
            .ok()
            .and_then(|s| Holder::from_json(&s))
        {
            Some(h) if platform::alive(h.pid) => {
                if !holders.contains(&h) {
                    holders.push(h);
                }
            }
            _ => unknown += 1,
        }
    }
    (holders, unknown)
}

/// Escape `s` per the JSON spec. Copied from amont-quiet (a leaf utility,
/// ADR-0039 allows the copy).
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

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    out.push(ch);
                }
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// The raw value of top-level `key` in the flat objects this crate writes: a
/// number as written, a string without its quotes, escapes left in place.
fn raw(s: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":");
    let rest = &s[s.find(&pat)? + pat.len()..];
    if let Some(body) = rest.strip_prefix('"') {
        let mut end = 0;
        let bytes = body.as_bytes();
        while end < bytes.len() {
            match bytes[end] {
                b'\\' => end += 2,
                b'"' => return Some(body[..end].to_string()),
                _ => end += 1,
            }
        }
        None
    } else {
        let end = rest.find([',', '}']).unwrap_or(rest.len());
        Some(rest[..end].trim().to_string())
    }
}

#[cfg(unix)]
mod platform {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    use std::os::unix::io::AsRawFd;
    use std::path::Path;

    pub const SUPPORTED: bool = true;

    extern "C" {
        #[link_name = "flock"]
        fn libc_flock(fd: i32, op: i32) -> i32;
        #[link_name = "getuid"]
        fn libc_getuid() -> u32;
        #[link_name = "kill"]
        fn libc_kill(pid: i32, sig: i32) -> i32;
    }
    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;

    pub fn uid() -> u32 {
        // SAFETY: getuid takes nothing and cannot fail.
        unsafe { libc_getuid() }
    }

    pub fn alive(pid: u32) -> bool {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        if pid <= 0 {
            return false;
        }
        // SAFETY: signal 0 sends nothing; it only asks whether the pid exists.
        if unsafe { libc_kill(pid, 0) } == 0 {
            return true;
        }
        const ESRCH: i32 = 3;
        std::io::Error::last_os_error().raw_os_error() != Some(ESRCH)
    }

    pub fn make_private(dir: &Path) -> Result<(), String> {
        if dir.symlink_metadata().is_ok() {
            return Ok(());
        }
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))
    }

    pub fn check_private(dir: &Path) -> Result<(), String> {
        let meta = dir
            .symlink_metadata()
            .map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        if meta.file_type().is_symlink() {
            return Err(format!("{} is a symbolic link", dir.display()));
        }
        if !meta.is_dir() {
            return Err(format!("{} is not a directory", dir.display()));
        }
        if meta.uid() != uid() {
            return Err(format!("{} belongs to another user", dir.display()));
        }
        let mode = meta.permissions().mode() & 0o777;
        if mode != 0o700 {
            return Err(format!("{} has mode {mode:o}, not 700", dir.display()));
        }
        Ok(())
    }

    /// `Ok(None)` only when another holder has the lock (EWOULDBLOCK);
    /// every other failure is an error the caller reports. The file is
    /// opened close-on-exec (std's default), so no child inherits the lock.
    pub fn try_lock(path: &Path) -> std::io::Result<Option<std::fs::File>> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;
        // SAFETY: a valid fd we own; flock touches no memory.
        if unsafe { libc_flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0 {
            return Ok(Some(file));
        }
        let e = std::io::Error::last_os_error();
        if e.kind() == std::io::ErrorKind::WouldBlock {
            Ok(None)
        } else {
            Err(e)
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn load1() -> Option<f64> {
        extern "C" {
            #[link_name = "getloadavg"]
            fn getloadavg_raw(loadavg: *mut f64, nelem: i32) -> i32;
        }
        let mut avg = [0.0f64; 3];
        // SAFETY: a valid buffer of three f64; getloadavg writes at most three.
        let n = unsafe { getloadavg_raw(avg.as_mut_ptr(), 3) };
        (n >= 1).then_some(avg[0])
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub fn load1() -> Option<f64> {
        None
    }
}

#[cfg(not(unix))]
mod platform {
    use std::path::Path;

    pub const SUPPORTED: bool = false;

    pub fn uid() -> u32 {
        0
    }
    pub fn alive(_pid: u32) -> bool {
        true
    }
    pub fn make_private(_dir: &Path) -> Result<(), String> {
        Ok(())
    }
    pub fn check_private(_dir: &Path) -> Result<(), String> {
        Ok(())
    }
    pub fn try_lock(_path: &Path) -> std::io::Result<Option<std::fs::File>> {
        Ok(None)
    }
    pub fn load1() -> Option<f64> {
        None
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("amont-slots-{}-{tag}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&d); // best-effort: a fresh scratch dir
        d
    }

    fn idle(now_ms: u128) -> Probe {
        Probe {
            load1: Some(1.0),
            cores: 12,
            now_ms,
        }
    }

    fn busy(now_ms: u128) -> Probe {
        Probe {
            load1: Some(20.0),
            cores: 12,
            now_ms,
        }
    }

    fn granted(p: Poll) -> Lease {
        match p {
            Poll::Granted(l) => l,
            Poll::Waiting(q) => panic!("expected a grant, waiting at {q:?}"),
        }
    }

    fn waiting(p: Poll) -> Queue {
        match p {
            Poll::Waiting(q) => q,
            Poll::Granted(_) => panic!("expected to wait"),
        }
    }

    /// An empty host admits the first waiter whatever the load (the floor).
    #[test]
    fn the_floor_admits_the_first_waiter_even_when_busy() {
        let d = scratch("floor");
        let mut w = Waiter::new_in(&d, Sizing::Fixed(2), 1, Holder::here("t", "a")).unwrap();
        let lease = granted(w.poll_with(busy(1_000_000)).unwrap());
        assert_eq!(lease.weight(), 1);
    }

    /// Automatic: a second holder needs load below the cores AND the ramp.
    #[test]
    fn auto_admits_a_second_holder_only_when_idle_and_after_the_ramp() {
        let d = scratch("auto");
        let t0 = 10_000_000;
        let mut a = Waiter::new_in(&d, Sizing::Auto, 1, Holder::here("t", "a")).unwrap();
        let _first = granted(a.poll_with(idle(t0)).unwrap());
        let mut b = Waiter::new_in(&d, Sizing::Auto, 1, Holder::here("t", "b")).unwrap();
        waiting(b.poll_with(idle(t0 + 10_000)).unwrap()); // ramp not elapsed
        waiting(b.poll_with(busy(t0 + 61_000)).unwrap()); // load too high
        let _second = granted(b.poll_with(idle(t0 + 61_000)).unwrap());
    }

    /// Three long jobs on an idle host: the ramp admits one per 60 s, so the
    /// third is still waiting at 90 s (the wrapper's default `--max-wait`,
    /// where it exits 75) and gets its slot at 120 s.
    #[test]
    fn the_ramp_keeps_a_third_waiter_queued_past_ninety_seconds() {
        let d = scratch("three");
        let t0 = 50_000_000;
        let mut a = Waiter::new_in(&d, Sizing::Auto, 1, Holder::here("t", "a")).unwrap();
        let _a = granted(a.poll_with(idle(t0)).unwrap());
        let mut b = Waiter::new_in(&d, Sizing::Auto, 1, Holder::here("t", "b")).unwrap();
        let mut c = Waiter::new_in(&d, Sizing::Auto, 1, Holder::here("t", "c")).unwrap();
        let _b = granted(b.poll_with(idle(t0 + 60_000)).unwrap());
        waiting(c.poll_with(idle(t0 + 90_000)).unwrap());
        let _c = granted(c.poll_with(idle(t0 + 120_000)).unwrap());
    }

    /// A process holding the admission lock (stopped, or stuck on I/O) never
    /// blocks a waiter: the poll answers at once, waiting.
    #[test]
    fn a_held_admission_lock_never_blocks_a_poll() {
        let d = scratch("admission");
        let mut w = Waiter::new_in(&d, Sizing::Fixed(1), 1, Holder::here("t", "a")).unwrap();
        let _stuck = platform::try_lock(&d.join("admission")).unwrap().unwrap();
        let started = Instant::now();
        let q = waiting(w.poll_with(idle(0)).unwrap());
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(q, Queue::default());
    }

    /// The automatic cap follows the machine admission sees: half its
    /// cores, at least one.
    #[test]
    fn the_auto_cap_is_half_the_cores_at_least_one() {
        assert_eq!(Sizing::Auto.cap_for(12), 6);
        assert_eq!(Sizing::Auto.cap_for(2), 1);
        assert_eq!(Sizing::Auto.cap_for(1), 1);
        assert_eq!(Sizing::Fixed(3).cap_for(64), 3);
    }

    /// Fixed sizing ignores load and ramp, and stops at the cap.
    #[test]
    fn fixed_sizing_stops_at_the_cap() {
        let d = scratch("fixed");
        let mut a = Waiter::new_in(&d, Sizing::Fixed(1), 1, Holder::here("t", "a")).unwrap();
        let _l = granted(a.poll_with(busy(0)).unwrap());
        let mut b = Waiter::new_in(&d, Sizing::Fixed(1), 1, Holder::here("t", "b")).unwrap();
        let q = waiting(b.poll_with(idle(0)).unwrap());
        assert_eq!(q.holders.len(), 1, "{q:?}");
        assert_eq!(q.holders[0].command, "a");
    }

    /// Only the head of the queue may take a slot.
    #[test]
    fn only_the_oldest_ticket_is_served() {
        let d = scratch("order");
        let mut holder = Waiter::new_in(&d, Sizing::Fixed(1), 1, Holder::here("t", "h")).unwrap();
        let held = granted(holder.poll_with(idle(0)).unwrap());
        let mut first = Waiter::new_in(&d, Sizing::Fixed(1), 1, Holder::here("t", "1")).unwrap();
        let mut second = Waiter::new_in(&d, Sizing::Fixed(1), 1, Holder::here("t", "2")).unwrap();
        waiting(first.poll_with(idle(0)).unwrap());
        drop(held);
        let q = waiting(second.poll_with(idle(0)).unwrap());
        assert_eq!((q.position, q.waiting), (1, 2), "{q:?}");
        let _l = granted(first.poll_with(idle(0)).unwrap());
    }

    /// A waiter that went away leaves the queue; its ticket is not served.
    #[test]
    fn a_dropped_waiter_leaves_the_queue() {
        let d = scratch("dropped");
        let gone = Waiter::new_in(&d, Sizing::Fixed(1), 1, Holder::here("t", "gone")).unwrap();
        drop(gone);
        let mut next = Waiter::new_in(&d, Sizing::Fixed(1), 1, Holder::here("t", "next")).unwrap();
        let _l = granted(next.poll_with(idle(0)).unwrap());
    }

    /// Weight 2 is all or none.
    #[test]
    fn weight_is_all_or_none() {
        let d = scratch("weight");
        let mut one = Waiter::new_in(&d, Sizing::Fixed(2), 1, Holder::here("t", "1")).unwrap();
        let _l = granted(one.poll_with(idle(0)).unwrap());
        let mut two = Waiter::new_in(&d, Sizing::Fixed(2), 2, Holder::here("t", "2")).unwrap();
        waiting(two.poll_with(idle(0)).unwrap());
    }

    /// A record whose writer is gone is not a holder: the slot reads as an
    /// old client's.
    #[test]
    fn a_dead_holders_record_is_ignored() {
        let d = scratch("dead");
        safe_dir(&d).unwrap();
        let _held = platform::try_lock(&d.join("slot-0")).unwrap().unwrap();
        let dead = Holder {
            pid: i32::MAX as u32,
            tool: "t".into(),
            ..Holder::default()
        };
        fs::write(d.join("holders/slot-0.who"), dead.to_json()).unwrap();
        let s = status_in(&d, Sizing::Fixed(1)).unwrap();
        assert!(s.holders.is_empty(), "{s:?}");
        assert_eq!(s.unknown, 1);
    }

    /// A fresh directory is made 0700 and accepted; a 0755 one and a
    /// symlink are refused.
    #[test]
    fn only_a_private_directory_is_accepted() {
        use std::os::unix::fs::PermissionsExt;
        let d = scratch("private");
        assert_eq!(safe_dir(&d), Ok(()));
        fs::set_permissions(&d, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(safe_dir(&d).unwrap_err().contains("mode 755"));
        let link = scratch("link");
        std::os::unix::fs::symlink(&d, &link).unwrap();
        assert!(safe_dir(&link).unwrap_err().contains("symbolic link"));
        let _ = fs::remove_file(&link); // best-effort: scratch
        let _ = fs::remove_dir_all(&d); // best-effort: scratch
    }

    /// The record round-trips, escapes included.
    #[test]
    fn a_holder_record_round_trips() {
        let h = Holder {
            pid: 42,
            tool: "relais".into(),
            repo: "/w/a \"b\"".into(),
            command: "cargo test\tx".into(),
            session: "s".into(),
            started_ms: 7,
        };
        assert_eq!(Holder::from_json(&h.to_json()), Some(h));
    }

    /// A lease removes its records, and its slots are free again.
    #[test]
    fn a_dropped_lease_frees_its_slots_and_records() {
        let d = scratch("lease");
        let mut w = Waiter::new_in(&d, Sizing::Fixed(1), 1, Holder::here("t", "a")).unwrap();
        let lease = granted(w.poll_with(idle(0)).unwrap());
        assert!(d.join("holders/slot-0.who").exists());
        drop(lease);
        assert!(!d.join("holders/slot-0.who").exists());
        let mut again = Waiter::new_in(&d, Sizing::Fixed(1), 1, Holder::here("t", "b")).unwrap();
        let _l = granted(again.poll_with(idle(0)).unwrap());
    }
}
