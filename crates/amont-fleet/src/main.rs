//! `amont-fleet` — what the hook fleet actually looks like on this machine.
//!
//! A separate binary from `amont` on purpose. This one may have
//! dependencies; the hook binary may not, because it runs on every commit in
//! every repo. See `scripts/check-no-deps.sh`, which enforces that in CI.
//!
//! This release is the scanner and `--json` only. The TUI will render over
//! exactly this data, so the JSON is the contract rather than a debug
//! afterthought — and it is also the accessible path, since screen readers do
//! not meaningfully work with a TUI.

mod apply;
mod bypasses;
mod checks;
mod downgrades;
mod fix;
mod progress;
mod scan;
mod severities;
mod shim;
mod skips;
mod tui;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// A path, safe to print.
///
/// Every path this tool shows was found by walking directories somebody else
/// owns, and `Path::display()` escapes nothing — a repository or a directory
/// can be named with control bytes in it. `--json` output is deliberately not
/// routed through here: it is a machine contract, and `json.rs` already escapes
/// what JSON requires.
fn shown(p: &Path) -> String {
    amont_runtime::ui::sanitize_path(p)
}

const USAGE: &str = "\
usage: amont-fleet [scan|tui|fix|install|uninstall] [--root <dir>] [--depth <n>] [--json]

  scan           report the fleet (default)
  tui            the interactive dashboard
  install        turn hooks on across the root
  uninstall      take OUR shims back out — never a hook somebody else wrote,
                 and never hook.skip or amont.severity
  fix            show what would be changed — DRY RUN unless --apply
  fix --apply    carry out the plan

  --root <dir>   where to look for repositories (default: $HOME/Developer when it exists)
  --depth <n>    directory levels to descend  (default: 6)
  --binary <p>   the binary shims should point at (default: the amont on PATH, else $HOME/.local/bin/amont)
  --agents-md    with fix/install: also roll out the AGENTS.md pointer
  --remove-unrecognized
                 ALSO delete pre-commit-* / pre-push-* files this tool did not
                 write. Off by default, and read the sentence below first.
  --json         emit the result as JSON

install and fix --apply never delete a hook they did not write: a
pre-commit-* or pre-push-* file without our marker is reported and left
exactly where it is.
";

#[derive(PartialEq)]
enum Mode {
    Scan,
    Fix,
    Tui,
    /// Turn hooks ON across a root. Named after intent, unlike `fix --apply`,
    /// which does the same writing but reads as repair — which is why nobody
    /// reached for it when they meant "set this up".
    Install,
    /// And off again.
    Uninstall,
}

struct Args {
    mode: Mode,
    root: PathBuf,
    depth: usize,
    json: bool,
    apply: bool,
    /// Which binary the shims should point at. Overridable because an install
    /// is not always at the default path, and because a comparison is only
    /// meaningful when both sides agree on the target.
    binary: Option<String>,
    /// Roll the AGENTS.md pointer out alongside fix/install. Opt-in per
    /// invocation, never bundled into `--apply` by default — writing tracked
    /// content across up to 96 repositories is a materially bigger action
    /// than the untracked `.git/hooks` shims apply already writes.
    agents_md: bool,
    /// Also delete `pre-commit-*` / `pre-push-*` files this tool did not write.
    ///
    /// Deliberately NOT called `--remove-stale`, and the naming is the safety
    /// feature. "Stale" already means something exact here: `stale_ours`, the
    /// retired per-check shims that carry OUR marker, which are removed by
    /// default because they are ours to remove. Reusing that word for files
    /// that are NOT ours is precisely what would get somebody to type this
    /// casually — it would read as tidying up after us. It is not: it deletes
    /// hooks other people wrote, in repositories this tool may never have
    /// touched, and the long spelling is there to be read before it is used.
    remove_unrecognized: bool,
}

/// `home` is threaded through rather than read from the environment here, so
/// the no-`$HOME`-and-no-`--root` refusal below is a plain unit test rather
/// than something that can only be exercised by mutating the real process
/// environment (racy, since tests in this binary run in parallel).
fn parse(argv: &[String], home: Option<&Path>) -> Result<Args, String> {
    let mut mode = Mode::Scan;
    let mut root: Option<PathBuf> = None;
    let mut depth = 6;
    let mut json = false;
    let mut apply = false;
    let mut binary: Option<String> = None;
    let mut agents_md = false;
    let mut remove_unrecognized = false;

    let mut it = argv.iter().peekable();
    if let Some(first) = it.peek() {
        match first.as_str() {
            "scan" => {
                mode = Mode::Scan;
                it.next();
            }
            "fix" => {
                mode = Mode::Fix;
                it.next();
            }
            "tui" => {
                mode = Mode::Tui;
                it.next();
            }
            "install" => {
                mode = Mode::Install;
                // Installing IS applying; requiring `--apply` as well would be
                // asking twice for one decision.
                apply = true;
                it.next();
            }
            "uninstall" => {
                mode = Mode::Uninstall;
                apply = true;
                it.next();
            }
            _ => {}
        }
    }
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--root" => {
                root = Some(it.next().ok_or("--root needs a directory")?.into());
            }
            "--depth" => {
                let v = it.next().ok_or("--depth needs a number")?;
                depth = v
                    .parse()
                    .map_err(|_| format!("--depth: {v:?} is not a number"))?;
            }
            "--apply" => apply = true,
            "--agents-md" => agents_md = true,
            "--remove-unrecognized" => remove_unrecognized = true,
            "--binary" => {
                binary = Some(it.next().ok_or("--binary needs a path")?.clone());
            }
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    // Resolved last, so it only runs when no explicit `--root` makes it moot.
    let root = match root {
        Some(r) => r,
        None => default_root(home).ok_or_else(|| {
            "no --root given and no ~/Developer to fall back to — refusing \
             to guess (the alternative is silently scanning, and `fix \
             --apply`ing, from wherever this happened to be launched). \
             Say where the fleet lives: --root <dir>"
                .to_string()
        })?,
    };
    Ok(Args {
        mode,
        root,
        depth,
        json,
        apply,
        binary,
        agents_md,
        remove_unrecognized,
    })
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// The `amont` a shim's PATH fallback would actually execute — absolute, or
/// nothing. This is the truthful bake target: shims resolve their baked
/// path first and PATH last, so baking anything OTHER than the PATH binary
/// makes the two disagree the day one of them upgrades.
fn amont_on_path() -> Option<String> {
    let exe = if cfg!(windows) { "amont.exe" } else { "amont" };
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(exe))
        .find(|candidate| candidate.is_file())
        .map(|candidate| candidate.to_string_lossy().into_owned())
}

/// The binary shims should point at: the one PATH would run, else the
/// install default. The install default alone was a PERSONAL default — it
/// said `~/.local/bin/amont` on a machine whose amont had moved to
/// homebrew's prefix, and every correctly-baked shim then read as a stale
/// bake. Probing PATH is not guessing: it is what execution would do.
///
/// `on_path` is injected rather than read here so the tests are about the
/// precedence, not about what happens to be installed on the test machine.
/// With neither source there is still no good guess and no pretending: a
/// bare relative `"amont"` baked into a shim is a silently broken install —
/// the same failure family as #79 (a wrong path from a missing env var),
/// just on the write side rather than the delete side.
fn default_binary(home: Option<&Path>, on_path: Option<String>) -> Option<String> {
    on_path.or_else(|| home.map(|h| h.join(".local/bin/amont").to_string_lossy().into_owned()))
}

/// `~/Developer` WHEN IT EXISTS — a convention this tool grew up in, kept
/// as a convenience, no longer presented as a fact about every machine. A
/// home without it gets the refusal below rather than a scan of nothing
/// (or worse: `fix --apply` recursing from `.`, which is #79's shape — a
/// surprising path from an absent env var — and why `home: None` must
/// never fall back to the current directory).
fn default_root(home: Option<&Path>) -> Option<PathBuf> {
    home.map(|h| h.join("Developer")).filter(|d| d.is_dir())
}

/// Same disposition as the hook binary's `die_on_sigpipe`, for the same
/// reason: `amont-fleet scan | head` must die quietly, not panic. See the
/// comment there for the full argument.
#[cfg(unix)]
fn die_on_sigpipe() {
    extern "C" {
        fn signal(signum: i32, handler: usize) -> usize;
    }
    const SIGPIPE: i32 = 13;
    const SIG_DFL: usize = 0;
    unsafe {
        signal(SIGPIPE, SIG_DFL);
    }
}

#[cfg(not(unix))]
fn die_on_sigpipe() {}

fn main() -> ExitCode {
    die_on_sigpipe();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // Help and version are inert requests, answered on stdout with exit 0 —
    // asking a program what it is must never be an error. Ahead of parse(),
    // whose `--help` arm predates this and reports usage as a failure.
    if argv.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if argv.iter().any(|a| a == "--version" || a == "-V") {
        println!("amont-fleet {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let args = match parse(&argv, home().as_deref()) {
        Ok(a) => a,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("amont-fleet: {e}");
            }
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    if !args.root.is_dir() {
        eprintln!(
            "amont-fleet: --root {} is not a directory",
            args.root.display()
        );
        return ExitCode::from(2);
    }

    let started = std::time::Instant::now();
    let installed = match args
        .binary
        .clone()
        .or_else(|| default_binary(home().as_deref(), amont_on_path()))
    {
        Some(b) => b,
        None => {
            eprintln!(
                "amont-fleet: no --binary given and $HOME is not set — \
                 refusing to guess which binary the shims should point at"
            );
            return ExitCode::from(2);
        }
    };

    if args.mode == Mode::Tui {
        return match tui::run(args.root.clone(), args.depth, installed.clone()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("amont-fleet: {e}");
                ExitCode::from(1)
            }
        };
    }

    // Say the walk has started, then keep saying where it has got to. The scan
    // spawns git in every repository it finds and, before this existed,
    // printed nothing at all until it was done — which on a fleet-sized tree
    // read as a hang, and "is it doing anything" is the one question a fleet
    // tool must never raise. An announcement alone was not enough: it says the
    // walk STARTED, not that it is still going. See `progress` for the rest,
    // including why every byte of this is gated on a terminal.
    let mut bar = progress::Bar::start(&args.root, args.depth);
    let scan = scan::scan(&args.root, args.depth, &installed, &mut |p| bar.update(p));
    // Before the first line of any report reaches stdout: the two streams share
    // a screen, and a report printed over a live status line interleaves with it.
    bar.finish();
    let elapsed = started.elapsed();

    if args.mode == Mode::Uninstall {
        let mut removed = 0usize;
        let mut repos = 0usize;
        let mut forgotten = 0usize;
        let mut left: Vec<(PathBuf, amont_runtime::hookfile::Refuse)> = Vec::new();
        let mut failed: Vec<(PathBuf, std::io::Error)> = Vec::new();
        for repo in scan.repos.iter().filter(|r| r.managed) {
            // A hooks directory outside the repository, or one git would not
            // name, is not ours to delete from any more than it is ours to
            // write to. Skipped rather than guessed at.
            let Some(hooks) = repo.hooks_dir.inside() else {
                continue;
            };
            let mut result = uninstall_repo(hooks);
            if !result.removed.is_empty() {
                repos += 1;
                removed += result.removed.len();
                println!("  {} {} shims", shown(&repo.path), result.removed.len());
            }
            // Only where shims actually came out: a repository we could not
            // disarm keeps its stamps, because those stamps are still true.
            // Bookkeeping is swept by the RUNTIME's own list, not a second
            // one here that could fall behind it.
            if !result.removed.is_empty() {
                // `repo.path` is relative to the scan root (it is what a human
                // recognises in a report); every git command here needs the
                // real one, or `git -C` resolves against this process's cwd
                // and silently forgets nothing at all.
                let gone =
                    amont_runtime::install::forget_bookkeeping_in(&args.root.join(&repo.path));
                if !gone.is_empty() {
                    forgotten += 1;
                    println!("    forgot {}", gone.join(", "));
                }
            }
            left.append(&mut result.left);
            failed.append(&mut result.failed);
        }
        println!("{removed} shims removed from {repos} repositories");
        if forgotten > 0 {
            // Named, not silent: revoking trust and deleting a stamp ref are
            // real changes to a repository, and the fleet does them 200 times.
            println!(
                "amont's own bookkeeping (stamps, ledgers, trust) forgotten in \
                 {forgotten} repositories"
            );
        }
        // Both blocks are printed even when empty-adjacent, because the whole
        // bug was that neither existed: three different failures collapsed into
        // one silent skip and the summary said `0 shims removed from 0
        // repositories`, exit 0, while four shims sat untouched.
        if !left.is_empty() {
            println!();
            println!("left alone (not ours):");
            // `Refuse::explain` already names the path, and names it with the
            // reason attached — which is the difference between "we skipped
            // something" and "we skipped THIS, because THAT".
            for (_, why) in &left {
                // `explain` sanitizes every borrowed value itself, so its own
                // line breaks survive — see `Refusal::explain`.
                println!("  {}", why.explain());
            }
        }
        if !failed.is_empty() {
            println!();
            println!("FAILED to remove:");
            for (path, e) in &failed {
                println!(
                    "  {}: {}",
                    shown(path),
                    amont_runtime::ui::sanitize(&e.to_string())
                );
            }
        }
        println!("hook.skip and amont.severity were left alone, as was any held work.");
        // A shim that is still installed and still running, after the user
        // asked for it to be gone, is not a success.
        return if failed.is_empty() {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        };
    }

    if args.mode == Mode::Fix || args.mode == Mode::Install {
        // The plan pass asks git, per repository, whether each hook path is
        // tracked before anything may be written — a spawn or two each,
        // which on a fleet is several silent seconds between the scan
        // report above and the first plan or apply line below. Same rule as
        // the scan bar: silence indistinguishable from a hang is not
        // allowed, at any phase.
        let mut steps = progress::Steps::start("planning", scan.repos.len());
        let plans: Vec<fix::FixPlan> = scan
            .repos
            .iter()
            .map(|r| {
                steps.step(&r.path);
                let intent = if args.mode == Mode::Install {
                    fix::Intent::Activate
                } else {
                    fix::Intent::Repair
                };
                fix::plan(
                    r,
                    &args.root.join(&r.path),
                    &installed,
                    intent,
                    args.agents_md,
                    args.remove_unrecognized,
                )
            })
            .collect();
        steps.finish();
        if args.apply {
            // One repository at a time, each line printed the moment its
            // outcome exists. Collecting every report and printing at the end
            // meant a run over dozens of repositories was silent for its
            // whole duration — same hang-shaped silence the scan line above
            // fixes, at the step where files are actually being written.
            let mut reports: Vec<apply::ApplyReport> = Vec::with_capacity(plans.len());
            // Same shape as the planning pass above; the output rule differs
            // by audience. A pipe gets every line, because lines are all a
            // pipe has. A terminal gets the live counter and keeps its
            // scrollback for the exceptional — a wall of identical success
            // lines is how the one FAILED line went unread.
            let mut applying = progress::Steps::start("applying", plans.len());
            for p in &plans {
                applying.step(&p.repo);
                let r = apply::ApplyReport {
                    repo: p.repo.clone(),
                    outcome: apply::apply(p),
                };
                if !args.json {
                    match &r.outcome {
                        apply::Outcome::Failed { .. } => applying.interrupt(&apply_line(&r)),
                        apply::Outcome::Applied { .. } if !applying.is_live() => {
                            use std::io::Write;
                            println!("{}", apply_line(&r));
                            let _ = std::io::stdout().flush();
                        }
                        _ => {}
                    }
                }
                reports.push(r);
            }
            applying.finish();
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&reports).unwrap_or_default()
                );
            } else {
                report_apply_summary(&reports, &plans);
            }
            let failed = reports
                .iter()
                .any(|r| matches!(r.outcome, apply::Outcome::Failed { .. }));
            return if failed {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            };
        }
        if args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&plans).unwrap_or_default()
            );
        } else {
            report_fix(&plans);
        }
        return if scan.looks_like_a_failed_scan() {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        };
    }

    if args.json {
        match serde_json::to_string_pretty(&scan) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("amont-fleet: cannot serialise scan: {e}");
                return ExitCode::from(1);
            }
        }
    } else {
        report(&scan, elapsed);
    }

    // A scan that found nothing exits NON-ZERO. "No repositories" has to be
    // actionable from a script as well as on screen, because the failure this
    // tool exists for is exactly that emptiness reading as success.
    if scan.looks_like_a_failed_scan() {
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

/// What `uninstall` did to one repository's hooks directory, in three lists.
///
/// Three lists because there are three outcomes and the old code had ONE:
///
/// ```text
/// if ours && !is_tracked(&path) && std::fs::remove_file(&path).is_ok() { here += 1; }
/// ```
///
/// "not ours", "tracked", and "the unlink failed" all fell out of that `&&`
/// chain as the same silent skip. VERIFIED: with a read-only `.git/hooks`, this
/// printed `0 shims removed from 0 repositories`, exited 0, and left all four
/// shims installed and running — the repository was not even listed. A tool
/// that reports success for work it did not do is worse than one that fails.
///
/// `left` is not a lesser outcome than `removed`. README promises that a hook
/// somebody else wrote is never taken, so naming what was left is the tool
/// keeping that promise out loud rather than quietly.
struct RepoUninstall {
    removed: Vec<PathBuf>,
    left: Vec<(PathBuf, amont_runtime::hookfile::Refuse)>,
    failed: Vec<(PathBuf, std::io::Error)>,
}

/// Take our four dispatchers back out of one hooks directory.
///
/// `guard_remove(path, expect_ours = true)` decides and `remove_regular`
/// performs, so ownership, tracked-ness, symlinks and file type are all
/// answered by [`amont_runtime::hookfile`] — the same module `install` and
/// `apply` ask, rather than a fourth predicate that can disagree with them.
/// `expect_ours` is the whole point here: `uninstall` removes only files
/// carrying our marker, including a symlink refusal, because deleting a link
/// and deleting the shim it points at are different acts and only one of them
/// was asked for.
fn uninstall_repo(hooks: &Path) -> RepoUninstall {
    let mut out = RepoUninstall {
        removed: Vec::new(),
        left: Vec::new(),
        failed: Vec::new(),
    };
    for name in amont_runtime::install::DISPATCHERS {
        let path = hooks.join(name);
        // Nothing there is nothing to report: `uninstall` run twice must be
        // quiet the second time, not four lines of "left alone".
        if amont_runtime::hookfile::classify(&path) == amont_runtime::hookfile::HookFile::Absent {
            continue;
        }
        match amont_runtime::hookfile::guard_remove(&path, true) {
            Err(refuse) => out.left.push((path, refuse)),
            Ok(()) => match amont_runtime::hookfile::remove_regular(&path) {
                Ok(()) => out.removed.push(path),
                Err(e) => out.failed.push((path, e)),
            },
        }
    }
    out
}

/// Every number carries its denominator. No bare adjectives.
fn report(s: &scan::FleetScan, elapsed: std::time::Duration) {
    if s.looks_like_a_failed_scan() {
        println!("No repositories found under {}", s.root.display());
        println!();
        println!(
            "Visited {} directories in {:.1}s and found 0 git repositories.",
            s.dirs_visited,
            elapsed.as_secs_f64()
        );
        println!("This is a SCAN FAILURE, not a clean fleet.");
        println!(
            "  • is --root correct?       (currently: {})",
            s.root.display()
        );
        println!("  • is --depth deep enough?  (currently: {})", s.depth);
        if s.downgraded_events > 0 {
            println!(
                "  {} problems that did not block across {} repositories",
                s.downgraded_events, s.repos_with_downgrades
            );
        }
        if !s.unreadable.is_empty() {
            println!("  • {} path(s) could not be read", s.unreadable.len());
        }
        return;
    }

    println!("{}", s.root.display());
    println!(
        "  {} git repositories · {} managed · {} unmanaged",
        s.git_dirs_found, s.managed_seen, s.unmanaged_seen
    );
    println!(
        "  {} hook directories · {} directories visited · {} subtrees skipped · {:.1}s",
        s.hook_dirs_seen,
        s.dirs_visited,
        s.excluded_dirs,
        elapsed.as_secs_f64()
    );
    if s.bypassed_commits > 0 {
        // Both numbers, per this function's own rule: a count without its
        // spread reads as one repo's problem when it may be the fleet's.
        println!(
            "  {} unverified commits across {} repositories",
            s.bypassed_commits, s.repos_with_bypasses
        );
    }
    if !s.unreadable.is_empty() {
        println!("  {} unreadable:", s.unreadable.len());
        for p in s.unreadable.iter().take(5) {
            println!("    {}", shown(p));
        }
    }
}

/// Dry-run summary. Counts carry denominators, and a refusal is never folded
/// into the same number as a change.
fn report_fix(plans: &[fix::FixPlan]) {
    let total = plans.len();
    let refused: Vec<&fix::FixPlan> = plans.iter().filter(|p| p.refused()).collect();
    let acting: Vec<&fix::FixPlan> = plans
        .iter()
        .filter(|p| !p.refused() && !p.is_noop())
        .collect();

    for p in &acting {
        println!("{}", shown(&p.repo));
        for r in &p.remove {
            println!("  rm    {}  ({:?})", shown(&r.path), r.reason);
        }
        for w in p.write.iter().filter(|w| w.changes) {
            println!("  write {}", shown(&w.path));
        }
        if let Some(w) = &p.write_agents_md {
            println!("  write {}", shown(&w.path));
        }
    }

    // Warnings are printed for EVERY plan, not only the acting ones. A repo
    // whose sole finding is "there is a hook here we did not write" needs
    // nothing done to it and so appears in none of the three buckets above —
    // which is exactly the repo whose warning would otherwise never be seen.
    report_warnings(plans);

    println!();
    println!(
        "  {} of {} repositories would change · {} refused · {} already correct",
        acting.len(),
        total,
        refused.len(),
        total - acting.len() - refused.len()
    );
    println!(
        "  {} removals · {} writes",
        acting.iter().map(|p| p.remove.len()).sum::<usize>(),
        acting
            .iter()
            .map(|p| p.write.iter().filter(|w| w.changes).count()
                + usize::from(p.write_agents_md.is_some()))
            .sum::<usize>()
    );
    println!();
    println!("  DRY RUN — nothing was written.");
}

/// Every repository we declined to act on, and why.
///
/// A refusal used to be reported as a bare count in the summary line. `1
/// refused` cannot be acted on: it does not say which repository, and it does
/// not distinguish "an application's data repo, correctly left alone" from
/// "git will not talk to this checkout and one `git config` would fix it".
fn report_refusals(plans: &[fix::FixPlan]) {
    let refused: Vec<&fix::FixPlan> = plans.iter().filter(|p| p.refused()).collect();
    if refused.is_empty() {
        return;
    }
    // `Unmanaged` is the overwhelmingly common one and is not news — most of a
    // machine's repositories are somebody else's. Listing ninety of those would
    // bury the four that mean something.
    let interesting: Vec<&fix::FixPlan> = refused
        .iter()
        .copied()
        .filter(|p| p.refuse.iter().any(|r| *r != fix::Refusal::Unmanaged))
        .collect();
    if interesting.is_empty() {
        return;
    }
    // The redirected-hooks refusal comes in fleets: eleven husky repositories
    // are one fact, not eleven four-line paragraphs. Group them by owner,
    // state the cause once, name the repositories compactly, give the remedy
    // once. Everything else stays itemised — those refusals are rare and
    // their explanations are path-specific.
    let mut grouped: Vec<(String, Vec<String>)> = Vec::new();
    let mut singles: Vec<&fix::FixPlan> = Vec::new();
    for p in &interesting {
        let redirected = p.refuse.iter().find_map(|r| match r {
            fix::Refusal::HooksDirRedirected { path } => {
                Some(amont_runtime::install::redirect_culprit(path).unwrap_or("another tool"))
            }
            _ => None,
        });
        // Grouping claims the whole repo only when the redirect is its ONLY
        // interesting refusal — a repo with more to say keeps its paragraph.
        let only = p
            .refuse
            .iter()
            .filter(|r| **r != fix::Refusal::Unmanaged)
            .count()
            == 1;
        match redirected {
            Some(owner) if only => {
                let owner = owner.to_string();
                match grouped.iter_mut().find(|(o, _)| *o == owner) {
                    Some((_, repos)) => repos.push(shown(&p.repo)),
                    None => grouped.push((owner, vec![shown(&p.repo)])),
                }
            }
            _ => singles.push(p),
        }
    }
    println!();
    for (owner, repos) in &grouped {
        println!(
            "{} {} refused — {owner} owns the hooks (core.hooksPath); nothing there was touched",
            amont_runtime::ui::warning_sign(),
            repos.len()
        );
        for line in wrap_list(repos, 92) {
            println!("    {line}");
        }
        println!("    hand dispatch back, per repo: git config --unset core.hooksPath");
    }
    if !singles.is_empty() {
        println!(
            "{} {} refused (nothing in these repositories was touched):",
            amont_runtime::ui::warning_sign(),
            singles.len()
        );
        for p in singles {
            println!("  {}", shown(&p.repo));
            for r in p.refuse.iter().filter(|r| **r != fix::Refusal::Unmanaged) {
                println!("    {}", r.explain());
            }
        }
    }
}

/// Pack names onto lines of at most `width` characters, ` · ` separated —
/// the difference between eleven repository names and eleven paragraphs.
fn wrap_list(names: &[String], width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for name in names {
        match lines.last_mut() {
            Some(last) if last.chars().count() + 3 + name.chars().count() <= width => {
                last.push_str(" · ");
                last.push_str(name);
            }
            _ => lines.push(name.clone()),
        }
    }
    lines
}

/// Everything found but not acted on, named.
///
/// Its own block, and its own heading, because these are the two states this
/// tool used to handle by SILENTLY DOING SOMETHING: deleting a hook somebody
/// else wrote (reported only as a number in `repoC -1 +4`), and writing into a
/// directory a scanned repository named. A count cannot say either of those; a
/// path can.
fn report_warnings(plans: &[fix::FixPlan]) {
    report_refusals(plans);
    // A redirected-hooks warning on a repo whose refusal already said
    // "husky owns the hooks" is the same fact twice — the refusal group
    // covers it. What stays here is what only this section can say: a
    // sub-hook somebody else wrote, or a hooks path outside the repository.
    let mut lines: Vec<String> = Vec::new();
    let mut sub_hooks = 0usize;
    for p in plans.iter().filter(|p| !p.warn.is_empty()) {
        let refused_redirect = p
            .refuse
            .iter()
            .any(|r| matches!(r, fix::Refusal::HooksDirRedirected { .. }));
        for w in &p.warn {
            match w {
                fix::Warning::UnrecognizedSubHook { path } => {
                    sub_hooks += 1;
                    lines.push(format!("  {}  (a hook we did not write)", shown(path)));
                }
                fix::Warning::HooksDirOutsideRepo { path } => {
                    lines.push(format!(
                        "  {}  ({}: core.hooksPath points OUTSIDE the repository)",
                        shown(path),
                        shown(&p.repo)
                    ));
                }
                fix::Warning::HooksDirRedirected { path } => {
                    if refused_redirect {
                        continue;
                    }
                    let owner =
                        amont_runtime::install::redirect_culprit(path).unwrap_or("another tool");
                    lines.push(format!(
                        "  {}  ({}: core.hooksPath — {owner} owns the hooks, amont is not running)",
                        shown(path),
                        shown(&p.repo)
                    ));
                }
            }
        }
    }
    if lines.is_empty() {
        return;
    }
    println!();
    println!("LEFT ALONE (not ours — nothing here is deleted or written):");
    for line in lines {
        println!("{line}");
    }
    if sub_hooks > 0 {
        println!("  (pass --remove-unrecognized to delete the sub-hooks above)");
    }
}

/// What actually happened. A failure is never folded into the same count as a
/// success, and "refused" is reported separately from "unchanged" — they look
/// identical in a total and mean opposite things.
/// One repository's outcome as a line — the piped stream's format,
/// unchanged, so anything grepping `FAILED at` keeps working. Refused and
/// Unchanged never reach the stream; the summary counts them.
fn apply_line(r: &apply::ApplyReport) -> String {
    match &r.outcome {
        apply::Outcome::Applied {
            removed: rm,
            written: wr,
        } => format!("{}  -{rm} +{wr}", shown(&r.repo)),
        apply::Outcome::Failed { error, at } => format!(
            "{}  FAILED at {at}: {}",
            shown(&r.repo),
            amont_runtime::ui::sanitize(error)
        ),
        apply::Outcome::Refused | apply::Outcome::Unchanged => String::new(),
    }
}

fn report_apply_summary(reports: &[apply::ApplyReport], plans: &[fix::FixPlan]) {
    let mut applied = 0usize;
    let (mut removed, mut written, mut refused, mut unchanged) = (0usize, 0usize, 0usize, 0usize);
    for r in reports {
        match &r.outcome {
            apply::Outcome::Applied {
                removed: rm,
                written: wr,
            } => {
                applied += 1;
                removed += rm;
                written += wr;
            }
            apply::Outcome::Refused => refused += 1,
            apply::Outcome::Unchanged => unchanged += 1,
            apply::Outcome::Failed { .. } => {}
        }
    }
    let failures: Vec<&apply::ApplyReport> = reports
        .iter()
        .filter(|r| matches!(r.outcome, apply::Outcome::Failed { .. }))
        .collect();
    let failed = failures.len();
    report_warnings(plans);
    // Failures repeat here, LAST before the counts, on purpose: the stream
    // said it when it happened, but on a fleet the stream has scrolled — the
    // bottom of the report is the one place the eye reliably lands.
    if !failures.is_empty() {
        println!();
        println!("{} {failed} failed:", amont_runtime::ui::error_sign());
        for f in &failures {
            if let apply::Outcome::Failed { error, at } = &f.outcome {
                println!("  {}", shown(&f.repo));
                // The repo is the line above and most errors begin by
                // repeating `at` — trim both, or one failure reads as three
                // copies of the same absolute path.
                let at_short = at
                    .split("/.git/")
                    .nth(1)
                    .map(|tail| format!(".git/{tail}"))
                    .unwrap_or_else(|| at.clone());
                let reason = error
                    .strip_prefix(at.as_str())
                    .map_or(error.as_str(), |t| t.trim_start());
                let reason = reason.strip_prefix("is ").unwrap_or(reason);
                println!(
                    "    at {}: {}",
                    amont_runtime::ui::sanitize(&at_short),
                    amont_runtime::ui::sanitize(reason)
                );
            }
        }
    }
    // The verdict glyph makes the last line scannable: ✗ something failed,
    // ! something was declined, ✓ everything the plan meant to do happened.
    let sign = if failed > 0 {
        amont_runtime::ui::error_sign()
    } else if refused > 0 {
        amont_runtime::ui::warning_sign()
    } else {
        amont_runtime::ui::valid_sign()
    };
    println!();
    println!(
        "{sign} {applied} of {} repositories changed · {refused} refused · {unchanged} already correct · {failed} failed",
        reports.len()
    );
    println!("  {removed} removed · {written} written");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Eleven names, three lines — the whole point over eleven paragraphs.
    /// Width is measured in characters and the separator counts.
    #[test]
    fn wrap_list_packs_names_and_respects_the_width() {
        let names: Vec<String> = (0..11).map(|i| format!("Perso/repo-{i:02}")).collect();
        let lines = wrap_list(&names, 50);
        assert!(lines.len() < names.len(), "{lines:?}");
        for line in &lines {
            assert!(line.chars().count() <= 50, "{line:?}");
        }
        let joined = lines.join(" · ");
        for name in &names {
            assert!(joined.contains(name.as_str()), "{name} lost");
        }
    }

    /// A name longer than the width still gets its own line rather than
    /// being dropped — the wrap never loses a repository.
    #[test]
    fn wrap_list_keeps_an_oversized_name() {
        let names = vec!["a-name-well-beyond-any-reasonable-width-limit-for-a-line".to_string()];
        assert_eq!(wrap_list(&names, 20), names);
    }

    #[test]
    fn parses_flags_and_defaults() {
        // A home whose ~/Developer EXISTS — the default root is adaptive
        // now, and a bare fake path would be the refusal case instead.
        let home = std::env::temp_dir().join(format!("fleet-parse-{}", std::process::id()));
        let _ = std::fs::create_dir_all(home.join("Developer"));
        let a = parse(&[], Some(&home)).expect("defaults");
        assert_eq!(a.depth, 6);
        assert!(!a.json);
        assert_eq!(a.root, home.join("Developer"));

        let a = parse(
            &["--depth".into(), "2".into(), "--json".into()],
            Some(&home),
        )
        .unwrap();
        assert_eq!(a.depth, 2);
        assert!(a.json);

        // And a home WITHOUT one is refused, with the remedy named.
        let bare = Path::new("/home/x");
        let Err(err) = parse(&[], Some(bare)) else {
            panic!("a home without ~/Developer must be refused");
        };
        assert!(err.contains("--root"), "{err}");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Deleting hooks other people wrote is opt-in, and the flag's absence is
    /// what makes it so. Asserted rather than assumed, because the default is
    /// the whole fix.
    #[test]
    fn deleting_other_peoples_hooks_is_off_unless_asked_for() {
        // Explicit --root: this test is about the flag, and the default
        // root is adaptive now (it needs a real ~/Developer).
        let home = Some(Path::new("/home/x"));
        let with_root = |mut v: Vec<String>| {
            v.extend(["--root".to_string(), "/tmp".to_string()]);
            v
        };
        assert!(!parse(&with_root(vec![]), home).unwrap().remove_unrecognized);
        assert!(
            !parse(&with_root(vec!["fix".into(), "--apply".into()]), home)
                .unwrap()
                .remove_unrecognized
        );
        assert!(
            !parse(&with_root(vec!["install".into()]), home)
                .unwrap()
                .remove_unrecognized
        );
        assert!(
            parse(
                &with_root(vec!["fix".into(), "--remove-unrecognized".into()]),
                home
            )
            .unwrap()
            .remove_unrecognized
        );
        // And the usage text has to say both halves out loud.
        assert!(USAGE.contains("--remove-unrecognized"), "{USAGE}");
        assert!(
            USAGE.contains("never delete a hook they did not write"),
            "{USAGE}"
        );
    }

    #[test]
    fn rejects_bad_input_loudly() {
        let home = Some(Path::new("/home/x"));
        assert!(parse(&["--depth".into(), "lots".into()], home).is_err());
        assert!(parse(&["--depth".into()], home).is_err());
        assert!(parse(&["--nope".into()], home).is_err());
    }

    /// The failure this exists to prevent: `$HOME` missing and no `--root`
    /// silently resolving to `.` — `fix --apply`/`install` would then act,
    /// recursively, on whatever directory the process happened to be
    /// launched from. #79 was exactly this shape, on the delete side.
    #[test]
    fn no_home_and_no_root_is_refused_not_guessed() {
        assert!(
            parse(&[], None).is_err(),
            "must refuse rather than default to the current directory"
        );
    }

    /// An explicit `--root` moots the whole question — `$HOME` need not even
    /// exist for this to work.
    #[test]
    fn an_explicit_root_needs_no_home() {
        let a = parse(&["--root".into(), "/somewhere".into()], None).expect("explicit --root");
        assert_eq!(a.root, PathBuf::from("/somewhere"));
    }

    #[test]
    fn default_root_and_binary_need_home() {
        assert_eq!(
            default_root(None),
            None,
            "no directory to guess without $HOME"
        );
        assert_eq!(
            default_binary(None, None),
            None,
            "no binary path to guess without $HOME and nothing on PATH"
        );

        // A home whose ~/Developer does not exist is a home with no default
        // root — the refusal, not a scan of nothing.
        let home = Path::new("/home/x");
        assert_eq!(default_root(Some(home)), None);
        assert_eq!(
            default_binary(Some(home), None),
            Some(home.join(".local/bin/amont").to_string_lossy().into_owned())
        );
    }

    /// The PATH binary outranks the install default: it is the one a shim's
    /// own fallback would execute, so baking anything else makes the two
    /// disagree the day one of them upgrades.
    #[test]
    fn the_path_binary_outranks_the_install_default() {
        let home = Path::new("/home/x");
        assert_eq!(
            default_binary(Some(home), Some("/opt/homebrew/bin/amont".into())),
            Some("/opt/homebrew/bin/amont".into())
        );
        assert_eq!(
            default_binary(None, Some("/usr/local/bin/amont".into())),
            Some("/usr/local/bin/amont".into())
        );
    }

    /// A ~/Developer that exists is still the default root — the convention
    /// is kept, only its universality was retired.
    #[test]
    fn an_existing_developer_dir_is_still_the_default_root() {
        let fake_home = std::env::temp_dir().join(format!("fleet-home-{}", std::process::id()));
        let _ = std::fs::create_dir_all(fake_home.join("Developer"));
        assert_eq!(
            default_root(Some(&fake_home)),
            Some(fake_home.join("Developer"))
        );
        let _ = std::fs::remove_dir_all(&fake_home);
    }
}
