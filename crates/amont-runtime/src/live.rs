//! One check, one block — and while it runs, one line: per-check output
//! capture plus a live progress region for the concurrent stage.
//!
//! Twenty checks used to print straight to inherited stdio from their own
//! threads, so two failing linters shuffled their lines together and the
//! reader un-shuffled them by hand — the dispatcher's roll-up existed partly
//! to apologise for it. Now every check writes into its own slot, and a
//! completed check's output reaches stdout as ONE locked write: contiguous,
//! whatever the other nineteen were doing.
//!
//! Three writers feed a slot:
//!
//! 1. The check's own thread, through [`say`] — which is what
//!    `common::ok/fail/warn` call. A thread with no slot installed (commit-msg,
//!    `amont install`, the dispatcher itself) prints directly, exactly as
//!    before; nothing outside a stage changes.
//! 2. A captured child's reader threads, through [`Stage::append_raw`] —
//!    they are not the check's thread, so the thread-local cannot carry the
//!    routing; the `Arc` is captured before the spawn instead.
//! 3. Nobody else. The dispatcher's own lines (skips, pins, the roll-up)
//!    happen strictly before or after the fan-out and stay direct.
//!
//! Order across checks is COMPLETION order — deterministic per block, not
//! per stage, which is the same nondeterminism the interleaved version had
//! without the shuffling. `amont.progress false` switches the whole
//! mechanism off and restores raw streaming for anyone who wants to watch a
//! tool write in real time.
//!
//! # The region
//!
//! When stderr is a real terminal ([`watching`]) the stage also paints a
//! live region UNDER the finished blocks: one line per running check —
//! braille spinner, name, elapsed — repainted every 80ms by a ticker
//! thread, shrinking as checks finish, gone without a trace when the stage
//! ends. Blocks go to stdout, the region to stderr; both feed one tty, and
//! every write to either happens under the same [`Stage::out`] lock, so a
//! block never tears a repaint in half. Piped, redirected, `TERM=dumb`, or
//! CI: [`watching`] is false, no ticker starts, and the region costs
//! nothing — which is also why the test suite (piped stdio throughout)
//! exercises capture but never the paint.

use std::cell::RefCell;
use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

/// The fleet spinner's frames (progress.rs) — cycled by elapsed time, so a
/// frame needs no state beyond the clock.
const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// The region never grows past this many check lines; the rest fold into
/// one `… and N more`. Twelve is the whole default fleet on one screen.
const MAX_LINES: usize = 12;

/// One check's place in the stage.
struct Slot {
    /// Sanitised at [`Stage::begin`]: a manifest-declared name is
    /// repo-derived text and the region writes it to a live terminal.
    name: String,
    /// Restamped by [`Stage::enter`], so a serial stage (pre-push) times
    /// each check from its own start, not the stage's.
    started: Instant,
    /// The last byte or line that landed in `buf` — what the region's
    /// `quiet` figure and the heartbeat's `last output` read.
    last_output: Instant,
    /// Elapsed seconds at which the non-tty heartbeat next speaks.
    next_beat: u64,
    buf: Vec<u8>,
    /// Entered and not yet finished — the region shows exactly these.
    running: bool,
    done: bool,
    /// The command this check is waiting on, while it runs: its output
    /// clock and what its CPU is doing — the same object the kill decision
    /// reads, so the displays can never disagree with it (ADR-0008).
    activity: Option<Arc<crate::hooks::common::Activity>>,
}

/// A running stage: the slots, and the one lock every terminal write inside
/// the stage goes through.
pub struct Stage {
    slots: Mutex<Vec<Slot>>,
    /// Serialises block emission and region repaints; the value is how many
    /// region lines are currently painted (what an erase must remove).
    out: Mutex<usize>,
    /// Painting at all? [`enabled`] && [`watching`], decided once at begin.
    live: bool,
    /// Is this the PUSH stage? Read by the heartbeat, which has something to
    /// say about a long gate there and nothing to say about one at commit
    /// time — see [`beat_line`]. Derived from the names, which already
    /// carry the trigger.
    on_push: bool,
    stop: AtomicBool,
}

thread_local! {
    /// Where [`say`] routes on THIS thread: a stage and a slot index.
    static SINK: RefCell<Option<(Arc<Stage>, usize)>> = const { RefCell::new(None) };
}

impl Stage {
    /// A stage over `names`, in dispatch order. Does nothing visible until
    /// checks start entering (the region) or finishing (the blocks).
    pub fn begin(settings: &crate::config::Settings, names: &[&str]) -> Arc<Stage> {
        let now = Instant::now();
        let stage = Arc::new(Stage {
            slots: Mutex::new(
                names
                    .iter()
                    .map(|n| Slot {
                        // Every name in a stage carries the stage's own
                        // prefix ("pre-commit-clippy"); the region drops it
                        // — twelve identical prefixes say nothing.
                        name: crate::ui::sanitize(
                            n.strip_prefix("pre-commit-")
                                .or_else(|| n.strip_prefix("pre-push-"))
                                .unwrap_or(n),
                        ),
                        started: now,
                        last_output: now,
                        next_beat: HEARTBEAT_SECS,
                        buf: Vec::new(),
                        running: false,
                        done: false,
                        activity: None,
                    })
                    .collect(),
            ),
            out: Mutex::new(0),
            live: enabled(settings) && watching(),
            // The names arrive fully qualified and the loop above has
            // already had to strip the trigger to display them, so the
            // stage can answer this without dispatch passing anything in.
            on_push: names.iter().any(|n| n.starts_with("pre-push-")),
            stop: AtomicBool::new(false),
        });
        if stage.live {
            // The ticker holds a Weak: the stage dropping is what ends it,
            // so a paint can never outlive the region's owner.
            let weak = Arc::downgrade(&stage);
            let own = settings.for_thread();
            let _ = std::thread::Builder::new()
                .name("amont-live".into())
                .spawn(move || tick(own, weak));
        } else if enabled(settings) {
            // Nobody is watching a terminal — an agent, CI, a pipe — and a
            // captured check shows nothing until it finishes. The heartbeat
            // is the one line a minute that says it is alive, which is the
            // difference between "wait" and "kill it" for whoever is on the
            // other end of the pipe.
            let weak = Arc::downgrade(&stage);
            let own = settings.for_thread();
            let _ = std::thread::Builder::new()
                .name("amont-heartbeat".into())
                .spawn(move || heartbeat(own, weak));
        }
        stage
    }

    /// Route this thread's [`say`] calls into slot `idx` until the guard
    /// drops. Installed by the dispatcher around each `check.run`. Also
    /// starts the slot's clock and puts it in the region.
    pub fn enter(self: &Arc<Stage>, idx: usize) -> SinkGuard {
        {
            let mut slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(slot) = slots.get_mut(idx) {
                slot.running = true;
                slot.started = Instant::now();
                slot.last_output = slot.started;
                slot.next_beat = HEARTBEAT_SECS;
            }
        }
        SINK.with(|s| *s.borrow_mut() = Some((Arc::clone(self), idx)));
        SinkGuard
    }

    /// Show slot `idx`'s spawned command in the displays while the returned
    /// guard lives. A check that runs several commands in turn attaches each
    /// one; between them the slot falls back to its own output clock.
    pub fn attach(
        self: &Arc<Stage>,
        idx: usize,
        activity: Arc<crate::hooks::common::Activity>,
    ) -> AttachGuard {
        let mut slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(slot) = slots.get_mut(idx) {
            slot.activity = Some(activity);
        }
        AttachGuard {
            stage: Arc::clone(self),
            idx,
        }
    }

    /// Append raw bytes (a captured child's output) to slot `idx`.
    pub fn append_raw(&self, idx: usize, bytes: &[u8]) {
        let mut slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(slot) = slots.get_mut(idx) {
            if !slot.done {
                slot.buf.extend_from_slice(bytes);
                slot.last_output = Instant::now();
            }
        }
    }

    fn append_line(&self, idx: usize, line: &str) {
        let mut slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(slot) = slots.get_mut(idx) {
            if !slot.done {
                slot.buf.extend_from_slice(line.as_bytes());
                slot.buf.push(b'\n');
                slot.last_output = Instant::now();
            }
        }
    }

    /// The check is over: emit everything it said as ONE contiguous write,
    /// with the region lifted out of the way first and repainted after —
    /// blocks pile up above, spinners stay below.
    ///
    /// Called by the dispatcher after `check.run` returns (still on the
    /// check's thread, so a torn-down thread cannot strand a buffer — the
    /// same `catch_unwind` that feeds the dead-check outcome runs first).
    pub fn finish(&self, settings: &crate::config::Settings, idx: usize) {
        let block = {
            let mut slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
            let Some(slot) = slots.get_mut(idx) else {
                return;
            };
            slot.done = true;
            slot.running = false;
            std::mem::take(&mut slot.buf)
        };
        if block.is_empty() && !self.live {
            return;
        }
        let mut drawn = self.out.lock().unwrap_or_else(|p| p.into_inner());
        if !block.is_empty() {
            if *drawn > 0 {
                let mut err = std::io::stderr().lock();
                let _ = write!(err, "\x1b[{}A\x1b[J", *drawn);
                let _ = err.flush();
                *drawn = 0;
            }
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            let _ = handle.write_all(&block);
            let _ = handle.flush();
        }
        self.repaint(settings, &mut drawn);
    }

    /// Erase and redraw the region in one stderr write. Lock order is
    /// `out` → `slots`, everywhere — never the reverse.
    fn repaint(&self, settings: &crate::config::Settings, drawn: &mut usize) {
        if !self.live {
            return;
        }
        let entries: Vec<Row> = {
            let slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
            let now = Instant::now();
            slots
                .iter()
                .filter(|s| s.running && !s.done)
                .map(|s| row_of(s, now, now.duration_since(s.started).as_secs_f64()))
                .collect()
        };
        let text = region(&entries, term_width(), budgets(settings));
        let mut paint = String::new();
        if *drawn > 0 {
            paint.push_str(&format!("\x1b[{}A\x1b[J", *drawn));
        }
        paint.push_str(&text);
        if paint.is_empty() {
            return;
        }
        let mut err = std::io::stderr().lock();
        let _ = err.write_all(paint.as_bytes());
        let _ = err.flush();
        *drawn = text.matches('\n').count();
    }
}

impl Drop for Stage {
    /// The stage's end erases whatever the region still shows — a Block
    /// verdict, a panic on the dispatcher path, anything: no spinner junk
    /// above the roll-up. (`get_mut`: dropping proves no other thread holds
    /// the stage, so the locks are free.)
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if !self.live {
            return;
        }
        let drawn = self.out.get_mut().unwrap_or_else(|p| p.into_inner());
        if *drawn > 0 {
            let mut err = std::io::stderr().lock();
            let _ = write!(err, "\x1b[{}A\x1b[J", *drawn);
            let _ = err.flush();
            *drawn = 0;
        }
    }
}

/// The ticker: repaint every 80ms until the stage drops or tells it to
/// stop. Holds only a `Weak`, so it can never keep a finished stage alive.
/// Owns its `Settings` (see [`crate::config::Settings::for_thread`]): a
/// spawned thread is `'static`, and the budgets must be read lazily, not
/// pre-resolved at the spawn.
fn tick(settings: crate::config::Settings, weak: Weak<Stage>) {
    loop {
        std::thread::sleep(std::time::Duration::from_millis(80));
        let Some(stage) = weak.upgrade() else { return };
        if stage.stop.load(Ordering::Relaxed) {
            return;
        }
        let mut drawn = stage.out.lock().unwrap_or_else(|p| p.into_inner());
        stage.repaint(&settings, &mut drawn);
    }
}

/// One running check, as the region and the heartbeat see it.
#[derive(Debug, Clone)]
pub struct Row {
    pub name: String,
    /// Seconds since the check entered.
    pub elapsed: f64,
    /// Seconds since it last wrote anything.
    pub quiet: f64,
    /// Seconds it has been silent AND idle on CPU — what the silence budget
    /// is judged against. Equal to `quiet` when CPU is not sampled.
    pub still: f64,
    pub cpu: RowCpu,
    /// The declared lock wait it is in, and for how many seconds
    /// (ADR-0009): shown instead of the silence countdown, which does not
    /// run meanwhile.
    pub wait: Option<(crate::hooks::wait::WaitKind, f64)>,
    /// The silence budget the kill decision is applying, stretched by the
    /// host's load, with the load and the factor; `None` while it is the
    /// configured one (ADR-0009).
    pub load: Option<(u64, crate::load::Load, u32)>,
}

/// What a running check's CPU is doing, as far as the displays may say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowCpu {
    /// No spawned command is attached (an in-process check, or between two
    /// commands): nothing to say.
    None,
    /// A command is attached but its CPU is not sampled here
    /// (`amont.idleCpuCredit false`, no silence budget, or the platform):
    /// silence alone counts.
    NotSampled,
    /// Sampled, but nothing fresh to report (not quiet long enough yet, or
    /// the last snapshot was incomplete).
    Unmeasured,
    /// Measurably working, at this many thousandths of a core.
    Busy(u32),
    /// Measured under the busy threshold.
    Idle,
}

/// A slot as a [`Row`], reading its attached command's clocks when there is
/// one. The quiet figure is the more recent of the slot's own lines and the
/// command's bytes (a captured command writes to one and not the other).
fn row_of(s: &Slot, now: Instant, elapsed: f64) -> Row {
    use crate::hooks::common::{CpuState, BUSY_MILLI_CORES};
    let slot_quiet = now.duration_since(s.last_output).as_secs_f64();
    let (quiet, still, cpu, wait, load) = match &s.activity {
        None => (slot_quiet, slot_quiet, RowCpu::None, None, None),
        Some(a) => {
            let quiet = slot_quiet.min(a.quiet_for().as_secs_f64());
            let still = quiet.min(a.still_for().as_secs_f64());
            let cpu = match (a.cpu_state(), a.fresh_rate()) {
                (CpuState::Off, _) => RowCpu::NotSampled,
                (_, Some(r)) if r >= BUSY_MILLI_CORES => RowCpu::Busy(r),
                (_, Some(_)) => RowCpu::Idle,
                (_, None) => RowCpu::Unmeasured,
            };
            let wait = a.waiting().map(|(k, d)| (k, d.as_secs_f64()));
            (quiet, still, cpu, wait, a.load_scale())
        }
    };
    Row {
        name: s.name.clone(),
        elapsed,
        quiet,
        still,
        cpu,
        wait,
        load,
    }
}

impl Row {
    /// The silence budget this row counts toward: the load-stretched one
    /// when the host stretched it, else the configured one.
    fn applied_idle(&self, budgets: Budgets) -> u64 {
        match self.load {
            Some((applied, _, _)) if applied > 0 => applied,
            _ => budgets.idle,
        }
    }
}

/// The clocks, as the region annotates them, in seconds: `idle` and
/// `ceiling` with `0` for off; `lock_wait` with `0` for "until the
/// ceiling".
#[derive(Debug, Clone, Copy)]
pub struct Budgets {
    pub idle: u64,
    pub ceiling: u64,
    pub lock_wait: u64,
    /// The extended silence budget, `idle × amont.idleLoadScale`: what a
    /// check answers to while its CPU cannot be measured.
    pub extended: u64,
}

fn budgets(settings: &crate::config::Settings) -> Budgets {
    let idle = crate::hooks::common::idle_timeout(settings);
    Budgets {
        idle,
        extended: idle.saturating_mul(crate::hooks::common::idle_load_scale(settings)),
        ceiling: crate::hooks::common::check_timeout(settings),
        lock_wait: match crate::hooks::common::lock_wait(settings) {
            crate::hooks::common::LockWait::Secs(s) => s,
            crate::hooks::common::LockWait::UntilCeiling => 0,
        },
    }
}

/// How long a check must be quiet before the region says so. A test suite
/// pauses this long between crates without anything being wrong; past it,
/// the reader wants to know the silence is being counted.
const QUIET_NOTE_SECS: f64 = 30.0;

/// The non-tty heartbeat's period: one line a minute per running check.
const HEARTBEAT_SECS: u64 = 60;

/// Elapsed time in a fixed six-column figure: `  3.2s` under a minute,
/// `8m12s` and `1h02m` above, so the column stays aligned as the suite
/// crosses the minute.
fn elapsed_column(secs: f64) -> String {
    if secs < 60.0 {
        format!("{secs:>5.1}s")
    } else {
        format!("{:>6}", crate::hooks::common::human_secs(secs as u64))
    }
}

/// The region's text: one `⠹ name  12.3s` line per running check, capped at
/// [`MAX_LINES`] plus a `… and N more` overflow line. Pure — the ticker is
/// a thin shell around this, and the tests drive it directly.
///
/// Two annotations, each only when it carries news: `· quiet 45s/2m` once
/// a check has been silent past [`QUIET_NOTE_SECS`] (with the silence
/// budget it is counting toward, when there is one), and `· 48m/60m` once
/// elapsed passes 80% of the ceiling — the cliff, shown before the fall.
fn region(entries: &[Row], width: usize, budgets: Budgets) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let pad = entries
        .iter()
        .take(MAX_LINES)
        .map(|r| r.name.chars().count())
        .max()
        .unwrap_or(0);
    let mut out = String::new();
    for row in entries.iter().take(MAX_LINES) {
        let frame = FRAMES[((row.elapsed * 10.0) as usize) % FRAMES.len()];
        let name = &row.name;
        let mut line = format!("{frame} {name:<pad$} {}", elapsed_column(row.elapsed));
        if let Some((kind, waited)) = row.wait {
            // A declared wait: no silence countdown runs, so the row says
            // what is being waited for and against which budget.
            line.push_str(&format!(
                " · {} {}",
                kind.short(),
                crate::hooks::common::human_secs(waited as u64)
            ));
            if budgets.lock_wait > 0 {
                line.push_str(&format!(
                    "/{}",
                    crate::hooks::common::human_secs(budgets.lock_wait)
                ));
            }
        } else if row.quiet >= QUIET_NOTE_SECS {
            let quiet = crate::hooks::common::human_secs(row.quiet as u64);
            match row.cpu {
                // Working: no countdown — no kill is coming — just how hard.
                RowCpu::Busy(m) => line.push_str(&format!(
                    " · quiet {quiet} · {}",
                    crate::hooks::common::cores(m)
                )),
                _ if budgets.idle == 0 => line.push_str(&format!(" · quiet {quiet}")),
                // The countdown counts what the kill decision counts: the
                // still-time, which only differs from the silence once CPU
                // work has pushed it back.
                // Unmeasured: the extended budget is the one that counts,
                // and the row says why the figure is not the usual one.
                RowCpu::Unmeasured => line.push_str(&format!(
                    " · quiet {quiet}/{} (CPU unmeasured)",
                    crate::hooks::common::human_secs(budgets.extended.max(budgets.idle))
                )),
                RowCpu::Idle if row.quiet - row.still >= 1.0 => line.push_str(&format!(
                    " · quiet {quiet} · idle {}/{}",
                    crate::hooks::common::human_secs(row.still as u64),
                    crate::hooks::common::human_secs(row.applied_idle(budgets))
                )),
                _ => line.push_str(&format!(
                    " · quiet {quiet}/{}",
                    crate::hooks::common::human_secs(row.applied_idle(budgets))
                )),
            }
            if let (Some((_, _, factor)), false) = (row.load, row.cpu == RowCpu::Unmeasured) {
                line.push_str(&format!(" (load {})", crate::load::factor_text(factor)));
            }
        }
        if budgets.ceiling > 0 && row.elapsed >= 0.8 * budgets.ceiling as f64 {
            line.push_str(&format!(
                " · {}/{}",
                crate::hooks::common::human_secs(row.elapsed as u64),
                crate::hooks::common::human_secs(budgets.ceiling)
            ));
        }
        if line.chars().count() > width {
            out.extend(line.chars().take(width));
        } else {
            out.push_str(&line);
        }
        out.push('\n');
    }
    if entries.len() > MAX_LINES {
        out.push_str(&format!("… and {} more\n", entries.len() - MAX_LINES));
    }
    out
}

/// The heartbeat: once a minute, for each check still running, one plain
/// line on stderr — elapsed, and how long since it last said anything.
/// Not a region: nothing is erased or repainted, because nobody is looking
/// at a cursor; whoever reads this reads a log.
///
/// The first beat for a check also names the two budgets, once, so the
/// reader can tell how far it is from being killed without opening the
/// docs. Written under the same `out` lock as the blocks, so a beat never
/// lands inside one.
/// Owns its `Settings` for the same reason [`tick`] does.
fn heartbeat(settings: crate::config::Settings, weak: Weak<Stage>) {
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let Some(stage) = weak.upgrade() else { return };
        if stage.stop.load(Ordering::Relaxed) {
            return;
        }
        let due: Vec<(Row, bool)> = {
            let mut slots = stage.slots.lock().unwrap_or_else(|p| p.into_inner());
            let now = Instant::now();
            let mut due = Vec::new();
            for s in slots.iter_mut().filter(|s| s.running && !s.done) {
                let elapsed = now.duration_since(s.started).as_secs();
                if elapsed >= s.next_beat {
                    let first = s.next_beat == HEARTBEAT_SECS;
                    s.next_beat += HEARTBEAT_SECS;
                    due.push((row_of(s, now, elapsed as f64), first));
                }
            }
            due
        };
        if due.is_empty() {
            continue;
        }
        let text: String = due
            .iter()
            .map(|(row, first)| beat_line(row, *first, budgets(&settings), stage.on_push))
            .collect();
        let _guard = stage.out.lock().unwrap_or_else(|p| p.into_inner());
        let mut err = std::io::stderr().lock();
        let _ = err.write_all(text.as_bytes());
        let _ = err.flush();
    }
}

/// One heartbeat line. Pure, for the tests.
///
/// On the FIRST beat of a PUSH gate it also names something no other part of
/// the system is placed to explain. `git push` opens its connection to the
/// remote, reads the remote refs — which is where the `pre-push` hook's own
/// stdin comes from — and only then calls the hook. The connection is
/// therefore already open and goes idle for exactly as long as the gate
/// runs, and a remote may close it before the gate finishes. git then
/// reports `Connection reset by peer`, which reads as a network fault and
/// says nothing about the seven minutes that caused it.
///
/// The note does NOT recommend ssh keepalive, and that omission is
/// deliberate: `ServerAliveInterval 60` was already in force on the machine
/// where this was diagnosed, and GitHub reset the connection anyway.
/// Whatever the remote is measuring, it is not packets. Recommending it
/// would be a confident instruction to change a setting that is probably
/// already on and cannot help, so the note says so and points at the thing
/// that does work.
///
/// Only on a first beat, so it is said once; only on a push, so a commit
/// gate never hears it. A first beat is a check that has already run a full
/// minute, which is the population at risk — no threshold to invent.
fn beat_line(row: &Row, first: bool, budgets: Budgets, on_push: bool) -> String {
    use crate::hooks::common::human_secs;
    // The prefix is byte-for-byte what it always was: log readers grep it.
    // What CPU sampling adds goes after it.
    let mut line = format!(
        "  … {} still running: {}, last output {} ago",
        row.name,
        human_secs(row.elapsed as u64),
        human_secs(row.quiet as u64)
    );
    if let Some((kind, waited)) = row.wait {
        line.push_str(&format!(
            ", waiting for {} {}",
            kind.describe(),
            human_secs(waited as u64)
        ));
        match budgets.lock_wait {
            0 => line.push_str(" (amont.lockWait 0: until the ceiling)"),
            s => line.push_str(&format!(" (amont.lockWait {})", human_secs(s))),
        }
    } else {
        match row.cpu {
            RowCpu::Busy(m) => line.push_str(&format!(", busy {}", crate::hooks::common::cores(m))),
            RowCpu::Idle => line.push_str(&format!(", CPU idle {}", human_secs(row.still as u64))),
            RowCpu::Unmeasured if budgets.idle > 0 => line.push_str(&format!(
                ", CPU unmeasured — extended budget {}",
                human_secs(budgets.extended.max(budgets.idle))
            )),
            RowCpu::Unmeasured => line.push_str(", CPU unmeasured"),
            RowCpu::None | RowCpu::NotSampled => {}
        }
        if let (Some((applied, load, factor)), false) = (row.load, row.cpu == RowCpu::Unmeasured) {
            line.push_str(&format!(
                ", budget {} (load avg {} on {} cores, {} — amont.idleLoadScale)",
                human_secs(applied),
                load.avg1_text(),
                load.cores,
                crate::load::factor_text(factor)
            ));
        }
    }
    if first {
        let idle = match row.applied_idle(budgets) {
            0 => "off".to_string(),
            s => human_secs(s),
        };
        let ceiling = match budgets.ceiling {
            0 => "off".to_string(),
            s => human_secs(s),
        };
        match row.cpu {
            // The budget that applies to THIS check, not the configured
            // one: while CPU is unmeasured that is the extended budget.
            RowCpu::Unmeasured if budgets.idle > 0 => line.push_str(&format!(
                " (killed after {} with no output while its CPU is unmeasured, or {ceiling} \
                 in total — amont.idleTimeout × amont.idleLoadScale / amont.timeout)",
                human_secs(budgets.extended.max(budgets.idle))
            )),
            RowCpu::Busy(_) | RowCpu::Idle if budgets.idle > 0 => line.push_str(&format!(
                " (killed after {idle} with no output and under 0.1 core of CPU, or \
                 {ceiling} in total — amont.idleTimeout / amont.timeout)"
            )),
            _ => line.push_str(&format!(
                " (killed after {idle} of silence or {ceiling} in total — amont.idleTimeout / amont.timeout)"
            )),
        }
        if row.cpu == RowCpu::NotSampled && budgets.idle > 0 {
            line.push_str("; CPU not sampled here, silence alone counts");
        }
        if on_push {
            // `concat!`, not a `\`-continued literal: a continuation keeps
            // the next line's indentation, which turns the message into runs
            // of spaces. Each line is its own literal and the newlines are
            // written down, so what is here is what a reader sees.
            line.push_str(concat!(
                "\n    git opened its connection to the remote before calling this",
                "\n    gate, and it stays idle until the gate finishes. A remote may",
                "\n    close it first — GitHub does — and the push then fails with",
                "\n    \"Connection reset by peer\", naming the network rather than the",
                "\n    wait. ssh keepalive does not prevent this.",
                "\n    Declaring this check at pre-commit moves it off the push path —",
                "\n    see \"Moving a gate entry earlier\" in the docs.",
            ));
        }
    }
    line.push('\n');
    line
}

/// `$COLUMNS` when it is exported and sane, else a conservative 80 — the
/// region's lines are short and an ioctl is not worth its portability. 80,
/// not wider: shells rarely export `COLUMNS`, and a region line longer than
/// the real terminal wraps, which breaks the erase arithmetic.
///
/// `pub` is now wider than it needs to be — the out-of-crate caller that
/// justified it, `amont-agent`, is its own project and carries its own copy.
/// Left public rather than narrowed in the same change that removed it.
pub fn term_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse::<usize>().ok())
        .filter(|w| *w >= 20)
        .unwrap_or(80)
}

/// Emits slot `idx`'s block when dropped — however the check's closure
/// exits, a panic included: the partial output of a check that died still
/// reaches the reader, above the dead-check verdict the runner fills in.
pub struct FinishOnDrop<'a> {
    stage: &'a Stage,
    idx: usize,
    /// Carried, because `Drop` takes no arguments and the finish paint
    /// needs the budgets. Same lifetime as the stage it belongs to.
    settings: &'a crate::config::Settings,
}

impl<'a> FinishOnDrop<'a> {
    pub fn new(
        settings: &'a crate::config::Settings,
        stage: &'a Stage,
        idx: usize,
    ) -> FinishOnDrop<'a> {
        FinishOnDrop {
            stage,
            idx,
            settings,
        }
    }
}

impl Drop for FinishOnDrop<'_> {
    fn drop(&mut self) {
        self.stage.finish(self.settings, self.idx);
    }
}

/// Uninstalls the thread's sink on drop, whatever path the check took out.
pub struct SinkGuard;

impl Drop for SinkGuard {
    fn drop(&mut self) {
        SINK.with(|s| *s.borrow_mut() = None);
    }
}

/// Detaches a command from its slot's displays when dropped. See
/// [`Stage::attach`].
pub struct AttachGuard {
    stage: Arc<Stage>,
    idx: usize,
}

impl Drop for AttachGuard {
    fn drop(&mut self) {
        let mut slots = self.stage.slots.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(slot) = slots.get_mut(self.idx) {
            slot.activity = None;
        }
    }
}

/// The sink installed on THIS thread, if any — how a child-capture helper on
/// the check's own thread learns where the reader threads should append.
pub fn current_sink() -> Option<(Arc<Stage>, usize)> {
    SINK.with(|s| s.borrow().clone())
}

/// One line of check output, wherever it should go.
///
/// THE funnel: `common::ok/fail/warn` call this, so a check's helper prints
/// land in its slot during a stage and on stdout everywhere else. `line` is
/// taken without a trailing newline, exactly like `println!`.
pub fn say(line: &str) {
    let routed = SINK.with(|s| {
        s.borrow().as_ref().map(|(stage, idx)| {
            stage.append_line(*idx, line);
        })
    });
    if routed.is_none() {
        println!("{line}");
    }
}

/// `println!`, stage-aware: formats and routes through [`say`]. What every
/// direct print inside a CHECK BODY becomes — a line printed raw from a
/// check thread bypasses the slot and interleaves, which is the bug this
/// module exists to close.
#[macro_export]
macro_rules! say {
    ($($arg:tt)*) => {
        $crate::live::say(&format!($($arg)*))
    };
}

/// Should a check's SUCCESS line be swallowed?
///
/// A hook that passes says one line per check, and on a clean run that is the
/// entire output: fourteen lines to say nothing happened. At a terminal those
/// lines are the reassurance that the gate ran. Captured — an agent's tool
/// result, a CI log — they are re-read on every later turn of the session and
/// say no more the tenth time than the first.
///
/// So the setting names WHO is reading, not how loud to be:
///
/// - `auto` (default) — quiet when nobody is watching, verbose at a terminal.
/// - `never` — every check says it passed, whoever is reading.
/// - `always` — quiet everywhere.
///
/// `auto` is the default because the reader it costs nothing is the one at a
/// terminal: `watching()` is true there, so a person sees exactly what they
/// saw before. The reader it saves is the one who cannot skim — a captured
/// log, an agent's tool result — and that reader was paying for fourteen
/// lines of nothing on every turn of a session. A default that is free for
/// one audience and compounding for the other is not a neutral default.
///
/// Only the success lines go. A failure, a warning, a check that could not
/// run, a repaired file, and the blocked summary are printed under every
/// setting: quiet is about the uneventful path, and nothing else.
pub fn quiet(settings: &crate::config::Settings) -> bool {
    *settings.quiet.get_or_init(|| {
        decide(
            crate::config::enumerated_or(settings, "amont.quiet", QUIET_VALUES, "auto"),
            watching(),
        )
    })
}

pub const QUIET_VALUES: &[&str] = &["never", "auto", "always"];

/// Pure, so the three-way decision is testable without a terminal or a config.
fn decide(setting: &str, watching: bool) -> bool {
    match setting {
        "always" => true,
        "auto" => !watching,
        // `never`. A value `enumerated_or` rejected never reaches here — it
        // complains and hands back the default, which is now `auto`.
        _ => false,
    }
}

/// Whether the capture mechanism is on at all. `amont.progress false` is the
/// escape hatch back to raw streaming — one knob, read once.
pub fn enabled(settings: &crate::config::Settings) -> bool {
    *settings
        .progress
        .get_or_init(|| crate::config::boolean_or(settings, "amont.progress", true))
}

/// Is anyone watching? True only when stderr is a real terminal that speaks
/// VT: not piped, not redirected, not `TERM=dumb` — and on Windows only
/// with `TERM` actually set, because bare conhost may not interpret the
/// cursor codes the region depends on. This is the paint gate; capture
/// ([`enabled`]) does not consult it.
pub fn watching() -> bool {
    static WATCHING: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *WATCHING.get_or_init(|| {
        if !std::io::stderr().is_terminal() {
            return false;
        }
        match std::env::var("TERM") {
            Ok(term) => term != "dumb",
            Err(_) => !cfg!(windows),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_settings() -> crate::config::Settings {
        crate::config::Settings::default()
    }

    #[test]
    fn quiet_asks_who_is_reading() {
        assert!(!decide("never", true));
        assert!(!decide("never", false));
        assert!(always_and_auto_agree_at_a_terminal());
        assert!(decide("auto", false), "captured: nobody is watching");
        assert!(decide("always", true));
        assert!(decide("always", false));
        // An unreadable value has already been reported by `enumerated_or`,
        // which hands back the default; silence is never assumed.
        assert!(!decide("shhh", false));
    }

    fn always_and_auto_agree_at_a_terminal() -> bool {
        !decide("auto", true) && decide("always", true)
    }

    /// The atomicity contract at the unit level: two threads writing
    /// interleaved lines into their own slots come out as two contiguous
    /// buffers, whatever the scheduler did.
    #[test]
    fn slots_do_not_share_a_buffer() {
        let stage = Stage::begin(&test_settings(), &["a", "b"]);
        std::thread::scope(|scope| {
            for idx in 0..2 {
                let stage = Arc::clone(&stage);
                scope.spawn(move || {
                    let _guard = stage.enter(idx);
                    for i in 0..50 {
                        say(&format!("check-{idx} line-{i}"));
                        std::thread::yield_now();
                    }
                });
            }
        });
        let slots = stage.slots.lock().unwrap();
        for idx in 0..2 {
            let text = String::from_utf8(slots[idx].buf.clone()).unwrap();
            assert_eq!(text.lines().count(), 50);
            assert!(
                text.lines()
                    .all(|l| l.starts_with(&format!("check-{idx} "))),
                "a foreign line landed in slot {idx}"
            );
        }
    }

    /// A thread with no sink prints; its lines never land in anyone's slot.
    #[test]
    fn no_sink_means_no_capture() {
        let stage = Stage::begin(&test_settings(), &["a"]);
        say("goes to stdout, not to a slot");
        let slots = stage.slots.lock().unwrap();
        assert!(slots[0].buf.is_empty());
    }

    /// After finish, late writes are dropped rather than stranded — a child
    /// reader thread that outlives its check must not corrupt a later block.
    #[test]
    fn a_finished_slot_takes_no_more_writes() {
        let stage = Stage::begin(&test_settings(), &["a"]);
        stage.append_raw(0, b"before\n");
        stage.finish(&test_settings(), 0);
        stage.append_raw(0, b"after\n");
        let slots = stage.slots.lock().unwrap();
        assert!(slots[0].buf.is_empty(), "a write landed after finish");
    }

    /// A repo-derived check name cannot smuggle control bytes onto a live
    /// terminal: sanitised at begin, once, for every later paint.
    #[test]
    fn a_slot_name_is_sanitised_at_begin() {
        let stage = Stage::begin(&test_settings(), &["evil\u{1b}[2Jname\rhere"]);
        let slots = stage.slots.lock().unwrap();
        assert!(!slots[0].name.contains('\u{1b}'), "{:?}", slots[0].name);
        assert!(!slots[0].name.contains('\r'), "{:?}", slots[0].name);
    }

    /// Region names drop the stage's own prefix — it is the same twelve
    /// characters on every line.
    #[test]
    fn a_slot_name_drops_the_stage_prefix() {
        let stage = Stage::begin(
            &test_settings(),
            &["pre-commit-clippy", "pre-push-run-tests", "bare"],
        );
        let slots = stage.slots.lock().unwrap();
        assert_eq!(slots[0].name, "clippy");
        assert_eq!(slots[1].name, "run-tests");
        assert_eq!(slots[2].name, "bare");
    }

    fn row(name: &str, elapsed: f64) -> Row {
        Row {
            name: name.into(),
            elapsed,
            quiet: 0.0,
            still: 0.0,
            cpu: RowCpu::None,
            wait: None,
            load: None,
        }
    }

    /// Under load the region counts toward the stretched budget and says
    /// so; the heartbeat names the budget, the load and the key; the first
    /// beat states the stretched figure.
    #[test]
    fn a_load_stretched_budget_is_shown_with_its_load() {
        let mut r = row("pre-push-run-tests-js", 240.0);
        r.quiet = 130.0;
        r.still = 130.0;
        r.cpu = RowCpu::Idle;
        r.load = Some((
            468,
            crate::load::Load {
                avg1_milli: 31_200,
                cores: 8,
            },
            3900,
        ));
        let text = region(&[r.clone()], 80, B);
        assert!(text.contains("· quiet 2m10s/7m48s (load ×3.9)"), "{text:?}");
        assert!(text.lines().all(|l| l.chars().count() <= 80), "{text:?}");
        let beat = beat_line(&r, false, B, false);
        assert!(
            beat.ends_with(
                ", CPU idle 2m10s, budget 7m48s (load avg 31.2 on 8 cores, ×3.9 — amont.idleLoadScale)\n"
            ),
            "{beat:?}"
        );
        let first = flat(&beat_line(&r, true, B, false));
        assert!(
            first.contains("killed after 7m48s with no output and under 0.1 core of CPU"),
            "{first:?}"
        );
    }

    const B: Budgets = Budgets {
        idle: 120,
        ceiling: 3600,
        lock_wait: 600,
        extended: 480,
    };

    /// While CPU is unmeasured the extended budget is the one that counts:
    /// the region counts toward it and says why, the heartbeat names it,
    /// and the first beat states that rule rather than the configured one.
    #[test]
    fn an_unmeasured_check_is_shown_against_the_extended_budget() {
        let mut r = row("pre-push-run-tests-js", 240.0);
        r.quiet = 130.0;
        r.still = 130.0;
        r.cpu = RowCpu::Unmeasured;
        let text = region(&[r.clone()], 80, B);
        assert!(
            text.contains("· quiet 2m10s/8m00s (CPU unmeasured)"),
            "{text:?}"
        );
        assert!(text.lines().all(|l| l.chars().count() <= 80), "{text:?}");
        let beat = beat_line(&r, false, B, false);
        assert!(
            beat.ends_with(", CPU unmeasured — extended budget 8m00s\n"),
            "{beat:?}"
        );
        let first = flat(&beat_line(&r, true, B, false));
        assert!(
            first.contains(
                "killed after 8m00s with no output while its CPU is unmeasured, or 1h00m in total"
            ),
            "{first:?}"
        );
        assert!(first.contains("amont.idleLoadScale"), "{first:?}");
    }

    /// A declared wait replaces the silence countdown in the region and
    /// rides after the heartbeat's unchanged prefix, naming its budget; both
    /// fit 80 columns with the longest built-in name.
    #[test]
    fn a_declared_wait_is_shown_against_its_own_budget() {
        use crate::hooks::wait::{CargoLockWhat, WaitKind};
        let mut r = row("pre-push-run-tests-js", 240.0);
        r.quiet = 130.0;
        r.still = 0.0;
        r.cpu = RowCpu::Unmeasured;
        r.wait = Some((WaitKind::CargoLock(CargoLockWhat::BuildDirectory), 90.0));
        let text = region(&[r.clone()], 80, B);
        assert!(text.contains("· cargo lock 1m30s/10m00s"), "{text:?}");
        assert!(!text.contains("quiet"), "no silence countdown: {text:?}");
        assert!(text.lines().all(|l| l.chars().count() <= 80), "{text:?}");
        let until = Budgets { lock_wait: 0, ..B };
        let text = region(&[r.clone()], 80, until);
        assert!(
            text.contains("· cargo lock 1m30s") && !text.contains("1m30s/"),
            "{text:?}"
        );

        let beat = beat_line(&r, false, B, false);
        assert_eq!(
            beat,
            "  … pre-push-run-tests-js still running: 4m00s, last output 2m10s ago, \
             waiting for the cargo lock on the build directory 1m30s (amont.lockWait 10m00s)\n"
        );
        let beat = beat_line(&r, false, until, false);
        assert!(
            beat.contains("(amont.lockWait 0: until the ceiling)"),
            "{beat:?}"
        );
    }

    /// The spinner frame comes from the clock: different elapsed, different
    /// frame; same elapsed, same frame.
    #[test]
    fn frames_advance_with_time() {
        let a = region(&[row("clippy", 0.0)], 80, B);
        let b = region(&[row("clippy", 0.1)], 80, B);
        let c = region(&[row("clippy", 1.0)], 80, B);
        assert_ne!(a.chars().next(), b.chars().next());
        assert_eq!(a.chars().next(), c.chars().next(), "10 frames per second");
    }

    /// Names pad to a column so the elapsed figures align — across the
    /// minute mark too, where the figure changes shape.
    #[test]
    fn region_lines_align() {
        let text = region(&[row("a", 0.0), row("longer-name", 0.0)], 80, B);
        let widths: Vec<usize> = text.lines().map(|l| l.chars().count()).collect();
        assert_eq!(widths[0], widths[1], "{text:?}");
        let text = region(&[row("a", 3.2), row("b", 492.0)], 80, B);
        let widths: Vec<usize> = text.lines().map(|l| l.chars().count()).collect();
        assert_eq!(widths[0], widths[1], "{text:?}");
        assert!(text.contains("8m12s"), "{text:?}");
    }

    /// Thirteen running checks paint as twelve lines and one overflow.
    #[test]
    fn region_caps_and_counts_the_rest() {
        let entries: Vec<Row> = (0..13).map(|i| row(&format!("check-{i}"), 0.0)).collect();
        let text = region(&entries, 80, B);
        assert_eq!(text.lines().count(), MAX_LINES + 1);
        assert!(text.ends_with("… and 1 more\n"), "{text:?}");
    }

    /// A narrow terminal truncates rather than wraps — a wrapped region
    /// line would break the erase arithmetic.
    #[test]
    fn region_respects_width() {
        let text = region(&[row("a-name-much-longer-than-the-terminal", 0.0)], 20, B);
        assert!(text.lines().all(|l| l.chars().count() <= 20), "{text:?}");
    }

    /// No running checks, no region — not even a blank line.
    #[test]
    fn an_empty_region_is_empty() {
        assert_eq!(region(&[], 80, B), "");
    }

    /// Silence is annotated only once it is news, and names the budget it
    /// counts toward — a check that just paused between crates says
    /// nothing extra.
    #[test]
    fn a_quiet_check_shows_its_silence_against_the_budget() {
        let mut r = row("cargo-test", 300.0);
        r.quiet = 5.0;
        assert!(!region(&[r.clone()], 80, B).contains("quiet"));
        r.quiet = 45.0;
        let text = region(&[r.clone()], 80, B);
        assert!(text.contains("quiet 45s/2m00s"), "{text:?}");
        let off = Budgets { idle: 0, ..B };
        let text = region(&[r], 80, off);
        assert!(
            text.contains("quiet 45s") && !text.contains('/'),
            "{text:?}"
        );
    }

    /// A silent check that is working shows how hard, with no countdown —
    /// no kill is coming; one whose CPU work pushed the still-time back
    /// counts down the still-time, which is what the kill decision uses.
    /// Both fit an 80-column terminal with a longish name.
    #[test]
    fn a_quiet_busy_check_shows_cores_and_an_idle_one_counts_down_the_still_time() {
        let mut r = row("vitest-workspace", 240.0);
        r.quiet = 130.0;
        r.still = 130.0;
        r.cpu = RowCpu::Busy(3900);
        let busy = region(&[r.clone()], 80, B);
        assert!(busy.contains("· quiet 2m10s · ~3.9 cores"), "{busy:?}");
        assert!(
            !busy.contains("/2m00s"),
            "no countdown while busy: {busy:?}"
        );

        r.cpu = RowCpu::Idle;
        r.still = 40.0;
        let idle = region(&[r.clone()], 80, B);
        assert!(idle.contains("· quiet 2m10s · idle 40s/2m00s"), "{idle:?}");

        r.cpu = RowCpu::Unmeasured;
        r.still = 130.0;
        let plain = region(&[r], 80, B);
        assert!(
            plain.contains("· quiet 2m10s/8m00s (CPU unmeasured)"),
            "{plain:?}"
        );

        for text in [busy, idle, plain] {
            assert!(text.lines().all(|l| l.chars().count() <= 80), "{text:?}");
        }
    }

    /// The heartbeat's prefix is unchanged — log readers grep it — and the
    /// CPU state rides after it.
    #[test]
    fn a_heartbeat_appends_the_cpu_state_after_an_unchanged_prefix() {
        let mut r = row("vitest", 240.0);
        r.quiet = 130.0;
        r.still = 40.0;
        let prefix = "  … vitest still running: 4m00s, last output 2m10s ago";
        for (cpu, suffix) in [
            (RowCpu::Busy(3900), ", busy ~3.9 cores\n"),
            (RowCpu::Idle, ", CPU idle 40s\n"),
            (
                RowCpu::Unmeasured,
                ", CPU unmeasured — extended budget 8m00s\n",
            ),
            (RowCpu::None, "\n"),
            (RowCpu::NotSampled, "\n"),
        ] {
            r.cpu = cpu;
            assert_eq!(beat_line(&r, false, B, false), format!("{prefix}{suffix}"));
        }
    }

    /// The first beat states the rule that actually applies to this check.
    #[test]
    fn the_first_beat_states_the_rule_in_force() {
        let mut r = row("vitest", 60.0);
        r.cpu = RowCpu::Idle;
        let sampled = beat_line(&r, true, B, false);
        assert!(
            flat(&sampled).contains(
                "killed after 2m00s with no output and under 0.1 core of CPU, or 1h00m in total"
            ),
            "{sampled:?}"
        );
        r.cpu = RowCpu::NotSampled;
        let not = beat_line(&r, true, B, false);
        assert!(
            not.contains("2m00s of silence or 1h00m in total"),
            "{not:?}"
        );
        assert!(
            not.contains("CPU not sampled here, silence alone counts"),
            "{not:?}"
        );
    }

    /// The ceiling appears once a check is 80% of the way to it — the cliff,
    /// shown before the fall — and never for a disabled ceiling.
    #[test]
    fn the_ceiling_shows_only_when_it_is_near() {
        assert!(!region(&[row("cargo-test", 1000.0)], 80, B).contains("/1h00m"));
        let text = region(&[row("cargo-test", 3000.0)], 80, B);
        assert!(text.contains("50m00s/1h00m"), "{text:?}");
        let off = Budgets { ceiling: 0, ..B };
        assert!(!region(&[row("cargo-test", 3000.0)], 80, off).contains("/"));
    }

    /// The heartbeat says how long, how quiet, and — the first time — the
    /// budgets, so a reader at the far end of a pipe can tell "wait" from
    /// "kill it" without the docs.
    #[test]
    fn a_heartbeat_names_the_budgets_once() {
        let mut r = row("cargo-test", 60.0);
        r.quiet = 2.0;
        let first = beat_line(&r, true, B, false);
        assert!(
            first.contains("cargo-test still running: 1m00s"),
            "{first:?}"
        );
        assert!(first.contains("last output 2s ago"), "{first:?}");
        assert!(
            first.contains("2m00s of silence or 1h00m in total"),
            "{first:?}"
        );
        assert!(first.contains("amont.idleTimeout"), "{first:?}");
        let later = beat_line(&r, false, B, false);
        assert!(!later.contains("amont.idleTimeout"), "{later:?}");
        let off = beat_line(
            &r,
            true,
            Budgets {
                idle: 0,
                ceiling: 0,
                lock_wait: 0,
                extended: 0,
            },
            false,
        );
        assert!(off.contains("off of silence or off in total"), "{off:?}");
    }

    /// The message, with newlines and indentation flattened.
    ///
    /// The note is wrapped for a terminal, so a literal substring can fall
    /// across a line break — asserting on `"may close it first"` failed for
    /// no better reason than that `may` ended a line. These tests are about
    /// what the message SAYS; re-wrapping it should not break them.
    fn flat(line: &str) -> String {
        line.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// A long PUSH gate is told what it is sitting on; a commit gate is not.
    ///
    /// The three negatives matter as much as the positive. Said on every
    /// beat it would be nagging; said at commit time it would be false —
    /// there is no connection open — and a future refactor that wires
    /// `on_push` to a constant would show up here and nowhere else.
    #[test]
    fn a_long_push_gate_is_told_what_it_is_sitting_on() {
        let r = row("cargo-test", 60.0);

        let pushing = flat(&beat_line(&r, true, B, true));
        assert!(
            pushing.contains("A remote may close it first"),
            "{pushing:?}"
        );
        assert!(pushing.contains("Connection reset by peer"), "{pushing:?}");
        assert!(
            pushing.contains("Moving a gate entry earlier"),
            "{pushing:?}"
        );

        // Once, not every minute.
        let later = flat(&beat_line(&r, false, B, true));
        assert!(!later.contains("close it first"), "{later:?}");

        // Never at commit time: nothing is waiting on a socket there.
        let committing = flat(&beat_line(&r, true, B, false));
        assert!(!committing.contains("close it first"), "{committing:?}");
    }

    /// The advice that does NOT appear, and must not come back.
    ///
    /// `ServerAliveInterval 60` is the obvious suggestion and it is wrong:
    /// it was already in force on the machine where this failure was
    /// diagnosed, and the remote reset the connection regardless. Telling
    /// every amont user to set it would be confident, actionable and
    /// useless. This test exists so that a future reader who has the same
    /// obvious idea meets an argument instead of a blank.
    #[test]
    fn the_push_note_does_not_recommend_ssh_keepalive() {
        let r = row("cargo-test", 60.0);
        let pushing = flat(&beat_line(&r, true, B, true));
        assert!(
            !pushing.contains("ServerAlive"),
            "keepalive was already on when this failed; recommending it \
             would be useless advice: {pushing:?}"
        );
        assert!(
            pushing.contains("ssh keepalive does not prevent this"),
            "say so, rather than leaving the reader to try it: {pushing:?}"
        );
    }
}
