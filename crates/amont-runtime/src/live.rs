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
    pub fn begin(names: &[&str]) -> Arc<Stage> {
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
                    })
                    .collect(),
            ),
            out: Mutex::new(0),
            live: enabled() && watching(),
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
            let _ = std::thread::Builder::new()
                .name("amont-live".into())
                .spawn(move || tick(weak));
        } else if enabled() {
            // Nobody is watching a terminal — an agent, CI, a pipe — and a
            // captured check shows nothing until it finishes. The heartbeat
            // is the one line a minute that says it is alive, which is the
            // difference between "wait" and "kill it" for whoever is on the
            // other end of the pipe.
            let weak = Arc::downgrade(&stage);
            let _ = std::thread::Builder::new()
                .name("amont-heartbeat".into())
                .spawn(move || heartbeat(weak));
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
    pub fn finish(&self, idx: usize) {
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
        self.repaint(&mut drawn);
    }

    /// Erase and redraw the region in one stderr write. Lock order is
    /// `out` → `slots`, everywhere — never the reverse.
    fn repaint(&self, drawn: &mut usize) {
        if !self.live {
            return;
        }
        let entries: Vec<Row> = {
            let slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
            let now = Instant::now();
            slots
                .iter()
                .filter(|s| s.running && !s.done)
                .map(|s| Row {
                    name: s.name.clone(),
                    elapsed: now.duration_since(s.started).as_secs_f64(),
                    quiet: now.duration_since(s.last_output).as_secs_f64(),
                })
                .collect()
        };
        let text = region(&entries, term_width(), budgets());
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
fn tick(weak: Weak<Stage>) {
    loop {
        std::thread::sleep(std::time::Duration::from_millis(80));
        let Some(stage) = weak.upgrade() else { return };
        if stage.stop.load(Ordering::Relaxed) {
            return;
        }
        let mut drawn = stage.out.lock().unwrap_or_else(|p| p.into_inner());
        stage.repaint(&mut drawn);
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
}

/// The two clocks, as the region annotates them: `(idle, ceiling)` in
/// seconds, `0` for off.
#[derive(Debug, Clone, Copy)]
pub struct Budgets {
    pub idle: u64,
    pub ceiling: u64,
}

fn budgets() -> Budgets {
    Budgets {
        idle: crate::hooks::common::idle_timeout(),
        ceiling: crate::hooks::common::check_timeout(),
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
        if row.quiet >= QUIET_NOTE_SECS {
            let quiet = crate::hooks::common::human_secs(row.quiet as u64);
            if budgets.idle > 0 {
                line.push_str(&format!(
                    " · quiet {quiet}/{}",
                    crate::hooks::common::human_secs(budgets.idle)
                ));
            } else {
                line.push_str(&format!(" · quiet {quiet}"));
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
fn heartbeat(weak: Weak<Stage>) {
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
                    due.push((
                        Row {
                            name: s.name.clone(),
                            elapsed: elapsed as f64,
                            quiet: now.duration_since(s.last_output).as_secs_f64(),
                        },
                        first,
                    ));
                }
            }
            due
        };
        if due.is_empty() {
            continue;
        }
        let text: String = due
            .iter()
            .map(|(row, first)| beat_line(row, *first, budgets(), stage.on_push))
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
    let mut line = format!(
        "  … {} still running: {}, last output {} ago",
        row.name,
        human_secs(row.elapsed as u64),
        human_secs(row.quiet as u64)
    );
    if first {
        let idle = match budgets.idle {
            0 => "off".to_string(),
            s => human_secs(s),
        };
        let ceiling = match budgets.ceiling {
            0 => "off".to_string(),
            s => human_secs(s),
        };
        line.push_str(&format!(
            " (killed after {idle} of silence or {ceiling} in total — amont.idleTimeout / amont.timeout)"
        ));
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

/// `$COLUMNS` when it is exported and sane, else a conservative 100 — the
/// region's lines are short and an ioctl is not worth its portability.
///
/// `pub` is now wider than it needs to be — the out-of-crate caller that
/// justified it, `amont-agent`, is its own project and carries its own copy.
/// Left public rather than narrowed in the same change that removed it.
pub fn term_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse::<usize>().ok())
        .filter(|w| *w >= 20)
        .unwrap_or(100)
}

/// Emits slot `idx`'s block when dropped — however the check's closure
/// exits, a panic included: the partial output of a check that died still
/// reaches the reader, above the dead-check verdict the runner fills in.
pub struct FinishOnDrop<'a> {
    stage: &'a Stage,
    idx: usize,
}

impl<'a> FinishOnDrop<'a> {
    pub fn new(stage: &'a Stage, idx: usize) -> FinishOnDrop<'a> {
        FinishOnDrop { stage, idx }
    }
}

impl Drop for FinishOnDrop<'_> {
    fn drop(&mut self) {
        self.stage.finish(self.idx);
    }
}

/// Uninstalls the thread's sink on drop, whatever path the check took out.
pub struct SinkGuard;

impl Drop for SinkGuard {
    fn drop(&mut self) {
        SINK.with(|s| *s.borrow_mut() = None);
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

/// Whether the capture mechanism is on at all. `amont.progress false` is the
/// escape hatch back to raw streaming — one knob, read once.
pub fn enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| crate::config::boolean_or("amont.progress", true))
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

    /// The atomicity contract at the unit level: two threads writing
    /// interleaved lines into their own slots come out as two contiguous
    /// buffers, whatever the scheduler did.
    #[test]
    fn slots_do_not_share_a_buffer() {
        let stage = Stage::begin(&["a", "b"]);
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
        let stage = Stage::begin(&["a"]);
        say("goes to stdout, not to a slot");
        let slots = stage.slots.lock().unwrap();
        assert!(slots[0].buf.is_empty());
    }

    /// After finish, late writes are dropped rather than stranded — a child
    /// reader thread that outlives its check must not corrupt a later block.
    #[test]
    fn a_finished_slot_takes_no_more_writes() {
        let stage = Stage::begin(&["a"]);
        stage.append_raw(0, b"before\n");
        stage.finish(0);
        stage.append_raw(0, b"after\n");
        let slots = stage.slots.lock().unwrap();
        assert!(slots[0].buf.is_empty(), "a write landed after finish");
    }

    /// A repo-derived check name cannot smuggle control bytes onto a live
    /// terminal: sanitised at begin, once, for every later paint.
    #[test]
    fn a_slot_name_is_sanitised_at_begin() {
        let stage = Stage::begin(&["evil\u{1b}[2Jname\rhere"]);
        let slots = stage.slots.lock().unwrap();
        assert!(!slots[0].name.contains('\u{1b}'), "{:?}", slots[0].name);
        assert!(!slots[0].name.contains('\r'), "{:?}", slots[0].name);
    }

    /// Region names drop the stage's own prefix — it is the same twelve
    /// characters on every line.
    #[test]
    fn a_slot_name_drops_the_stage_prefix() {
        let stage = Stage::begin(&["pre-commit-clippy", "pre-push-run-tests", "bare"]);
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
        }
    }

    const B: Budgets = Budgets {
        idle: 120,
        ceiling: 3600,
    };

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
