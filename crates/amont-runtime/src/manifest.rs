//! `amont.conf` — checks a repository declares for itself.
//!
//! A third party cannot add a Rust module without rebuilding this binary, so
//! extension means declared commands.
//!
//! The manifest is **committed at the repository root**, and that is the point.
//! `.git/hooks` is not committed, so under the old filename-prefix mechanism a
//! team could never actually share a custom hook — every member had to install
//! it by hand, and nothing told them when it changed. That flaw mattered more
//! than the lexicographic ordering usually cited against prefixes.
//!
//! ```text
//! # stage       name        scope   severity  command
//! pre-commit    lint-shell  *.sh    block     scripts/lint-shell.sh
//! pre-push      smoke       *       warn      make smoke
//! ```
//!
//! Whitespace-delimited, in file order. TOML would be nicer to write and costs a
//! dependency tree that would then run on every commit in ninety-six
//! repositories; for four fields and a command, the twenty lines of parsing win.
//! See `scripts/check-no-deps.sh` for why that trade is the default here.
//!
//! ## No shell
//!
//! The command is split on whitespace and executed directly. There is no shell,
//! so no pipes, no redirection, no globbing and no quoting. Two reasons, and the
//! second is the one that decided it: Windows has no `sh`, and every emulation
//! of one this project has tried has been a source of bugs; and a manifest line
//! that silently gained shell semantics would be a much larger thing to have
//! introduced than it looks. A pipeline belongs in a script the line invokes.
//!
//! ## A line that cannot be understood is not skipped
//!
//! A malformed line means a check the repository asked for is not running, which
//! is precisely the "looks verified, enforced nothing" failure `Outcome` exists
//! to name. So a broken line still produces a check — one that runs to
//! `Unavailable` and says why. It appears in the dispatcher's "could not run"
//! roll-up like any other gap, rather than needing a mechanism of its own.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::check::{Check, Fix, Outcome, Scope, Severity, Stage};
use crate::hooks::common::Restaged;
use crate::registry::{Ctx, CHECKS, ENTRYPOINTS};

pub const MANIFEST: &str = "amont.conf";

/// Why a line could not be used.
///
/// A type rather than a `String`: the prose belongs in `Display`, and a caller
/// that wants to ask "was this a duplicate?" should not have to grep for the
/// word. The tests used to assert on substrings, which coupled them to wording
/// and would have kept passing if the wording stayed while the meaning changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    MissingFields,
    MissingName,
    /// Names a check compiled into the binary.
    NameTaken(String),
    /// The name is a trigger, or carries one as a prefix.
    ///
    /// `pre-commit  pre-commit-clippy  …` would declare a check whose SHORT
    /// name is another check's full id, so `hook.skip pre-commit-clippy` would
    /// mean two things at once. The stage column supplies the trigger; writing
    /// it again in the name is the one way to make an id ambiguous.
    TriggerInName(String),
    /// A second USABLE line claiming a name already claimed ON THE SAME
    /// TRIGGER. The same name on both triggers is two checks, not a clash.
    Duplicate(String),
    BadStage(String),
    BadScope(String),
    BadSeverity(String),
    /// A `tool` line with the wrong shape.
    BadTool,
    /// A `severity`/`skip`/`set` line with the wrong shape; carries the usage.
    BadPolicyLine(&'static str),
    /// A `set` line naming a key policy may not reach.
    UnsettableKey(String),
    /// A `pre-push` line asked to rewrite files.
    ///
    /// Refused HERE, beside `NameTaken` and `Duplicate`, rather than as a
    /// runtime "contract violation" at push time: same fact, discovered
    /// earlier, by more people, at the moment it is cheapest to fix. A pre-push
    /// hook must not modify the worktree or index — the pushed commit would
    /// then differ from the tree the developer is looking at.
    FixOnPrePush,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::MissingFields => {
                write!(f, "expected 5 fields: stage name scope severity command")
            }
            ParseError::MissingName => write!(f, "missing name"),
            ParseError::NameTaken(n) => write!(f, "{n:?} already names a check"),
            ParseError::TriggerInName(n) => write!(
                f,
                "{n:?} must not be a trigger or start with one — the stage column says which"
            ),
            ParseError::Duplicate(n) => write!(f, "{n:?} is declared twice on one trigger"),
            ParseError::BadStage(t) => {
                write!(f, "stage {t:?} must be `pre-commit` or `pre-push`")
            }
            ParseError::BadScope(t) => write!(
                f,
                "scope {t:?} must be `*`, `*.<ext>`, or a bare filename (no `/`), \
                 optionally followed by `+<file>` the repository must carry"
            ),
            ParseError::BadTool => write!(
                f,
                "a tool pin is exactly `tool <program> <version-substring>`"
            ),
            ParseError::BadPolicyLine(usage) => write!(f, "a policy line is `{usage}`"),
            ParseError::UnsettableKey(k) => write!(
                f,
                "set {k:?} is not a policy-settable key — see docs/custom-checks.md"
            ),
            ParseError::FixOnPrePush => write!(
                f,
                "`fix` is only for pre-commit — a pre-push hook must not rewrite files"
            ),
            ParseError::BadSeverity(t) => {
                write!(f, "severity {t:?} must be `block` or `warn`")
            }
        }
    }
}

/// A line that parsed. Every field means something.
///
/// `program` and `args` rather than one `argv`: a runnable check must have a
/// command, and splitting the head off makes that structural instead of a
/// `split_first` guard that can only ever be dead code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    /// `Fix::Rewrite` when the command column began `fix `.
    pub fix: Fix,
    /// The command column carried the `files` marker.
    pub files: bool,
    pub name: String,
    pub stage: Stage,
    pub severity: Severity,
    /// Extensions that gate it. Empty means any change — the `*` scope.
    pub exts: Vec<String>,
    /// Exact filenames that gate it — the scope column's bare tokens.
    pub names: Vec<String>,
    /// Files the REPOSITORY must carry for this check to apply at all — the
    /// scope column's `+` half. Empty means always applicable, which is what
    /// every declaration meant before `amont add` made a line something you
    /// could acquire rather than write.
    pub opt_in: Vec<String>,
    pub program: String,
    pub args: Vec<String>,
}

impl Declared {
    /// `<trigger>-<name>`, the same shape a built-in has.
    ///
    /// This is what `hook.skip` and `amont.severity.<key>` resolve against,
    /// so a declared check answers to its trigger and its short name exactly as
    /// a compiled-in one does. Before it had an id, `hook.skip pre-commit`
    /// silenced fifteen built-ins and left every declared check running.
    pub fn id(&self) -> String {
        format!("{}-{}", self.stage.as_str(), self.name)
    }

    /// The command as written, for display.
    pub fn command(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// One manifest line: usable, or not.
///
/// A SUM, not a struct with an `Option<why>` beside the fields. The struct
/// form let a broken line carry a severity, a scope and an argv that meant
/// nothing — and it produced a wrong diagnosis: because broken and usable
/// entries shared one list, a valid line was rejected as "declared twice" for
/// colliding with a line that could not run. Dedup now sees only `Usable`.
///
/// Separate from `External` because the fleet reads ninety-six manifests and may
/// re-read them on every refresh, while `External` holds a `Scope` whose
/// `&'static` slices are LEAKED.
/// `tool <program> <version-substring>` — a version this repository expects
/// of a tool its checks drive, so cross-machine skew is a printed fact
/// instead of "the hook is flaky here". Verified once per hook run, warn-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolPin {
    pub program: String,
    /// A substring `<program> --version`'s first line must contain — `0.6.`
    /// pins a minor, `0.6.3` pins a patch. Substring, not semver: the point
    /// is agreement between machines, not range arithmetic.
    pub want: String,
}

/// A committed policy statement about a BUILT-IN (or declared) check — the
/// team's decision, shipped with the repository, trust-gated like everything
/// else the manifest says. See `policy`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyLine {
    /// `severity <check|short-name|trigger> warn|block`
    Severity { target: String, severity: Severity },
    /// `skip <check|short-name|trigger>`
    Skip { target: String },
    /// `set <key> <value>` — a committed default for an allowlisted
    /// `amont.*` config key. `key` is the FULL git key (`amont.timeout`),
    /// canonicalised from whatever case the file used, because git keys are
    /// case-insensitive and a policy file stricter than git is a trap.
    Set { key: String, value: String },
}

/// The keys `set` may reach, in canonical spelling. Deliberately absent:
/// `fix` (a committed file must not change what already-trusted commands may
/// DO to your working tree — a different consent than "I read these
/// commands"), `trusted`, `conventions`, `recordBypasses`, `progress`,
/// `knownIdentity`, and the `severity.*` family (its own line kind).
pub const SETTABLE: &[&str] = &[
    "largeFileWarn",
    "largeFileBlock",
    "commit.gitmoji",
    "commit.subjectMax",
    "commit.descriptionMax",
    "commit.bodyWrap",
    "autoRebase",
    "timeout",
    "testPushedTree",
    "minVersion",
];

impl PolicyLine {
    /// The one-line rendering the trust prompt shows — consent must see the
    /// policy it is granting.
    pub fn describe(&self) -> String {
        match self {
            PolicyLine::Severity { target, severity } => {
                format!("severity  {target}  {}", severity.as_str())
            }
            PolicyLine::Skip { target } => format!("skip      {target}"),
            PolicyLine::Set { key, value } => format!("set       {key}  {value}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Usable(Declared),
    /// A tool version pin — carries no check.
    Tool(ToolPin),
    /// A policy statement — carries no check either. The line number rides
    /// along because a "names no check here" note must point somewhere.
    Policy {
        what: PolicyLine,
        lineno: usize,
    },
    Broken {
        /// The declared name, or `<file>:<lineno>` when the line has none — a
        /// gap has to be nameable to be reportable.
        name: String,
        /// Broken lines land on pre-commit unless the stage token parsed: seen
        /// on every commit beats seen on every push.
        stage: Stage,
        lineno: usize,
        why: ParseError,
    },
}

impl Line {
    pub fn name(&self) -> &str {
        match self {
            Line::Usable(d) => &d.name,
            Line::Tool(pin) => &pin.program,
            Line::Policy {
                what: PolicyLine::Severity { target, .. } | PolicyLine::Skip { target },
                ..
            } => target,
            Line::Policy {
                what: PolicyLine::Set { key, .. },
                ..
            } => key,
            Line::Broken { name, .. } => name,
        }
    }

    /// Does this line declare a CHECK? `Tool` and `Policy` lines do not, and
    /// every consumer that projects lines into checks — `parse`, the trust
    /// prompt, the fleet's declared column — must filter on this rather than
    /// pattern-match variants it will forget to extend. A `Broken` line IS a
    /// check (one that runs to Unavailable and says why).
    pub fn is_check(&self) -> bool {
        matches!(self, Line::Usable(_) | Line::Broken { .. })
    }
    pub fn stage(&self) -> Stage {
        match self {
            Line::Usable(d) => d.stage,
            // A pin has no stage; it is verified at both. The value only
            // feeds displays that will not ask a pin for one. Policy lines
            // likewise — `is_check()` keeps both out of anything that would.
            Line::Tool(_) | Line::Policy { .. } => Stage::PreCommit,
            Line::Broken { stage, .. } => *stage,
        }
    }
    /// `Some(reason)` when this line declares a check that cannot run.
    pub fn broken(&self) -> Option<String> {
        match self {
            Line::Usable(_) | Line::Tool(_) | Line::Policy { .. } => None,
            Line::Broken { lineno, why, .. } => Some(format!("line {lineno}: {why}")),
        }
    }

    /// `<trigger>-<name>`, matching `Declared::id` and `External::id`. A broken
    /// line has one too: `hook.skip pre-commit` should silence its nag exactly
    /// as it silences the checks that do run.
    pub fn id(&self) -> String {
        format!("{}-{}", self.stage().as_str(), self.name())
    }

    /// Consume into the identity every line has, and either the declaration or
    /// the reason there is none.
    ///
    /// Both consumers — `External::from` and the fleet's projection — used to
    /// destructure this by hand, and both carried an arm for a combination the
    /// type forbids, because they computed the reason BEFORE matching. Written
    /// once, that arm has nowhere to appear.
    pub fn into_parts(self) -> (String, Stage, Result<Declared, String>) {
        let name = self.name().to_string();
        let stage = self.stage();
        let parsed = match self {
            Line::Usable(d) => Ok(d),
            // Reachable only through a consumer that skipped `is_check()` —
            // kept total so the type stays honest, worded so a leak is
            // recognisable in whatever display it lands in.
            Line::Tool(pin) => Err(format!("tool pin: {} {}", pin.program, pin.want)),
            Line::Policy { what, .. } => Err(format!("policy: {}", what.describe())),
            Line::Broken { lineno, why, .. } => Err(format!("line {lineno}: {why}")),
        };
        (name, stage, parsed)
    }
}

/// A check a repository declares, rather than one compiled in.
pub struct External {
    /// `<trigger>-<name>` — what `hook.skip` and `amont.severity.<key>`
    /// resolve against, and what `Check::name` returns. Built-ins have had this
    /// shape all along; declared checks answering to a bare name were invisible
    /// to `hook.skip pre-commit`.
    pub id: String,
    /// The name as written in the manifest — the "short name" of the vocabulary
    /// — used for messages. A line too malformed to name itself falls back to
    /// its position, and reading `pre-commit-amont.conf:3` back to somebody
    /// helps nobody.
    pub short_name: String,
    pub stage: Stage,
    pub kind: Kind,
}

/// The two things an external can be. `Scope` and `Severity` live only on the
/// runnable side, so a broken external cannot carry a severity nobody applies.
pub enum Kind {
    Runnable {
        scope: Scope,
        severity: Severity,
        program: String,
        args: Vec<String>,
        fix: Fix,
        /// The command column began `files ` — append the matched paths to
        /// the argv, the way a builtin hands its tool the staged list.
        files: bool,
    },
    Unusable {
        why: String,
    },
}

impl Check for External {
    fn name(&self) -> &str {
        &self.id
    }
    fn stage(&self) -> Stage {
        self.stage
    }
    /// Derived for an unusable check rather than stored: it never runs, so its
    /// scope is a question with no answer, and computing one here keeps the
    /// DATA from carrying a value that means nothing.
    fn scope(&self) -> Scope {
        match &self.kind {
            Kind::Runnable { scope, .. } => *scope,
            Kind::Unusable { .. } => Scope::ALWAYS,
        }
    }
    fn severity(&self) -> Severity {
        match &self.kind {
            Kind::Runnable { severity, .. } => *severity,
            // Never consulted: an unusable check reports `Unavailable`, which
            // no severity can turn into a block.
            Kind::Unusable { .. } => Severity::Warn,
        }
    }

    fn run(&self, ctx: &Ctx) -> Outcome {
        let (scope, program, args, fix, files) = match &self.kind {
            Kind::Runnable {
                scope,
                program,
                args,
                fix,
                files,
                ..
            } => (scope, program, args, *fix, *files),
            Kind::Unusable { why } => {
                crate::hooks::common::warn(&format!(
                    "{MANIFEST}: {} — {}",
                    crate::ui::highlight(&self.short_name),
                    // Carries repo tokens: `BadStage("…")` quotes the manifest.
                    crate::ui::sanitize(why)
                ));
                return Outcome::Unavailable;
            }
        };

        // The scope gate lives HERE, unlike a built-in's, which enforces its own
        // in its first three lines. A declared command has no way to know what
        // was staged, so if this did not gate it, `*.sh` would run on every
        // commit and the column would be decoration.
        //
        // Which files to test against depends on the stage: what is staged for
        // a commit, what is being pushed for a push. `*` short-circuits before
        // either is computed, which is the common case.
        // A check whose whole job is to rewrite has nothing to say when nobody
        // asked for rewriting — so it does not RUN, rather than running and
        // having its result discarded. Gating only the re-staging let the
        // command edit files with `amont.fix` off, which is precisely the
        // surprise the gate exists to prevent.
        //
        // `Unavailable`, not `Passed`. `check.rs` defines `Unavailable` as
        // "COULD NOT RUN — a tool is missing, or the opt-in config is absent",
        // which is exactly this; `Passed` is the one verdict it must not
        // report, because the dispatcher's roll-up and the fleet dashboard
        // then show a check that never executed as clean. With a message,
        // because every other `Unavailable` in this codebase says what was
        // missing and an unexplained count on every commit is worse than none.
        if fix == Fix::Rewrite && !crate::hooks::common::fixing_enabled() {
            crate::hooks::common::warn(&format!(
                "{}: declares fix, and {} is off — not run",
                crate::ui::highlight(&self.short_name),
                crate::ui::highlight("amont.fix")
            ));
            return Outcome::Unavailable;
        }

        let in_scope = match self.stage {
            Stage::PreCommit => crate::hooks::common::staged_files(&[]),
            Stage::PrePush => crate::pushrefs::changed_files(ctx.push.get()),
        };
        if !scope.is_unscoped() && !scope.matches(&in_scope) {
            return Outcome::Passed;
        }
        // The paths the declaration's scope actually matched — what a builtin
        // would be handed. Computed here, after the gate, and given to the
        // command two ways: `$AMONT_FILES` always (newline-separated, so a
        // wrapper script never re-derives `git diff --cached` and diverges
        // from the set this gate judged — `amont run --all-files` overrides
        // the set in-process, invisibly to any child that asks git itself),
        // and appended to the argv when the declaration carries the `files`
        // marker.
        let matched = scoped(scope, &in_scope);
        // A files-taking command with no files has nothing to judge — running
        // it bare would make most linters error on an empty argv, blocking a
        // commit over nothing. The builtin convention, applied here.
        if files && matched.is_empty() {
            return Outcome::Passed;
        }
        let root = crate::hooks::common::repo_root();
        // Through `program()`, exactly like every builtin: on Windows,
        // `Command::new("npx")` cannot start `npx.cmd`, and an external that
        // fails to SPAWN reports Unavailable — warn, never block — so the
        // check would silently never run. The guard test that enforces this
        // for builtins scans only `src/hooks/`; this call is the manifest's
        // half of the same rule.
        let mut cmd = Command::new(crate::hooks::common::program(program));
        cmd.args(args).current_dir(&root).stdin(Stdio::null());
        // Env, not only argv: newline-separated so ordinary shell loops can
        // read it. A path with a newline in its name would split wrong — the
        // list is repo-controlled and such a path is already hostile input —
        // and a change set too large for an environment variable (rare, but
        // E2BIG kills the spawn outright) travels as an empty variable
        // instead, which a wrapper treats exactly like "derive it yourself".
        let joined = matched.join("\n");
        cmd.env(
            "AMONT_FILES",
            if joined.len() <= 100_000 {
                joined.as_str()
            } else {
                ""
            },
        );
        if files {
            cmd.args(&matched);
        }
        crate::hooks::common::strip_git_env(&mut cmd);
        // Under the deadline: repo-authored code that outlives the budget is
        // killed and FAILS — "hung" must not read as "passed", and pre-push
        // runs these serially where one hang stalls the entire push.
        let status = match crate::hooks::common::status_streamed(&mut cmd) {
            Ok(crate::hooks::common::Ran::Status(s)) => Ok(s),
            Ok(crate::hooks::common::Ran::TimedOut(budget)) => {
                crate::hooks::common::say_timed_out(&self.short_name, budget);
                return Outcome::Failed;
            }
            Err(e) => Err(e),
        };
        match status {
            // A command that could not be STARTED has not judged anything. This
            // is the distinction `Outcome` was added for: reporting a missing
            // `shellcheck` as a lint failure sends someone hunting for a lint
            // error that does not exist.
            Err(e) => {
                crate::hooks::common::warn(&format!(
                    "{MANIFEST}: {} could not run {} — {}",
                    crate::ui::highlight(&self.short_name),
                    crate::ui::highlight(program),
                    // The io error's text embeds the program name it tried.
                    crate::ui::sanitize(&e.to_string())
                ));
                Outcome::Unavailable
            }
            Ok(s) if s.success() => {
                // A declared fixer that ran clean may still have rewritten
                // something; re-stage exactly what moved. Only its own scope,
                // so it cannot stage a file it never looked at.
                if fix == Fix::Rewrite && crate::hooks::common::fixing_enabled() {
                    match crate::hooks::common::restage(&matched) {
                        Restaged::Staged => {
                            crate::hooks::common::ok(&format!(
                                "{} fixed and re-staged",
                                crate::ui::highlight(&self.short_name)
                            ));
                            return Outcome::Fixed;
                        }
                        // `git add` failed, so the index still holds whatever
                        // the command has already replaced on disk. This used
                        // to be indistinguishable from "nothing moved" and was
                        // reported as a pass.
                        Restaged::Failed(stuck) => {
                            crate::hooks::common::fail(&format!(
                                "{} rewrote files but {} failed — the index still holds the \
                                 OLD content: {}",
                                crate::ui::highlight(&self.short_name),
                                crate::ui::highlight("git add"),
                                crate::ui::sanitize(&stuck.join(", "))
                            ));
                            return Outcome::Failed;
                        }
                        Restaged::Nothing => {}
                    }
                }
                Outcome::Passed
            }
            Ok(_) => {
                crate::hooks::common::fail(&format!(
                    "{} failed (output above)",
                    crate::ui::highlight(&self.short_name)
                ));
                Outcome::Failed
            }
        }
    }
}

/// The paths this check's scope actually covers.
fn scoped(scope: &Scope, paths: &[String]) -> Vec<String> {
    if scope.is_unscoped() {
        return paths.to_vec();
    }
    paths.iter().filter(|p| scope.covers(p)).cloned().collect()
}

/// `Scope` holds `&'static` slices so a built-in can be a `const`. A parsed
/// manifest has neither, so its extension list is leaked.
///
/// This is bounded and deliberate: the manifest is read at most once per
/// process, holds a handful of short strings, and the process is a git hook that
/// exits in milliseconds. The alternative — a lifetime on `Scope` — would
/// propagate through the trait, both dispatchers and the fleet crate to buy back
/// a few hundred bytes that the kernel reclaims moments later.
fn leak(exts: Vec<String>) -> &'static [&'static str] {
    let refs: Vec<&'static str> = exts
        .into_iter()
        .map(|s| &*Box::leak(s.into_boxed_str()))
        .collect();
    Box::leak(refs.into_boxed_slice())
}

/// `*` means any change; `*.sh` or `*.sh,*.bash` gate on extensions; and
/// `<triggers>+<opt-ins>` adds the repository condition a builtin has.
///
/// # `+`, and why there is now an opt-in at all
///
/// This used to read: *"No `opt_in` counterpart, because the manifest IS the
/// opt-in: a repository that does not want the check deletes the line."* That
/// was true while every line was one somebody typed. `amont add` ended it — a
/// vendored line is in your manifest because you took a whole PACK, not
/// because you chose that tool, so a packaged `rubocop` fired on every `.rb`
/// in a repository with no rubocop config and simply errored. That is the
/// "ninety-five nags" `hooks::yamllint` documents avoiding, which is a
/// courtesy every builtin has had and no declaration could express.
///
/// The separator is `+` because that is what `amont list` already PRINTS
/// between the two halves — `inert here — needs .rs + Cargo.toml`. What you
/// read in the listing is what you write in the file. No spaces: the column
/// grammar splits on whitespace.
///
/// Both sides are comma-separated, and each side is an OR — any trigger, and
/// any one opt-in. Between the sides it is an AND, matching `Scope`'s own
/// semantics.
///
/// Splitting on the FIRST `+` means a filename containing one is not
/// expressible. That is the same class of limit as directories being
/// inexpressible, and it is stated in `docs/custom-checks.md` rather than
/// worked around with quoting this grammar deliberately does not have.
///
/// Returns owned strings rather than a `Scope`, so validating a manifest
/// costs nothing permanent. Only `External::from` turns these into the
/// `&'static` form `Scope` requires.
type ParsedScope = (Vec<String>, Vec<String>, Vec<String>);

fn parse_scope(token: &str) -> Result<ParsedScope, ParseError> {
    let (triggers, opt_in) = match token.split_once('+') {
        None => (token, Vec::new()),
        Some((left, right)) => {
            // `*.rb+` promises a condition and states none. Refused rather
            // than read as "no condition", which is the opposite of what the
            // author was reaching for.
            let opts: Vec<String> = right
                .split(',')
                .filter(|p| !p.is_empty())
                .map(str::to_string)
                .collect();
            if opts.is_empty() || right.contains('+') {
                return Err(ParseError::BadScope(token.to_string()));
            }
            for o in &opts {
                if o.contains(['*', '?', '[', '/']) {
                    return Err(ParseError::BadScope(o.clone()));
                }
            }
            // `+cfg` with nothing on the left is "any change, if this repo has
            // cfg" — a real shape, and `*` is how the trigger half spells
            // "anything".
            (if left.is_empty() { "*" } else { left }, opts)
        }
    };
    if triggers == "*" {
        return Ok((Vec::new(), Vec::new(), opt_in));
    }
    let mut exts = Vec::new();
    let mut names = Vec::new();
    for part in triggers.split(',') {
        if let Some(ext) = part.strip_prefix('*').filter(|ext| ext.starts_with('.')) {
            exts.push(ext.to_string());
            continue;
        }
        // A bare token is an exact FILENAME — `package.json`, `Dockerfile`,
        // `.prettierrc` — matched against the basename. Directories are not
        // expressible (a `/` is refused), and anything that LOOKS like a glob
        // (`*`, `?`, `[`) is refused as the typo it almost certainly is —
        // this grammar deliberately has no globs to mis-guess.
        if !part.is_empty() && !part.contains(['*', '?', '[', '/']) {
            names.push(part.to_string());
            continue;
        }
        return Err(ParseError::BadScope(part.to_string()));
    }
    Ok((exts, names, opt_in))
}

fn parse_stage(token: &str) -> Option<Stage> {
    match token {
        "pre-commit" => Some(Stage::PreCommit),
        "pre-push" => Some(Stage::PrePush),
        _ => None,
    }
}

/// An identity already spoken for. An external must not be able to shadow
/// `pre-push-branch-protect` — nor silently lose to it, which is what a
/// first-match lookup would do without this.
///
/// Judged on the ID, which is why `clippy` on `pre-push` is now legal: it is
/// `pre-push-clippy`, a different check from `pre-commit-clippy`. Judged on the
/// bare name, as it was, the two collided and the second was refused.
///
fn name_is_taken(id: &str) -> bool {
    CHECKS.iter().any(|c| c.name == id) || ENTRYPOINTS.iter().any(|(n, _)| *n == id)
}

/// A short name that says its own trigger — either by being one, or by starting
/// with one.
///
/// Both make an id ambiguous rather than merely ugly. `pre-commit` as a name
/// gives `hook.skip pre-commit` two readings; `pre-commit-clippy` as a name
/// gives a check whose SHORT name is the built-in's FULL id, so one skip
/// silences both. The stage column already says which trigger this is.
fn name_says_its_trigger(name: &str) -> bool {
    crate::TRIGGERS
        .iter()
        .any(|t| name == *t || name.starts_with(&format!("{t}-")))
}

/// The four leading tokens and the untouched remainder, or `None` when the line
/// does not have them.
///
/// The arity is in the TYPE. Returning a `Vec` made "four or it is malformed"
/// a rule every caller had to remember and none could be checked against.
///
/// NOT `splitn(5, char::is_whitespace)`: that splits at the FIRST whitespace
/// character every time, so a file aligned into columns — which is how the
/// format invites you to write it — yields empty fields for every run of
/// spaces after the first.
fn tokenise(line: &str) -> Option<([&str; 4], &str)> {
    let mut fields: [&str; 4] = [""; 4];
    let mut rest = line;
    for slot in fields.iter_mut() {
        rest = rest.trim_start();
        let i = rest.find(char::is_whitespace)?;
        *slot = &rest[..i];
        rest = &rest[i..];
    }
    let command = rest.trim();
    (!command.is_empty()).then_some((fields, command))
}

pub fn parse_lines(text: &str) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let lineno = i + 1;
        out.push(parse_line(lineno, line, &out));
    }
    out
}

/// One line, given the lines already accepted.
///
/// `earlier` is read ONLY for the duplicate check, and only its `Usable`
/// entries — a line that cannot run does not reserve its name. Before this, a
/// valid declaration was rejected as "declared twice" for colliding with a
/// broken one, which pointed the reader at the wrong line entirely.
fn parse_line(lineno: usize, line: &str, earlier: &[Line]) -> Line {
    // `tool` was never a valid stage, so claiming it as a keyword breaks no
    // manifest that ever parsed. Handled before `tokenise`, whose five-column
    // shape a three-token pin does not have.
    if line == "tool" || line.starts_with("tool ") || line.starts_with("tool\t") {
        let mut it = line.split_whitespace().skip(1);
        return match (it.next(), it.next(), it.next()) {
            (Some(program), Some(want), None) => Line::Tool(ToolPin {
                program: program.to_string(),
                want: want.to_string(),
            }),
            _ => broken_at(
                lineno,
                format!("{MANIFEST}:{lineno}"),
                None,
                ParseError::BadTool,
            ),
        };
    }
    // `severity` and `skip` were never valid stages either — the same
    // keyword claim `tool` made. Fixed arity; anything else is a positional
    // gap, never a phantom check named after its target (a broken
    // `severity clippy loud` must not mint `pre-commit-clippy`).
    if line == "severity" || line.starts_with("severity ") || line.starts_with("severity\t") {
        let mut it = line.split_whitespace().skip(1);
        return match (it.next(), it.next(), it.next()) {
            (Some(target), Some(word), None) => match Severity::parse(word) {
                Some(severity) => Line::Policy {
                    what: PolicyLine::Severity {
                        target: target.to_string(),
                        severity,
                    },
                    lineno,
                },
                None => broken_at(
                    lineno,
                    format!("{MANIFEST}:{lineno}"),
                    None,
                    ParseError::BadSeverity(word.to_string()),
                ),
            },
            _ => broken_at(
                lineno,
                format!("{MANIFEST}:{lineno}"),
                None,
                ParseError::BadPolicyLine("severity <check> warn|block"),
            ),
        };
    }
    if line == "skip" || line.starts_with("skip ") || line.starts_with("skip\t") {
        let mut it = line.split_whitespace().skip(1);
        return match (it.next(), it.next()) {
            (Some(target), None) => Line::Policy {
                what: PolicyLine::Skip {
                    target: target.to_string(),
                },
                lineno,
            },
            _ => broken_at(
                lineno,
                format!("{MANIFEST}:{lineno}"),
                None,
                ParseError::BadPolicyLine("skip <check>"),
            ),
        };
    }
    if line == "set" || line.starts_with("set ") || line.starts_with("set\t") {
        let mut it = line.split_whitespace().skip(1);
        return match (it.next(), it.next(), it.next()) {
            (Some(key), Some(value), None) => {
                // Case-insensitive against the allowlist, canonical spelling
                // stored — `set commit.subjectmax 72` must work, because git
                // would have accepted the key in any case.
                match SETTABLE.iter().find(|k| k.eq_ignore_ascii_case(key)) {
                    Some(canonical) => Line::Policy {
                        what: PolicyLine::Set {
                            key: format!("amont.{canonical}"),
                            value: value.to_string(),
                        },
                        lineno,
                    },
                    None => broken_at(
                        lineno,
                        format!("{MANIFEST}:{lineno}"),
                        None,
                        ParseError::UnsettableKey(key.to_string()),
                    ),
                }
            }
            _ => broken_at(
                lineno,
                format!("{MANIFEST}:{lineno}"),
                None,
                ParseError::BadPolicyLine("set <key> <value>"),
            ),
        };
    }
    let (fields, command) = match tokenise(line) {
        Some(t) => t,
        // No name to report: fall back to the position, which is the only
        // handle a reader has on a line this malformed.
        None => {
            return broken_at(
                lineno,
                name_or_position(tokenise(line).map(|(f, _)| f[1]).unwrap_or(""), lineno),
                None,
                ParseError::MissingFields,
            )
        }
    };
    let [stage_tok, declared, scope_tok, severity_tok] = fields;
    let stage = parse_stage(stage_tok);
    let name = name_or_position(declared, lineno);
    let fail = |why| broken_at(lineno, name.clone(), stage, why);

    if declared.is_empty() {
        return fail(ParseError::MissingName);
    }
    // The stage is settled BEFORE the name is judged, because the identity
    // being judged is `<trigger>-<name>` and there is no such thing without a
    // trigger. Ordered the other way, a line with an unusable stage was refused
    // for a name clash that could not be assessed yet.
    let Some(stage) = stage else {
        return fail(ParseError::BadStage(stage_tok.to_string()));
    };
    if name_says_its_trigger(declared) {
        return fail(ParseError::TriggerInName(declared.to_string()));
    }
    let id = format!("{}-{}", stage.as_str(), declared);
    if name_is_taken(&id) {
        return fail(ParseError::NameTaken(declared.to_string()));
    }
    if earlier
        .iter()
        .any(|l| matches!(l, Line::Usable(d) if d.id() == id))
    {
        return fail(ParseError::Duplicate(declared.to_string()));
    }
    let (exts, names, opt_in) = match parse_scope(scope_tok) {
        Ok(e) => e,
        Err(why) => return fail(why),
    };
    let Some(severity) = Severity::parse(severity_tok) else {
        return fail(ParseError::BadSeverity(severity_tok.to_string()));
    };
    // `fix` and `files` are leading markers on the command column rather
    // than extra fields, so every manifest written before them still parses.
    // A loop, so `fix files cmd` and `files fix cmd` both work — two markers
    // whose order carries no meaning must not be order-sensitive.
    let mut command = command;
    let mut wants_fix = false;
    let mut wants_files = false;
    loop {
        if let Some(rest) = command.strip_prefix("fix ") {
            command = rest.trim_start();
            wants_fix = true;
            continue;
        }
        if let Some(rest) = command.strip_prefix("files ") {
            command = rest.trim_start();
            wants_files = true;
            continue;
        }
        break;
    }
    if wants_fix && stage == Stage::PrePush {
        return fail(ParseError::FixOnPrePush);
    }
    // `tokenise` guarantees a non-empty command, so the split cannot fail.
    let mut argv = command.split_whitespace().map(str::to_owned);
    let Some(program) = argv.next() else {
        return fail(ParseError::MissingFields);
    };
    Line::Usable(Declared {
        fix: if wants_fix { Fix::Rewrite } else { Fix::None },
        files: wants_files,
        name: declared.to_string(),
        stage,
        severity,
        exts,
        names,
        opt_in,
        program,
        args: argv.collect(),
    })
}

fn name_or_position(declared: &str, lineno: usize) -> String {
    if declared.is_empty() {
        format!("{MANIFEST}:{lineno}")
    } else {
        declared.to_string()
    }
}

fn broken_at(lineno: usize, name: String, stage: Option<Stage>, why: ParseError) -> Line {
    Line::Broken {
        name,
        stage: stage.unwrap_or(Stage::PreCommit),
        lineno,
        why,
    }
}

impl From<Line> for External {
    fn from(l: Line) -> External {
        let (name, stage, parsed) = l.into_parts();
        let kind = match parsed {
            Ok(d) => Kind::Runnable {
                // `ALWAYS` only when NOTHING narrows it. An opt-in alone is a
                // narrowing — `+Gemfile` means "any change, in a Ruby
                // repository" — and collapsing that to `ALWAYS` would run the
                // check everywhere, which is the exact bug this field exists
                // to prevent.
                scope: if d.exts.is_empty() && d.names.is_empty() && d.opt_in.is_empty() {
                    Scope::ALWAYS
                } else {
                    Scope {
                        files: leak(d.exts),
                        names: leak(d.names),
                        opt_in: leak(d.opt_in),
                        not_during: &[],
                    }
                },
                severity: d.severity,
                program: d.program,
                args: d.args,
                fix: d.fix,
                files: d.files,
            },
            Err(why) => Kind::Unusable { why },
        };
        let id = format!("{}-{}", stage.as_str(), name);
        External {
            id,
            short_name: name,
            stage,
            kind,
        }
    }
}

pub fn parse(text: &str) -> Vec<External> {
    parse_lines(text)
        .into_iter()
        .filter(Line::is_check)
        .map(External::from)
        .collect()
}

/// The manifest for `root`, or an empty list. Read once per process.
pub fn read(root: &Path) -> Vec<External> {
    std::fs::read_to_string(root.join(MANIFEST))
        .map(|t| parse(&t))
        .unwrap_or_default()
}

/// The same file, without building the `Scope`s — for a reader that inspects
/// many repositories and must not leak once per manifest per refresh.
pub fn read_lines(root: &Path) -> Vec<Line> {
    std::fs::read_to_string(root.join(MANIFEST))
        .map(|t| parse_lines(&t))
        .unwrap_or_default()
}

/// Everything one repository's manifest declares, parsed and trust-gated
/// ONCE, owned by the entrypoint that loaded it and lent down through `Ctx`.
///
/// This replaced two process-global `OnceLock`s keyed on the working
/// directory at first call — safe in a hook, which handles one repository
/// and exits, and a trap for anything that walks many. Owned data has no
/// first-call: the caller says which repository it means, every time.
#[derive(Default)]
pub struct Manifest {
    /// Declared checks — untrusted ones present but `Unusable`, so the
    /// decision waiting on the reader stays visible. See [`gate`].
    pub externals: Vec<External>,
    /// Tool version pins — DROPPED entirely when untrusted, because verifying
    /// one executes `<program> --version` for a name the repository chose,
    /// which is exactly the consent the trust model exists to collect.
    pub pins: Vec<ToolPin>,
    /// Whether an `amont.conf` EXISTS in this repository — the committed
    /// declaration that this project subscribes to amont's conventions.
    /// Presence, not content: an empty file declares, and declaring executes
    /// nothing, so this is safe to read before any trust decision. What the
    /// file SAYS stays trust-gated above.
    pub declared: bool,
    /// Committed `severity`/`skip` policy — POPULATED ONLY WHEN TRUSTED,
    /// like the pins: applying a repository's opinion about your safety net
    /// is exactly the consent the trust model collects. See `policy`.
    pub policy: crate::policy::Policy,
    /// `trust::why(state)` when policy lines exist but the manifest is not
    /// trusted — the dispatchers say it once per stage, because policy that
    /// silently does not apply is a silent behaviour change.
    pub policy_withheld: Option<&'static str>,
    /// `severity`/`skip` targets that name no check here, with positions —
    /// a typo in committed policy must be loud, not a phantom check.
    pub policy_notes: Vec<String>,
}

/// Read and trust-gate `root`'s manifest.
///
/// ONE read. The bytes that get PARSED and the bytes that get HASHED are the
/// same bytes: an earlier shape `read(root)`-ed and then let `trust::state`
/// open the file a second time, so anything that changed it in between — a
/// `git checkout`, a watcher, a `make` target already running — produced a
/// trust decision about content that is not the content about to be
/// executed.
///
/// Each call leaks the `Scope` slices it builds (see [`leak`]) — pennies for
/// the hook path, which loads once per process, and the reason the fleet
/// keeps reading [`read_lines`] instead: a scanner must not leak once per
/// repository per refresh.
pub fn load(root: &Path) -> Manifest {
    // Non-UTF-8 yields nothing, as it always has: `parse` takes a `&str`,
    // and a manifest we cannot read as text is one we cannot act on. Not
    // lossy — that would invent a manifest nobody wrote.
    let declared = root.join(MANIFEST).exists();
    let Ok(bytes) = std::fs::read(root.join(MANIFEST)) else {
        return Manifest {
            declared,
            ..Manifest::default()
        };
    };
    let Ok(text) = String::from_utf8(bytes.clone()) else {
        return Manifest {
            declared,
            ..Manifest::default()
        };
    };
    let state = crate::trust::state_of(root, &bytes);
    let lines = parse_lines(&text);
    let externals = gate(
        lines
            .iter()
            .filter(|l| l.is_check())
            .cloned()
            .map(External::from)
            .collect(),
        state,
    );
    let trusted = state == crate::trust::State::Trusted;
    let pins = if trusted {
        lines
            .iter()
            .filter_map(|l| match l {
                Line::Tool(pin) => Some(pin.clone()),
                _ => None,
            })
            .collect()
    } else {
        Vec::new()
    };
    let has_policy = lines.iter().any(|l| matches!(l, Line::Policy { .. }));
    let (policy, policy_notes) = if trusted {
        crate::policy::Policy::from_lines(&lines)
    } else {
        (crate::policy::Policy::default(), Vec::new())
    };
    // NOTE: load() computes but never INSTALLS the policy — the entrypoints
    // do, immediately after this returns and before any config read. A
    // multi-repo walker (the fleet) reads `read_lines` and must never seed
    // the process-global store.
    let policy_withheld = if has_policy && !trusted {
        crate::trust::why(state)
    } else {
        None
    };
    Manifest {
        externals,
        pins,
        declared,
        policy,
        policy_withheld,
        policy_notes,
    }
}

/// Check every trusted pin against the tool actually on this machine, and say
/// what disagrees. Warn-only, once per hook run, at BOTH stages: skew never
/// blocks a commit — its cost is a check disagreeing with CI, and the fix is
/// a human decision — but it stops being invisible, which is the whole point.
pub fn verify_tool_pins(pins: &[ToolPin]) {
    for pin in pins {
        match version_of(&pin.program) {
            None => crate::hooks::common::warn(&format!(
                "{} is pinned to {} in {MANIFEST}, but `{} --version` would not run",
                crate::ui::highlight(&pin.program),
                crate::ui::sanitize(&pin.want),
                crate::ui::sanitize(&pin.program),
            )),
            Some(v) if !v.contains(&pin.want) => crate::hooks::common::warn(&format!(
                "{} reports {} — {MANIFEST} pins {}; this machine may disagree with CI",
                crate::ui::highlight(&pin.program),
                crate::ui::sanitize(&v),
                crate::ui::sanitize(&pin.want),
            )),
            _ => {}
        }
    }
}

/// First line of `<program> --version`, resolved the way every check resolves
/// a tool. A version probe answers in milliseconds or not at all, so it is
/// deliberately not under the check deadline.
fn version_of(program: &str) -> Option<String> {
    let out = Command::new(crate::hooks::common::program(program))
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        return None;
    }
    Some(line.to_string())
}

/// Apply a trust verdict to what the manifest declared.
///
/// Untrusted declarations are kept and DISABLED, not dropped. The names stay
/// visible in `amont list`, in the dashboard and in the "could not run"
/// roll-up, because a repository quietly declaring checks that never run is the
/// failure this project is arranged against — and the reader needs to know
/// there is a decision waiting for them.
///
/// Split out of [`load`] so the rule can be asserted without a repository on
/// disk — and kept split even now that `load` takes an explicit root, because
/// a trust verdict is one input among several there and this is the part with
/// the rule in it.
pub(crate) fn gate(declared: Vec<External>, state: crate::trust::State) -> Vec<External> {
    match crate::trust::why(state) {
        None => declared,
        Some(reason) => declared
            .into_iter()
            .map(|external| External {
                kind: Kind::Unusable {
                    why: reason.to_string(),
                },
                ..external
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(text: &str) -> Line {
        let mut v = parse_lines(text);
        assert_eq!(v.len(), 1, "expected one entry from {text:?}");
        v.pop().expect("one")
    }

    /// The error a line produced, as a VALUE. Tests used to match on the prose,
    /// which coupled them to wording and would have kept passing if the wording
    /// stayed while the meaning changed.
    fn why(l: &Line) -> ParseError {
        match l {
            Line::Broken { why, .. } => why.clone(),
            Line::Usable(d) => panic!("{} parsed when it should not have", d.name),
            Line::Tool(pin) => panic!("{} parsed as a pin, not a broken line", pin.program),
            Line::Policy { what, .. } => {
                panic!("{} parsed as policy, not a broken line", what.describe())
            }
        }
    }

    fn usable(l: &Line) -> &Declared {
        match l {
            Line::Usable(d) => d,
            Line::Broken { name, why, .. } => panic!("{name} failed to parse: {why}"),
            Line::Tool(pin) => panic!("{} is a tool pin, not a declaration", pin.program),
            Line::Policy { what, .. } => {
                panic!("{} is policy, not a declaration", what.describe())
            }
        }
    }

    /// The scope column's bare tokens are exact filenames — basename-matched,
    /// so `package.json` cannot be counterfeited by `not-package.json` — and
    /// they mix freely with extensions. Directories and free `*` stay refused.
    #[test]
    fn a_bare_scope_token_is_an_exact_filename() {
        let line = one("pre-commit  lockcheck  package.json  block  ./check.sh\n");
        let d = usable(&line);
        assert!(d.exts.is_empty());
        assert_eq!(d.names, ["package.json"]);

        let line = one("pre-commit  x  *.ts,package.json,.prettierrc  block  ./x\n");
        let d = usable(&line);
        assert_eq!(d.exts, [".ts"]);
        assert_eq!(d.names, ["package.json", ".prettierrc"]);

        assert_eq!(
            why(&one("pre-commit  x  src/package.json  block  ./x\n")),
            ParseError::BadScope("src/package.json".into())
        );
        assert_eq!(
            why(&one("pre-commit  x  pkg*  block  ./x\n")),
            ParseError::BadScope("pkg*".into())
        );
    }

    /// `+` adds the repository condition a builtin has always had.
    ///
    /// The separator is the one `amont list` already prints between the two
    /// halves (`needs .rs + Cargo.toml`), so what a reader sees in the listing
    /// is what they write in the file.
    #[test]
    fn a_scope_can_name_what_the_repository_must_carry() {
        let line = one("pre-commit  rubocop  *.rb+.rubocop.yml  block  rubocop\n");
        let d = usable(&line);
        assert_eq!(d.exts, [".rb"]);
        assert!(d.names.is_empty());
        assert_eq!(d.opt_in, [".rubocop.yml"]);

        // Both sides comma-separated: any trigger, and any ONE opt-in.
        let line = one("pre-commit  x  *.rb,Rakefile+.rubocop.yml,.rubocop_todo.yml  block  ./x\n");
        let d = usable(&line);
        assert_eq!(d.exts, [".rb"]);
        assert_eq!(d.names, ["Rakefile"]);
        assert_eq!(d.opt_in, [".rubocop.yml", ".rubocop_todo.yml"]);

        // No trigger half: "any change, in a repository that carries this".
        let line = one("pre-commit  x  +Gemfile  block  ./x\n");
        let d = usable(&line);
        assert!(d.exts.is_empty() && d.names.is_empty());
        assert_eq!(d.opt_in, ["Gemfile"]);
    }

    /// Every declaration written before `+` existed keeps its meaning: no
    /// opt-in is the old behaviour, which is "applies wherever it triggers".
    #[test]
    fn a_scope_without_a_plus_is_unchanged() {
        let line = one("pre-commit  x  *.ts,package.json  block  ./x\n");
        let d = usable(&line);
        assert!(d.opt_in.is_empty());
        let line = one("pre-commit  x  *  block  ./x\n");
        let d = usable(&line);
        assert!(d.opt_in.is_empty() && d.exts.is_empty() && d.names.is_empty());
    }

    /// A `+` that promises a condition and names none is refused, rather than
    /// read as "no condition" — which is the opposite of what was reached for.
    #[test]
    fn a_malformed_opt_in_half_is_refused() {
        for bad in [
            "*.rb+",            // promised, not given
            "*.rb+,",           // still nothing
            "*.rb+a+b",         // a second separator is not a filename
            "*.rb+sub/dir.yml", // directories are inexpressible, as on the left
            "*.rb+*.yml",       // this grammar has no globs to mis-guess
        ] {
            let line = one(&format!("pre-commit  x  {bad}  block  ./x\n"));
            assert!(
                matches!(line, Line::Broken { .. }),
                "{bad} should be refused, got {line:?}"
            );
        }
    }

    /// The pin grammar: exactly three tokens, claimed from a first token that
    /// was never a valid stage — no manifest that ever parsed changes meaning.
    #[test]
    fn a_tool_pin_parses_and_a_malformed_one_is_broken() {
        let line = one("tool  ruff  0.6.\n");
        assert_eq!(
            line,
            Line::Tool(ToolPin {
                program: "ruff".into(),
                want: "0.6.".into()
            })
        );
        assert_eq!(why(&one("tool ruff\n")), ParseError::BadTool);
        assert_eq!(why(&one("tool ruff 0.6. extra\n")), ParseError::BadTool);
        // `tool` is only a keyword in column one — a check NAMED tool still
        // parses as the check it always was.
        let line = one("pre-commit  tool  *  block  make tool\n");
        assert_eq!(usable(&line).name, "tool");
    }

    #[test]
    fn parses_the_documented_example() {
        let v = parse_lines(
            "# stage       name        scope   severity  command\n\
             pre-commit    lint-shell  *.sh    block     scripts/lint-shell.sh\n\
             pre-push      smoke       *       warn      make smoke\n",
        );
        assert_eq!(v.len(), 2);

        let a = usable(&v[0]);
        assert_eq!(a.name, "lint-shell");
        assert_eq!(a.stage, Stage::PreCommit);
        assert_eq!(a.severity, Severity::Block);
        assert_eq!(a.program, "scripts/lint-shell.sh");
        assert!(a.args.is_empty());
        assert_eq!(a.exts, [".sh"]);

        let b = usable(&v[1]);
        assert_eq!(b.stage, Stage::PrePush);
        assert_eq!(b.severity, Severity::Warn);
        // A command with arguments is split, not handed to a shell.
        assert_eq!(b.program, "make");
        assert_eq!(b.args, ["smoke"]);
        assert!(b.exts.is_empty(), "`*` gates on nothing");
    }

    /// Blank lines and comments are not entries, and must not become broken
    /// ones — a file that is mostly documentation would otherwise report a
    /// dozen gaps.
    #[test]
    fn comments_and_blank_lines_produce_nothing() {
        assert!(parse_lines("\n  \n# just a comment\n\t# indented\n").is_empty());
    }

    /// The rule the module commits to: a line that cannot be understood still
    /// yields a check, so its absence is visible. Matched by VARIANT.
    #[test]
    fn a_malformed_line_becomes_a_visible_gap() {
        let cases: [(&str, ParseError); 4] = [
            (
                "pre-commit lint-shell *.sh block\n",
                ParseError::MissingFields,
            ),
            (
                "nonsense lint-shell *.sh block x\n",
                ParseError::BadStage("nonsense".into()),
            ),
            (
                "pre-commit lint-shell ?.sh block x\n",
                ParseError::BadScope("?.sh".into()),
            ),
            (
                "pre-commit lint-shell *.sh loud x\n",
                ParseError::BadSeverity("loud".into()),
            ),
        ];
        for (text, expected) in cases {
            assert_eq!(why(&one(text)), expected, "for {text:?}");
        }
    }

    /// The prose still has to locate the line, even though the tests no longer
    /// depend on its wording.
    #[test]
    fn a_gap_reports_where_it_is() {
        let l = one("pre-commit lint-shell *.sh loud x\n");
        let said = l.broken().expect("broken");
        assert!(said.contains("line 1"), "{said}");
        assert!(said.contains("severity"), "{said}");
    }

    /// `fix` on a pre-push line is refused where every other bad declaration is
    /// refused — on every commit, named and located — rather than as a runtime
    /// "contract violation" discovered later at push time by fewer people.
    #[test]
    fn fix_is_refused_on_a_pre_push_line() {
        assert_eq!(
            why(&one("pre-push  smoke  *  block  fix make smoke\n")),
            ParseError::FixOnPrePush
        );
        // …and accepted on pre-commit.
        let line = one("pre-commit  fmt  *  block  fix make format\n");
        let declared = usable(&line);
        assert_eq!(declared.fix, Fix::Rewrite);
        assert_eq!(declared.program, "make");
        assert_eq!(declared.args, ["format"]);
    }

    /// Every manifest written before `fix` existed must still parse the same.
    #[test]
    fn a_command_that_merely_starts_with_fix_is_not_a_marker() {
        let line = one("pre-commit  x  *  block  fixup-tool --check\n");
        let declared = usable(&line);
        assert_eq!(declared.fix, Fix::None);
        assert_eq!(declared.program, "fixup-tool");
    }

    /// The `files` marker, alone and stacked with `fix` in either order —
    /// two markers whose order carries no meaning must not be
    /// order-sensitive.
    #[test]
    fn the_files_marker_parses_alone_and_in_either_order_with_fix() {
        let line = one("pre-commit  sc  *.sh  block  files shellcheck\n");
        let declared = usable(&line);
        assert!(declared.files);
        assert_eq!(declared.fix, Fix::None);
        assert_eq!(declared.program, "shellcheck");

        for text in [
            "pre-commit  fmt  *  block  fix files prettier --write\n",
            "pre-commit  fmt  *  block  files fix prettier --write\n",
        ] {
            let line = one(text);
            let declared = usable(&line);
            assert!(declared.files, "for {text:?}");
            assert_eq!(declared.fix, Fix::Rewrite, "for {text:?}");
            assert_eq!(declared.program, "prettier", "for {text:?}");
            assert_eq!(declared.args, ["--write"], "for {text:?}");
        }
    }

    /// A command that merely starts with `files` keeps its name, exactly as
    /// `fixup-tool` keeps its own.
    #[test]
    fn a_command_that_merely_starts_with_files_is_not_a_marker() {
        let line = one("pre-commit  x  *  block  files-checker --strict\n");
        let declared = usable(&line);
        assert!(!declared.files);
        assert_eq!(declared.program, "files-checker");
    }

    /// A gap with no name cannot be reported, and a line this broken has none.
    #[test]
    fn a_nameless_line_is_named_after_its_position() {
        let l = one("pre-commit\n");
        assert_eq!(l.name(), "amont.conf:1");
        assert_eq!(why(&l), ParseError::MissingFields);
    }

    /// An external must not be able to take a built-in's id — it would either
    /// shadow `pre-push-branch-protect` or silently lose to it, and neither is
    /// something a repository should be able to do by editing a text file.
    ///
    /// Judged on the id, so the same declaration is refused on one trigger and
    /// accepted on the other. That is not a loophole: `pre-push-clippy` is a
    /// different check from `pre-commit-clippy`, and nothing is shadowed.
    #[test]
    fn a_built_in_id_is_refused() {
        assert_eq!(
            why(&one("pre-commit clippy *.rs block x\n")),
            ParseError::NameTaken("clippy".into())
        );
        assert!(matches!(
            one("pre-push clippy *.rs block x\n"),
            Line::Usable(_)
        ));
        // And a pre-push built-in is protected on pre-push, not on pre-commit,
        // for the same reason.
        assert_eq!(
            why(&one("pre-push pull-rebase * block x\n")),
            ParseError::NameTaken("pull-rebase".into())
        );
        assert!(matches!(
            one("pre-commit pull-rebase * block x\n"),
            Line::Usable(_)
        ));
    }

    /// The stage column says which trigger a line is for. Saying it again in
    /// the name is the one way to make an id ambiguous: `pre-commit-clippy` as
    /// a NAME is a check whose short name is the built-in's full id, so one
    /// `hook.skip` would silence both.
    #[test]
    fn a_name_that_says_its_own_trigger_is_refused() {
        for name in ["pre-commit", "pre-push", "pre-commit-clippy", "pre-push-x"] {
            assert_eq!(
                why(&one(&format!("pre-commit {name} * block x\n"))),
                ParseError::TriggerInName(name.into()),
                "{name}"
            );
        }
        // A name that merely begins with the same letters is fine — the trigger
        // has to be followed by the separator to count.
        assert!(matches!(
            one("pre-commit pre-commitish * block x\n"),
            Line::Usable(_)
        ));
    }

    /// Two USABLE lines with one ID: the second cannot be addressed by
    /// `hook.skip` or by a severity override, so it is refused.
    #[test]
    fn a_duplicate_id_is_refused() {
        let v = parse_lines(
            "pre-commit smoke * block a\n\
             pre-commit smoke * block b\n",
        );
        assert_eq!(v.len(), 2);
        assert_eq!(usable(&v[0]).id(), "pre-commit-smoke");
        assert_eq!(why(&v[1]), ParseError::Duplicate("smoke".into()));
    }

    /// The same name on both triggers is TWO checks, and this used to refuse
    /// the second. Somebody wanting a `show-unicorn` on commit and on push had
    /// no way to write it, and no way to skip or downgrade one without the
    /// other — the bare name could not tell them apart.
    #[test]
    fn the_same_name_on_two_triggers_is_allowed() {
        let v = parse_lines(
            "pre-commit show-unicorn * block a\n\
             pre-push   show-unicorn * block b\n",
        );
        assert_eq!(v.len(), 2);
        assert_eq!(usable(&v[0]).id(), "pre-commit-show-unicorn");
        assert_eq!(usable(&v[1]).id(), "pre-push-show-unicorn");

        // And each is separately addressable, while the short name takes both —
        // which is the whole vocabulary, applied to declared checks.
        for (id, only) in [
            ("pre-commit-show-unicorn", "pre-push-show-unicorn"),
            ("pre-push-show-unicorn", "pre-commit-show-unicorn"),
        ] {
            assert!(crate::skip_suppresses(id, id));
            assert!(!crate::skip_suppresses(only, id));
        }
        assert!(crate::skip_suppresses(
            "pre-commit-show-unicorn",
            "show-unicorn"
        ));
        assert!(crate::skip_suppresses(
            "pre-push-show-unicorn",
            "show-unicorn"
        ));
        assert!(crate::skip_suppresses(
            "pre-commit-show-unicorn",
            "pre-commit"
        ));
        assert!(!crate::skip_suppresses(
            "pre-push-show-unicorn",
            "pre-commit"
        ));
    }

    /// A line that cannot run does not RESERVE its name.
    ///
    /// It used to: broken and usable entries shared one list, so a valid
    /// declaration was rejected as "declared twice" for colliding with a line
    /// that could never execute — pointing the reader at the wrong line, and
    /// forcing them to fix the first before the second would work at all.
    #[test]
    fn a_broken_line_does_not_reserve_its_name() {
        let v = parse_lines(
            "pre-commit smoke * LOUD   make a\n\
             pre-commit smoke * block  make b\n",
        );
        assert_eq!(v.len(), 2);
        assert_eq!(why(&v[0]), ParseError::BadSeverity("LOUD".into()));
        let good = usable(&v[1]);
        assert_eq!(good.name, "smoke");
        assert_eq!(good.program, "make");
    }

    /// Alignment is cosmetic. A file someone has lined up with tabs, or not
    /// lined up at all, must parse identically.
    #[test]
    fn field_alignment_does_not_matter() {
        let spaced = one("pre-commit      lint-shell    *.sh      block     make lint\n");
        let tabbed = one("pre-commit\tlint-shell\t*.sh\tblock\tmake lint\n");
        assert_eq!(usable(&spaced), usable(&tabbed));
        assert_eq!(usable(&spaced).args, ["lint"]);
    }

    #[test]
    fn several_extensions_can_gate_one_check() {
        let e = External::from(one("pre-commit shell *.sh,*.bash block make lint\n"));
        assert!(e.scope().matches(&["a.bash".into()]));
        assert!(e.scope().matches(&["a.sh".into()]));
        assert!(!e.scope().matches(&["a.zsh".into()]));
    }

    /// `tokenise` states its arity in the type, so "four tokens then a command"
    /// is checked rather than remembered.
    #[test]
    fn tokenise_wants_four_fields_and_a_command() {
        assert!(tokenise("a b c").is_none(), "too few fields");
        assert!(tokenise("a b c d").is_none(), "four fields, no command");
        // Trailing whitespace reaches the four fields but still leaves nothing
        // to run — the case the `?` on the last field cannot catch.
        assert!(
            tokenise("a b c d   ").is_none(),
            "command is all whitespace"
        );
        assert!(tokenise("a b c d\t").is_none(), "command is a tab");
        let (fields, cmd) = tokenise("a  b\tc   d   run it").expect("four and a command");
        assert_eq!(fields, ["a", "b", "c", "d"]);
        assert_eq!(cmd, "run it");
    }

    /// An unusable line carries no command at all — the type has nowhere to put
    /// one, which is the point of the split.
    #[test]
    fn an_unusable_external_holds_no_command() {
        let e = External::from(one("pre-commit lint-shell *.sh loud echo hi\n"));
        assert!(matches!(e.kind, Kind::Unusable { .. }));
        // And it can never block, whatever severity anyone configures.
        assert_eq!(e.severity(), Severity::Warn);
    }

    /// A missing manifest is the normal case and must not be an error.
    #[test]
    fn a_repository_with_no_manifest_declares_nothing() {
        assert!(read(Path::new("/nonexistent-c8f2")).is_empty());
        assert!(read_lines(Path::new("/nonexistent-c8f2")).is_empty());
    }

    /// `Line` exists to spare the dashboard a leak, not to become a second
    /// opinion about what a manifest says.
    #[test]
    fn the_leaking_and_non_leaking_parsers_agree() {
        let text = "pre-commit  lint-shell  *.sh,*.bash  block  make lint\n\
                    pre-push    smoke       *            warn   make smoke\n\
                    pre-commit  broken      ?            block  x\n";
        let lines = parse_lines(text);
        let externals = parse(text);
        assert_eq!(lines.len(), externals.len());
        for (l, e) in lines.iter().zip(&externals) {
            assert_eq!(l.id(), e.name(), "the id is what a check answers to");
            assert_eq!(
                l.name(),
                e.short_name,
                "and the short name is what it is called"
            );
            assert_eq!(l.stage(), e.stage());
            assert_eq!(
                l.broken().is_some(),
                matches!(e.kind, Kind::Unusable { .. })
            );
            if let Line::Usable(d) = l {
                assert_eq!(d.severity, e.severity());
                // The scope the dashboard would DESCRIBE is the scope the
                // dispatcher would ENFORCE.
                assert_eq!(d.exts, e.scope().files);
            }
        }
    }
}
