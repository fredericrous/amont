//! What it would take to bring one repository back to the shipped state.
//!
//! A plan, never an action. PR 5 executes exactly this structure; nothing here
//! touches the filesystem, which is what makes the parity differential against
//! `propagate.sh` meaningful — two implementations can be compared before
//! either is trusted to delete anything.
//!
//! The refusals are the important half. `make install` destroyed tracked source
//! twice in this repo's history, both times because its guard FAILED OPEN:
//! anything it could not confirm became safe to delete. So a plan refuses on
//! anything it cannot positively establish, and a refusal suppresses the whole
//! repo rather than the individual file — a half-applied fix is how a repo ends
//! up with both `pre-commit-ruff.zsh` and `pre-commit-ruff`, running ruff twice.
//!
//! ## Where "is this ours, and may we touch it?" is answered
//!
//! Not here. [`amont_runtime::hookfile`] owns every one of those questions —
//! ownership, tracked-ness, symlinks, file type — and this module only turns its
//! answers into refusals.
//!
//! That module exists because this one used to answer them itself, and got both
//! halves wrong in the same three lines:
//!
//! ```text
//! Command::new("git").args(["ls-files", "--error-unmatch"]).arg(path)   // ABSOLUTE path
//!     .current_dir(dir).output().map(|o| o.status.success()).unwrap_or(false)
//! ```
//!
//! - git normalises a pathspec LEXICALLY but resolves its working directory
//!   PHYSICALLY, so handing it the absolute `<repo>/.git/hooks/pre-commit` when
//!   `.git/hooks` is a symlink into the working tree returns `exit 1, pathspec
//!   did not match` — "untracked" — for a file git plainly tracks. That is the
//!   exact shape of both incidents this guard was written for.
//! - `.map(|o| o.status.success()).unwrap_or(false)` collapsed a spawn failure,
//!   a `fatal: detected dubious ownership` (every repo owned by another uid,
//!   which is every repo inside a container bind mount) and a permissions error
//!   into the same "untracked" — the opposite of the rule this module's own doc
//!   states two paragraphs up.
//!
//! `hookfile::tracked` asks with the BASENAME from `-C <parent>` and answers
//! tri-state, so "could not tell" is its own refusal ([`Refusal::TrackedUnknown`])
//! rather than a clean "no".

use std::path::{Path, PathBuf};

use amont_runtime::hookfile::Tracked;
use serde::Serialize;

use crate::scan::{AgentsMdState, Repo};
use crate::shim::{self, DISPATCHERS};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "refusal", rename_all = "snake_case")]
pub enum Refusal {
    /// No shim dispatches to the binary. Either somebody else's repo or, as
    /// `propagate.sh` also decided, an application's data repo that commits
    /// programmatically and must never gain hooks.
    Unmanaged,
    /// The hooks directory could not be read. Absence of evidence, not evidence
    /// of absence.
    UnreadableHooks,
    /// git tracks this path. An install step must never delete tracked source,
    /// whatever the path resolution says.
    Tracked { path: PathBuf },
    /// git could not say whether it tracks this path. Suppresses the whole repo
    /// exactly as [`Refusal::Tracked`] does, and deliberately so: "we could not
    /// tell whether this file is somebody's tracked source" is precisely the
    /// ignorance the two overwrite incidents were made of, and the predicate
    /// that used to live here answered it "no".
    ///
    /// The common cause is `fatal: detected dubious ownership in repository`,
    /// which git emits on EVERY call for a repository owned by another uid — so
    /// this is not an exotic state, it is the steady state inside a container
    /// bind mount. `why` carries git's own first line, and the rendered message
    /// names `git config --global --add safe.directory <path>`, because a
    /// refusal that does not say what to do next is indistinguishable from a
    /// bug.
    TrackedUnknown { path: PathBuf, why: String },
    /// A dispatcher file exists here and is not one of ours — somebody wrote
    /// their own `pre-commit`. Activating would overwrite it.
    ///
    /// The planner already refused to touch `pre-commit-*` SUB-hooks it did not
    /// recognise; the four dispatchers themselves had no such guard, and
    /// activation is the first mode that writes them into a repository nobody
    /// had installed into. Same failure, one filename along.
    ForeignHook { names: Vec<String> },
    /// `AGENTS.md` carries an unpaired marker. Not routed through `Tracked`:
    /// this file is SUPPOSED to be tracked, so a malformed block is its own
    /// refusal rather than a false positive on that guard.
    AgentsMdMalformed { path: PathBuf },
    /// The binary path is not one a shim will accept. The shim resolves a
    /// relative path against the WORKING TREE, so baking one across a fleet
    /// would hand every repository the chance to answer for its own hooks.
    UnbakeableBinary { binary: String },
    /// This repository's hooks resolve to a directory outside it.
    ///
    /// `core.hooksPath` may be an absolute path anywhere on the disk, and
    /// `Intent::Activate` used to `create_dir_all` it and write four 0o755 files
    /// into it — so a scanned repository could name `/etc/amont`, or another
    /// checkout's working tree, and a fleet-wide `install` would oblige.
    HooksDirOutsideRepo { path: PathBuf },
    /// `core.hooksPath` points somewhere inside the repository that is not its
    /// own `hooks` directory: another tool owns dispatch.
    ///
    /// Refused rather than obliged, and the per-repo `amont install` now
    /// refuses it too — the two used to disagree, which is how the fleet came to
    /// report eleven husky repositories as "drifted" rather than as not running
    /// amont at all.
    HooksDirRedirected { path: PathBuf },
    /// git would not say where this repository's hooks live. The old code
    /// answered that question with `repo.join(".git/hooks")`, which is a guess
    /// everywhere and a WRONG guess for a repository whose `.git` is a file.
    HooksDirUnknown { why: String },
}

impl Refusal {
    /// One line, naming what stopped us and — where there is one — the command
    /// that resolves it.
    ///
    /// This exists because a refusal used to be reported as a NUMBER. `1
    /// refused` in a fleet-wide summary is indistinguishable from a bug: the
    /// reader cannot tell an application data repository we correctly declined
    /// from a repository we could not read because git will not talk to it. The
    /// `TrackedUnknown` case is the sharpest — its fix is one `git config` away
    /// and the old output did not even say which repository was affected.
    ///
    /// **Every borrowed value is sanitized HERE, and the result is printed
    /// raw.** The call site used to run `ui::sanitize` over the whole assembled
    /// string, which escapes `\n` — correctly, for a path scraped off somebody's
    /// disk, where an embedded newline could forge a line of our own report. The
    /// cost was that it also escaped OUR deliberate line breaks, so
    /// `TrackedUnknown` has been rendering its `git config --add safe.directory`
    /// remedy as a literal `\x0a` for as long as it has existed — the one
    /// refusal whose whole point is telling the reader what to type.
    ///
    /// Sanitizing the parts rather than the whole keeps both properties: a
    /// hostile path still cannot inject a line, and the template can breathe.
    pub fn explain(&self) -> String {
        use amont_runtime::ui::{sanitize, sanitize_path};
        match self {
            Refusal::Unmanaged => "no shim of ours here — not adopting it".to_string(),
            Refusal::UnreadableHooks => "the hooks directory could not be read".to_string(),
            Refusal::Tracked { path } => format!(
                "{} is TRACKED by git — that is somebody's source, not our hook",
                sanitize_path(path)
            ),
            Refusal::TrackedUnknown { path, why } => format!(
                "cannot tell whether {} is tracked ({})\n      \
                 If this is a repository you own: \
                 git config --global --add safe.directory {}",
                sanitize_path(path),
                sanitize(why),
                sanitize_path(path.parent().unwrap_or(path))
            ),
            Refusal::ForeignHook { names } => format!(
                "{} was written by somebody else — activating would overwrite it",
                sanitize(&names.join(", "))
            ),
            Refusal::AgentsMdMalformed { path } => {
                format!("{} carries an unpaired marker", sanitize_path(path))
            }
            Refusal::UnbakeableBinary { binary } => {
                format!("{} is not a path a shim will accept", sanitize(binary))
            }
            Refusal::HooksDirOutsideRepo { path } => format!(
                "core.hooksPath resolves to {}, OUTSIDE the repository — \
                 not creating or writing there",
                sanitize_path(path)
            ),
            Refusal::HooksDirRedirected { path } => {
                let owner = amont_runtime::install::redirect_culprit(path)
                    .map(|t| format!("{t} owns"))
                    .unwrap_or_else(|| "another tool owns".to_string());
                format!(
                    "core.hooksPath resolves to {} — {owner} the hooks here, \
                     so amont is not running\n      \
                     Hand dispatch back: git config --unset core.hooksPath",
                    sanitize_path(path)
                )
            }
            Refusal::HooksDirUnknown { why } => {
                format!("git would not say where the hooks are ({})", sanitize(why))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalReason {
    /// Ours, but no longer shipped — the 16 per-check shims retired when checks
    /// moved in-process.
    StaleOurs,
    /// A hand-written sub-hook. Nothing dispatches these since the move
    /// in-process, so it looks installed and never runs.
    ///
    /// Only ever produced under `--remove-unrecognized`. By default these are
    /// [`Warning::UnrecognizedSubHook`] and are left exactly where they are.
    ForeignSubHook,
    /// The node-era `package.json` that forced CommonJS. No hook is node now.
    VestigialPackageJson,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Removal {
    pub path: PathBuf,
    pub reason: RemovalReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WriteShim {
    pub path: PathBuf,
    pub baked: String,
    /// False when the file already has exactly these bytes. The write is listed
    /// either way, because `propagate.sh` writes all four unconditionally and
    /// the parity gate compares the SET of writes — but the UI should be able
    /// to say "4 writes, 0 of them changes".
    pub changes: bool,
}

/// Rolling the `AGENTS.md` pointer out. Only ever present when the caller
/// opted in (`plan`'s `agents_md` argument) — writing tracked content across
/// a whole fleet is a materially bigger action than the untracked `.git/hooks`
/// shims `write` covers, so it never happens implicitly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WriteAgentsMd {
    pub path: PathBuf,
    /// Always true: this only appears in a plan when the file is missing or
    /// drifted. A repo already at the generated block gets no entry at all,
    /// unlike `WriteShim` — which lists all four dispatchers unconditionally
    /// for parity with `propagate.sh`. There is no such parity target here.
    pub changes: bool,
}

/// Something the reader must be told that does NOT stop the repair.
///
/// A second channel next to `refuse`, and the separation is the point. A
/// refusal suppresses the whole repository, which is right when we cannot
/// establish that acting is safe and wrong when we simply found something worth
/// mentioning: a stranger's `pre-push-mine.sh` sitting in `.git/hooks` must not
/// block repairing four broken dispatchers in the same directory. Folding the
/// two together forces a choice between "say nothing" and "do nothing", and
/// this tool has been on both sides of that choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "warning", rename_all = "snake_case")]
pub enum Warning {
    /// A `pre-commit-*` / `pre-push-*` file in `.git/hooks` that we did not
    /// write. Nothing dispatches these since the checks moved in-process, so it
    /// looks installed and never runs — worth saying, and NOT ours to delete.
    ///
    /// It was deleted, by default, in repositories this tool had never touched,
    /// with no confirmation, and reported only as a number in `repoC -1 +4`. A
    /// hand-written `pre-commit-secrets-scan` went that way in the reproduction.
    /// README.md has stated the opposite posture the whole time: "a hook you
    /// wrote yourself is left alone."
    ///
    /// The removal was never a decision. `scripts/propagate.sh` (see
    /// `git show 90b0d30^:scripts/propagate.sh`, around lines 82-87) did
    /// `rm -f "$hooks"/pre-commit-* "$hooks"/pre-push-*` as a ONE-TIME migration
    /// sweep when the 16 per-check shims retired in `81cdf9d` — a glob that
    /// could not tell our retired shims from anybody else's files, run once,
    /// deliberately. `fix.rs` inherited it wholesale as standing behaviour and
    /// `tests/parity.rs` then pinned it as golden, which is how a migration
    /// one-liner became a promise the tool kept making to every repository it
    /// ever scanned.
    UnrecognizedSubHook { path: PathBuf },
    /// The repository's hooks resolve outside the repository itself.
    ///
    /// Also present in `refuse`, as the only condition that appears on both
    /// channels. The two say different things to different readers: the warning
    /// NAMES the directory in the fix/install report and in the dashboard, so
    /// somebody can go and look at what their `core.hooksPath` is pointing at,
    /// while the refusal is what stops `apply` writing there. Dropping either
    /// one loses something — the refusal alone is silent about where, the
    /// warning alone would not suppress the write.
    HooksDirOutsideRepo { path: PathBuf },
    /// `core.hooksPath` handed dispatch to another tool inside the repository.
    /// On both channels for the same reason as `HooksDirOutsideRepo`.
    HooksDirRedirected { path: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FixPlan {
    pub repo: PathBuf,
    /// Absolute path to the repository, carried so `apply` can RE-RESOLVE what
    /// this plan resolved rather than trusting a field that was computed before
    /// the user was even shown a preview.
    pub repo_abs: PathBuf,
    /// Repair or activate. Carried for the same reason: `apply` re-verifies the
    /// refusals that depend on it, and re-deriving intent from the shape of the
    /// plan would be guessing at the caller's decision.
    pub intent: Intent,
    /// Where this plan resolved the hooks to. `apply` re-resolves and compares.
    pub hooks: crate::scan::HooksDir,
    pub refuse: Vec<Refusal>,
    pub warn: Vec<Warning>,
    pub remove: Vec<Removal>,
    pub write: Vec<WriteShim>,
    pub write_agents_md: Option<WriteAgentsMd>,
}

impl FixPlan {
    /// A plan that would change nothing. Applying twice must produce one of
    /// these the second time.
    ///
    /// Warnings do not count. A repository whose only finding is "there is a
    /// hook here we did not write" needs nothing done to it, and reporting it as
    /// a pending change every run would train the reader to ignore the number.
    pub fn is_noop(&self) -> bool {
        self.remove.is_empty()
            && self.write.iter().all(|w| !w.changes)
            && self.write_agents_md.is_none()
    }
    pub fn refused(&self) -> bool {
        !self.refuse.is_empty()
    }
}

/// git's answer about one path, as the refusal it implies — or `None` when git
/// said a clean "not tracked" and there is nothing to refuse.
///
/// Files under `.git/hooks` are never tracked, so in the normal case this is
/// pure defence. It exists because the normal case is not the one that broke:
/// `~/.config/git/git-templates` is a SYMLINK to a checkout, and an install
/// step that resolved through it deleted tracked templates twice.
///
/// The question itself is [`amont_runtime::hookfile::tracked`]'s to answer;
/// see this module's doc for the two ways the version that used to live here
/// failed open.
pub fn tracked_refusal(path: &Path) -> Option<Refusal> {
    match amont_runtime::hookfile::tracked(path) {
        Tracked::No => None,
        Tracked::Yes => Some(Refusal::Tracked {
            path: path.to_path_buf(),
        }),
        Tracked::Unknown { why } => Some(Refusal::TrackedUnknown {
            path: path.to_path_buf(),
            why,
        }),
    }
}

/// Is this dispatcher path one activation may claim — nothing there, or already
/// one of ours?
///
/// Every other answer (a stranger's hook, a compiled binary, a symlink, a
/// directory, an unreadable file) is a repository we do not activate. Shared
/// with `apply`, which asks the same question again at the moment of writing.
pub fn is_absent_or_ours(path: &Path) -> bool {
    matches!(
        amont_runtime::hookfile::classify(path),
        amont_runtime::hookfile::HookFile::Absent | amont_runtime::hookfile::HookFile::Ours
    )
}

/// Why we are planning: repairing an installation, or making one.
///
/// The difference is exactly one refusal. `fix` declines an unmanaged
/// repository because there is nothing there to repair and writing into one
/// would be a decision it was not asked to make. `install` IS that decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    Repair,
    Activate,
}

/// What it would take to bring `repo` back to the shipped state.
///
/// `remove_unrecognized` promotes every [`Warning::UnrecognizedSubHook`] back
/// into a [`Removal`] — the pre-`--remove-unrecognized` behaviour, now opt-in.
/// See that variant for what it deletes and why it used to be the default.
pub fn plan(
    repo: &Repo,
    repo_abs: &Path,
    binary: &str,
    intent: Intent,
    agents_md: bool,
    remove_unrecognized: bool,
) -> FixPlan {
    let hooks_dir = crate::scan::hooks_dir_for(repo_abs);
    let mut p = FixPlan {
        repo: repo.path.clone(),
        repo_abs: repo_abs.to_path_buf(),
        intent,
        hooks: hooks_dir.clone(),
        refuse: Vec::new(),
        warn: Vec::new(),
        remove: Vec::new(),
        write: Vec::new(),
        write_agents_md: None,
    };

    // Before anything else: a path the shim will not accept must not be baked
    // into one repository, let alone a fleet of them.
    if !amont_runtime::install::is_bakeable(binary) {
        p.refuse.push(Refusal::UnbakeableBinary {
            binary: binary.to_string(),
        });
        return p;
    }
    // A repository whose hooks another one in this scan already owns gets an
    // empty plan — not a refusal, because nothing is wrong: a linked worktree
    // and its main repo are one hooks directory wearing two paths. Writing the
    // four shims through both would write the same file twice and report it as
    // eight, which is the shape of the `192 removals across 96 repos` number
    // that started this whole exercise.
    if repo.shares_hooks_with.is_some() {
        return p;
    }
    // A hostile redirect is reported AHEAD of `Unmanaged`, and the predicate is
    // the runtime's, shared with `amont install` so the two cannot come to
    // different conclusions about the same repository.
    //
    // Ahead of `Unmanaged` because that refusal is filtered out of the report as
    // noise — correctly, since most of a machine's repositories belong to
    // somebody else. But a repository holding OUR shims in
    // `<git-common-dir>/hooks` while `core.hooksPath` sends git elsewhere is one
    // where amont was installed and then silently stopped running. Reporting
    // that as "no shim of ours here" is precisely backwards, and it is the state
    // every `duro-*` repository was in.
    if let crate::scan::HooksDir::Redirected {
        path,
        hostile: true,
        ..
    } = &hooks_dir
    {
        {
            p.warn
                .push(Warning::HooksDirRedirected { path: path.clone() });
            p.refuse
                .push(Refusal::HooksDirRedirected { path: path.clone() });
            return p;
        }
    }
    if !repo.managed && intent == Intent::Repair {
        p.refuse.push(Refusal::Unmanaged);
        return p;
    }
    // Containment BEFORE creation. `Intent::Activate` used to `create_dir_all`
    // whatever `core.hooksPath` named, which is how a fleet-wide install could
    // create and populate a directory a scanned repository chose.
    let hooks = match &hooks_dir {
        crate::scan::HooksDir::In { path } => path.clone(),
        crate::scan::HooksDir::Outside { path } => {
            p.warn
                .push(Warning::HooksDirOutsideRepo { path: path.clone() });
            p.refuse
                .push(Refusal::HooksDirOutsideRepo { path: path.clone() });
            return p;
        }
        // A redirect that got past the hostility check above is followed, which
        // is what this project has always done for a repository keeping its
        // hooks somewhere it chose (`tooling/hooks` under version control).
        crate::scan::HooksDir::Redirected { path, .. } => path.clone(),
        crate::scan::HooksDir::Unknown { why } => {
            p.refuse.push(Refusal::HooksDirUnknown { why: why.clone() });
            return p;
        }
    };
    // Activating creates the directory; repairing does not, because a missing
    // `.git/hooks` in a repo we thought was managed is a fact worth reporting
    // rather than papering over.
    if intent == Intent::Activate && !hooks.is_dir() {
        let _ = std::fs::create_dir_all(&hooks);
    }
    if !hooks.is_dir() {
        p.refuse.push(Refusal::UnreadableHooks);
        return p;
    }

    for name in &repo.stale_ours {
        p.remove.push(Removal {
            path: hooks.join(name),
            reason: RemovalReason::StaleOurs,
        });
    }
    // NOT a removal by default. These files are not ours; see
    // `Warning::UnrecognizedSubHook`. A warning does not suppress the repo
    // either — a stranger's `pre-push-mine.sh` must not block repairing four
    // broken dispatchers sitting in the same directory.
    for name in &repo.foreign_subs {
        let path = hooks.join(name);
        if remove_unrecognized {
            p.remove.push(Removal {
                path,
                reason: RemovalReason::ForeignSubHook,
            });
        } else {
            p.warn.push(Warning::UnrecognizedSubHook { path });
        }
    }
    if repo.hook_pkgjson {
        p.remove.push(Removal {
            path: hooks.join("package.json"),
            reason: RemovalReason::VestigialPackageJson,
        });
    }
    p.remove.sort_by(|a, b| a.path.cmp(&b.path));

    // Only when activating: repairing a managed repo means its dispatchers are
    // already ours, and a drifted one is a repair rather than a stranger.
    //
    // Asked through `hookfile::classify`, which does not follow links and does
    // not need the file to be UTF-8. The predicate here used to be
    // `read_to_string(..).map(|t| !is_our_shim(&t)).unwrap_or(false)` — so a
    // compiled hook (`Err(InvalidData)`) and a symlink to a tracked file (the
    // target's bytes) both came back "not foreign", and activation wrote over
    // them.
    if intent == Intent::Activate {
        let foreign: Vec<String> = DISPATCHERS
            .into_iter()
            .filter(|name| !is_absent_or_ours(&hooks.join(name)))
            .map(str::to_owned)
            .collect();
        if !foreign.is_empty() {
            p.refuse.push(Refusal::ForeignHook { names: foreign });
            return p;
        }
    }

    // A repository that ships its own binary is left exactly as it is.
    //
    // `Repair` means "put back what we install". Here there is nothing to put
    // back: `amont init` baked this from the repository's own `prepare`, the
    // package manager keeps it current, and rewriting it to OUR binary would
    // undo a deliberate arrangement — and break it outright on a machine that
    // has no binary of ours to point at. See [`shim::BakeState::SelfManaged`].
    //
    // `Activate` is not exempt: turning hooks ON in a repository is a different
    // request, and one that has to be able to write the first shims. It never
    // reaches this case anyway, since a repo with our shims already baked is
    // `managed` and therefore repaired rather than activated.
    if intent == Intent::Repair && matches!(repo.baked, crate::shim::BakeState::SelfManaged { .. })
    {
        return p;
    }

    let rendered = shim::render(binary);
    for name in DISPATCHERS {
        let path = hooks.join(name);
        let changes = std::fs::read_to_string(&path)
            .map(|c| c != rendered)
            .unwrap_or(true);
        p.write.push(WriteShim {
            path,
            baked: binary.to_string(),
            changes,
        });
    }

    // Fail closed: one tracked path — or one path git would not answer about —
    // suppresses the WHOLE repo, because a partially applied plan is how a repo
    // ends up running a check twice.
    let refusals: Vec<Refusal> = p
        .remove
        .iter()
        .map(|r| &r.path)
        .chain(p.write.iter().map(|w| &w.path))
        .filter_map(|path| tracked_refusal(path))
        .collect();
    if !refusals.is_empty() {
        p.refuse.extend(refusals);
        p.remove.clear();
        p.write.clear();
    }

    // Independent of the hook-shim logic above: AGENTS.md is a plain tracked
    // file, not a `.git/hooks` dispatcher, so it is neither routed through
    // `is_tracked` nor suppressed by a hook-shim refusal. Read from the scan's
    // own `repo.agents_md` rather than re-touching the filesystem here —
    // `apply` re-checks fresh at the moment of action, the same split already
    // used for `is_tracked`.
    if agents_md {
        let path = repo_abs.join("AGENTS.md");
        match repo.agents_md {
            AgentsMdState::Missing | AgentsMdState::Drifted => {
                p.write_agents_md = Some(WriteAgentsMd {
                    path,
                    changes: true,
                });
            }
            AgentsMdState::UpToDate => {}
            AgentsMdState::Malformed => {
                p.refuse.push(Refusal::AgentsMdMalformed { path });
            }
        }
    }

    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shim::{BakeState, ShimState};

    fn repo(managed: bool) -> Repo {
        Repo {
            path: PathBuf::from("r"),
            managed,
            shims: vec![ShimState::Missing; 4],
            baked: BakeState::None,
            stale_ours: Vec::new(),
            foreign_subs: Vec::new(),
            hook_pkgjson: false,
            languages: Vec::new(),
            applicable: Vec::new(),
            skips: Vec::new(),
            severities: Vec::new(),
            declared: Vec::new(),
            trusted: None,
            agents_md: AgentsMdState::Missing,
            hooks_dir: crate::scan::HooksDir::In {
                path: std::path::PathBuf::from(".git/hooks"),
            },
            shares_hooks_with: None,
        }
    }

    #[test]
    fn an_unmanaged_repo_is_refused_not_fixed() {
        let p = plan(
            &repo(false),
            Path::new("/nowhere"),
            "/bin/gh",
            Intent::Repair,
            false,
            false,
        );
        assert_eq!(p.refuse, vec![Refusal::Unmanaged]);
        assert!(
            p.remove.is_empty() && p.write.is_empty(),
            "never adopt a repo"
        );
    }

    /// A repository whose `.git` is there but whose hooks directory is not.
    /// `Repair` does not create it — a missing `.git/hooks` in a repo we thought
    /// was managed is a fact worth reporting rather than papering over.
    #[test]
    fn a_missing_hooks_dir_is_refused() {
        let dir = std::env::temp_dir().join(format!("fixplan-nohooks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        let p = plan(&repo(true), &dir, "/bin/gh", Intent::Repair, false, false);
        assert_eq!(p.refuse, vec![Refusal::UnreadableHooks]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// CHANGED ON PURPOSE. This case — a repository path that is not there at
    /// all — used to fall out as `UnreadableHooks`, because `hooks_dir_for`
    /// answered every failure with the guess `repo.join(".git/hooks")` and the
    /// guess then failed an `is_dir()`. The right refusal for it, and the reason
    /// `HooksDirUnknown` exists, is that we do not know where this repository's
    /// hooks are: for a repo whose `.git` is a FILE, the old guess was not a
    /// conservative default but a wrong answer, and `UnreadableHooks` reported
    /// it as if we had looked.
    #[test]
    fn a_repo_git_cannot_reach_at_all_refuses_as_unknown() {
        let p = plan(
            &repo(true),
            Path::new("/definitely/not/here"),
            "/bin/gh",
            Intent::Repair,
            false,
            false,
        );
        assert!(
            matches!(p.refuse.as_slice(), [Refusal::HooksDirUnknown { why }] if !why.is_empty()),
            "{:?}",
            p.refuse
        );
        assert!(p.remove.is_empty() && p.write.is_empty());
    }

    #[test]
    fn removals_are_classified_and_sorted() {
        let dir = std::env::temp_dir().join(format!("fixplan-{}", std::process::id()));
        let hooks = dir.join(".git/hooks");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&hooks).unwrap();

        let mut r = repo(true);
        r.stale_ours = vec!["pre-commit-ruff".into()];
        r.foreign_subs = vec!["pre-push-mine.sh".into()];
        r.hook_pkgjson = true;

        // CHANGED ON PURPOSE: `pre-push-mine.sh` used to be a third REMOVAL
        // here, `ForeignSubHook`. It is not ours, so by default it is now a
        // warning and stays where it is; the flag is what puts it back on the
        // removal list. See `Warning::UnrecognizedSubHook` for the evidence.
        let p = plan(&r, &dir, "/bin/gh", Intent::Repair, false, false);
        assert_eq!(p.remove.len(), 2, "{:?}", p.remove);
        let reasons: Vec<_> = p.remove.iter().map(|r| r.reason).collect();
        assert!(reasons.contains(&RemovalReason::StaleOurs));
        assert!(reasons.contains(&RemovalReason::VestigialPackageJson));
        assert!(!reasons.contains(&RemovalReason::ForeignSubHook));
        assert_eq!(
            p.warn,
            vec![Warning::UnrecognizedSubHook {
                path: hooks.join("pre-push-mine.sh")
            }],
            "and it must be NAMED, not silently skipped"
        );
        assert_eq!(
            p.write.len(),
            4,
            "all four are written, as propagate.sh does"
        );
        assert!(p.write.iter().all(|w| w.changes), "none exist yet");

        // The opt-in restores exactly the third removal, and drops the warning.
        let opted_in = plan(&r, &dir, "/bin/gh", Intent::Repair, false, true);
        assert_eq!(opted_in.remove.len(), 3, "{:?}", opted_in.remove);
        assert!(opted_in
            .remove
            .iter()
            .any(|r| r.reason == RemovalReason::ForeignSubHook));
        assert!(opted_in.warn.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A hook somebody else wrote must not stop us repairing the four
    /// dispatchers sitting next to it. That is the difference between the `warn`
    /// channel and the `refuse` one, and folding them together would force a
    /// choice between "say nothing" and "do nothing".
    #[test]
    fn a_stranger_s_hook_warns_without_suppressing_the_repair() {
        let dir = std::env::temp_dir().join(format!("fixplan-warn-{}", std::process::id()));
        let hooks = dir.join(".git/hooks");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&hooks).unwrap();

        let mut r = repo(true);
        r.foreign_subs = vec!["pre-push-mine.sh".into()];
        let p = plan(&r, &dir, "/bin/gh", Intent::Repair, false, false);

        assert!(!p.refused(), "{:?}", p.refuse);
        assert_eq!(p.write.len(), 4, "all four dispatchers still planned");
        assert!(p.remove.is_empty());
        assert_eq!(p.warn.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Writing bytes that are already there is listed but marked as no change,
    /// so "fix" on a healthy repo is visibly a no-op.
    #[test]
    fn an_already_correct_repo_plans_no_changes() {
        let dir = std::env::temp_dir().join(format!("fixplan-ok-{}", std::process::id()));
        let hooks = dir.join(".git/hooks");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&hooks).unwrap();
        for n in DISPATCHERS {
            std::fs::write(hooks.join(n), shim::render("/bin/gh")).unwrap();
        }
        let p = plan(&repo(true), &dir, "/bin/gh", Intent::Repair, false, false);
        assert!(p.is_noop(), "{p:?}");
        assert_eq!(p.write.len(), 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A repo with no `.git/hooks`, otherwise healthy, so only the
    /// `agents_md` argument is under test.
    fn healthy_repo_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("fixplan-agents-md-{name}-{}", std::process::id()));
        let hooks = dir.join(".git/hooks");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&hooks).unwrap();
        for n in DISPATCHERS {
            std::fs::write(hooks.join(n), shim::render("/bin/gh")).unwrap();
        }
        dir
    }

    #[test]
    fn agents_md_is_never_planned_without_opting_in() {
        let dir = healthy_repo_dir("optout");
        let mut r = repo(true);
        r.agents_md = AgentsMdState::Missing;
        let p = plan(&r, &dir, "/bin/gh", Intent::Repair, false, false);
        assert!(p.write_agents_md.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_or_drifted_agents_md_is_planned_when_opted_in() {
        for state in [AgentsMdState::Missing, AgentsMdState::Drifted] {
            let dir = healthy_repo_dir("plan");
            let mut r = repo(true);
            r.agents_md = state;
            let p = plan(&r, &dir, "/bin/gh", Intent::Repair, true, false);
            assert!(!p.is_noop(), "{p:?}");
            let w = p.write_agents_md.expect("must plan a write");
            assert!(w.changes);
            assert_eq!(w.path, dir.join("AGENTS.md"));
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn an_up_to_date_agents_md_is_not_replanned() {
        let dir = healthy_repo_dir("uptodate");
        let mut r = repo(true);
        r.agents_md = AgentsMdState::UpToDate;
        let p = plan(&r, &dir, "/bin/gh", Intent::Repair, true, false);
        assert!(p.write_agents_md.is_none());
        assert!(p.is_noop(), "{p:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_malformed_agents_md_refuses_the_repo() {
        let dir = healthy_repo_dir("malformed");
        let mut r = repo(true);
        r.agents_md = AgentsMdState::Malformed;
        let p = plan(&r, &dir, "/bin/gh", Intent::Repair, true, false);
        assert_eq!(
            p.refuse,
            vec![Refusal::AgentsMdMalformed {
                path: dir.join("AGENTS.md")
            }]
        );
        assert!(p.write_agents_md.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The guard that keeps the apply path away from tracked source. Proven
    /// against a real repository rather than mocked, because the whole point is
    /// that it agrees with git.
    ///
    /// It used to assert a BOOLEAN predicate, `fix::is_tracked`, which this
    /// replaces. Two things were wrong with that predicate and neither was
    /// visible from a test shaped like this one, which is why the assertions
    /// now go through `hookfile::tracked`: it asked with the ABSOLUTE path
    /// (wrong answer whenever `.git/hooks` is a symlink into the working tree —
    /// the setup at the centre of both incidents), and it turned every git
    /// failure into "untracked". See this module's doc.
    #[test]
    fn a_tracked_path_refuses_and_an_untracked_one_does_not() {
        let dir = std::env::temp_dir().join(format!("fixplan-tracked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .expect("git");
        };
        git(&["init", "-q", "--template=", "."]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(dir.join("tracked.txt"), "x").unwrap();
        std::fs::write(dir.join("untracked.txt"), "x").unwrap();
        git(&["add", "tracked.txt"]);
        git(&["commit", "-q", "--no-verify", "-m", "chore: seed"]);

        assert_eq!(
            tracked_refusal(&dir.join("tracked.txt")),
            Some(Refusal::Tracked {
                path: dir.join("tracked.txt")
            })
        );
        assert_eq!(tracked_refusal(&dir.join("untracked.txt")), None);
        assert_eq!(tracked_refusal(&dir.join("does-not-exist.txt")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A repository that ships its own binary is left ALONE by a repair, and
    /// the same repository with an ordinary stale bake is still repaired.
    ///
    /// One test, two halves, on a REAL repository with REAL shims on disk. The
    /// first version of this hand-built the `Repo` fixture and asserted "no
    /// writes" — which passed before the exemption existed, because a fixture
    /// pointing at a path with no `.git` is refused for unrelated reasons. A
    /// test that cannot fail is worse than no test, and the control half below
    /// is what makes the first half mean anything.
    ///
    /// The bake state is DERIVED from the files rather than hand-constructed,
    /// so `bake_state`'s classification and `plan`'s reaction to it are proved
    /// together — a rename in one that the other missed would show up here.
    #[test]
    fn a_repair_spares_a_package_managed_bake_and_still_fixes_a_stale_one() {
        let base = std::env::temp_dir().join(format!("fixplan-selfmanaged-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);

        // `baked_at` is the path written INTO the shims; `ours` is what the
        // fleet would install. They always differ here — that is the whole
        // question — so what separates the two halves is only whether the baked
        // path lives in a node_modules tree.
        let plan_for = |name: &str, baked_at: &str| {
            let dir = base.join(name);
            let hooks = dir.join(".git/hooks");
            std::fs::create_dir_all(&hooks).unwrap();
            std::process::Command::new("git")
                .args(["init", "-q", "--template=", "."])
                .current_dir(&dir)
                .output()
                .expect("git init");
            let rendered = shim::render(baked_at);
            let mut shims = Vec::new();
            for n in DISPATCHERS {
                std::fs::write(hooks.join(n), &rendered).unwrap();
                shims.push(crate::shim::classify(&hooks.join(n)));
            }
            let mut r = repo(true);
            r.baked = crate::shim::bake_state(&shims, "/usr/local/bin/amont");
            let p = plan(
                &r,
                &dir,
                "/usr/local/bin/amont",
                Intent::Repair,
                false,
                false,
            );
            (r.baked.clone(), p)
        };

        let (state, p) = plan_for("npm", "/w/node_modules/amont-darwin-arm64/bin/amont");
        assert!(
            matches!(state, crate::shim::BakeState::SelfManaged { .. }),
            "expected SelfManaged, got {state:?}"
        );
        assert!(
            !p.refused(),
            "refused for an unrelated reason: {:?}",
            p.refuse
        );
        assert!(p.write.is_empty(), "a repair rewrote them: {:?}", p.write);

        // The control. Same repository shape, same mismatch against our binary,
        // only the baked path differs — and this one MUST be repaired.
        let (state, p) = plan_for("stale", "/somewhere/else/amont");
        assert!(
            matches!(state, crate::shim::BakeState::Stale { .. }),
            "expected Stale, got {state:?}"
        );
        assert!(
            !p.refused(),
            "refused for an unrelated reason: {:?}",
            p.refuse
        );
        assert_eq!(
            p.write.len(),
            DISPATCHERS.len(),
            "repair skipped a genuinely stale bake"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// `plan()` must find and repair shims at a redirected `core.hooksPath`,
    /// not at `.git/hooks` — the write-side counterpart to the scanner's own
    /// `core_hooks_path_is_honoured_not_assumed`. A REAL repository, unlike
    /// `repo()`'s fixtures: `core.hooksPath` only means anything to git
    /// itself, which is exactly the predicate this is proving.
    #[test]
    fn plan_finds_shims_at_a_redirected_hooks_path() {
        let dir = std::env::temp_dir().join(format!("fixplan-hookspath-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .expect("git");
        };
        git(&["init", "-q", "--template=", "."]);
        git(&["config", "core.hooksPath", "tooling/hooks"]);
        let hooks = dir.join("tooling/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::write(
            hooks.join("pre-commit-ruff"),
            "#!/bin/sh\nexec x --hooks-dir y pre-commit-ruff\n",
        )
        .unwrap();

        let mut r = repo(true);
        r.stale_ours = vec!["pre-commit-ruff".into()];
        let p = plan(&r, &dir, "/bin/gh", Intent::Repair, false, false);

        assert!(!p.refused(), "{:?}", p.refuse);
        assert_eq!(
            p.remove,
            vec![Removal {
                path: hooks.join("pre-commit-ruff"),
                reason: RemovalReason::StaleOurs,
            }]
        );
        assert!(
            p.write.iter().all(|w| w.path.starts_with(&hooks)),
            "shims must be planned at the redirected path, not .git/hooks: {:?}",
            p.write
        );
        assert!(
            !dir.join(".git/hooks").exists(),
            "fixture: the default location was never created"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
