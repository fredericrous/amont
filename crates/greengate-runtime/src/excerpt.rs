//! The part of a captured run worth reading when it failed.
//!
//! `amont-agent quiet` keeps the whole output in a log file and prints only
//! this: the failure blocks of the run, then the runner's own summary. The
//! heuristic never decides pass or fail (the child's exit code does); it only
//! chooses which lines to show.
//!
//! Hand-written over lowercased lines with word-boundary checks, on purpose:
//! no `regex` dependency (docs/plans/2026-10-11-test-output-keeps-its-status.md,
//! Decisions). The same function exists in amont, which shares no crate with
//! this one; the golden fixtures in `tests/fixtures/excerpt/` are what keep the
//! two copies saying the same thing.
//!
//! ## Memory
//!
//! The run's output can be gigabytes, so the accumulator is fed in chunks and
//! holds at most 64 KB whatever it is fed: the line being assembled (1 KB), a
//! ring of the last 40 lines held to `60 KB - bound`, and the excerpt itself
//! (finished blocks plus the block in progress) held to `bound`.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashSet, VecDeque};
use std::hash::{Hash, Hasher};

/// The excerpt total when `--max-bytes` is not given.
pub const DEFAULT_BOUND: usize = 16 * 1024;
/// The largest `--max-bytes` accepted.
pub const MAX_BOUND: usize = 48 * 1024;

/// What the excerpt and the ring may hold together, leaving the rest of the
/// 64 KB for the line being assembled and bookkeeping.
const HELD: usize = 60 * 1024;
const LINE_CAP: usize = 1024;
const RING_LINES: usize = 40;
const SUMMARY_LINES: usize = 12;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Idle,
    /// A block started at a matched line; it runs to a blank line or a header.
    Block,
    /// cargo's `failures:` … `test result:` section, taken whole.
    Section,
}

pub struct Accumulator {
    bound: usize,
    partial: Vec<u8>,
    partial_cut: bool,
    ring: VecDeque<String>,
    ring_bytes: usize,
    mode: Mode,
    block: Vec<String>,
    block_bytes: usize,
    block_dropped: usize,
    seen: HashSet<u64>,
    excerpt: Vec<String>,
    excerpt_bytes: usize,
    dropped: usize,
    /// Lines seen so far; the line numbers the open block spans; and, beside
    /// each ring line, whether a printed block already holds it — the
    /// summary skips those, so a block near the end is not printed twice.
    lines: usize,
    block_first: usize,
    block_last: usize,
    ring_printed: VecDeque<bool>,
    #[cfg(feature = "test-hooks")]
    peak: usize,
}

impl Accumulator {
    pub fn new(bound: usize) -> Self {
        Accumulator {
            bound: bound.clamp(1, MAX_BOUND),
            partial: Vec::new(),
            partial_cut: false,
            ring: VecDeque::new(),
            ring_bytes: 0,
            mode: Mode::Idle,
            block: Vec::new(),
            block_bytes: 0,
            block_dropped: 0,
            seen: HashSet::new(),
            excerpt: Vec::new(),
            excerpt_bytes: 0,
            dropped: 0,
            lines: 0,
            block_first: 0,
            block_last: 0,
            ring_printed: VecDeque::new(),
            #[cfg(feature = "test-hooks")]
            peak: 0,
        }
    }

    /// Bytes of text currently held, the figure the 64 KB bound is about.
    #[cfg(test)]
    pub fn held(&self) -> usize {
        self.partial.len() + self.ring_bytes + self.excerpt_bytes + self.block_bytes
    }

    #[cfg(feature = "test-hooks")]
    pub fn peak(&self) -> usize {
        self.peak
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if b == b'\n' {
                self.end_line();
            } else if self.partial.len() < LINE_CAP {
                self.partial.push(b);
            } else {
                self.partial_cut = true;
            }
        }
        #[cfg(feature = "test-hooks")]
        {
            let now = self.partial.len() + self.ring_bytes + self.excerpt_bytes + self.block_bytes;
            self.peak = self.peak.max(now);
        }
    }

    fn end_line(&mut self) {
        let raw = std::mem::take(&mut self.partial);
        let cut = std::mem::take(&mut self.partial_cut);
        let mut text = clean(&String::from_utf8_lossy(&raw));
        if cut {
            text.push_str(" …[line cut]");
        }
        self.line(text);
    }

    fn line(&mut self, text: String) {
        self.lines += 1;
        self.ring_bytes += text.len() + 1;
        self.ring.push_back(text.clone());
        self.ring_printed.push_back(false);
        let room = HELD.saturating_sub(self.bound);
        while self.ring.len() > RING_LINES || (self.ring_bytes > room && self.ring.len() > 1) {
            if let Some(old) = self.ring.pop_front() {
                self.ring_bytes -= old.len() + 1;
                self.ring_printed.pop_front();
            }
        }
        // A single line over the room (bound 48 KB leaves 12 KB, a line is
        // at most ~1 KB) cannot happen; the guard above keeps one line.

        let lower = text.to_lowercase();
        let trim = lower.trim();

        match self.mode {
            Mode::Section => {
                if trim.starts_with("test result:") {
                    self.flush();
                    self.mode = Mode::Idle;
                } else {
                    self.push_block(text);
                }
            }
            Mode::Block => {
                if trim.is_empty() {
                    self.flush();
                    self.mode = Mode::Idle;
                } else if trim == "failures:" {
                    self.flush();
                    self.mode = Mode::Section;
                    self.push_block(text);
                } else if is_header(&text, trim) {
                    self.flush();
                    self.mode = Mode::Idle;
                    self.maybe_start(text, trim);
                } else {
                    self.push_block(text);
                }
            }
            Mode::Idle => {
                if trim == "failures:" {
                    self.mode = Mode::Section;
                    self.push_block(text);
                } else {
                    self.maybe_start(text, trim);
                }
            }
        }
    }

    fn maybe_start(&mut self, text: String, trim: &str) {
        if trim.starts_with("test result:") || trim.starts_with("---- ") {
            return;
        }
        if matches(&text, trim) {
            self.mode = Mode::Block;
            self.push_block(text);
        }
    }

    fn push_block(&mut self, text: String) {
        let room = self
            .bound
            .saturating_sub(self.excerpt_bytes + self.block_bytes);
        if self.block.is_empty() && self.block_dropped == 0 {
            self.block_first = self.lines;
        }
        self.block_last = self.lines;
        if text.len() < room {
            self.block_bytes += text.len() + 1;
            self.block.push(text);
        } else {
            self.block_dropped += 1;
        }
    }

    fn flush(&mut self) {
        let lines = std::mem::take(&mut self.block);
        let dropped = std::mem::take(&mut self.block_dropped);
        self.block_bytes = 0;
        if lines.is_empty() && dropped == 0 {
            return;
        }
        if self.excerpt_bytes >= self.bound {
            // Full: count, and keep no hash, so the set stays bounded too.
            self.dropped += lines.len() + dropped;
            return;
        }
        let mut h = DefaultHasher::new();
        lines.hash(&mut h);
        dropped.hash(&mut h);
        // Printed now, or already printed as an identical block: either way
        // the summary must not repeat these lines.
        let first_in_ring = self.lines + 1 - self.ring.len();
        for (i, printed) in self.ring_printed.iter_mut().enumerate() {
            let n = first_in_ring + i;
            if (self.block_first..=self.block_last).contains(&n) {
                *printed = true;
            }
        }
        if !self.seen.insert(h.finish()) {
            return;
        }
        self.dropped += dropped;
        for l in lines {
            if self.excerpt_bytes + l.len() < self.bound {
                self.excerpt_bytes += l.len() + 1;
                self.excerpt.push(l);
            } else {
                self.dropped += 1;
            }
        }
    }

    /// The text to print for a failed run. `log` names the file the rest is in.
    pub fn finish(&mut self, log: &str) -> String {
        if !self.partial.is_empty() || self.partial_cut {
            self.end_line();
        }
        self.flush();
        self.mode = Mode::Idle;

        let mut out = String::new();
        if self.excerpt.is_empty() && self.dropped == 0 {
            for l in tail(&self.ring, RING_LINES) {
                out.push_str(&l);
                out.push('\n');
            }
        } else {
            for l in &self.excerpt {
                out.push_str(l);
                out.push('\n');
            }
            if self.dropped > 0 {
                out.push_str(&format!("… {} more lines, full log: {log}\n", self.dropped));
            }
            // The last lines of the output, less those a block printed.
            let start = self.ring.len().saturating_sub(SUMMARY_LINES);
            let unprinted: VecDeque<String> = self
                .ring
                .iter()
                .zip(&self.ring_printed)
                .skip(start)
                .filter(|(_, printed)| !**printed)
                .map(|(l, _)| l.clone())
                .collect();
            for l in tail(&unprinted, SUMMARY_LINES) {
                out.push_str(&l);
                out.push('\n');
            }
        }
        if out.is_empty() {
            out.push_str("(no output)\n");
        }
        out
    }
}

/// The last `n` lines of the ring without blank lines at either edge.
fn tail(ring: &VecDeque<String>, n: usize) -> Vec<String> {
    let skip = ring.len().saturating_sub(n);
    let mut v: Vec<String> = ring.iter().skip(skip).cloned().collect();
    while v.last().is_some_and(|l| l.trim().is_empty()) {
        v.pop();
    }
    let lead = v.iter().take_while(|l| l.trim().is_empty()).count();
    v.drain(..lead);
    v
}

/// Strip ANSI escape sequences and keep what a terminal would show after a
/// carriage return (progress bars rewrite their line).
pub fn clean(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut it = line.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('[') => {
                for n in it.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&n) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(n) = it.next() {
                    if n == '\u{7}' {
                        break;
                    }
                    if n == '\u{1b}' {
                        it.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    let last = out
        .split('\r')
        .rfind(|s| !s.is_empty())
        .unwrap_or("")
        .to_string();
    last.trim_end().to_string()
}

fn is_header(text: &str, trim: &str) -> bool {
    trim.starts_with("---- ")
        || trim == "failures:"
        || trim.starts_with("test result:")
        || trim.starts_with("=====")
        || text.trim_start().starts_with("FAIL ")
}

fn is_noise(trim: &str) -> bool {
    [
        "compiling ",
        "downloaded ",
        "downloading ",
        "finished ",
        "fresh ",
        "updating ",
        "locking ",
        "blocking ",
        "running ",
    ]
    .iter()
    .any(|p| trim.starts_with(p))
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether a line reports a failure: a failure word on a word boundary that
/// is not a success report (`0 failed`, `no errors found`, a passing test
/// whose name contains `error`).
fn matches(text: &str, trim: &str) -> bool {
    if is_noise(trim) {
        return false;
    }
    if trim.starts_with('✓') || trim.starts_with('✔') {
        return false;
    }
    if trim.starts_with("test ") && trim.ends_with("... ok") {
        return false;
    }
    if text.contains('✗') || text.contains('×') {
        return true;
    }
    let mut prev = String::new();
    for tok in text.split(|c: char| !is_word(c)).filter(|t| !t.is_empty()) {
        let lower = tok.to_lowercase();
        let keyword = matches!(
            lower.as_str(),
            "error"
                | "errors"
                | "failed"
                | "failure"
                | "failures"
                | "fail"
                | "panicked"
                | "assert"
                | "assertion"
                | "assertionerror"
                | "traceback"
                | "exception"
        ) || (tok.len() > 5
            && (tok.ends_with("Error") || tok.ends_with("Exception")));
        if keyword && prev != "0" && prev != "no" {
            return true;
        }
        prev = lower;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(input: &str) -> String {
        let mut a = Accumulator::new(DEFAULT_BOUND);
        a.feed(input.as_bytes());
        a.finish("LOG")
    }

    #[test]
    fn a_block_near_the_end_is_not_printed_twice() {
        let out = run("building\nthread 'x' panicked at src/a.rs:3:5:\n  assertion left == right\n\ntest result: FAILED. 1 passed; 1 failed\n");
        assert_eq!(out.matches("panicked at").count(), 1, "{out}");
        assert!(out.contains("test result: FAILED"), "{out}");
    }

    #[test]
    fn success_lines_do_not_match() {
        for l in [
            "test result: ok. 3 passed; 0 failed; 0 ignored",
            "no errors found",
            "0 errors, 0 warnings",
            "test parse::handles_error_variants ... ok",
            "   Compiling error-chain v0.12.4",
            "    ✓ reports the error",
        ] {
            let lower = l.to_lowercase();
            assert!(!matches(l, lower.trim()), "{l}");
        }
    }

    #[test]
    fn failure_lines_match() {
        for l in [
            "thread 'x' panicked at src/a.rs:1:2:",
            "test a::b ... FAILED",
            "E   AssertionError: no",
            "tests/t.py:14: AssertionError",
            "error[E0308]: mismatched types",
            "  3:7   error    'unused' is assigned",
            "× formats currency",
            "FAIL  src/format.test.ts",
            "TypeError: x is not a function",
        ] {
            let lower = l.to_lowercase();
            assert!(matches(l, lower.trim()), "{l}");
        }
    }

    #[test]
    fn nothing_matched_gives_the_last_lines() {
        let input: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let out = run(&input);
        assert_eq!(out.lines().count(), 40);
        assert!(out.ends_with("line 99\n"));
    }

    #[test]
    fn empty_output_says_so() {
        assert_eq!(run(""), "(no output)\n");
    }

    #[test]
    fn a_long_line_is_cut_with_a_marker() {
        let out = run(&format!("error {}\n", "x".repeat(5000)));
        assert!(out.contains("…[line cut]"));
        assert!(out.len() < 3000);
    }

    #[test]
    fn ansi_and_carriage_returns_are_stripped() {
        assert_eq!(clean("\u{1b}[31merror\u{1b}[0m: x"), "error: x");
        assert_eq!(clean("10%\r50%\r100% done"), "100% done");
    }

    #[test]
    fn the_accumulator_stays_bounded() {
        for bound in [1024, DEFAULT_BOUND, MAX_BOUND] {
            let mut a = Accumulator::new(bound);
            let chunk: String = (0..2000)
                .map(|i| {
                    if i % 3 == 0 {
                        format!("error: failure number {i} {}\n", "y".repeat(900))
                    } else {
                        format!("plain line {i} {}\n", "z".repeat(1500))
                    }
                })
                .collect();
            // ~7 MB per pass, a few passes: far past every bound.
            for _ in 0..4 {
                a.feed(chunk.as_bytes());
                assert!(a.held() <= 64 * 1024, "held {} at bound {bound}", a.held());
            }
            let out = a.finish("LOG");
            assert!(out.contains("more lines, full log: LOG"));
        }
    }
}
