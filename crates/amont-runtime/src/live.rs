//! One check, one block: per-check output capture for the concurrent stage.
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

use std::cell::RefCell;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// One check's place in the stage.
struct Slot {
    #[allow(dead_code)] // the live region (next change) reads these two
    name: String,
    #[allow(dead_code)]
    started: Instant,
    buf: Vec<u8>,
    done: bool,
}

/// A running stage: the slots, and the one lock every terminal write inside
/// the stage goes through.
pub struct Stage {
    slots: Mutex<Vec<Slot>>,
    /// Serialises block emission (and, next change, region repaints).
    out: Mutex<()>,
}

thread_local! {
    /// Where [`say`] routes on THIS thread: a stage and a slot index.
    static SINK: RefCell<Option<(Arc<Stage>, usize)>> = const { RefCell::new(None) };
}

impl Stage {
    /// A stage over `names`, in dispatch order. Does nothing visible until
    /// checks start finishing.
    pub fn begin(names: &[&str]) -> Arc<Stage> {
        let now = Instant::now();
        Arc::new(Stage {
            slots: Mutex::new(
                names
                    .iter()
                    .map(|n| Slot {
                        name: (*n).to_string(),
                        started: now,
                        buf: Vec::new(),
                        done: false,
                    })
                    .collect(),
            ),
            out: Mutex::new(()),
        })
    }

    /// Route this thread's [`say`] calls into slot `idx` until the guard
    /// drops. Installed by the dispatcher around each `check.run`.
    pub fn enter(self: &Arc<Stage>, idx: usize) -> SinkGuard {
        SINK.with(|s| *s.borrow_mut() = Some((Arc::clone(self), idx)));
        SinkGuard
    }

    /// Append raw bytes (a captured child's output) to slot `idx`.
    pub fn append_raw(&self, idx: usize, bytes: &[u8]) {
        let mut slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(slot) = slots.get_mut(idx) {
            if !slot.done {
                slot.buf.extend_from_slice(bytes);
            }
        }
    }

    fn append_line(&self, idx: usize, line: &str) {
        let mut slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(slot) = slots.get_mut(idx) {
            if !slot.done {
                slot.buf.extend_from_slice(line.as_bytes());
                slot.buf.push(b'\n');
            }
        }
    }

    /// The check is over: emit everything it said as ONE contiguous write.
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
            std::mem::take(&mut slot.buf)
        };
        if block.is_empty() {
            return;
        }
        let _out = self.out.lock().unwrap_or_else(|p| p.into_inner());
        let stdout = std::io::stdout();
        let mut handle = stdout.lock();
        let _ = handle.write_all(&block);
        let _ = handle.flush();
    }
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
}
