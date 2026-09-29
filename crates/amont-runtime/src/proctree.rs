//! How much CPU a check's process tree is spending — the second half of
//! "is this silent check stuck, or just quiet?" (ADR-0008, `hooks.liveness`).
//!
//! The silence budget used to be the whole test: a tool that printed nothing
//! for `amont.idleTimeout` was killed. vitest without a terminal prints its
//! summary and nothing before it, so a suite with every test passing looked
//! exactly like a hang. What a hang does NOT do is burn CPU, so this module
//! measures that, and the caller keeps a silent check alive while its tree is
//! measurably working.
//!
//! Three facts shape everything below, each measured, not assumed:
//!
//! - **A sum over LIVE processes goes down while a suite works.** A test
//!   runner that forks a worker per file loses each worker's CPU the moment
//!   it exits. So every process is counted WITH the children it has already
//!   reaped (Linux `cutime`/`cstime`, macOS `ri_child_*`), which only grows.
//! - **Reaping moves CPU from child to parent.** A child credited while alive
//!   would be credited again when its parent's reaped-children counter jumps.
//!   [`Tracker`] subtracts a vanished process's last known total from its
//!   nearest surviving ancestor, so old work is never counted twice — however
//!   late the reaping, at any depth.
//! - **macOS reports mach ticks, not nanoseconds.** On Apple Silicon one tick
//!   is 41.67 ns; read as nanoseconds a busy suite looks ~42x idle. The
//!   timebase is applied on every read.
//!
//! No process is spawned and no crate is added (the crate is dependency-free,
//! decisions:ADR-0020): Linux reads `/proc/<pid>/stat`, macOS calls libproc,
//! which `std` already links. Everything else gets [`Snapshot::Unavailable`]
//! and the caller falls back to the silence-only rule.
//!
//! A snapshot is BOUNDED — a process cap, a retry cap and a wall deadline
//! ([`Limits`]) — and anything that hits a bound is [`Snapshot::Partial`],
//! which earns no credit. The figures are a heuristic answering "is anything
//! working?", not CPU accounting.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

/// A process, as distinct from its pid: pids are reused, and a reused pid
/// must not inherit the old process's CPU. `start` is the kernel's own start
/// stamp (Linux `starttime`, macOS `ri_proc_start_abstime`) — compared for
/// equality only, never converted to wall time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Id {
    pub pid: u32,
    pub start: u64,
}

/// One process in a snapshot: who it is, whose child it is, and the CPU it
/// and every child it has reaped have used, in nanoseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Proc {
    pub id: Id,
    pub ppid: u32,
    pub cpu_ns: u64,
}

/// What one look at the tree produced. Only `Complete` is ever measured
/// from: a snapshot that stopped short would read a missing process as zero
/// work, which is the one error this module must never make in the
/// "stuck" direction.
#[derive(Debug, PartialEq, Eq)]
pub enum Snapshot {
    Complete(Vec<Proc>),
    /// A bound was hit, or part of the tree could not be listed.
    Partial,
    /// The root itself could not be read (gone, or not measurable here).
    Unavailable,
}

/// The bounds on one snapshot. The sampler runs on its own thread, so these
/// protect nothing about the ceiling clock; they keep one look at a huge or
/// churning tree from turning into an unbounded one.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_procs: usize,
    pub max_retries: u32,
    pub deadline: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_procs: 4096,
            max_retries: 2,
            deadline: Duration::from_millis(100),
        }
    }
}

/// What `read` found at a pid.
pub enum Read {
    Proc(Proc),
    /// Exited between being listed and being read — skipped, not an error.
    Gone,
    /// Could not be read for another reason.
    Failed,
}

/// Walk the tree under `root`, plus every `seen` process still alive, within
/// `limits`. Platform code supplies how to list a pid's children and how to
/// read one pid; the walk, the bounds and the reuse guard live here so they
/// are tested on every OS.
///
/// `read` gets the pid and the parent it was reached from (macOS reads no
/// ppid of its own). A `seen` process whose pid now carries a different
/// start stamp was reused and is skipped. A `seen` process reparented away
/// keeps counting, with its children — which is the only way an orphaned
/// worker's CPU stays in view.
pub fn walk(
    root: u32,
    seen: &[Id],
    limits: &Limits,
    clock: &mut dyn FnMut() -> Instant,
    children_of: &mut dyn FnMut(u32) -> Option<Vec<u32>>,
    read: &mut dyn FnMut(u32, u32) -> Read,
) -> Snapshot {
    let started = clock();
    let root_proc = match read(root, 0) {
        Read::Proc(p) => p,
        Read::Gone | Read::Failed => return Snapshot::Unavailable,
    };
    let mut out = vec![root_proc];
    let mut have: HashSet<u32> = HashSet::from([root]);
    let mut queue: VecDeque<u32> = VecDeque::from([root]);
    let mut orphans = seen.iter().filter(|id| id.pid != root);
    loop {
        let parent = match queue.pop_front() {
            Some(p) => p,
            None => {
                // The tree under the root is done; pick up the next seen
                // process that this walk has not reached.
                let Some(id) = orphans.by_ref().find(|id| !have.contains(&id.pid)) else {
                    break;
                };
                if clock().duration_since(started) > limits.deadline {
                    return Snapshot::Partial;
                }
                match read(id.pid, 1) {
                    Read::Proc(p) if p.id == *id => {
                        have.insert(p.id.pid);
                        out.push(p);
                        if out.len() > limits.max_procs {
                            return Snapshot::Partial;
                        }
                        queue.push_back(p.id.pid);
                    }
                    Read::Proc(_) | Read::Gone => {}
                    Read::Failed => return Snapshot::Partial,
                }
                continue;
            }
        };
        let Some(kids) = children_of(parent) else {
            return Snapshot::Partial;
        };
        for kid in kids {
            if !have.insert(kid) {
                continue;
            }
            if clock().duration_since(started) > limits.deadline {
                return Snapshot::Partial;
            }
            match read(kid, parent) {
                Read::Proc(p) => {
                    out.push(p);
                    if out.len() > limits.max_procs {
                        return Snapshot::Partial;
                    }
                    queue.push_back(kid);
                }
                Read::Gone => {}
                Read::Failed => return Snapshot::Partial,
            }
        }
    }
    Snapshot::Complete(out)
}

/// Parse one `/proc/<pid>/stat` into a [`Proc`]. Bytes, not a string: the
/// command name is up to 15 arbitrary bytes and may hold `)`, spaces, a
/// newline or non-UTF-8, so the fields are found after the LAST `)`.
/// After it: state [0], ppid [1], utime/stime/cutime/cstime [11..=14],
/// starttime [19]. cutime/cstime are printed signed; a negative one is 0.
pub fn parse_proc_stat(stat: &[u8], tick_ns: u64) -> Option<Proc> {
    let open = stat.iter().position(|&b| b == b'(')?;
    let close = stat.iter().rposition(|&b| b == b')')?;
    let pid: u32 = std::str::from_utf8(&stat[..open])
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let rest = std::str::from_utf8(stat.get(close + 1..)?).ok()?;
    let f: Vec<&str> = rest.split_ascii_whitespace().collect();
    let ppid: u32 = f.get(1)?.parse().ok()?;
    let unsigned = |i: usize| -> Option<u64> { f.get(i)?.parse().ok() };
    let signed = |i: usize| -> Option<u64> {
        let v: i64 = f.get(i)?.parse().ok()?;
        Some(u64::try_from(v).unwrap_or(0))
    };
    let ticks = unsigned(11)?
        .checked_add(unsigned(12)?)?
        .checked_add(signed(13)?)?
        .checked_add(signed(14)?)?;
    Some(Proc {
        id: Id {
            pid,
            start: unsigned(19)?,
        },
        ppid,
        cpu_ns: ticks.checked_mul(tick_ns)?,
    })
}

/// How much work one window between two complete snapshots showed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub start: Instant,
    pub end: Instant,
    pub gain_ns: u64,
}

impl Window {
    /// The window's work in thousandths of one core (1000 = one core busy
    /// for the whole window).
    pub fn milli_cores(&self) -> u32 {
        let wall = self.end.duration_since(self.start).as_nanos().max(1);
        let milli = u128::from(self.gain_ns) * 1000 / wall;
        u32::try_from(milli).unwrap_or(u32::MAX)
    }
}

/// What one sample told the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Observation {
    /// A complete snapshot with nothing to compare against: the first one,
    /// or the first after an incomplete one. Credits nothing.
    Baseline,
    /// Two consecutive complete snapshots, and the work between them.
    Window(Window),
    /// The snapshot was incomplete: nothing measured, and the next complete
    /// one starts over as a baseline.
    Unmeasured,
}

/// Turns successive snapshots into windows of work, remembering which
/// processes it has seen so a reparented worker keeps counting.
#[derive(Default)]
pub struct Tracker {
    prev: Option<(Instant, HashMap<Id, Proc>)>,
}

impl Tracker {
    /// The processes of the last complete snapshot: the next walk re-reads
    /// them even if they are no longer under the root.
    pub fn seen(&self) -> Vec<Id> {
        self.prev
            .as_ref()
            .map(|(_, m)| m.keys().copied().collect())
            .unwrap_or_default()
    }

    pub fn observe(&mut self, now: Instant, snap: Snapshot) -> Observation {
        let Snapshot::Complete(procs) = snap else {
            self.prev = None;
            return Observation::Unmeasured;
        };
        let cur: HashMap<Id, Proc> = procs.into_iter().map(|p| (p.id, p)).collect();
        let Some((then, prev)) = self.prev.replace((now, cur)) else {
            return Observation::Baseline;
        };
        let cur = &self.prev.as_ref().expect("just replaced").1;
        Observation::Window(Window {
            start: then,
            end: now,
            gain_ns: gain_ns(&prev, cur),
        })
    }
}

/// The CPU spent between two complete snapshots.
///
/// - A process in both: what it gained, never negative (macOS zeroes the
///   reaped-children times on `exec`; a drop is not negative work).
/// - A process only in `cur`: new since `prev` (nothing joins a tree
///   later except by being born into it), so all of it counts.
/// - A process only in `prev` vanished. Its last known total was already
///   credited; its nearest ancestor still present absorbs that total when it
///   reaps it, so the total is subtracted from that ancestor's gain. Walking
///   up through ancestors that vanished too is what keeps a deep tree reaped
///   bottom-up from being counted once per level.
///
/// What stays heuristic: a process born and reaped between two samples was
/// never seen, so its whole CPU lands on its parent whenever the reaping
/// happens — at most one interval of its work, possibly in a later window.
pub fn gain_ns(prev: &HashMap<Id, Proc>, cur: &HashMap<Id, Proc>) -> u64 {
    let by_pid: HashMap<u32, &Proc> = prev.values().map(|p| (p.id.pid, p)).collect();
    let mut transferred: HashMap<Id, u64> = HashMap::new();
    for gone in prev.values().filter(|p| !cur.contains_key(&p.id)) {
        let mut up = gone.ppid;
        let mut hops = 0;
        while let Some(parent) = by_pid.get(&up) {
            if cur.contains_key(&parent.id) {
                *transferred.entry(parent.id).or_default() += gone.cpu_ns;
                break;
            }
            up = parent.ppid;
            hops += 1;
            if hops > prev.len() {
                break; // a ppid cycle cannot happen; do not trust that
            }
        }
    }
    let mut gain: u64 = 0;
    for p in cur.values() {
        let add = match prev.get(&p.id) {
            Some(old) => p
                .cpu_ns
                .saturating_sub(old.cpu_ns)
                .saturating_sub(transferred.get(&p.id).copied().unwrap_or(0)),
            None => p.cpu_ns,
        };
        gain = gain.saturating_add(add);
    }
    gain
}

/// One snapshot of the tree under `root` on this platform.
pub fn snapshot(root: u32, seen: &[Id], limits: &Limits) -> Snapshot {
    platform::snapshot(root, seen, limits)
}

/// Whether [`snapshot`] can measure anything on this platform at all.
pub const SUPPORTED: bool = cfg!(any(target_os = "linux", target_os = "macos"));

#[cfg(target_os = "linux")]
mod platform {
    use super::{parse_proc_stat, walk, Id, Limits, Proc, Read, Snapshot};
    use std::collections::HashMap;
    use std::time::Instant;

    // Externs rather than a dependency: `scripts/check-no-deps.sh` keeps the
    // crate crate-free, and `sysconf` takes and returns plain integers.
    extern "C" {
        #[link_name = "sysconf"]
        fn libc_sysconf_raw(name: i32) -> std::os::raw::c_long;
    }
    const SC_CLK_TCK: i32 = 2; // the same value in glibc and musl

    fn tick_ns() -> u64 {
        // SAFETY: sysconf takes an integer and returns one; no pointers.
        let hz = unsafe { libc_sysconf_raw(SC_CLK_TCK) };
        let hz = if hz > 0 { hz as u64 } else { 100 };
        1_000_000_000 / hz
    }

    pub(super) fn snapshot(root: u32, seen: &[Id], limits: &Limits) -> Snapshot {
        let started = Instant::now();
        let tick = tick_ns();
        // One pass over /proc: every process, keyed by pid, with children
        // listed per parent. Only `stat` is read — it never blocks on a
        // process in uninterruptible sleep, unlike `cmdline`.
        let Ok(dir) = std::fs::read_dir("/proc") else {
            return Snapshot::Unavailable;
        };
        let mut procs: HashMap<u32, Proc> = HashMap::new();
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for entry in dir.flatten() {
            if started.elapsed() > limits.deadline {
                return Snapshot::Partial;
            }
            let name = entry.file_name();
            let Some(pid) = name.to_str().and_then(|n| n.parse::<u32>().ok()) else {
                continue;
            };
            let Ok(bytes) = std::fs::read(format!("/proc/{pid}/stat")) else {
                continue; // exited since the directory was listed
            };
            if let Some(p) = parse_proc_stat(&bytes, tick) {
                children.entry(p.ppid).or_default().push(pid);
                procs.insert(pid, p);
            }
        }
        walk(
            root,
            seen,
            limits,
            &mut || started.max(Instant::now()),
            &mut |pid| Some(children.get(&pid).cloned().unwrap_or_default()),
            &mut |pid, _| match procs.get(&pid) {
                Some(p) => Read::Proc(*p),
                None => Read::Gone,
            },
        )
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{walk, Id, Limits, Proc, Read, Snapshot};
    use std::time::Instant;

    /// `struct rusage_info_v2` from `<sys/resource.h>`: a 16-byte uuid then
    /// eighteen `uint64_t`. Times are in MACH TICKS, not nanoseconds.
    #[repr(C)]
    #[derive(Default)]
    struct RusageInfoV2 {
        uuid: [u8; 16],
        user_time: u64,
        system_time: u64,
        pkg_idle_wkups: u64,
        interrupt_wkups: u64,
        pageins: u64,
        wired_size: u64,
        resident_size: u64,
        phys_footprint: u64,
        proc_start_abstime: u64,
        proc_exit_abstime: u64,
        child_user_time: u64,
        child_system_time: u64,
        child_pkg_idle_wkups: u64,
        child_interrupt_wkups: u64,
        child_pageins: u64,
        child_elapsed_abstime: u64,
        diskio_bytesread: u64,
        diskio_byteswritten: u64,
    }
    const _: () = assert!(std::mem::size_of::<RusageInfoV2>() == 160);
    const RUSAGE_INFO_V2: i32 = 2;

    #[repr(C)]
    #[derive(Default)]
    struct MachTimebaseInfo {
        numer: u32,
        denom: u32,
    }

    // libproc and mach live in libSystem, which std already links.
    extern "C" {
        #[link_name = "proc_pid_rusage"]
        fn proc_pid_rusage_raw(pid: i32, flavor: i32, buffer: *mut RusageInfoV2) -> i32;
        #[link_name = "proc_listchildpids"]
        fn proc_listchildpids_raw(ppid: i32, buffer: *mut i32, buffersize: i32) -> i32;
        #[link_name = "mach_timebase_info"]
        fn mach_timebase_info_raw(info: *mut MachTimebaseInfo) -> i32;
    }

    fn timebase() -> (u128, u128) {
        let mut tb = MachTimebaseInfo::default();
        // SAFETY: writes one two-u32 struct we own.
        let rc = unsafe { mach_timebase_info_raw(&mut tb) };
        if rc != 0 || tb.denom == 0 {
            return (1, 1);
        }
        (u128::from(tb.numer), u128::from(tb.denom))
    }

    fn rusage(pid: u32) -> Option<RusageInfoV2> {
        let mut ri = RusageInfoV2::default();
        let pid = i32::try_from(pid).ok()?;
        // SAFETY: the buffer is a correctly sized, owned `rusage_info_v2`
        // (size asserted above) for the flavor we name.
        let rc = unsafe { proc_pid_rusage_raw(pid, RUSAGE_INFO_V2, &mut ri) };
        (rc == 0).then_some(ri)
    }

    /// Children of `pid`, growing the buffer when it came back full, at most
    /// `retries` times. `proc_listchildpids` returns a COUNT of pids (unlike
    /// `proc_listpids`, which returns bytes).
    fn children(pid: u32, retries: u32) -> Option<Vec<u32>> {
        let ppid = i32::try_from(pid).ok()?;
        let mut cap: usize = 256;
        for _ in 0..=retries {
            let mut buf = vec![0i32; cap];
            let bytes = i32::try_from(cap * std::mem::size_of::<i32>()).ok()?;
            // SAFETY: `buf` holds `cap` i32 and we pass its size in bytes.
            let n = unsafe { proc_listchildpids_raw(ppid, buf.as_mut_ptr(), bytes) };
            if n < 0 {
                return None;
            }
            let n = n as usize;
            if n < cap {
                buf.truncate(n);
                return Some(
                    buf.into_iter()
                        .filter_map(|p| u32::try_from(p).ok())
                        .collect(),
                );
            }
            cap *= 4;
        }
        None
    }

    pub(super) fn snapshot(root: u32, seen: &[Id], limits: &Limits) -> Snapshot {
        let (numer, denom) = timebase();
        let to_ns = |ticks: u64| -> u64 {
            u64::try_from(u128::from(ticks) * numer / denom).unwrap_or(u64::MAX)
        };
        let retries = limits.max_retries;
        walk(
            root,
            seen,
            limits,
            &mut Instant::now,
            &mut |pid| children(pid, retries),
            &mut |pid, parent| match rusage(pid) {
                Some(ri) => Read::Proc(Proc {
                    id: Id {
                        pid,
                        start: ri.proc_start_abstime,
                    },
                    ppid: parent,
                    cpu_ns: to_ns(
                        ri.user_time
                            .saturating_add(ri.system_time)
                            .saturating_add(ri.child_user_time)
                            .saturating_add(ri.child_system_time),
                    ),
                }),
                // ESRCH (reaped) or EPERM (another user's): skip that pid.
                None => Read::Gone,
            },
        )
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    use super::{Id, Limits, Snapshot};

    pub(super) fn snapshot(_root: u32, _seen: &[Id], _limits: &Limits) -> Snapshot {
        Snapshot::Unavailable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(pid: u32) -> Id {
        Id {
            pid,
            start: 1000 + u64::from(pid),
        }
    }
    fn p(pid: u32, ppid: u32, cpu_ms: u64) -> Proc {
        Proc {
            id: id(pid),
            ppid,
            cpu_ns: cpu_ms * 1_000_000,
        }
    }
    fn map(ps: &[Proc]) -> HashMap<Id, Proc> {
        ps.iter().map(|p| (p.id, *p)).collect()
    }
    const MS: u64 = 1_000_000;

    // --- /proc/<pid>/stat parsing ---------------------------------------

    fn stat(pid: u32, comm: &[u8], ppid: u32, times: [i64; 4], start: u64) -> Vec<u8> {
        let mut v = format!("{pid} (").into_bytes();
        v.extend_from_slice(comm);
        v.extend_from_slice(
            format!(
                ") S {ppid} 1 1 0 -1 4194304 100 0 0 0 {} {} {} {} 20 0 1 0 {start} 1000 100",
                times[0], times[1], times[2], times[3]
            )
            .as_bytes(),
        );
        v
    }

    #[test]
    fn a_plain_stat_line_parses_to_self_plus_reaped_cpu() {
        let s = stat(42, b"node", 7, [100, 50, 30, 20], 555);
        let got = parse_proc_stat(&s, 10 * MS).unwrap();
        assert_eq!(
            got.id,
            Id {
                pid: 42,
                start: 555
            }
        );
        assert_eq!(got.ppid, 7);
        assert_eq!(got.cpu_ns, 200 * 10 * MS);
    }

    #[test]
    fn the_command_name_may_hold_parens_spaces_newlines_and_non_utf8() {
        for comm in [
            &b"node (vitest 1)"[..],
            b"a b\nc",
            b"x)y) z",
            &[0xff, 0xfe, b')'],
        ] {
            let s = stat(9, comm, 3, [1, 1, 1, 1], 77);
            let got = parse_proc_stat(&s, MS).unwrap_or_else(|| panic!("{comm:?}"));
            assert_eq!(
                (got.id.pid, got.ppid, got.id.start, got.cpu_ns),
                (9, 3, 77, 4 * MS)
            );
        }
    }

    #[test]
    fn a_negative_reaped_time_counts_as_zero() {
        let s = stat(5, b"sh", 1, [10, 0, -3, -1], 1);
        assert_eq!(parse_proc_stat(&s, MS).unwrap().cpu_ns, 10 * MS);
    }

    #[test]
    fn a_truncated_or_garbled_line_is_none() {
        assert!(parse_proc_stat(b"", MS).is_none());
        assert!(parse_proc_stat(b"12 (x) S 1", MS).is_none());
        assert!(
            parse_proc_stat(b"nope (x) S 1 1 1 0 -1 0 0 0 0 0 1 1 1 1 20 0 1 0 5", MS).is_none()
        );
    }

    // --- the bounded walk ------------------------------------------------

    struct World {
        procs: HashMap<u32, Proc>,
    }
    impl World {
        fn new(ps: &[Proc]) -> Self {
            World {
                procs: ps.iter().map(|p| (p.id.pid, *p)).collect(),
            }
        }
        fn kids(&self, pid: u32) -> Vec<u32> {
            let mut k: Vec<u32> = self
                .procs
                .values()
                .filter(|p| p.ppid == pid)
                .map(|p| p.id.pid)
                .collect();
            k.sort_unstable();
            k
        }
        fn walk(&self, root: u32, seen: &[Id], limits: &Limits) -> Snapshot {
            let t = Instant::now();
            walk(
                root,
                seen,
                limits,
                &mut || t,
                &mut |pid| Some(self.kids(pid)),
                &mut |pid, _| match self.procs.get(&pid) {
                    Some(p) => Read::Proc(*p),
                    None => Read::Gone,
                },
            )
        }
    }

    fn pids(s: &Snapshot) -> Vec<u32> {
        let Snapshot::Complete(v) = s else {
            panic!("{s:?}")
        };
        let mut out: Vec<u32> = v.iter().map(|p| p.id.pid).collect();
        out.sort_unstable();
        out
    }

    #[test]
    fn the_walk_takes_the_root_and_every_descendant_and_nothing_else() {
        let w = World::new(&[p(10, 1, 0), p(11, 10, 0), p(12, 11, 0), p(99, 1, 0)]);
        assert_eq!(pids(&w.walk(10, &[], &Limits::default())), vec![10, 11, 12]);
    }

    #[test]
    fn an_unreadable_root_is_unavailable() {
        let w = World::new(&[p(11, 10, 0)]);
        assert_eq!(w.walk(10, &[], &Limits::default()), Snapshot::Unavailable);
    }

    #[test]
    fn a_seen_orphan_keeps_counting_with_its_children_but_a_reused_pid_does_not() {
        // 12 was a grandchild; its parent exited and it was reparented to 1.
        let w = World::new(&[p(10, 1, 0), p(12, 1, 0), p(13, 12, 0), p(20, 1, 0)]);
        let reused = Id { pid: 20, start: 1 }; // pid 20 now belongs to someone else
        assert_eq!(
            pids(&w.walk(10, &[id(12), reused], &Limits::default())),
            vec![10, 12, 13]
        );
    }

    #[test]
    fn an_orphan_never_seen_before_reparenting_is_invisible() {
        let w = World::new(&[p(10, 1, 0), p(12, 1, 0)]);
        assert_eq!(pids(&w.walk(10, &[], &Limits::default())), vec![10]);
    }

    #[test]
    fn exceeding_the_process_cap_is_partial() {
        let mut ps = vec![p(10, 1, 0)];
        ps.extend((0..5).map(|i| p(100 + i, 10, 0)));
        let w = World::new(&ps);
        let limits = Limits {
            max_procs: 5,
            ..Limits::default()
        };
        assert_eq!(w.walk(10, &[], &limits), Snapshot::Partial);
        let limits = Limits {
            max_procs: 6,
            ..Limits::default()
        };
        assert!(matches!(w.walk(10, &[], &limits), Snapshot::Complete(_)));
    }

    #[test]
    fn passing_the_deadline_is_partial() {
        let ps = [p(10, 1, 0), p(11, 10, 0), p(12, 10, 0)];
        let w = World::new(&ps);
        let base = Instant::now();
        let mut ticks = 0u64;
        let got = walk(
            10,
            &[],
            &Limits::default(),
            &mut || {
                ticks += 1;
                base + Duration::from_millis(60 * ticks)
            },
            &mut |pid| Some(w.kids(pid)),
            &mut |pid, _| Read::Proc(w.procs[&pid]),
        );
        assert_eq!(got, Snapshot::Partial);
    }

    #[test]
    fn a_child_list_that_cannot_be_read_is_partial() {
        let w = World::new(&[p(10, 1, 0), p(11, 10, 0)]);
        let t = Instant::now();
        let got = walk(
            10,
            &[],
            &Limits::default(),
            &mut || t,
            &mut |_| None,
            &mut |pid, _| Read::Proc(w.procs[&pid]),
        );
        assert_eq!(got, Snapshot::Partial);
    }

    #[test]
    fn a_child_gone_between_listing_and_reading_is_skipped() {
        let w = World::new(&[p(10, 1, 0), p(11, 10, 0)]);
        let t = Instant::now();
        let got = walk(
            10,
            &[],
            &Limits::default(),
            &mut || t,
            &mut |pid| Some(if pid == 10 { vec![11, 12] } else { vec![] }),
            &mut |pid, _| w.procs.get(&pid).map_or(Read::Gone, |p| Read::Proc(*p)),
        );
        assert_eq!(pids(&got), vec![10, 11]);
    }

    // --- gain: what counts as work -----------------------------------------

    #[test]
    fn a_process_in_both_snapshots_counts_what_it_gained_never_less_than_zero() {
        let prev = map(&[p(10, 1, 100), p(11, 10, 500)]);
        let cur = map(&[p(10, 1, 350), p(11, 10, 0)]); // 11 exec'd: macOS zeroes
        assert_eq!(gain_ns(&prev, &cur), 250 * MS);
    }

    #[test]
    fn a_process_new_since_the_last_snapshot_counts_whole() {
        let prev = map(&[p(10, 1, 100)]);
        let cur = map(&[p(10, 1, 100), p(11, 10, 400)]);
        assert_eq!(gain_ns(&prev, &cur), 400 * MS);
    }

    #[test]
    fn fork_per_file_work_is_counted_through_the_parents_reaped_time() {
        // Workers born and reaped between samples: never seen, but the
        // parent's reaped-children counter carries their CPU.
        let prev = map(&[p(10, 1, 1000)]);
        let cur = map(&[p(10, 1, 9000)]);
        assert_eq!(gain_ns(&prev, &cur), 8000 * MS);
    }

    #[test]
    fn a_child_reaped_late_is_not_credited_again() {
        // 11 burnt 2 s long ago (credited then), slept, and is reaped now:
        // the parent's counter jumps by 2 s, and none of it is new work.
        let prev = map(&[p(10, 1, 100), p(11, 10, 2000)]);
        let cur = map(&[p(10, 1, 2100)]);
        assert_eq!(gain_ns(&prev, &cur), 0);
    }

    #[test]
    fn a_deep_tree_reaped_bottom_up_is_not_counted_once_per_level() {
        // Six levels, all credited while alive; all reaped in one window,
        // each parent carrying its descendants' totals up to the root.
        let chain: Vec<Proc> = (0..6)
            .map(|i| p(10 + i, if i == 0 { 1 } else { 9 + i }, 1000))
            .collect();
        let prev = map(&chain);
        let cur = map(&[p(10, 1, 6000)]);
        assert_eq!(gain_ns(&prev, &cur), 0);
    }

    #[test]
    fn new_work_alongside_a_late_reap_still_counts() {
        let prev = map(&[p(10, 1, 100), p(11, 10, 2000)]);
        let cur = map(&[p(10, 1, 2600)]); // 500 ms of the parent's own work
        assert_eq!(gain_ns(&prev, &cur), 500 * MS);
    }

    #[test]
    fn an_orphan_reaped_by_init_takes_nothing_from_the_tree() {
        let prev = map(&[p(10, 1, 100), p(12, 1, 3000)]);
        let cur = map(&[p(10, 1, 400)]);
        assert_eq!(gain_ns(&prev, &cur), 300 * MS);
    }

    // --- the tracker: baselines and resets -----------------------------

    #[test]
    fn the_first_complete_snapshot_is_only_a_baseline() {
        let mut t = Tracker::default();
        let now = Instant::now();
        assert_eq!(
            t.observe(now, Snapshot::Complete(vec![p(10, 1, 99_000)])),
            Observation::Baseline
        );
    }

    #[test]
    fn an_incomplete_snapshot_measures_nothing_and_the_next_one_rebaselines() {
        let mut t = Tracker::default();
        let t0 = Instant::now();
        let s = Duration::from_secs(1);
        t.observe(t0, Snapshot::Complete(vec![p(10, 1, 0)]));
        assert_eq!(
            t.observe(t0 + s, Snapshot::Partial),
            Observation::Unmeasured
        );
        assert!(t.seen().is_empty());
        assert_eq!(
            t.observe(t0 + 2 * s, Snapshot::Complete(vec![p(10, 1, 5000)])),
            Observation::Baseline
        );
        match t.observe(t0 + 3 * s, Snapshot::Complete(vec![p(10, 1, 5500)])) {
            Observation::Window(w) => {
                assert_eq!(w.gain_ns, 500 * MS);
                assert_eq!(w.start, t0 + 2 * s);
                assert_eq!(w.milli_cores(), 500);
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn the_tracker_remembers_what_it_saw_for_the_next_walk() {
        let mut t = Tracker::default();
        t.observe(
            Instant::now(),
            Snapshot::Complete(vec![p(10, 1, 0), p(12, 10, 0)]),
        );
        let mut seen = t.seen();
        seen.sort_by_key(|i| i.pid);
        assert_eq!(seen, vec![id(10), id(12)]);
    }

    #[test]
    fn four_busy_cores_read_as_four_thousand_milli_cores() {
        let t0 = Instant::now();
        let w = Window {
            start: t0,
            end: t0 + Duration::from_secs(10),
            gain_ns: 40 * 1_000 * MS,
        };
        assert_eq!(w.milli_cores(), 4000);
    }

    // --- the real platform source ----------------------------------------

    /// A child that burns CPU shows it while alive, and its parent carries
    /// it after reaping — on the platforms that measure at all. On Apple
    /// Silicon this is the test that catches reading mach ticks as ns.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn the_platform_sees_a_burning_child_and_its_reaped_time() {
        use std::process::Command;
        let me = std::process::id();
        let before = match snapshot(me, &[], &Limits::default()) {
            Snapshot::Complete(v) => v.iter().find(|p| p.id.pid == me).unwrap().cpu_ns,
            s => panic!("{s:?}"),
        };
        // A shell loop burning ~0.5 s of CPU, then exiting; we reap it.
        let status = Command::new("sh")
            .args(["-c", "i=0; end=$(( $(date +%s) + 1 )); while [ $(date +%s) -lt $end ]; do i=$((i+1)); done"])
            .status()
            .unwrap();
        assert!(status.success());
        let after = match snapshot(me, &[], &Limits::default()) {
            Snapshot::Complete(v) => v.iter().find(|p| p.id.pid == me).unwrap().cpu_ns,
            s => panic!("{s:?}"),
        };
        let reaped = after.saturating_sub(before);
        assert!(
            reaped >= 300 * MS,
            "reaped child CPU only {} ms",
            reaped / MS
        );
        assert!(
            reaped < 60_000 * MS,
            "implausible {} ms — wrong time unit?",
            reaped / MS
        );
    }
}
