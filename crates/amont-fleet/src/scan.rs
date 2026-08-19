//! Walking a directory tree for git repositories, and counting everything.
//!
//! The counters are the point. `scripts/propagate.sh` printed scalars with no
//! denominator, and twice reported something that could not be true: 192
//! removals per hook across 96 repos holding one copy each, and
//! `0 copies / 0 distinct` from a `-maxdepth` that matched nothing. The second
//! is the dangerous one — a broken scan and a clean fleet produced identical
//! output.
//!
//! So this records how it arrived at its answer: directories visited, entries
//! it could not read, subtrees it deliberately skipped. A caller can then tell
//! "found nothing because there is nothing" from "found nothing because I
//! looked in the wrong place", which no scalar can express.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use crate::severities::{self, SeverityOverride};
use crate::shim::{self, BakeState, ShimState, DISPATCHERS};
use crate::skips::{self, SkipEntry};

/// One `amont.conf` line, for display and for `--json`.
///
/// A SUM, like the `Line` it projects. Flattening it into `severity` +
/// `command` + `Option<broken>` would rebuild here exactly the shape the
/// runtime just stopped using: fields that mean nothing when the line is
/// unusable, and a renderer that has to remember which.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeclaredCheck {
    pub name: String,
    pub stage: String,
    #[serde(flatten)]
    pub state: DeclaredState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DeclaredState {
    Usable {
        severity: String,
        /// Extensions that gate it. Empty means every change.
        exts: Vec<String>,
        command: String,
    },
    /// The check does not run, and saying so is the whole reason the dashboard
    /// reads these files.
    Unusable { why: String },
}

impl DeclaredCheck {
    pub fn is_unusable(&self) -> bool {
        matches!(self.state, DeclaredState::Unusable { .. })
    }
}

/// A projection rather than a re-parse: `manifest::read_lines` is the same
/// parser the dispatcher uses, so the dashboard cannot form its own opinion
/// about what a manifest means.
fn declared_checks(repo: &Path) -> Vec<DeclaredCheck> {
    amont_runtime::manifest::read_lines(repo)
        .into_iter()
        // Only CHECK lines: a tool pin used to fall through `into_parts` as
        // an error here and count as `broken` in the TUI — a manifest doing
        // exactly what the docs suggest looked damaged. Policy lines get
        // their own surfacing (a later change); neither is a check.
        .filter(|line| line.is_check())
        .map(|line| {
            let (name, stage, parsed) = line.into_parts();
            DeclaredCheck {
                name,
                stage: stage.as_str().to_string(),
                state: match parsed {
                    Ok(declared) => DeclaredState::Usable {
                        severity: declared.severity.as_str().to_string(),
                        command: declared.command(),
                        exts: {
                            // Filename tokens ride in the same display list:
                            // the dashboard's question is "fires when", and
                            // both kinds of token answer it.
                            let mut fires_on = declared.exts;
                            fires_on.extend(declared.names);
                            fires_on
                        },
                    },
                    Err(why) => DeclaredState::Unusable { why },
                },
            }
        })
        .collect()
}

/// Subtrees never worth descending. Matches the exclusions the shell sweep
/// used, so the two agree about what "the fleet" means.
pub const EXCLUDED: [&str; 6] = ["node_modules", "target", "dist", "build", ".venv", "vendor"];

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Repo {
    /// Path relative to the scan root, which is what a human recognises.
    pub path: PathBuf,
    /// At least one file in `.git/hooks` dispatches to the binary.
    pub managed: bool,
    /// One entry per git-invoked hook, in `DISPATCHERS` order.
    pub shims: Vec<ShimState>,
    pub baked: BakeState,
    /// Our files that we no longer ship — the 16 per-check shims retired when
    /// checks moved in-process, and anything else removed upstream.
    pub stale_ours: Vec<String>,
    /// Hand-written `pre-commit-*` / `pre-push-*` sub-hooks. Nothing dispatches
    /// these any more, so they LOOK installed and never run.
    pub foreign_subs: Vec<String>,
    /// The node-era `package.json` that forced CommonJS. No hook is node now.
    pub hook_pkgjson: bool,
    /// Manifests present at the repo root. Display only — the LANG column.
    pub languages: Vec<String>,
    /// Checks that would ever fire here, from each check's own `Scope`
    /// evaluated against this repo's tracked files. Not inferred by this crate:
    /// a fourth copy of that rule was what `LANGUAGES` used to be.
    pub applicable: Vec<String>,
    /// `hook.skip` entries, resolved: what each one suppresses and where it
    /// came from. Bare strings hid both — a value need not be a check id (a
    /// trigger silences fifteen), and local/global are indistinguishable once
    /// merged.
    pub skips: Vec<SkipEntry>,
    /// `amont.severity.*` entries. A downgraded check still runs and still
    /// prints its failure, so unlike a skip it leaves no trace on screen — a
    /// repo that enforces nothing reads exactly like one that enforces
    /// everything unless this column says otherwise.
    pub severities: Vec<SeverityOverride>,
    /// Commits made here that carry no record their commit-time gate ran —
    /// `--no-verify` and its cousins, read from the runtime's local ledger.
    /// A rising count is the first symptom of a gate people have started
    /// routing around (a slow check, a flaky one), and until it was counted
    /// the hooks detected the signal on every commit and threw it away.
    pub bypasses: crate::bypasses::Bypasses,
    /// Checks this repo declares in `amont.conf`. Invisible to the fleet
    /// view until now: a repo could be running a command on every commit that
    /// no dashboard column mentioned, and a manifest line nobody can parse is a
    /// check that silently is not running.
    pub declared: Vec<DeclaredCheck>,
    /// Whether this repo's `amont.conf` is trusted here. `None` when there
    /// is no manifest — which is almost every repo, and must read differently
    /// from "declared something and it is not running".
    pub trusted: Option<bool>,
    /// Whether `AGENTS.md` carries an up-to-date pointer at this check's own
    /// generated block, and if not, why.
    pub agents_md: AgentsMdState,
    /// Where this repo's hooks resolve to, and whether that is inside it.
    pub hooks_dir: HooksDir,
    /// Set when another repository already seen in this scan shares this one's
    /// hooks directory, naming that repository.
    ///
    /// Recognising `.git` FILES created this problem the same day it fixed the
    /// bigger one: a submodule and a linked worktree keep their hooks in the
    /// superproject/main repo, so one hooks directory became reachable from two
    /// entries in `repos`. Without this, `fix --apply` would write the same four
    /// files twice and count them twice, and the dashboard would report a repo
    /// as needing an install it had already received through its sibling.
    ///
    /// The first repository encountered in the walk's (stable, sorted) order
    /// owns the directory; every later one points at it.
    pub shares_hooks_with: Option<PathBuf>,
}

/// A local wrapper around `amont_runtime::agents_md::CheckResult`: that
/// type lives in the dependency-free crate and cannot derive `Serialize`
/// itself, the same reason `severities::Level` wraps
/// `amont_runtime::check::Severity` rather than re-exporting it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentsMdState {
    UpToDate,
    Missing,
    Drifted,
    /// An unpaired marker — not something this crate, any more than the
    /// dependency-free one, tries to guess its way out of.
    Malformed,
}

fn agents_md_state(repo: &Path) -> AgentsMdState {
    use amont_runtime::agents_md::CheckResult;
    match amont_runtime::agents_md::check(&repo.join("AGENTS.md")) {
        Ok(CheckResult::MatchesGenerated) => AgentsMdState::UpToDate,
        Ok(CheckResult::NotPresent) => AgentsMdState::Missing,
        Ok(CheckResult::Drifted) => AgentsMdState::Drifted,
        Err(_) => AgentsMdState::Malformed,
    }
}

/// A whole scan, including how it was performed.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FleetScan {
    pub root: PathBuf,
    pub depth: usize,
    pub git_dirs_found: usize,
    pub hook_dirs_seen: usize,
    pub managed_seen: usize,
    pub unmanaged_seen: usize,
    /// Paths that exist but could not be read. Never silently dropped: an
    /// unreadable repo is not an absent one.
    pub unreadable: Vec<PathBuf>,
    /// Repositories whose hooks resolve somewhere this tool will not touch —
    /// outside the repository, or nowhere git would name. Counted rather than
    /// merely listed per-repo, because the number that matters to a reader is
    /// "how much of this fleet did I decline to act on".
    pub hooks_outside_seen: usize,
    pub excluded_dirs: usize,
    pub dirs_visited: usize,
    /// Unverified commits summed across the fleet, with the repo count as
    /// the denominator's denominator — every number here carries context.
    pub bypassed_commits: usize,
    pub repos_with_bypasses: usize,
    pub repos: Vec<Repo>,
}

impl FleetScan {
    /// True when the scan looked at essentially nothing. The caller is expected
    /// to render this as a FAILURE rather than as an empty success — the single
    /// rule this whole tool exists to enforce.
    pub fn looks_like_a_failed_scan(&self) -> bool {
        self.git_dirs_found == 0
    }
}

/// A file is ours only if it carries our shim marker AND is an ordinary file we
/// could actually read.
///
/// Asked once, via [`amont_runtime::hookfile::classify`], rather than
/// re-tested here. This answer feeds `stale_ours`, which `fix::plan` turns
/// straight into a REMOVAL list, and `is_managed`, which decides whether the
/// tool will act on a repository at all — so both halves of a wrong answer are
/// expensive:
///
/// - The body was once `s.contains("--hooks-dir")`, matched by a hand-written
///   hook that merely mentions the flag in a comment or forwards it to another
///   tool. That claimed somebody else's file as ours to delete.
/// - Then it was `read_to_string(..).map(is_our_shim).unwrap_or(false)`, which
///   FOLLOWS SYMLINKS: a dispatcher that is a link to a shim in the working tree
///   read as ours, so the repo counted as managed and `fix --apply` wrote
///   through the link. `hookfile::classify` reports the link as a link.
fn is_ours(path: &Path) -> bool {
    matches!(
        amont_runtime::hookfile::classify(path),
        amont_runtime::hookfile::HookFile::Ours
    )
}

/// Where a scanned repository's hooks live — and whether that is somewhere this
/// tool is willing to touch.
///
/// The `Outside` variant is the whole reason this is a sum type rather than a
/// `PathBuf`. `core.hooksPath` may be an ABSOLUTE path anywhere on the disk, and
/// `Intent::Activate` used to `create_dir_all` whatever came back and write four
/// 0o755 files into it. Nothing canonicalised, nothing compared it against the
/// repository — so a repo could name `/etc/amont`, or another checkout's
/// working tree, and a fleet-wide `install` would create it and populate it.
///
/// **The asymmetry with per-repo `amont install` is deliberate.** That
/// command keeps honouring its own repository's `core.hooksPath`, absolute or
/// not: you are standing in that repository and you configured it yourself, so
/// the redirect is your instruction. The fleet refuses, because it is walking
/// ninety-six repositories it did not configure and a redirect there is a fact
/// about somebody else's checkout, not an instruction to this tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "where", rename_all = "snake_case")]
pub enum HooksDir {
    /// Inside the repository's own working tree, or inside its git common
    /// directory. Both count: a submodule's and a linked worktree's hooks live
    /// in the superproject/main repo's common dir and are perfectly legitimate,
    /// and a repo redirecting `core.hooksPath` to `tooling/hooks` within itself
    /// is redirecting within itself.
    In { path: PathBuf },
    /// Inside the repository, and NOT its own `<git-common-dir>/hooks` —
    /// `core.hooksPath` handed dispatch to another tool.
    ///
    /// Split out of `In`, which used to swallow it, and the silence cost
    /// something concrete: every `duro-*` repository ran husky, so
    /// `core.hooksPath` was `.husky/_`, so the whole fleet reported them as
    /// "drifted" — four shims that did not match, in a directory husky
    /// regenerates on the next `npm install`. What was actually true is that
    /// amont was not running in them at all, and a direct push to `main` went
    /// through unchallenged for as long as that lasted.
    ///
    /// `hostile` is decided ONCE here, at scan time, by
    /// [`amont_runtime::install::redirect_is_hostile`] — the same predicate
    /// `amont install` uses, so a repository cannot be judged one way by the
    /// dashboard and the other by the installer.
    ///
    /// Stored rather than recomputed because that predicate reads four files,
    /// and `inside()` is called per row per frame by the TUI. A cheap accessor
    /// that quietly did filesystem I/O would be a render-loop stall nobody
    /// looking at the call site could see.
    ///
    /// Hostile is reported and never written to; benign is followed exactly as
    /// before, which is what keeps a deliberate `tooling/hooks` working. The
    /// remedy for the hostile case is `git config --unset core.hooksPath`, which
    /// the message says.
    Redirected {
        path: PathBuf,
        own: PathBuf,
        hostile: bool,
    },
    /// Resolved to a directory that is neither. Reported, never created, never
    /// written to, never deleted from.
    Outside { path: PathBuf },
    /// git would not say. Not a guess-worthy state: the old code fell back to
    /// `repo.join(".git/hooks")`, which for a repository whose `.git` is a FILE
    /// (a submodule, a linked worktree) is not a fallback but a wrong answer.
    Unknown { why: String },
}

impl HooksDir {
    /// The directory, when it is one we may act on. `None` is the refusal.
    pub fn inside(&self) -> Option<&Path> {
        match self {
            HooksDir::In { path } => Some(path),
            // A benign redirect is a directory this repository chose and we have
            // always honoured. Only a hostile one is withheld.
            HooksDir::Redirected {
                path,
                hostile: false,
                ..
            } => Some(path),
            _ => None,
        }
    }

    /// Whether another tool has taken hook dispatch away from this repository.
    pub fn is_hostile_redirect(&self) -> bool {
        matches!(self, HooksDir::Redirected { hostile: true, .. })
    }

    /// A fragment for the middle of a sentence: "the hooks directory is {}".
    pub fn describe(&self) -> String {
        match self {
            HooksDir::In { path } => path.display().to_string(),
            HooksDir::Redirected {
                path,
                hostile: true,
                ..
            } => match amont_runtime::install::redirect_culprit(path) {
                Some(tool) => format!("{} — {tool} owns the hooks here", path.display()),
                None => format!(
                    "{} — core.hooksPath sent git here, amont is not running",
                    path.display()
                ),
            },
            HooksDir::Redirected { path, .. } => {
                format!("{} — via core.hooksPath", path.display())
            }
            HooksDir::Outside { path } => {
                format!("{} — OUTSIDE the repository", path.display())
            }
            HooksDir::Unknown { why } => format!("unresolvable ({why})"),
        }
    }
}

/// Where `repo`'s hooks actually live, resolved by git and then contained.
///
/// `git rev-parse` is the authority for the resolution: relative
/// `core.hooksPath`, `~` expansion, `.git`-file indirection and worktree
/// indirection are git's rules to get right, not ours to reimplement. This used
/// to skip the call unless `.git/config`'s own bytes mentioned `hooksPath`, on
/// the theory that a config file which does not mention it cannot have set it —
/// true of THAT FILE, but the value can just as well come from global or system
/// config, or from a file `.git/config` merely `include`s. One `git` call per
/// repo, always.
///
/// Three paths come back from one invocation, and all three are needed:
///
/// | asked | used for |
/// |---|---|
/// | `--git-path hooks` | where the hooks are |
/// | `--git-common-dir` | a submodule's / linked worktree's hooks live here |
/// | `--show-toplevel`  | an in-repo `core.hooksPath` redirect lives here |
///
/// Containment is tested against EITHER of the last two, and compares
/// git-reported paths only — never a git-reported path against one this process
/// built — because git resolves symlinks in what it prints (`/tmp` comes back as
/// `/private/tmp` on macOS) and a lexical comparison across that boundary would
/// report every repository under a symlinked root as `Outside`.
///
/// Having compared in git's spelling, the answer is returned in the CALLER's
/// wherever it can be: hooks under the toplevel come back re-anchored onto
/// `repo`. Everything downstream — the removal list, the write list, the
/// root-relative paths the report and the parity gate compare — is built from
/// this, and handing back `/private/var/…` for a `--root` of `/var/…` would make
/// every one of those paths fail to relativise against the root the user typed.
/// A submodule's or linked worktree's hooks live outside the toplevel by
/// definition and keep git's absolute answer, which is the only true one there.
///
/// ## The one fallback, and its guard
///
/// `fatal: not a git repository` with a `.git` that IS a directory falls back to
/// `<repo>/.git/hooks` as `In`. That is not the old guess wearing a hat: a
/// directory git does not recognise as a repository cannot have a
/// `core.hooksPath`, so there is nothing to resolve and exactly one place the
/// walk could have meant. It is also load-bearing for the test suites — the
/// fixtures throughout this crate build `<name>/.git/hooks` in a bare temp dir,
/// and refusing those would fail every one of them for a reason unrelated to
/// what it tests, the same accommodation `hookfile::tracked` makes and for the
/// same reason. The `.git`-is-a-DIRECTORY guard is what keeps it honest: when
/// `.git` is a FILE and git will not resolve the pointer inside it, we do not
/// know where the hooks are, and `Unknown` says so.
pub fn hooks_dir_for(repo: &Path) -> HooksDir {
    let out = match Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "hooks",
            "--git-common-dir",
            "--show-toplevel",
        ])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            return HooksDir::Unknown {
                why: format!("could not run git: {e}"),
            }
        }
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.to_lowercase().contains("not a git repository") && repo.join(".git").is_dir() {
            return HooksDir::In {
                path: repo.join(".git").join("hooks"),
            };
        }
        return HooksDir::Unknown {
            why: first_line(&stderr),
        };
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut lines = stdout.lines();
    let (Some(hooks), Some(common), Some(top)) = (lines.next(), lines.next(), lines.next()) else {
        return HooksDir::Unknown {
            why: "git rev-parse answered with fewer paths than it was asked for".to_string(),
        };
    };
    let hooks = PathBuf::from(hooks);
    let own = Path::new(common).join("hooks");

    // Re-root a path that lands inside the working tree onto the caller's own
    // `repo`, rather than handing back git's. They name the same directory and
    // are not the same string: git canonicalizes, so on macOS a repository under
    // `$TMPDIR` comes back as `/private/var/…` where the caller said `/var/…`.
    // `apply` compares its re-resolved directory against the one the plan
    // carried, so a plan holding git's spelling and an apply holding the
    // caller's would never match.
    let rooted = |p: &Path| -> PathBuf {
        match amont_runtime::hookfile::resolve_lexical(p)
            .strip_prefix(amont_runtime::hookfile::resolve_lexical(Path::new(top)))
        {
            Ok(rel) => repo.join(rel),
            Err(_) => p.to_path_buf(),
        }
    };

    // `Outside` FIRST, and it outranks everything below. `core.hooksPath` may
    // name any absolute path on the disk — `/etc/amont`, another checkout's
    // working tree — and that is the case where nothing may be created, written
    // or deleted at all. `Redirected` is the weaker finding (a directory inside
    // this repository), so classifying a `/elsewhere` path as merely redirected
    // would trade the strong refusal for the soft one.
    let within_repo = amont_runtime::hookfile::is_within(&hooks, Path::new(top))
        || amont_runtime::hookfile::is_within(&hooks, Path::new(common));
    if !within_repo {
        return HooksDir::Outside { path: hooks };
    }
    // Then the redirect, BEFORE deciding it is ours. A redirect landing inside
    // the working tree (`.husky/_`, `tooling/hooks`) satisfies the containment
    // test above, so asking containment alone is exactly what let husky's
    // directory pass as this repository's own.
    if amont_runtime::hookfile::resolve_lexical(&hooks)
        != amont_runtime::hookfile::resolve_lexical(&own)
    {
        let hostile = amont_runtime::install::redirect_is_hostile(&hooks, &own);
        return HooksDir::Redirected {
            path: rooted(&hooks),
            own,
            hostile,
        };
    }
    HooksDir::In {
        path: rooted(&hooks),
    }
}

/// The git common directory, which is what two "repositories" sharing one hooks
/// directory have in common — a linked worktree and its main repo report the
/// same one. `None` when git would not say, in which case no sharing can be
/// established and none is claimed.
fn common_dir_for(repo: &Path) -> Option<PathBuf> {
    amont_runtime::git::stdout_in(
        repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .map(PathBuf::from)
}

fn first_line(s: &str) -> String {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("no output")
        .trim()
        .to_string()
}

pub fn is_managed(hooks: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(hooks) else {
        return false;
    };
    entries.flatten().any(|e| is_ours(&e.path()))
}

/// Progress emitted while walking, so a caller can paint rows as they are
/// found rather than after 7 seconds of nothing. Visibility of system status is
/// the first usability heuristic, and this scan is well past the ~400ms at
/// which an interface stops feeling immediate.
pub enum Progress<'a> {
    /// A directory the walk has just entered: how many so far, and which one.
    ///
    /// The path is not decoration. A bare count answers "is it moving"; when
    /// the answer is no — a repository on a network mount, a `git` call that
    /// will not return — only the path answers "moving through WHAT", which is
    /// the difference between a hang a reader can act on and one they can only
    /// wait out.
    Visited {
        count: usize,
        dir: &'a Path,
    },
    Found(&'a Repo),
}

/// The callback is not optional, and there is no wrapper that discards it.
/// There WAS one — `scan_with` for the dashboard, `scan` for everyone else —
/// and it is how every plain command came to run a seven-second walk in
/// silence: the quiet version was the one that was easy to call. A caller with
/// nothing to draw passes `&mut |_| {}` and has to look at what it is throwing
/// away to write it.
pub fn scan(
    root: &Path,
    depth: usize,
    installed_binary: &str,
    on: &mut dyn FnMut(Progress),
) -> FleetScan {
    let mut w = Walk {
        root: root.to_path_buf(),
        installed_binary: installed_binary.to_string(),
        seen_common: std::collections::HashMap::new(),
        scan: FleetScan {
            root: root.to_path_buf(),
            depth,
            git_dirs_found: 0,
            hook_dirs_seen: 0,
            managed_seen: 0,
            unmanaged_seen: 0,
            unreadable: Vec::new(),
            hooks_outside_seen: 0,
            excluded_dirs: 0,
            dirs_visited: 0,
            bypassed_commits: 0,
            repos_with_bypasses: 0,
            repos: Vec::new(),
        },
    };
    walk(&mut w, root, depth, on);
    w.scan.repos.sort_by(|a, b| a.path.cmp(&b.path));
    w.scan
}

/// State the walk carries that is not part of the reported scan.
///
/// `seen_common` is the one that earns the struct: answering "have I already
/// scanned something that shares this hooks directory?" needs a memory that
/// outlives one directory's recursion, and threading a seventh `&mut` argument
/// through `walk` was how the previous shape started to go wrong.
struct Walk {
    root: PathBuf,
    installed_binary: String,
    seen_common: std::collections::HashMap<PathBuf, PathBuf>,
    scan: FleetScan,
}

/// Walks `dir`, following symlinks — DELIBERATELY, not merely because
/// `Path::is_dir` happens to. `~/.config/git/git-templates` is commonly a
/// symlink to a checkout (see `install::TemplateDir`), and a linked worktree
/// reaches its repository the same way; either can sit inside `--root`, and a
/// scan that refused to follow them would silently under-report the fleet —
/// exactly the "0 copies / 0 distinct" failure this module's own doc warns
/// about.
///
/// The `depth` budget bounds the cost, but a symlink can still walk the scan
/// to a directory outside `--root` that a filesystem path comparison would
/// not expect. That is not this function's problem to solve: `fix`/`apply`
/// never trust a path scan reported — every removal and write is re-verified
/// through `hookfile`'s guards at the moment of action — so a symlink reaching
/// tracked source changes what gets REPORTED here, never what a later `--apply`
/// is willing to touch.
///
/// ## `.git` is not always a directory
///
/// The entry test used to be `path.is_dir()` before the name was even looked
/// at, so a repository whose `.git` is a FILE holding `gitdir: …` — every
/// SUBMODULE, and every linked WORKTREE — was invisible. Not scanned, not
/// installed into, and not counted as uncovered: the dashboard reported a clean
/// fleet while commits inside a submodule ran no checks at all. That is the
/// exact failure mode this crate's module doc opens with, one file type along.
///
/// The pointer inside a `.git` file is never parsed here. `git rev-parse`
/// resolves it (see [`hooks_dir_for`]), because the format has grown relative
/// gitdirs, worktree indirection and `commondir` files, and a hand-rolled
/// reader of it would be a fourth place that can disagree with git.
///
/// Entries are SORTED before they are acted on. Ordering used to be whatever
/// `read_dir` returned, which was fine while every repo was independent; it
/// stopped being fine when a linked worktree and its main repo can both claim
/// the same hooks directory and exactly one of them must be recorded as the
/// owner. An unstable order there means an unstable `shares_hooks_with`.
fn walk(w: &mut Walk, dir: &Path, budget: usize, on: &mut dyn FnMut(Progress)) {
    w.scan.dirs_visited += 1;
    on(Progress::Visited {
        count: w.scan.dirs_visited,
        dir: dir.strip_prefix(&w.root).unwrap_or(dir),
    });
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => {
            w.scan.unreadable.push(dir.to_path_buf());
            return;
        }
    };

    let mut repos = Vec::new();
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();

        // Name first, TYPE SECOND: a `.git` file is a repository just as much as
        // a `.git` directory is.
        if name == ".git" {
            repos.push(path.parent().unwrap_or(&path).to_path_buf());
            continue;
        }
        if !path.is_dir() {
            continue;
        }
        if EXCLUDED.contains(&name.as_str()) {
            w.scan.excluded_dirs += 1;
            continue;
        }
        subdirs.push(path);
    }
    repos.sort();
    subdirs.sort();

    for repo in repos {
        let found = found_repo(w, &repo);
        on(Progress::Found(&found));
        w.scan.repos.push(found);
    }

    if budget == 0 {
        return;
    }
    for subdir in subdirs {
        walk(w, &subdir, budget - 1, on);
    }
}

/// One repository, counted and inspected. Split out of [`walk`] only because
/// the counting is now five decisions rather than two, and burying them inside
/// a loop over directory entries is how `hook_dirs_seen` came to be incremented
/// for a directory nobody had established was a hooks directory.
fn found_repo(w: &mut Walk, repo: &Path) -> Repo {
    w.scan.git_dirs_found += 1;
    let hooks_dir = hooks_dir_for(repo);

    // Sharing is established from the git COMMON directory, not from the hooks
    // path: two repos can legitimately be told to use one `core.hooksPath`
    // without being the same repository, and it is the repository identity that
    // decides whether writing twice is writing the same file twice.
    // Resolved once and reused below for the bypass ledger — one
    // `rev-parse` per repo, not two, across a whole fleet.
    let common = common_dir_for(repo);
    let shares_hooks_with = common.clone().and_then(|common| {
        use std::collections::hash_map::Entry;
        let rel = repo.strip_prefix(&w.root).unwrap_or(repo).to_path_buf();
        match w.seen_common.entry(common) {
            // The FIRST repository to claim a common directory keeps it, so a
            // third sharer points at the same owner as the second rather than
            // at the second itself.
            Entry::Occupied(e) => Some(e.get().clone()),
            Entry::Vacant(e) => {
                e.insert(rel);
                None
            }
        }
    });

    match &hooks_dir {
        HooksDir::In { path } => {
            if path.is_dir() {
                w.scan.hook_dirs_seen += 1;
            }
        }
        _ => w.scan.hooks_outside_seen += 1,
    }

    // A repository we will not look inside is never "managed": claiming so would
    // put it in the population `fix` acts on, which is the one thing an
    // unresolvable or out-of-repo hooks directory must not do.
    let managed = hooks_dir.inside().is_some_and(is_managed);
    if managed {
        w.scan.managed_seen += 1;
    } else {
        w.scan.unmanaged_seen += 1;
    }
    let inspected = inspect(
        &w.root,
        repo,
        hooks_dir,
        managed,
        shares_hooks_with,
        common.as_deref(),
        &w.installed_binary,
    );
    w.scan.bypassed_commits += inspected.bypasses.total;
    if inspected.bypasses.total > 0 {
        w.scan.repos_with_bypasses += 1;
    }
    inspected
}

/// Everything we can learn about one repository from its hooks directory.
///
/// When `hooks_dir` is not [`HooksDir::In`] this reads NOTHING from it — not
/// even to report what is there. That is a deliberate refusal to look rather
/// than an inability: a repository can point `core.hooksPath` at any directory
/// on the disk, and enumerating it would put somebody else's filenames into this
/// tool's output (and into `stale_ours`, which is a removal list). The four
/// dispatchers come back as `Unreadable`, naming why, which is honest and — the
/// part that matters — is not `Missing`, the state that makes `fix` write.
fn inspect(
    root: &Path,
    repo: &Path,
    hooks_dir: HooksDir,
    managed: bool,
    shares_hooks_with: Option<PathBuf>,
    common_dir: Option<&Path>,
    installed_binary: &str,
) -> Repo {
    let (mut stale_ours, mut foreign_subs) = (Vec::new(), Vec::new());
    let mut hook_pkgjson = false;
    let shims: Vec<ShimState> = match hooks_dir.inside() {
        None => vec![
            ShimState::Unreadable {
                why: format!("hooks directory {}", hooks_dir.describe()),
            };
            DISPATCHERS.len()
        ],
        Some(hooks) => {
            if let Ok(entries) = std::fs::read_dir(hooks) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.ends_with(".sample") || DISPATCHERS.contains(&name.as_str()) {
                        continue;
                    }
                    if name == "package.json" {
                        hook_pkgjson = std::fs::read_to_string(&p)
                            .map(|c| c.contains("Forces Node"))
                            .unwrap_or(false);
                        continue;
                    }
                    if is_ours(&p) {
                        stale_ours.push(name);
                    } else if name.starts_with("pre-commit-") || name.starts_with("pre-push-") {
                        foreign_subs.push(name);
                    }
                }
            }
            DISPATCHERS
                .iter()
                .map(|n| shim::classify(&hooks.join(n)))
                .collect()
        }
    };
    stale_ours.sort();
    foreign_subs.sort();

    Repo {
        path: repo.strip_prefix(root).unwrap_or(repo).to_path_buf(),
        managed,
        baked: shim::bake_state(&shims, installed_binary),
        shims,
        stale_ours,
        foreign_subs,
        hook_pkgjson,
        languages: languages(repo),
        applicable: applicable_checks(repo),
        skips: skips::read(repo),
        severities: severities::read(repo),
        bypasses: common_dir.map(crate::bypasses::read).unwrap_or_default(),
        declared: declared_checks(repo),
        trusted: match amont_runtime::trust::state(repo) {
            amont_runtime::trust::State::NoManifest => None,
            amont_runtime::trust::State::Trusted => Some(true),
            _ => Some(false),
        },
        agents_md: agents_md_state(repo),
        hooks_dir,
        shares_hooks_with,
    }
}

/// Root-level manifests only. Deliberately an approximation: the hooks
/// themselves resolve the NEAREST manifest, so a repo can hold Rust in a
/// subdirectory and show no `rust` here. It drives a display column, never a
/// verdict.
fn languages(repo: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let has = |f: &str| repo.join(f).is_file();
    if has("Cargo.toml") {
        out.push("rust".into());
    }
    if has("package.json") {
        out.push("js".into());
    }
    if has("pyproject.toml") || has("requirements.txt") || has("setup.py") {
        out.push("python".into());
    }
    if has("kustomization.yaml") || has("kustomization.yml") || repo.join("k8s").is_dir() {
        out.push("k8s".into());
    }
    out
}

/// Which checks could ever fire in this repository.
///
/// One `git ls-files` per repo, evaluated against each check's declared
/// `Scope`. Coarser than what a check enforces at commit time — `cargo-fmt`
/// resolves the NEAREST ancestor `Cargo.toml` while this asks whether the repo
/// contains one at all — and deliberately so: the dispatcher answers "does this
/// apply to these staged files", and this answers "would it ever fire here".
/// Over-approximating is the safe direction; the alternative was a table in
/// this crate guessing at rules the checks already own.
fn applicable_checks(repo: &Path) -> Vec<String> {
    let Ok(out) = Command::new("git")
        .args(["ls-files"])
        .current_dir(repo)
        .output()
    else {
        return Vec::new();
    };
    let paths: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    applicable_from_paths(&paths)
}

/// The pure half: which checks could ever fire in a repository containing these
/// paths.
///
/// Split out because the rollup's fixtures used to recompute this filter
/// themselves. A test that builds its own answer cannot fail when the answer is
/// wrong — the fixtures call this now.
pub fn applicable_from_paths(paths: &[String]) -> Vec<String> {
    amont_runtime::registry::CHECKS
        .iter()
        .filter(|c| c.scope.matches(paths))
        .map(|c| c.name.to_string())
        .collect()
}
