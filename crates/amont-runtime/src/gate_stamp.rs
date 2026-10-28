//! The record that turns "a declaration exists" into "the check ran".
//!
//! Moving a gate entry to commit time (`docs/checks.md`, "Moving a gate entry
//! earlier") makes the push gate skip a script because a `pre-commit`
//! declaration covers it. A declaration is a promise on paper: a commit made
//! with `--no-verify`, from a libgit2 client that runs no hooks, or on a
//! machine without amont was never judged by it — and until this module
//! existed, push time had no way to tell those commits from checked ones.
//!
//! Three hooks share one record:
//!
//! 1. **pre-commit** ([`record`]) writes a one-shot marker into `$GIT_DIR`
//!    naming the gate scripts that actually ran, bound to the tree the commit
//!    is about to write (`git write-tree` — during pre-commit the index IS
//!    the commit's content; `staged_only` parks only the working tree).
//! 2. **post-commit** ([`bind_to_head`]) consumes the marker and, when the
//!    marker's tree matches `HEAD^{tree}`, stamps the commit in a notes ref.
//!    `--no-verify` skips pre-commit but NOT post-commit, so an unchecked
//!    commit arrives here with no marker and gets no stamp — which is the
//!    entire point. The tree comparison makes an aborted commit's leftover
//!    marker harmless, and a retried commit of the SAME tree correctly
//!    stamped: the check really did run on exactly that content.
//! 3. **pre-push** ([`stamps_for`]) reads the stamps back and suppresses a
//!    gate script only for pushes whose relevant commits all carry it.
//! 4. **pre-push, on the way out** ([`stamp_push`]) writes the push-time
//!    gates that PASSED onto each pushed tip — and `amont run pre-push`,
//!    which drives the same dispatcher, writes the same stamp for `HEAD`.
//!    The next push of that tree finds the stamp and skips the gate. That is
//!    what lets the suite run BEFORE git opens its connection: a rehearsal
//!    stamps, the push verifies. And a push that passed the gate but died on
//!    the wire — a remote that dropped the idle session while the suite ran
//!    — retries in seconds instead of minutes. Push-time tokens are full
//!    ids (`pre-push-run-tests-js`), commit-time ones are script names
//!    (`test`); the two share a note and never collide.
//!
//! Every failure mode points the same direction: no marker, a mismatched
//! tree, a missing note, a rewritten hash — all mean "no stamp", and no stamp
//! means the push gate RUNS. Nothing here can let an unchecked commit
//! through; it can only cost a redundant gate run.
//!
//! # The second half of the note: what a run COST, and how it ENDED
//!
//! The token line above answers one question — may this gate be skipped for
//! this content — and it is deliberately a set, with no history in it. A
//! stamp says a gate passed; it cannot say a gate has been passing in four
//! seconds since the day its test runner stopped finding any tests.
//!
//! So the note carries optional extra lines, one per RUN:
//!
//! ```text
//! amont-gate-v1 pre-push-cargo-test
//! run 1726900000 pre-push-cargo-test pass 412391
//! run 1726903600 pre-push-audit-js fail 903
//! ```
//!
//! Additive on purpose, and in the only direction that is safe: every reader
//! that existed before these lines did reads `body.lines().next()` and is
//! unaffected, and an amont old enough to rewrite the note without them loses
//! history — never a verdict. The `run` lines are EVIDENCE, read by
//! [`crate::gate_evidence`]; nothing in this module's skip decisions consults
//! them, so a corrupted or forged one cannot make a check be skipped.
//!
//! One more kind of line, and the one exception to that rule (ADR-0013):
//!
//! ```text
//! deferred cargo-test
//! ```
//!
//! A commit-time gate the busy policy deferred to CI. Pre-push reads it
//! ([`deferred_for`]) so the gate's push twin defers too instead of running
//! the suite with the remote's connection held open. It can only ever move a
//! gate to CI — it is never a token, never stamps, never attests — so a
//! forged line makes CI run more, never less. Kept apart from the `run`
//! history, whose [`MAX_RUNS`] trim must not forget it.
//!
//! The note's key is the tree, which makes the record a per-FINGERPRINT one
//! for free: two runs of one gate against one tree are two lines in one note,
//! and if they disagree the gate is flaky on content that did not change.
//!
//! Why a notes ref and not config: notes are keyed by commit, garbage-collect
//! with unreachable commits (an `amont.checked.<hash>` config key would
//! outlive every rebase forever), stay local (notes refs are not pushed by
//! default), and stay out of `git log` (only `refs/notes/commits` displays by
//! default). `amont uninstall` deletes the ref; see `uninstall_repo_hooks`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// First token of the marker file and of every note body. Versioned like
/// `staged_only::FORMAT`: a future amont that changes the shape bumps this,
/// and an old record is ignored rather than misread.
pub const FORMAT: &str = "amont-gate-v1";

/// The notes ref, spelled the way `git notes --ref` wants it.
pub const NOTES_REF: &str = "amont-gate";

/// The same ref, fully qualified — what `git update-ref -d` needs.
pub const NOTES_FULL_REF: &str = "refs/notes/amont-gate";

/// The second ref: stamps keyed by a gate's declared-input fingerprint
/// (`.github/attest-inputs`, see [`crate::attest_inputs`]) instead of by the
/// whole tree. A gate whose inputs a repository declares is reused on any tree
/// where those inputs are byte-identical — the closing plan commit changes the
/// tree and none of them. Every key there is an oid that is not an object, as
/// in attest's own inputs ref, so `git notes prune` on it would drop
/// everything: never run it, and never merge the ref with `cat_sort_uniq`.
pub const INPUTS_REF: &str = "amont-gate-inputs";
pub const INPUTS_FULL_REF: &str = "refs/notes/amont-gate-inputs";

/// The marker's filename inside `$GIT_DIR`.
const MARKER: &str = "amont-gate";

/// The switch for push-time stamps — both writing them and honouring them.
/// On by default: a stamp is amont's own record of a gate it ran on exactly
/// this content, the same trust the commit-time stamps have always carried.
const PUSH_STAMPS: &str = "amont.pushStamps";

/// Does this repository reuse push-time stamps?
pub fn push_stamps_enabled(settings: &crate::config::Settings) -> bool {
    crate::config::boolean_or(settings, PUSH_STAMPS, true)
}

/// The switch for commit-time reuse — a pre-commit gate that already ran
/// clean against exactly this staged tree is not run again. On by default,
/// for the same reason `pushStamps` is: the record is amont's own, bound to
/// the content, and identical content is what a test suite reads.
const COMMIT_STAMPS: &str = "amont.commitStamps";

/// Does this repository reuse a gate's verdict across commit attempts of
/// one tree?
pub fn commit_stamps_enabled(settings: &crate::config::Settings) -> bool {
    crate::config::boolean_or(settings, COMMIT_STAMPS, true)
}

/// The first word of an evidence line. A line that does not start with it is
/// not one, and is dropped rather than guessed at.
const RUN: &str = "run";

/// How many runs one note keeps, newest last. A note is read and rewritten on
/// the push path, so it has to stay small; 64 runs of one tree is already far
/// more retries than any content sees, and the statistics this feeds are
/// computed ACROSS notes, not within one.
const MAX_RUNS: usize = 64;

/// How a single run of one gate ended.
///
/// The check vocabulary, kept whole rather than flattened to pass/fail:
/// `Unavailable` is the outcome the fleet audit of 2026-09-19 kept finding —
/// a gate that could not run, reported as a warning, counted by nobody — and
/// collapsing it into "fail" would hide it all over again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    Passed,
    Failed,
    Warned,
    Fixed,
    Unavailable,
    Inert,
    // Tree gates (ADR-0024): why a commit's tree was NOT proven. None is a
    // verdict about the content; together they are the hit rate's misses.
    /// No completion marker in the current cache namespace.
    Cold,
    /// Another run held the gate's cache lock.
    Busy,
    /// The tool the gate would run is not the one CI resolves.
    Skew,
    /// The tree was not exactly the commit's (unstaged, untracked, ignored
    /// outside the allow-list, or an operation in progress).
    Withheld,
    /// Still running when the commit was ready, or past its deadline.
    Cancelled,
    /// Not started: its last run would not fit in this commit's cover plus
    /// the slack.
    Slow,
    /// Not run: the busy policy deferred it to CI (ADR-0013).
    Deferred,
}

impl RunOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            RunOutcome::Passed => "pass",
            RunOutcome::Failed => "fail",
            RunOutcome::Warned => "warn",
            RunOutcome::Fixed => "fixed",
            RunOutcome::Unavailable => "unavailable",
            RunOutcome::Inert => "inert",
            RunOutcome::Cold => "cold",
            RunOutcome::Busy => "busy",
            RunOutcome::Skew => "skew",
            RunOutcome::Withheld => "withheld",
            RunOutcome::Cancelled => "cancelled",
            RunOutcome::Slow => "slow",
            RunOutcome::Deferred => "deferred",
        }
    }

    /// The parse side. `None` for anything unknown — a future amont may write
    /// an outcome this one has never heard of, and inventing a meaning for it
    /// would be worse than dropping the line.
    pub fn parse(s: &str) -> Option<RunOutcome> {
        Some(match s {
            "pass" => RunOutcome::Passed,
            "fail" => RunOutcome::Failed,
            "warn" => RunOutcome::Warned,
            "fixed" => RunOutcome::Fixed,
            "unavailable" => RunOutcome::Unavailable,
            "inert" => RunOutcome::Inert,
            "cold" => RunOutcome::Cold,
            "busy" => RunOutcome::Busy,
            "skew" => RunOutcome::Skew,
            "withheld" => RunOutcome::Withheld,
            "cancelled" => RunOutcome::Cancelled,
            "slow" => RunOutcome::Slow,
            "deferred" => RunOutcome::Deferred,
            _ => return None,
        })
    }

    /// Did this run reach a verdict about the content? Only these two are
    /// evidence about whether a gate works — the rest are a gate declining to
    /// judge, and counting them as passes is how a no-op looks healthy.
    pub fn is_verdict(self) -> bool {
        matches!(self, RunOutcome::Passed | RunOutcome::Failed)
    }
}

/// One run of one gate: when, which, how it ended, how long it took.
///
/// Wall clock, in milliseconds, measured around the check's own `run` — not
/// CPU time and not the hook's total, because the number this exists to catch
/// is "the suite that used to take eleven minutes returned in 0.4 seconds",
/// and that is a wall-clock claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// Seconds since the epoch, as the machine that ran it saw them.
    pub at: u64,
    pub gate: String,
    pub outcome: RunOutcome,
    pub ms: u64,
}

impl Run {
    fn render(&self) -> String {
        format!(
            "{RUN} {} {} {} {}",
            self.at,
            self.gate,
            self.outcome.as_str(),
            self.ms
        )
    }

    /// `run <epoch> <gate> <outcome> <ms>`, or nothing. Every field must be
    /// there and parse; a half-understood line is dropped whole.
    fn parse(line: &str) -> Option<Run> {
        let mut t = line.split_whitespace();
        if t.next() != Some(RUN) {
            return None;
        }
        let at = t.next()?.parse().ok()?;
        let gate = t.next()?.to_string();
        let outcome = RunOutcome::parse(t.next()?)?;
        let ms = t.next()?.parse().ok()?;
        Some(Run {
            at,
            gate,
            outcome,
            ms,
        })
    }
}

/// A parsed note body: the skip tokens, and the runs recorded against this
/// key.
///
/// THE reader and THE writer of the format, so that the three callers that
/// rewrite a note cannot each drop a half of it they were not thinking
/// about. `stamp_push` merging tokens used to render the note from its own
/// token list alone, which — once runs existed — would have erased every one
/// of them on the next push.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Note {
    pub tokens: Vec<String>,
    /// Commit-time gates deferred to CI on this content; see the module
    /// documentation. Never trimmed with the runs.
    pub deferred: Vec<String>,
    pub runs: Vec<Run>,
}

/// The first word of a deferral line.
const DEFERRED: &str = "deferred";

impl Note {
    /// Parse a note body. A first line that is not ours yields an EMPTY note:
    /// a note somebody else wrote into our ref vouches for nothing, and its
    /// remaining lines are not evidence either.
    pub fn parse(body: &str) -> Note {
        let mut lines = body.lines();
        let Some(first) = lines.next() else {
            return Note::default();
        };
        let mut tokens = first.split_whitespace();
        if tokens.next() != Some(FORMAT) {
            return Note::default();
        }
        let mut deferred = Vec::new();
        let mut runs = Vec::new();
        for line in lines {
            let mut t = line.split_whitespace();
            match (t.next(), t.next(), t.next()) {
                (Some(DEFERRED), Some(gate), None) => {
                    if !deferred.iter().any(|d| d == gate) {
                        deferred.push(gate.to_string());
                    }
                }
                _ => runs.extend(Run::parse(line)),
            }
        }
        Note {
            tokens: tokens.map(str::to_string).collect(),
            deferred,
            runs,
        }
    }

    /// The bytes to hand `git notes add -m`. Line one is exactly what it has
    /// always been, so every reader that stops there sees no change at all.
    pub fn render(&self) -> String {
        let mut body = format!("{FORMAT} {}", self.tokens.join(" "));
        for gate in &self.deferred {
            body.push('\n');
            body.push_str(&format!("{DEFERRED} {gate}"));
        }
        for run in &self.runs {
            body.push('\n');
            body.push_str(&run.render());
        }
        body
    }

    fn add_tokens(&mut self, tokens: &[String]) {
        for t in tokens {
            if !self.tokens.iter().any(|have| have == t) {
                self.tokens.push(t.clone());
            }
        }
    }

    /// Append runs, keeping the newest [`MAX_RUNS`].
    fn add_runs(&mut self, runs: &[Run]) {
        self.runs.extend(runs.iter().cloned());
        if self.runs.len() > MAX_RUNS {
            self.runs.drain(..self.runs.len() - MAX_RUNS);
        }
    }
}

/// The note at `key`, parsed. An absent note, an absent ref and a git that
/// would not answer are all an empty note — the same direction everything
/// here fails in.
fn note_at(key: &str) -> Note {
    note_in(NOTES_REF, key)
}

fn note_in(notes_ref: &str, key: &str) -> Note {
    crate::git::stdout(&["notes", "--ref", notes_ref, "show", key])
        .map(|body| Note::parse(&body))
        .unwrap_or_default()
}

/// The tokens of the gates `tree`'s declared inputs vouch for: for each gate
/// the spec at `tree` declares, the note filed under `K(gate, fingerprint)`.
///
/// Only a token written under that gate's own name is honoured from the note
/// at its key: a note somebody else wrote into the ref cannot vouch for a
/// gate it was not filed under. No spec, an invalid or ambiguous one, a gate
/// with no fingerprint on `tree` (a required path missing), a git that would
/// not answer: all contribute nothing, and nothing means the gate runs.
fn input_tokens(tree: &str) -> Vec<String> {
    let Some(spec) = crate::attest_inputs::spec_at(tree) else {
        return canon_tokens(tree);
    };
    let mut out: Vec<String> = Vec::new();
    for (gate, _) in &spec.gates {
        if !manifest_is_an_input(tree, &spec, gate) {
            continue;
        }
        let Some(fp) = crate::attest_inputs::fingerprint(tree, &spec, gate) else {
            continue;
        };
        let Some(key) = crate::attest_inputs::input_key(gate, &fp) else {
            continue;
        };
        for t in note_in(INPUTS_REF, &key).tokens {
            if crate::gate_identity::base(&t) == gate.as_str() && !out.contains(&t) {
                out.push(t);
            }
        }
    }
    out
}

/// The tokens `tree`'s canonical fingerprint vouches for: a repository that
/// declares nothing is reused on any tree that differs only in the CONTENT of
/// its plans (attest SPEC, "Canonical tree"). One note per fingerprint holds
/// the tokens of every gate that passed on it, as the tree note does for a
/// tree. A declared (even invalid) spec, an opt-out, or a git that would not
/// answer: no canonical route, nothing contributed, the gate runs.
fn canon_tokens(tree: &str) -> Vec<String> {
    canon_key_of(tree)
        .map(|key| note_in(INPUTS_REF, &key).tokens)
        .unwrap_or_default()
}

/// The key of the canonical note of `tree`, if the canonical route is open.
fn canon_key_of(tree: &str) -> Option<String> {
    let canon = crate::attest_inputs::canonical_route(tree)?;
    crate::attest_inputs::canon_gate_key(&canon)
}

/// File `tokens` under the canonical key of `tree`. Best-effort and merged,
/// like [`file_inputs`]. The manifest needs no rule here: `amont.conf` is part
/// of the canonical tree, so a changed declaration is a changed fingerprint.
fn file_canon(tree: &str, tokens: &[String]) {
    let Some(key) = canon_key_of(tree) else {
        return;
    };
    let mut note = note_in(INPUTS_REF, &key);
    note.add_tokens(tokens);
    let _ = write_note_in(INPUTS_REF, &key, &note);
}

/// File `tokens` under the fingerprint keys of the gates `tree` declares, in
/// addition to wherever the caller put them by tree; with no spec at all, under
/// the canonical key. Best-effort, silent, and merged: a miss here is one
/// redundant gate run, which is the safe direction.
fn file_inputs(tree: &str, tokens: &[String]) {
    if tokens.is_empty() {
        return;
    }
    let Some(spec) = crate::attest_inputs::spec_at(tree) else {
        file_canon(tree, tokens);
        return;
    };
    for (gate, _) in &spec.gates {
        let mine: Vec<String> = tokens
            .iter()
            .filter(|t| crate::gate_identity::base(t) == gate.as_str())
            .cloned()
            .collect();
        if mine.is_empty() || !manifest_is_an_input(tree, &spec, gate) {
            continue;
        }
        let Some(fp) = crate::attest_inputs::fingerprint(tree, &spec, gate) else {
            continue;
        };
        let Some(key) = crate::attest_inputs::input_key(gate, &fp) else {
            continue;
        };
        let mut note = note_in(INPUTS_REF, &key);
        note.add_tokens(&mine);
        let _ = write_note_in(INPUTS_REF, &key, &note);
    }
}

/// May a gate's stamp travel by fingerprint on `tree`? A gate's scope and its
/// opt-ins live in the manifest, and under a whole-tree key a manifest change
/// was a tree change; under a fingerprint it is one only if the gate declares
/// the manifest. So a tree that tracks `amont.conf` offers the route only to a
/// gate whose declared inputs cover it, and a scope-only edit then misses.
fn manifest_is_an_input(tree: &str, spec: &crate::attest_inputs::Spec, gate: &str) -> bool {
    let tracked = crate::git::succeeds(&[
        "cat-file",
        "-e",
        &format!("{tree}:{}", crate::manifest::MANIFEST),
    ]);
    !tracked || crate::attest_inputs::declares(spec, gate, crate::manifest::MANIFEST)
}

/// Does the inputs ref exist here? One spawn that lets a repository nothing
/// was ever filed for (an opted-out one, or one whose stamps never passed)
/// skip the fingerprint and canonical routes entirely on the push path.
fn inputs_ref_exists() -> bool {
    crate::git::succeeds(&["rev-parse", "-q", "--verify", INPUTS_FULL_REF])
}

/// The stamp tokens on `tree` (a tree object id). Tree gates are read here
/// and only here (ADR-0024): `tree:<name>` on the pushed tree proves the gate
/// on exactly that content, whichever commit carried it.
pub fn tree_tokens(tree: &str) -> Vec<String> {
    let mut tokens = note_at(tree).tokens;
    if inputs_ref_exists() {
        for t in input_tokens(tree) {
            if !tokens.contains(&t) {
                tokens.push(t);
            }
        }
    }
    tokens
}

/// Add `tokens` to the gate note on `tree` — how a rehearsal records tree
/// gates it proved in its snapshot of exactly that tree. Merged, never
/// rendered from the tokens alone, so evidence already on the note survives.
pub fn stamp_tree(tree: &str, tokens: &[String]) -> bool {
    let mut note = note_at(tree);
    note.add_tokens(tokens);
    let ok = write_note(tree, &note);
    file_inputs(tree, tokens);
    ok
}

/// Write `note` at `key`. Best-effort, like every writer here.
fn write_note(key: &str, note: &Note) -> bool {
    write_note_in(NOTES_REF, key, note)
}

fn write_note_in(notes_ref: &str, key: &str, note: &Note) -> bool {
    let body = note.render();
    crate::git::succeeds(&["notes", "--ref", notes_ref, "add", "-f", "-m", &body, key])
}

/// Record what a gate run COST and how it ENDED, against the content it ran
/// on.
///
/// Evidence only: nothing in this module reads these back to decide whether a
/// check may be skipped, which is why recording a FAILED run — the case the
/// stamp path deliberately has no opinion about — is safe here.
///
/// Best-effort and silent. A note git refused costs a row in a report nobody
/// is blocked on; warning about it on every push would train people to ignore
/// the warnings that do gate something.
pub fn record_runs(key: &str, runs: &[Run]) {
    // A gate name with whitespace in it would forge a second field on read.
    // Refused rather than escaped: every id this repository can produce is
    // already free of it, so the escaping would be a code path no real input
    // ever reaches — untested by construction.
    let runs: Vec<Run> = runs
        .iter()
        .filter(|r| !r.gate.is_empty() && !r.gate.contains(char::is_whitespace))
        .cloned()
        .collect();
    if runs.is_empty() {
        return;
    }
    let mut note = note_at(key);
    note.add_runs(&runs);
    let _ = write_note(key, &note);
}

/// pre-commit: record on `tree` (the staged tree, which is the commit's)
/// that the busy policy deferred `gates`, by their commit-time script names.
/// Best-effort and silent: a lost line runs the push twin, the safe way.
pub fn record_deferred(tree: &str, gates: &[String]) {
    let gates: Vec<&String> = gates
        .iter()
        .filter(|g| !g.is_empty() && !g.contains(char::is_whitespace))
        .collect();
    if gates.is_empty() {
        return;
    }
    let mut note = note_at(tree);
    for g in gates {
        if !note.deferred.contains(g) {
            note.deferred.push(g.clone());
        }
    }
    let _ = write_note(tree, &note); // best-effort: a lost line runs the push twin, the safe way
}

/// Which of `commits` carry a deferral of `gate` on their tree's note (or
/// their own). Read only to defer a push twin, never to skip one: a commit
/// in this set still has no stamp.
pub fn deferred_for(commits: &[String], gate: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    if commits.is_empty() {
        return out;
    }
    let mut args: Vec<&str> = vec!["log", "--no-walk", "--format=%H %T"];
    args.extend(commits.iter().map(String::as_str));
    let Some(list) = crate::git::stdout(&args) else {
        return out;
    };
    for line in list.lines() {
        let mut it = line.split_whitespace();
        let (Some(commit), Some(tree)) = (it.next(), it.next()) else {
            continue;
        };
        if [commit, tree]
            .iter()
            .any(|key| note_at(key).deferred.iter().any(|d| d == gate))
        {
            out.insert(commit.to_string());
        }
    }
    out
}

/// `$GIT_DIR/amont-gate` — the worktree-PRIVATE gitdir, deliberately: the
/// commit this marker waits for happens in this worktree. The stamps the
/// marker becomes live in the common dir (a notes ref) and are shared.
fn marker_path() -> Option<PathBuf> {
    let dir = crate::git::stdout(&["rev-parse", "--git-dir"])?;
    Some(std::path::Path::new(&dir).join(MARKER))
}

/// pre-commit: record that `scripts` ran clean against the tree the commit
/// will carry.
///
/// Called on EVERY pre-commit verdict, with an empty list when nothing
/// qualifying ran (or the commit is about to be blocked) — an aborted or
/// unchecked attempt must not inherit a previous attempt's marker.
///
/// Best-effort throughout: a failure to record costs one redundant gate run
/// at push time, which is the safe direction, and a pre-commit that failed a
/// COMMIT over bookkeeping would be the tail wagging the dog.
pub fn record(scripts: &[&str]) {
    let Some(path) = marker_path() else { return };
    if scripts.is_empty() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    // The index, as the object id `git commit` is about to seal. Inherits
    // `GIT_INDEX_FILE`, so `git commit -a`'s temporary index answers here
    // too. Pure read of the index: writes objects, touches no ref.
    let Some(tree) = crate::git::stdout(&["write-tree"]) else {
        // Not "nothing ran": git could not name the tree, so nothing may be
        // vouched for. Dropping the marker is the fail-safe half (the gate
        // re-runs at push); saying so is the half that was missing.
        crate::hooks::common::warn(
            "git would not name the staged tree — this commit records no gate stamp",
        );
        let _ = std::fs::remove_file(&path);
        return;
    };
    let mut body = format!("{FORMAT}\n{tree}\n");
    for s in scripts {
        body.push_str(s);
        body.push('\n');
    }
    let _ = std::fs::write(&path, body);
}

/// pre-commit, before a gate runs: the scripts a previous run already
/// vouched for against the tree this commit is about to seal.
///
/// Two records answer, both bound to the TREE (`git write-tree` of the index
/// under the staged-only hold — the commit's content, not the working
/// tree):
///
/// 1. the one-shot marker [`record`] left behind. post-commit consumes it,
///    so a marker that is still there belongs to an attempt that never
///    reached post-commit — a `commit-msg` refusal, an editor closed on an
///    empty message, a Ctrl-C at the prompt. The gates ran, the tree is the
///    same, the verdict stands: measured on this machine, a subject three
///    characters too long replayed a ten-minute suite for nothing.
/// 2. the tree note [`bind_to_head`] writes — a reword, a `reset --soft` and
///    re-commit, or a rebase that kept the tree.
///
/// Every failure mode reads as "nothing is vouched for", which runs the
/// gate: no `write-tree`, a marker for another tree, a wrong format, a git
/// that would not answer. Reuse can only ever skip a run the record proves
/// happened on this exact content; it can never let unjudged content
/// through — the direction every function in this module fails in.
///
/// Repo-controlled inputs to the gate (its command line, its scope) live in
/// the manifest, which is part of the tree: a changed declaration is a
/// changed tree, and nothing is reused by tree. A gate reused by fingerprint
/// gets the same guarantee only if it declares the manifest as an input
/// ([`manifest_is_an_input`]); otherwise it is not offered the route at all.
pub fn vouched_for_staged_tree() -> HashSet<String> {
    let mut out = HashSet::new();
    let Some(tree) = crate::git::stdout(&["write-tree"]) else {
        return out;
    };
    if let Some(path) = marker_path() {
        if let Ok(body) = std::fs::read_to_string(&path) {
            let mut lines = body.lines();
            if lines.next() == Some(FORMAT) && lines.next() == Some(tree.as_str()) {
                out.extend(lines.filter(|l| !l.trim().is_empty()).map(str::to_string));
            }
        }
    }
    // The tree note: `notes show` fails loudly on an absent note, and a
    // failure here is simply "no note" — the marker's answer stands alone.
    // Tokens are `<name>@<identity>` (`gate_identity`): commit-time ones are
    // named by script, push-time ones by full id (`pre-push-…`), which no
    // pre-commit declaration is named after — harmless in the set, and a
    // reader skips only on the complete token. The note's `run` lines are evidence and are not
    // tokens: `Note` keeps the two apart so nothing here can be skipped by a
    // line that only records how long something took.
    out.extend(note_at(&tree).tokens);
    // A gate the repository declares inputs for is also vouched for by any
    // earlier tree whose declared inputs are byte-identical to this staged
    // tree's: a plan-only commit changes the tree and none of them. The tree
    // here is the INDEX's (`write-tree`), so partial staging is judged on what
    // is about to be committed, not on the working copy.
    if inputs_ref_exists() {
        out.extend(input_tokens(&tree));
    }
    out
}

/// post-commit: consume the marker; stamp HEAD when the tree still matches.
///
/// One-shot by construction — the marker is deleted before anything is
/// judged, so no path through here can leave it to vouch for a later commit.
///
/// Returns the scripts it actually stamped (empty on every no-stamp path,
/// including a note git refused). The caller subtracts this from what the
/// manifest declares to learn what the commit dodged — [`crate::bypass`]
/// keeps that count. Two records, two questions: the stamp gates a check,
/// the ledger only counts.
pub fn bind_to_head() -> Vec<String> {
    let Some(path) = marker_path() else {
        return Vec::new();
    };
    let Ok(body) = std::fs::read_to_string(&path) else {
        return Vec::new(); // no marker: nothing ran at pre-commit, nothing to stamp
    };
    let _ = std::fs::remove_file(&path);
    let mut lines = body.lines();
    if lines.next() != Some(FORMAT) {
        return Vec::new();
    }
    let Some(tree) = lines.next() else {
        return Vec::new();
    };
    let scripts: Vec<&str> = lines.filter(|l| !l.trim().is_empty()).collect();
    if scripts.is_empty() {
        return Vec::new();
    }
    let Some(head_tree) = crate::git::stdout(&["rev-parse", "HEAD^{tree}"]) else {
        crate::hooks::common::warn(
            "git would not name this commit's tree — no gate stamp was written",
        );
        return Vec::new();
    };
    // A different tree means this commit is not the one pre-commit judged —
    // the marker is a dead letter from an aborted attempt.
    if head_tree != tree {
        return Vec::new();
    }
    let scripts: Vec<String> = scripts.iter().map(|s| s.to_string()).collect();
    // The stamp goes on the TREE as well as the commit, and the tree is the
    // one that survives the way work actually reaches `main`.
    //
    // A squash-merge is performed by the forge: it produces a commit nobody
    // here ever saw, carrying no note, so a later `git push` of a tag on
    // that commit re-ran every gate the branch had already proved. Measured
    // on this repository: a branch push took 13 seconds and the tag push
    // that followed took the full suite and died on a reset connection.
    //
    // The tree is identical across that merge whenever the base has not
    // moved — five of five merges in one afternoon here — and identical
    // trees are identical CONTENT, which is the only thing a test suite
    // reads. That is the same argument `attest` makes for signing the tree
    // rather than the commit, and this module's marker has been tree-bound
    // since it was written; this only carries the binding through to the
    // note.
    //
    // Both, not either: the commit note is what a `git log --notes` reader
    // sees, and dropping it would make the stamps invisible in the place
    // people look for them.
    // MERGED into whatever is there, never rendered from the scripts alone:
    // a pre-push run against this same tree may already have recorded
    // evidence lines on it, and an `add -f` built from this list would erase
    // them.
    let mut tree_note = note_at(&head_tree);
    tree_note.add_tokens(&scripts);
    let _ = write_note(&head_tree, &tree_note);
    file_inputs(&head_tree, &scripts);
    let mut head_note = note_at("HEAD");
    head_note.add_tokens(&scripts);
    if !write_note("HEAD", &head_note) {
        // A note git refused is not a stamp — and the push will re-run these
        // checks, which is right but looks arbitrary unless it is said.
        crate::hooks::common::warn(
            "git refused to write the gate stamp — these checks will run again at push",
        );
        return Vec::new();
    }
    // The ledger counts NAMES: what the commit dodged is judged against what
    // the manifest declares, and the identity is the stamp's business alone.
    scripts
        .iter()
        .map(|s| crate::gate_identity::base(s).to_string())
        .collect()
}

/// pre-push, after every block gate passed: record that `gates` passed
/// against `tree`, the content of `commit` — MERGING with whatever the note
/// already says, because a commit-time stamp (`test`) may already sit there
/// and `notes add -f` would otherwise erase it.
///
/// Best-effort, like every writer here: a stamp that could not be written
/// costs one redundant gate run on the next push, which is the safe
/// direction. Returns whether the commit's note was written.
pub fn stamp_push(commit: &str, tree: &str, gates: &[String]) -> bool {
    if gates.is_empty() {
        return false;
    }
    let mut written = false;
    for key in [tree, commit] {
        let mut note = note_at(key);
        note.add_tokens(gates);
        let ok = write_note(key, &note);
        if key == commit {
            written = ok;
        }
    }
    file_inputs(tree, gates);
    if !written {
        crate::hooks::common::warn(
            "git refused to write the push stamp — these gates will run again on the next push",
        );
    }
    written
}

/// pre-push: which of `commits` carry a stamp, and for which scripts.
///
/// One `notes list` narrows the reads to commits that have a note at all;
/// absent ref, unparseable note, wrong format version — all read as "no
/// stamp", which re-runs the gate.
pub fn stamps_for(commits: &[String]) -> HashMap<String, Vec<String>> {
    let mut out = HashMap::new();
    if commits.is_empty() {
        return out;
    }
    let Some(list) = crate::git::stdout(&["notes", "--ref", NOTES_REF, "list"]) else {
        // NOT the absent-ref case, whatever an older comment here claimed:
        // `notes list` exits 0 with empty output when the ref does not exist,
        // so that arrives as `Some("")` and falls through as "nothing is
        // stamped" — correctly. Reaching HERE means git could not answer at
        // all. Same verdict (the gates re-run: never skip work on a question
        // we could not ask), different sentence, because a transient git
        // failure that reads as "nothing is stamped" is indistinguishable
        // from the real thing — which is exactly how one flaky spawn cost a
        // day of not-diagnosing.
        crate::hooks::common::warn(
            "git would not list the gate stamps — every gated check will run again",
        );
        return out;
    };
    let noted: HashSet<&str> = list
        .lines()
        .filter_map(|l| l.split_whitespace().nth(1))
        .collect();
    // Commit -> tree, in ONE spawn rather than one per commit: this is the
    // push path, and a `rev-parse` each would be a process per pushed
    // commit to answer a question `git log` answers in a batch.
    let mut trees: HashMap<String, String> = HashMap::new();
    {
        let mut args: Vec<&str> = vec!["log", "--no-walk", "--format=%H %T"];
        args.extend(commits.iter().map(String::as_str));
        if let Some(out) = crate::git::stdout(&args) {
            for line in out.lines() {
                let mut it = line.split_whitespace();
                if let (Some(c), Some(t)) = (it.next(), it.next()) {
                    trees.insert(c.to_string(), t.to_string());
                }
            }
        }
        // No mapping is not an error: every commit simply falls back to the
        // commit-keyed lookup below, which is what happened before trees
        // were stamped at all.
    }

    // Declared-input stamps are keyed by fingerprint, not by tree, so a commit
    // whose tree carries no note can still be vouched for. Asked only when the
    // ref exists: a repository nothing was filed for pays one spawn. Once
    // stamps are filed (declared or canonical), each pushed commit pays the
    // few git spawns of its route.
    let by_inputs = inputs_ref_exists();

    for commit in commits {
        // The commit's own note AND its TREE's, merged. Both are written only
        // for that tree (`bind_to_head` checks `HEAD^{tree}`, `stamp_push` is
        // handed the commit's own tree), so every token on either vouches for
        // exactly this content.
        //
        // Not "the commit's note, else the tree's". A squash-merge produces a
        // commit this machine never saw — no note — while the content, and
        // therefore the tree, is the one the gates ran on, and falling through
        // to the tree is what lets a tag push on a merged commit skip work the
        // branch already proved. But the same preference hid the tree's push
        // tokens behind a commit note after an AMEND that kept the tree:
        // post-commit wrote a commit note holding only the commit-time
        // scripts, `stamps_for` then stopped there, and the push gate ran
        // again for content it had already judged.
        //
        // The direction of failure is unchanged: no note on either, an
        // unparseable one, or a git that would not answer all mean "no
        // stamp", and no stamp runs the gate.
        let mut tokens: Vec<String> = Vec::new();
        let mut keys: Vec<&str> = Vec::new();
        if noted.contains(commit.as_str()) {
            keys.push(commit);
        }
        if let Some(tree) = trees.get(commit).filter(|t| noted.contains(t.as_str())) {
            keys.push(tree);
        }
        for key in keys {
            for t in note_at(key).tokens {
                if !tokens.contains(&t) {
                    tokens.push(t);
                }
            }
        }
        if by_inputs {
            if let Some(tree) = trees.get(commit) {
                for t in input_tokens(tree) {
                    if !tokens.contains(&t) {
                        tokens.push(t);
                    }
                }
            }
        }
        if tokens.is_empty() {
            continue;
        }
        out.insert(commit.clone(), tokens);
    }
    out
}

/// uninstall: forget everything this module ever wrote here.
///
/// The stamps are OUR bookkeeping — unlike `hook.skip` and `amont.severity`,
/// which are the user's statements and are never touched.
pub fn forget() -> bool {
    let marker = marker_path().is_some_and(|path| std::fs::remove_file(&path).is_ok());
    let notes = crate::git::succeeds(&["update-ref", "-d", NOTES_FULL_REF]);
    let inputs = crate::git::succeeds(&["update-ref", "-d", INPUTS_FULL_REF]);
    marker || notes || inputs
}

/// The same, for a repository this process is not standing in.
pub fn forget_in(repo: &std::path::Path) -> bool {
    let marker = crate::git::stdout_in(repo, &["rev-parse", "--absolute-git-dir"])
        .is_some_and(|dir| std::fs::remove_file(std::path::Path::new(&dir).join(MARKER)).is_ok());
    let notes = crate::git::succeeds_in(repo, &["update-ref", "-d", NOTES_FULL_REF]);
    let inputs = crate::git::succeeds_in(repo, &["update-ref", "-d", INPUTS_FULL_REF]);
    marker || notes || inputs
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// A real repository, because every function here is a conversation with
    /// git — hand-rolled fixtures would test the conversation we imagined.
    fn repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gate-stamp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "--template=", "."]);
        git(&dir, &["config", "user.email", "t@t.test"]);
        git(&dir, &["config", "user.name", "t"]);
        dir
    }

    /// A fixture git call that FAILS where it fails.
    ///
    /// This used to discard the exit status, and that is how a rare flake
    /// stayed unreadable for a day: if the setup `git commit` did not
    /// happen, the test carried on to an unborn HEAD, and the panic landed
    /// three lines later on a missing gate stamp — a product-shaped
    /// failure for a fixture-shaped cause. Same rule the checks obey:
    /// git failing is not git answering.
    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "fixture: git {args:?} in {} exited {:?}: {}",
            dir.display(),
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// The module talks to the repo at the process cwd; these tests each set
    /// it. Serialised via the crate-wide lock, because cwd is process-global
    /// and `attest`'s tests move it too.
    fn in_repo<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
        let _guard = crate::TEST_CWD.lock().unwrap_or_else(|p| p.into_inner());
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir).unwrap();
        let r = f();
        std::env::set_current_dir(prev).unwrap();
        r
    }

    #[test]
    fn the_commit_note_does_not_hide_the_tree_notes_push_tokens() {
        let dir = repo("union");
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        git(&dir, &["add", "a.ts"]);
        in_repo(&dir, || {
            record(&["test"]);
            git(&dir, &["commit", "-qm", "chore: a"]);
            bind_to_head();
            let head = git(&dir, &["rev-parse", "HEAD"]);
            let tree = git(&dir, &["rev-parse", "HEAD^{tree}"]);
            // A push-time stamp lands on the TREE only (the shape a rehearsal
            // leaves), while the commit note keeps just the commit-time script.
            assert!(stamp_tree(&tree, &["pre-push-cargo-test".to_string()]));
            assert_eq!(note_at(&head).tokens, vec!["test".to_string()]);
            let stamps = stamps_for(std::slice::from_ref(&head));
            assert_eq!(
                stamps.get(&head).map(Vec::as_slice),
                Some(&["test".to_string(), "pre-push-cargo-test".to_string()][..]),
                "both notes speak for the same tree"
            );
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_commit_with_only_a_tree_note_still_reads_the_tree_tokens() {
        let dir = repo("tree-only");
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        git(&dir, &["add", "a.ts"]);
        git(&dir, &["commit", "-qm", "chore: a"]);
        in_repo(&dir, || {
            let head = git(&dir, &["rev-parse", "HEAD"]);
            let tree = git(&dir, &["rev-parse", "HEAD^{tree}"]);
            assert!(stamp_tree(&tree, &["pre-push-cargo-test".to_string()]));
            let stamps = stamps_for(std::slice::from_ref(&head));
            assert_eq!(
                stamps.get(&head).map(Vec::as_slice),
                Some(&["pre-push-cargo-test".to_string()][..])
            );
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_commit_with_only_a_commit_note_still_reads_its_tokens() {
        let dir = repo("commit-only");
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        git(&dir, &["add", "a.ts"]);
        git(&dir, &["commit", "-qm", "chore: a"]);
        in_repo(&dir, || {
            let head = git(&dir, &["rev-parse", "HEAD"]);
            let mut note = Note::default();
            note.add_tokens(&["typecheck".to_string()]);
            assert!(write_note(&head, &note));
            let stamps = stamps_for(std::slice::from_ref(&head));
            assert_eq!(
                stamps.get(&head).map(Vec::as_slice),
                Some(&["typecheck".to_string()][..])
            );
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_token_on_both_notes_is_read_once() {
        let dir = repo("dedup");
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        git(&dir, &["add", "a.ts"]);
        in_repo(&dir, || {
            record(&["test"]);
            git(&dir, &["commit", "-qm", "chore: a"]);
            bind_to_head(); // writes `test` on the commit AND the tree
            let head = git(&dir, &["rev-parse", "HEAD"]);
            let stamps = stamps_for(std::slice::from_ref(&head));
            assert_eq!(
                stamps.get(&head).map(Vec::as_slice),
                Some(&["test".to_string()][..])
            );
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_recorded_marker_becomes_a_stamp_on_the_matching_commit() {
        let dir = repo("roundtrip");
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        git(&dir, &["add", "a.ts"]);
        in_repo(&dir, || {
            record(&["typecheck", "test"]);
            git(&dir, &["commit", "-qm", "chore: a"]);
            let stamped = bind_to_head();
            assert_eq!(
                stamped,
                vec!["typecheck".to_string(), "test".to_string()],
                "bind_to_head reports the scripts it stamped"
            );
            let head = git(&dir, &["rev-parse", "HEAD"]);
            let stamps = stamps_for(std::slice::from_ref(&head));
            assert_eq!(
                stamps.get(&head).map(Vec::as_slice),
                Some(&["typecheck".to_string(), "test".to_string()][..])
            );
            // One-shot: the marker is gone.
            // One-shot: the marker is gone. The fixture's own path, not
            // `marker_path()` — that helper spawns git, and a transient
            // spawn failure on a loaded runner reads as `None` here while
            // production code correctly treats it as "no marker". Seen once,
            // on Windows, as an unwrap panic in a sibling test.
            assert!(!dir.join(".git").join(MARKER).exists());
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_marker_for_a_different_tree_stamps_nothing() {
        let dir = repo("stale");
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        git(&dir, &["add", "a.ts"]);
        in_repo(&dir, || {
            record(&["typecheck"]);
            // The commit that actually lands carries DIFFERENT content — the
            // aborted-attempt-then-different-retry shape.
            std::fs::write(dir.join("a.ts"), "y").unwrap();
            git(&dir, &["add", "a.ts"]);
            git(&dir, &["commit", "-qm", "chore: different"]);
            assert!(
                bind_to_head().is_empty(),
                "bind_to_head reports nothing when the tree moved"
            );
            let head = git(&dir, &["rev-parse", "HEAD"]);
            assert!(
                stamps_for(&[head]).is_empty(),
                "a stale marker must not vouch"
            );
            assert!(
                !dir.join(".git").join(MARKER).exists(),
                "consumed either way"
            );
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_record_clears_a_previous_marker() {
        let dir = repo("clears");
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        git(&dir, &["add", "a.ts"]);
        in_repo(&dir, || {
            record(&["typecheck"]);
            assert!(dir.join(".git").join(MARKER).exists());
            record(&[]);
            assert!(!dir.join(".git").join(MARKER).exists());
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The version guard's REJECT branch, fed a hand-written marker: an old
    /// (or future) format is ignored rather than misread — the doc's claim,
    /// now pinned. Every other test's markers come from record() itself and
    /// so always carry the current FORMAT.
    #[test]
    fn a_marker_in_an_unknown_format_stamps_nothing() {
        let dir = repo("wrongformat");
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        git(&dir, &["add", "a.ts"]);
        in_repo(&dir, || {
            let tree = git(&dir, &["write-tree"]);
            let marker = dir.join(".git").join(MARKER);
            std::fs::write(&marker, format!("amont-gate-v99\n{tree}\ntypecheck\n")).unwrap();
            git(&dir, &["commit", "-qm", "chore: a"]);
            bind_to_head();
            let head = git(&dir, &["rev-parse", "HEAD"]);
            assert!(
                stamps_for(std::slice::from_ref(&head)).is_empty(),
                "an unknown format was trusted"
            );
            assert!(!marker.exists(), "consumed either way");
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A note somebody else wrote into OUR ref is not a stamp. Absent this,
    /// `git notes --ref=amont-gate add` would be a one-line way to vouch for
    /// an unchecked commit — the parsing trust boundary of the whole chain.
    #[test]
    fn a_foreign_note_is_not_a_stamp() {
        let dir = repo("foreignnote");
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        git(&dir, &["add", "a.ts"]);
        in_repo(&dir, || {
            git(&dir, &["commit", "-qm", "chore: a"]);
            git(
                &dir,
                &[
                    "notes",
                    "--ref",
                    NOTES_REF,
                    "add",
                    "-m",
                    "typecheck test",
                    "HEAD",
                ],
            );
            let head = git(&dir, &["rev-parse", "HEAD"]);
            assert!(
                stamps_for(std::slice::from_ref(&head)).is_empty(),
                "a note without the format token was trusted"
            );
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An absent notes ref is `Some("")`, not `None` — the distinction the
    /// warning on that branch depends on. If git ever starts failing here
    /// instead, this test fails and the warning stops being a lie.
    #[test]
    fn a_repo_with_no_stamps_answers_emptily_rather_than_failing() {
        let dir = repo("no-stamps");
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        git(&dir, &["add", "a.ts"]);
        git(&dir, &["commit", "-qm", "chore: a"]);
        in_repo(&dir, || {
            assert_eq!(
                crate::git::stdout(&["notes", "--ref", NOTES_REF, "list"]).as_deref(),
                Some(""),
                "an absent notes ref must be an ANSWER, not a failure — the \
                 no-stamps path and the git-is-broken path are told apart by it"
            );
            let head = git(&dir, &["rev-parse", "HEAD"]);
            assert!(stamps_for(&[head]).is_empty());
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The compatibility promise, in both directions.
    ///
    /// A note written before evidence lines existed is ONE line, and it must
    /// parse to exactly the tokens it always did — every skip decision in
    /// this module reads that line, and a reader that needed the new shape
    /// would stop honouring every stamp on every machine the day it shipped.
    #[test]
    fn a_note_from_before_the_run_lines_parses_unchanged() {
        let note = Note::parse("amont-gate-v1 typecheck test");
        assert_eq!(note.tokens, vec!["typecheck", "test"]);
        assert!(note.runs.is_empty());
        // And round-trips to the same bytes, so an old amont reading a note
        // this one rewrote sees what it wrote.
        assert_eq!(note.render(), "amont-gate-v1 typecheck test");
    }

    /// The new shape: line one unchanged, evidence after it.
    #[test]
    fn run_lines_are_read_without_disturbing_the_tokens() {
        let body = "amont-gate-v1 pre-push-cargo-test\n\
                    run 1726900000 pre-push-cargo-test pass 412391\n\
                    run 1726903600 pre-push-audit-js fail 903\n";
        let note = Note::parse(body);
        assert_eq!(note.tokens, vec!["pre-push-cargo-test"]);
        assert_eq!(note.runs.len(), 2);
        assert_eq!(
            note.runs[0],
            Run {
                at: 1_726_900_000,
                gate: "pre-push-cargo-test".into(),
                outcome: RunOutcome::Passed,
                ms: 412_391,
            }
        );
        assert_eq!(note.runs[1].outcome, RunOutcome::Failed);
        assert_eq!(note.render(), body.trim_end());
    }

    /// A line this version does not understand is dropped, not guessed at —
    /// so a future amont may add a field (or an outcome) without an older one
    /// inventing a meaning for it.
    #[test]
    fn an_unreadable_run_line_is_dropped_and_the_rest_survives() {
        let note = Note::parse(
            "amont-gate-v1 test\n\
             run 1726900000 pre-push-cargo-test pass 400\n\
             run tomorrow pre-push-cargo-test pass 400\n\
             run 1726900001 pre-push-cargo-test sideways 400\n\
             banana\n\
             run 1726900002 pre-push-cargo-test fail 500\n",
        );
        assert_eq!(note.tokens, vec!["test"]);
        assert_eq!(note.runs.len(), 2, "{:?}", note.runs);
        assert_eq!(note.runs[1].outcome, RunOutcome::Failed);
    }

    /// A deferral line (ADR-0013) round-trips, is never a token, and is not
    /// trimmed with the run history: more than [`MAX_RUNS`] later runs on the
    /// same tree must not make the push twin forget the commit deferred it.
    #[test]
    fn a_deferral_is_never_a_token_and_outlives_the_run_trim() {
        let mut note = Note::parse("amont-gate-v1 lint\ndeferred cargo-test\n");
        assert_eq!(note.tokens, vec!["lint"]);
        assert_eq!(note.deferred, vec!["cargo-test"]);
        let runs: Vec<Run> = (0..MAX_RUNS as u64 + 10)
            .map(|i| Run {
                at: 1_726_900_000 + i,
                gate: "pre-push-audit-js".into(),
                outcome: RunOutcome::Passed,
                ms: 1,
            })
            .collect();
        note.add_runs(&runs);
        let again = Note::parse(&note.render());
        assert_eq!(again.runs.len(), MAX_RUNS);
        assert_eq!(again.deferred, vec!["cargo-test"]);
        assert_eq!(again.tokens, vec!["lint"]);
    }

    /// A note somebody else wrote into our ref is not evidence either. The
    /// token line is the trust boundary for BOTH halves of the note.
    #[test]
    fn a_foreign_note_yields_no_runs() {
        let note = Note::parse("hello\nrun 1726900000 pre-push-cargo-test pass 400\n");
        assert!(note.tokens.is_empty() && note.runs.is_empty());
    }

    /// Recording evidence must never cost a stamp. `stamp_push` and
    /// `bind_to_head` rewrite the same note, and the first version of this
    /// rendered the note from its own token list — which would have erased
    /// every run line on the next push.
    #[test]
    fn a_stamp_and_its_evidence_survive_each_other() {
        let dir = repo("evidence");
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        git(&dir, &["add", "a.ts"]);
        in_repo(&dir, || {
            git(&dir, &["commit", "-qm", "chore: a"]);
            let head = git(&dir, &["rev-parse", "HEAD"]);
            let tree = git(&dir, &["rev-parse", "HEAD^{tree}"]);
            record_runs(
                &tree,
                &[Run {
                    at: 1_726_900_000,
                    gate: "pre-push-cargo-test".into(),
                    outcome: RunOutcome::Failed,
                    ms: 412_391,
                }],
            );
            // …and now a later push of the same content passes and stamps it.
            assert!(stamp_push(&head, &tree, &["pre-push-cargo-test".into()]));
            let note = note_at(&tree);
            assert_eq!(note.tokens, vec!["pre-push-cargo-test"]);
            assert_eq!(note.runs.len(), 1, "the evidence survived the stamp");
            // The stamp still reads back as a stamp.
            let stamps = stamps_for(std::slice::from_ref(&head));
            assert_eq!(
                stamps.get(&head).map(Vec::as_slice),
                Some(&["pre-push-cargo-test".to_string()][..])
            );
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The whole point of the evidence half: it is not a stamp. A recorded
    /// run — even a passing one — must not let the gate be skipped, because
    /// skipping is decided by the token line and nothing else.
    #[test]
    fn a_recorded_run_is_not_a_stamp() {
        let dir = repo("not-a-stamp");
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        git(&dir, &["add", "a.ts"]);
        in_repo(&dir, || {
            git(&dir, &["commit", "-qm", "chore: a"]);
            let head = git(&dir, &["rev-parse", "HEAD"]);
            let tree = git(&dir, &["rev-parse", "HEAD^{tree}"]);
            record_runs(
                &tree,
                &[Run {
                    at: 1_726_900_000,
                    gate: "pre-push-cargo-test".into(),
                    outcome: RunOutcome::Passed,
                    ms: 412_391,
                }],
            );
            assert!(
                stamps_for(std::slice::from_ref(&head)).is_empty(),
                "a run line vouched for a gate — evidence must never gate"
            );
            assert!(
                !vouched_for_staged_tree().contains("pre-push-cargo-test"),
                "a run line vouched at commit time"
            );
            // It IS readable as history, though.
            let history = crate::gate_evidence::history_in(&dir);
            assert_eq!(history.runs.len(), 1);
            assert_eq!(history.runs[0].0, tree, "keyed by the content it ran on");
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn forget_removes_the_stamps() {
        let dir = repo("forget");
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        git(&dir, &["add", "a.ts"]);
        in_repo(&dir, || {
            record(&["typecheck"]);
            git(&dir, &["commit", "-qm", "chore: a"]);
            bind_to_head();
            let head = git(&dir, &["rev-parse", "HEAD"]);
            assert!(!stamps_for(std::slice::from_ref(&head)).is_empty());
            forget();
            assert!(stamps_for(&[head]).is_empty());
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A squash-merge produces a commit this machine never saw, and the
    /// stamp has to survive it.
    ///
    /// This is the case that motivated tree-stamping. A branch is verified
    /// locally and stamped; the forge squashes it onto `main` as a NEW
    /// commit with no note; pushing a tag on that commit re-ran every gate
    /// the branch had already proved. Measured on this repository: 13
    /// seconds for the branch push, then the full suite for the tag push,
    /// which died on a reset connection.
    ///
    /// The tree is what the gates actually read, and it is identical across
    /// that merge whenever the base has not moved — five of five merges in
    /// one afternoon here.
    #[test]
    fn a_stamp_survives_a_commit_being_rewritten_with_the_same_tree() {
        let dir = repo("squashed");
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        git(&dir, &["add", "a.ts"]);
        in_repo(&dir, || {
            record(&["test"]);
            git(&dir, &["commit", "-qm", "feat: on a branch"]);
            assert_eq!(bind_to_head(), vec!["test".to_string()]);
            let branch_tip = git(&dir, &["rev-parse", "HEAD"]);

            // Stand in for the forge's squash: a DIFFERENT commit object
            // with the SAME tree. `--amend` with a new message is the
            // cheapest way to get exactly that shape.
            git(
                &dir,
                &[
                    "commit",
                    "-q",
                    "--amend",
                    "-m",
                    "feat: squashed by the forge",
                ],
            );
            let merged = git(&dir, &["rev-parse", "HEAD"]);
            assert_ne!(merged, branch_tip, "the fixture must produce a new commit");
            assert_eq!(
                git(&dir, &["rev-parse", "HEAD^{tree}"]),
                git(&dir, &["rev-parse", &format!("{branch_tip}^{{tree}}")]),
                "…carrying the same tree, which is the whole premise"
            );

            let stamps = stamps_for(std::slice::from_ref(&merged));
            assert_eq!(
                stamps.get(&merged).map(Vec::as_slice),
                Some(&["test".to_string()][..]),
                "the stamp must follow the content, not the commit hash"
            );
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// And it must not follow anything else. A commit whose tree was never
    /// stamped gets nothing, however many other stamps exist — otherwise
    /// this widening would vouch for content nobody checked.
    #[test]
    fn a_different_tree_gets_no_stamp_from_the_fallback() {
        let dir = repo("othertree");
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        git(&dir, &["add", "a.ts"]);
        in_repo(&dir, || {
            record(&["test"]);
            git(&dir, &["commit", "-qm", "chore: stamped"]);
            assert_eq!(bind_to_head(), vec!["test".to_string()]);

            // Different CONTENT, so a different tree, and no marker: this
            // commit was never judged.
            std::fs::write(dir.join("b.ts"), "y").unwrap();
            git(&dir, &["add", "b.ts"]);
            git(&dir, &["commit", "-qm", "chore: unjudged"]);
            let unstamped = git(&dir, &["rev-parse", "HEAD"]);

            assert!(
                stamps_for(std::slice::from_ref(&unstamped)).is_empty(),
                "a tree nobody stamped must not inherit one"
            );
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- declared-input reuse (`.github/attest-inputs`) -------------------

    /// A repository declaring that gate `test` reads only `a.ts`, with one
    /// commit stamped `test@abc`. The plans directory is where a later,
    /// plan-only commit will change the tree without touching any input.
    fn declared(name: &str, spec: &str) -> PathBuf {
        let dir = repo(name);
        std::fs::create_dir_all(dir.join(".github")).unwrap();
        std::fs::create_dir_all(dir.join("docs/plans")).unwrap();
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        std::fs::write(dir.join("docs/plans/p.md"), "plan v1").unwrap();
        std::fs::write(dir.join(".github/attest-inputs"), spec).unwrap();
        git(&dir, &["add", "-A"]);
        in_repo(&dir, || {
            record(&["test@abc"]);
            git(&dir, &["commit", "-qm", "feat: judged"]);
            assert_eq!(bind_to_head(), vec!["test".to_string()]);
        });
        dir
    }

    /// Change only the plan and commit it WITHOUT a marker: nothing was
    /// judged for this tree, so only the fingerprint route can speak.
    fn plan_only_commit(dir: &Path) -> String {
        std::fs::write(dir.join("docs/plans/p.md"), "plan v2, closing").unwrap();
        git(dir, &["add", "docs/plans/p.md"]);
        git(dir, &["commit", "-qm", "docs(plan): done"]);
        git(dir, &["rev-parse", "HEAD"])
    }

    #[test]
    fn a_plan_only_commit_reuses_the_stamp_of_a_declared_gate() {
        let dir = declared("plan-only", "test a.ts\n");
        in_repo(&dir, || {
            let head = plan_only_commit(&dir);
            let stamps = stamps_for(std::slice::from_ref(&head));
            assert_eq!(
                stamps.get(&head).map(Vec::as_slice),
                Some(&["test@abc".to_string()][..]),
                "the closing commit changed the tree and none of the declared inputs"
            );
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_staged_tree_of_a_plan_only_commit_is_vouched_for() {
        let dir = declared("staged", "test a.ts\n");
        in_repo(&dir, || {
            std::fs::write(dir.join("docs/plans/p.md"), "plan v2").unwrap();
            git(&dir, &["add", "docs/plans/p.md"]);
            assert!(vouched_for_staged_tree().contains("test@abc"));
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn partial_staging_is_judged_on_the_index_not_the_working_copy() {
        let dir = declared("partial", "test a.ts\n");
        in_repo(&dir, || {
            // The plan is staged; an unstaged edit to a declared input sits in
            // the working copy. The commit would carry only the plan.
            std::fs::write(dir.join("docs/plans/p.md"), "plan v2").unwrap();
            git(&dir, &["add", "docs/plans/p.md"]);
            std::fs::write(dir.join("a.ts"), "unstaged edit").unwrap();
            assert!(vouched_for_staged_tree().contains("test@abc"));

            // The other way round: the input IS staged, so it misses even
            // though the working copy was put back.
            std::fs::write(dir.join("a.ts"), "staged edit").unwrap();
            git(&dir, &["add", "a.ts"]);
            std::fs::write(dir.join("a.ts"), "x").unwrap();
            assert!(!vouched_for_staged_tree().contains("test@abc"));
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_changed_declared_input_misses() {
        let dir = declared("input-changed", "test a.ts\n");
        in_repo(&dir, || {
            std::fs::write(dir.join("a.ts"), "changed").unwrap();
            git(&dir, &["add", "a.ts"]);
            git(&dir, &["commit", "-qm", "feat: unjudged"]);
            let head = git(&dir, &["rev-parse", "HEAD"]);
            assert!(stamps_for(std::slice::from_ref(&head)).is_empty());
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_changed_declaration_misses() {
        let dir = declared("declaration-changed", "test a.ts\n");
        in_repo(&dir, || {
            // The spec's own blob is in every fingerprint: widening what the
            // gate reads is a different declaration, so nothing is reused.
            std::fs::write(dir.join("b.ts"), "y").unwrap();
            std::fs::write(dir.join(".github/attest-inputs"), "test a.ts b.ts\n").unwrap();
            git(&dir, &["add", "-A"]);
            git(&dir, &["commit", "-qm", "chore: widen the declaration"]);
            let head = git(&dir, &["rev-parse", "HEAD"]);
            assert!(stamps_for(std::slice::from_ref(&head)).is_empty());
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_declared_config_file_is_an_input_like_any_other() {
        // A gate's scope lives in a tracked file (`amont.conf`). Declared, a
        // scope-only edit changes the fingerprint and the stamp misses.
        let dir = declared("config", "test a.ts amont.conf\n");
        in_repo(&dir, || {
            // `declared` ran before amont.conf existed: it is a required path,
            // so that first commit had no fingerprint and filed nothing.
            assert!(!inputs_ref_exists());

            std::fs::write(dir.join("amont.conf"), "scope = src\n").unwrap();
            git(&dir, &["add", "amont.conf"]);
            record(&["test@abc"]);
            git(&dir, &["commit", "-qm", "chore: config"]);
            assert_eq!(bind_to_head(), vec!["test".to_string()]);

            std::fs::write(dir.join("docs/plans/p.md"), "plan v2").unwrap();
            git(&dir, &["add", "docs/plans/p.md"]);
            git(&dir, &["commit", "-qm", "docs(plan): done"]);
            let plan = git(&dir, &["rev-parse", "HEAD"]);
            assert!(stamps_for(std::slice::from_ref(&plan)).contains_key(&plan));

            std::fs::write(dir.join("amont.conf"), "scope = lib\n").unwrap();
            git(&dir, &["add", "amont.conf"]);
            git(&dir, &["commit", "-qm", "chore: scope-only change"]);
            let scope = git(&dir, &["rev-parse", "HEAD"]);
            assert!(
                stamps_for(std::slice::from_ref(&scope)).is_empty(),
                "a scope-only change must miss"
            );
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_declared_stamp_is_read_back_as_its_complete_token() {
        // The identity comparison lives in the readers (`pair_verdict`, the
        // push gate): they skip only on the complete token they compute. This
        // pins what the new route owes them: tokens come back verbatim, never
        // widened to the gate's name, and a second identity under the same key
        // is kept apart from the first.
        let dir = declared("identity", "test a.ts\n");
        in_repo(&dir, || {
            let head = plan_only_commit(&dir);
            let tokens = stamps_for(std::slice::from_ref(&head))
                .remove(&head)
                .unwrap_or_default();
            assert_eq!(tokens, vec!["test@abc".to_string()]);

            let tree = git(&dir, &["rev-parse", "HEAD^{tree}"]);
            assert!(stamp_tree(&tree, &["test@def".to_string()]));
            let tokens = tree_tokens(&tree);
            assert!(tokens.contains(&"test@abc".to_string()));
            assert!(tokens.contains(&"test@def".to_string()));
            assert!(!tokens.contains(&"test@zzz".to_string()));
            assert!(!tokens.contains(&"test".to_string()));
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_gate_that_does_not_declare_the_manifest_is_not_offered_the_route() {
        // `amont.conf` is tracked but the gate's declaration omits it: a
        // scope-only edit would be invisible to the fingerprint, so the route
        // is refused outright. Nothing is filed and nothing is reused.
        let dir = repo("manifest-undeclared");
        std::fs::create_dir_all(dir.join(".github")).unwrap();
        std::fs::create_dir_all(dir.join("docs/plans")).unwrap();
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        std::fs::write(dir.join("amont.conf"), "scope = src\n").unwrap();
        std::fs::write(dir.join("docs/plans/p.md"), "v1").unwrap();
        std::fs::write(dir.join(".github/attest-inputs"), "test a.ts\n").unwrap();
        git(&dir, &["add", "-A"]);
        in_repo(&dir, || {
            record(&["test@abc"]);
            git(&dir, &["commit", "-qm", "feat: judged"]);
            bind_to_head();
            assert!(!inputs_ref_exists(), "the route must not open");
            let head = plan_only_commit(&dir);
            assert!(stamps_for(std::slice::from_ref(&head)).is_empty());
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_gate_declaring_the_manifest_opens_the_route() {
        // The counterpart: with `amont.conf` among its declared inputs the
        // gate is filed by fingerprint again.
        let dir = declared("manifest-declared", "test a.ts amont.conf\n");
        in_repo(&dir, || {
            std::fs::write(dir.join("amont.conf"), "scope = src\n").unwrap();
            git(&dir, &["add", "amont.conf"]);
            record(&["test@abc"]);
            git(&dir, &["commit", "-qm", "chore: config"]);
            bind_to_head();
            assert!(inputs_ref_exists());
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_undeclared_gate_keeps_whole_tree_behaviour() {
        let dir = declared("undeclared", "test a.ts\n");
        in_repo(&dir, || {
            // `lint` passed on the first tree but the spec declares only
            // `test`: the plan-only commit is a new tree for `lint`.
            let tree = git(&dir, &["rev-parse", "HEAD^{tree}"]);
            assert!(stamp_tree(&tree, &["lint@zzz".to_string()]));
            let head = plan_only_commit(&dir);
            let tokens = stamps_for(std::slice::from_ref(&head))
                .remove(&head)
                .unwrap_or_default();
            assert!(tokens.contains(&"test@abc".to_string()));
            assert!(!tokens.iter().any(|t| t.starts_with("lint@")));
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_invalid_or_ambiguous_spec_falls_back_to_whole_tree_behaviour() {
        // A glob invalidates the whole spec.
        let dir = declared("invalid", "test src/*\n");
        in_repo(&dir, || {
            assert!(!inputs_ref_exists(), "no valid spec, nothing is filed");
            let head = plan_only_commit(&dir);
            assert!(stamps_for(std::slice::from_ref(&head)).is_empty());
        });
        let _ = std::fs::remove_dir_all(&dir);

        // Both locations present: ambiguous, the route is off.
        let dir = repo("ambiguous");
        std::fs::create_dir_all(dir.join(".github")).unwrap();
        std::fs::create_dir_all(dir.join(".forgejo")).unwrap();
        std::fs::create_dir_all(dir.join("docs/plans")).unwrap();
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        std::fs::write(dir.join("docs/plans/p.md"), "v1").unwrap();
        std::fs::write(dir.join(".github/attest-inputs"), "test a.ts\n").unwrap();
        std::fs::write(dir.join(".forgejo/attest-inputs"), "test a.ts\n").unwrap();
        git(&dir, &["add", "-A"]);
        in_repo(&dir, || {
            record(&["test@abc"]);
            git(&dir, &["commit", "-qm", "feat: judged"]);
            bind_to_head();
            assert!(!inputs_ref_exists());
            let head = plan_only_commit(&dir);
            assert!(stamps_for(std::slice::from_ref(&head)).is_empty());
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_gate_that_did_not_pass_leaves_no_declared_input_stamp() {
        let dir = repo("failed");
        std::fs::create_dir_all(dir.join(".github")).unwrap();
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        std::fs::write(dir.join(".github/attest-inputs"), "test a.ts\n").unwrap();
        git(&dir, &["add", "-A"]);
        in_repo(&dir, || {
            // pre-commit blocked or ran nothing: an empty record, no marker.
            record(&[]);
            git(&dir, &["commit", "-qm", "feat: unjudged"]);
            assert!(bind_to_head().is_empty());
            let tree = git(&dir, &["rev-parse", "HEAD^{tree}"]);
            let head = git(&dir, &["rev-parse", "HEAD"]);
            assert!(!stamp_push(&head, &tree, &[]));
            assert!(!inputs_ref_exists());
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stamp_tree_and_tree_tokens_use_the_inputs_ref() {
        let dir = declared("rehearsal", "test a.ts\n");
        in_repo(&dir, || {
            // A rehearsal proves a gate in a snapshot of this tree …
            let tree = git(&dir, &["rev-parse", "HEAD^{tree}"]);
            assert!(stamp_tree(&tree, &["test@rehearsed".to_string()]));
            // … and a later tree with identical declared inputs reads it back.
            plan_only_commit(&dir);
            let later = git(&dir, &["rev-parse", "HEAD^{tree}"]);
            assert_ne!(later, tree);
            assert!(tree_tokens(&later).contains(&"test@rehearsed".to_string()));
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_inputs_ref_is_keyed_the_way_attest_keys_its_own() {
        let dir = declared("key", "test a.ts\n");
        in_repo(&dir, || {
            let tree = git(&dir, &["rev-parse", "HEAD^{tree}"]);
            let spec = crate::attest_inputs::spec_at(&tree).expect("spec");
            let fp = crate::attest_inputs::fingerprint(&tree, &spec, &spec.gates[0].0).expect("fp");
            let key = crate::attest_inputs::input_key("test", &fp).expect("key");
            let listed = git(&dir, &["notes", "--ref", INPUTS_REF, "list"]);
            assert!(
                listed
                    .lines()
                    .any(|l| l.split_whitespace().nth(1) == Some(key.as_str())),
                "{listed}"
            );
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_foreign_token_in_the_inputs_ref_vouches_for_nothing() {
        let dir = declared("foreign", "test a.ts\n");
        in_repo(&dir, || {
            let tree = git(&dir, &["rev-parse", "HEAD^{tree}"]);
            let spec = crate::attest_inputs::spec_at(&tree).expect("spec");
            let fp = crate::attest_inputs::fingerprint(&tree, &spec, &spec.gates[0].0).expect("fp");
            let key = crate::attest_inputs::input_key("test", &fp).expect("key");
            // Somebody files a token for ANOTHER gate under `test`'s key.
            let mut note = note_in(INPUTS_REF, &key);
            note.add_tokens(&["publish@evil".to_string()]);
            assert!(write_note_in(INPUTS_REF, &key, &note));
            assert!(!tree_tokens(&tree).contains(&"publish@evil".to_string()));
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn forget_removes_the_inputs_ref_too() {
        let dir = declared("forget-inputs", "test a.ts\n");
        in_repo(&dir, || {
            assert!(inputs_ref_exists());
            assert!(forget());
            assert!(!inputs_ref_exists());
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A repository that declares nothing: `a.ts`, a plan, and optionally an
    /// `amont.conf`, judged once (`test@abc`).
    fn canonical(name: &str, conf: Option<&str>) -> PathBuf {
        let dir = repo(name);
        std::fs::create_dir_all(dir.join("docs/plans")).unwrap();
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        std::fs::write(dir.join("docs/plans/p.md"), "plan v1").unwrap();
        if let Some(conf) = conf {
            std::fs::write(dir.join("amont.conf"), conf).unwrap();
        }
        git(&dir, &["add", "-A"]);
        in_repo(&dir, || {
            record(&["test@abc"]);
            git(&dir, &["commit", "-qm", "feat: judged"]);
            assert_eq!(bind_to_head(), vec!["test".to_string()]);
        });
        dir
    }

    fn vouched(dir: &Path, commit: &str) -> Vec<String> {
        in_repo(dir, || {
            stamps_for(std::slice::from_ref(&commit.to_string()))
                .remove(commit)
                .unwrap_or_default()
        })
    }

    #[test]
    fn with_no_spec_a_plans_only_commit_is_vouched_for_canonically() {
        let dir = canonical("canon-plan-edit", None);
        in_repo(&dir, || {
            assert!(inputs_ref_exists(), "the canonical stamp is filed");
            let head = plan_only_commit(&dir);
            assert_eq!(
                stamps_for(std::slice::from_ref(&head))
                    .remove(&head)
                    .unwrap_or_default(),
                vec!["test@abc".to_string()]
            );
            // The staged tree, as pre-commit judges it, is vouched for too.
            std::fs::write(dir.join("docs/plans/p.md"), "plan v3").unwrap();
            git(&dir, &["add", "docs/plans/p.md"]);
            assert!(vouched_for_staged_tree().contains("test@abc"));
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_plan_added_deleted_or_renamed_is_not_vouched_for() {
        let dir = canonical("canon-plan-exists", None);
        std::fs::write(dir.join("docs/plans/q.md"), "second plan").unwrap();
        git(&dir, &["add", "docs/plans/q.md"]);
        git(&dir, &["commit", "-qm", "docs(plan): add"]);
        let added = git(&dir, &["rev-parse", "HEAD"]);
        assert!(vouched(&dir, &added).is_empty(), "a plan added");
        git(&dir, &["mv", "docs/plans/p.md", "docs/plans/r.md"]);
        git(&dir, &["commit", "-qm", "docs(plan): rename"]);
        let renamed = git(&dir, &["rev-parse", "HEAD"]);
        assert!(vouched(&dir, &renamed).is_empty(), "a plan renamed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn with_no_spec_any_other_change_is_not_vouched_for() {
        let dir = canonical("canon-code-edit", Some("gate test = make test\n"));
        std::fs::write(dir.join("a.ts"), "changed").unwrap();
        git(&dir, &["commit", "-qam", "feat: code"]);
        let code = git(&dir, &["rev-parse", "HEAD"]);
        assert!(vouched(&dir, &code).is_empty(), "a source edit");
        // amont.conf is part of the canonical tree: a scope-only edit misses.
        std::fs::write(dir.join("amont.conf"), "gate test = make other\n").unwrap();
        git(&dir, &["commit", "-qam", "chore: config"]);
        let conf = git(&dir, &["rev-parse", "HEAD"]);
        assert!(vouched(&dir, &conf).is_empty(), "a manifest edit");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_exempt_off_closes_the_canonical_route() {
        for (name, conf) in [
            ("off", "set planExempt off\n"),
            ("spelled", "SET  PLANEXEMPT\tOff\r\n"),
            ("after", "set planExempt on\nset planExempt off\n"),
        ] {
            let dir = canonical(&format!("canon-off-{name}"), Some(conf));
            in_repo(&dir, || {
                assert!(!inputs_ref_exists(), "{name}: nothing is filed");
                let head = plan_only_commit(&dir);
                assert!(
                    stamps_for(std::slice::from_ref(&head)).is_empty(),
                    "{name}: a plans-only commit is a new tree"
                );
            });
            let _ = std::fs::remove_dir_all(&dir);
        }
        // `on`, a trailing comment and a comment line do not opt out.
        for (name, conf) in [
            ("on", "set planExempt on\n"),
            ("comment", "set planExempt off # no\n"),
            ("commented", "# set planExempt off\n"),
        ] {
            let dir = canonical(&format!("canon-on-{name}"), Some(conf));
            let head = plan_only_commit(&dir);
            assert!(!vouched(&dir, &head).is_empty(), "{name}: still canonical");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn a_spec_that_cannot_be_used_means_no_canonical_stamps() {
        // A directory where the spec should be: listed, unreadable as a blob.
        let dir = repo("canon-unreadable-spec");
        std::fs::create_dir_all(dir.join(".github/attest-inputs")).unwrap();
        std::fs::create_dir_all(dir.join("docs/plans")).unwrap();
        std::fs::write(dir.join(".github/attest-inputs/x"), "x").unwrap();
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        std::fs::write(dir.join("docs/plans/p.md"), "v1").unwrap();
        git(&dir, &["add", "-A"]);
        in_repo(&dir, || {
            record(&["test@abc"]);
            git(&dir, &["commit", "-qm", "feat: judged"]);
            bind_to_head();
            assert!(!inputs_ref_exists(), "an unreadable spec opens nothing");
            let head = plan_only_commit(&dir);
            assert!(stamps_for(std::slice::from_ref(&head)).is_empty());
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_specs_mean_no_canonical_stamps() {
        let dir = repo("canon-ambiguous");
        std::fs::create_dir_all(dir.join(".github")).unwrap();
        std::fs::create_dir_all(dir.join(".forgejo")).unwrap();
        std::fs::create_dir_all(dir.join("docs/plans")).unwrap();
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        std::fs::write(dir.join("docs/plans/p.md"), "v1").unwrap();
        std::fs::write(dir.join(".github/attest-inputs"), "test a.ts\n").unwrap();
        std::fs::write(dir.join(".forgejo/attest-inputs"), "test a.ts\n").unwrap();
        git(&dir, &["add", "-A"]);
        in_repo(&dir, || {
            record(&["test@abc"]);
            git(&dir, &["commit", "-qm", "feat: judged"]);
            bind_to_head();
            assert!(!inputs_ref_exists());
            let head = plan_only_commit(&dir);
            assert!(stamps_for(std::slice::from_ref(&head)).is_empty());
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_valid_spec_keeps_precedence_over_the_canonical_route() {
        // `lint` is not declared: it must NOT ride the canonical route.
        let dir = declared("canon-precedence", "test a.ts\n");
        in_repo(&dir, || {
            let tree = git(&dir, &["rev-parse", "HEAD^{tree}"]);
            assert!(stamp_tree(&tree, &["lint@zzz".to_string()]));
            let head = plan_only_commit(&dir);
            let tokens = stamps_for(std::slice::from_ref(&head))
                .remove(&head)
                .unwrap_or_default();
            assert!(tokens.contains(&"test@abc".to_string()));
            assert!(!tokens.iter().any(|t| t.starts_with("lint@")));
        });
        let _ = std::fs::remove_dir_all(&dir);
    }
}
