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

  --root <dir>   where to look for repositories (default: $HOME/Developer)
  --depth <n>    directory levels to descend  (default: 6)
  --binary <p>   the binary shims should point at (default: $HOME/.local/bin/amont)
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
            "no --root given and $HOME is not set — refusing to guess \
             (the alternative is silently scanning, and `fix --apply`ing, \
             from wherever this happened to be launched)"
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

/// Where `make install` puts the binary. Shims baked with anything else are
/// reported as stale, which is the GUI-client failure mode.
///
/// `home: None` has no good guess and must not pretend to: a bare relative
/// `"amont"` baked into a shim is a silently broken install — the same
/// failure family as #79 (a wrong path from a missing env var), just on the
/// write side rather than the delete side.
fn default_binary(home: Option<&Path>) -> Option<String> {
    home.map(|h| h.join(".local/bin/amont").to_string_lossy().into_owned())
}

/// `home: None` must not fall back to `.` — `fix --apply`/`install` would
/// then operate, recursively, from wherever the process happened to be
/// launched, with only `is_dir()` standing between that and doing real work.
/// #79 was exactly this: a surprising path from an absent env var.
fn default_root(home: Option<&Path>) -> Option<PathBuf> {
    home.map(|h| h.join("Developer"))
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
        .or_else(|| default_binary(home().as_deref()))
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
            left.append(&mut result.left);
            failed.append(&mut result.failed);
        }
        println!("{removed} shims removed from {repos} repositories");
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
        println!("hook.skip and amont.severity were left alone.");
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
            for p in &plans {
                let r = apply::ApplyReport {
                    repo: p.repo.clone(),
                    outcome: apply::apply(p),
                };
                if !args.json {
                    print_apply_line(&r);
                }
                reports.push(r);
            }
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
    println!();
    println!("REFUSED (nothing in these repositories was touched):");
    for p in interesting {
        println!("  {}", shown(&p.repo));
        for r in p.refuse.iter().filter(|r| **r != fix::Refusal::Unmanaged) {
            println!("    {}", r.explain());
        }
    }
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
    let warned: Vec<&fix::FixPlan> = plans.iter().filter(|p| !p.warn.is_empty()).collect();
    if warned.is_empty() {
        return;
    }
    println!();
    println!("LEFT ALONE (not ours — nothing here is deleted or written):");
    for p in warned {
        for w in &p.warn {
            match w {
                fix::Warning::UnrecognizedSubHook { path } => {
                    println!("  {}  (a hook we did not write)", shown(path));
                }
                fix::Warning::HooksDirOutsideRepo { path } => {
                    println!(
                        "  {}  ({}: core.hooksPath points OUTSIDE the repository)",
                        shown(path),
                        shown(&p.repo)
                    );
                }
                fix::Warning::HooksDirRedirected { path } => {
                    let owner =
                        amont_runtime::install::redirect_culprit(path).unwrap_or("another tool");
                    println!(
                        "  {}  ({}: core.hooksPath — {owner} owns the hooks, amont is not running)",
                        shown(path),
                        shown(&p.repo)
                    );
                }
            }
        }
    }
    println!("  (pass --remove-unrecognized to delete the sub-hooks above)");
}

/// What actually happened. A failure is never folded into the same count as a
/// success, and "refused" is reported separately from "unchanged" — they look
/// identical in a total and mean opposite things.
/// One repository's outcome, printed the moment it happened — streaming is
/// the point, see the apply loop. Refused and Unchanged stay silent here
/// exactly as they always have; the summary counts them. The explicit flush
/// is for a piped stdout, which buffers whole blocks and would otherwise
/// hold the stream back until exit — the same silence this exists to end.
fn print_apply_line(r: &apply::ApplyReport) {
    use std::io::Write;
    match &r.outcome {
        apply::Outcome::Applied {
            removed: rm,
            written: wr,
        } => println!("{}  -{rm} +{wr}", shown(&r.repo)),
        apply::Outcome::Failed { error, at } => println!(
            "{}  FAILED at {at}: {}",
            shown(&r.repo),
            amont_runtime::ui::sanitize(error)
        ),
        apply::Outcome::Refused | apply::Outcome::Unchanged => return,
    }
    let _ = std::io::stdout().flush();
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
    let failed = reports
        .iter()
        .filter(|r| matches!(r.outcome, apply::Outcome::Failed { .. }))
        .count();
    report_warnings(plans);
    println!();
    println!(
        "  {applied} of {} repositories changed · {refused} refused · {unchanged} already correct · {failed} failed",
        reports.len()
    );
    println!("  {removed} removed · {written} written");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flags_and_defaults() {
        let home = Path::new("/home/x");
        let a = parse(&[], Some(home)).expect("defaults");
        assert_eq!(a.depth, 6);
        assert!(!a.json);
        assert_eq!(a.root, home.join("Developer"));

        let a = parse(&["--depth".into(), "2".into(), "--json".into()], Some(home)).unwrap();
        assert_eq!(a.depth, 2);
        assert!(a.json);
    }

    /// Deleting hooks other people wrote is opt-in, and the flag's absence is
    /// what makes it so. Asserted rather than assumed, because the default is
    /// the whole fix.
    #[test]
    fn deleting_other_peoples_hooks_is_off_unless_asked_for() {
        let home = Some(Path::new("/home/x"));
        assert!(!parse(&[], home).unwrap().remove_unrecognized);
        assert!(
            !parse(&["fix".into(), "--apply".into()], home)
                .unwrap()
                .remove_unrecognized
        );
        assert!(
            !parse(&["install".into()], home)
                .unwrap()
                .remove_unrecognized
        );
        assert!(
            parse(&["fix".into(), "--remove-unrecognized".into()], home)
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
            default_binary(None),
            None,
            "no binary path to guess without $HOME"
        );

        let home = Path::new("/home/x");
        assert_eq!(default_root(Some(home)), Some(home.join("Developer")));
        assert_eq!(
            default_binary(Some(home)),
            Some(home.join(".local/bin/amont").to_string_lossy().into_owned())
        );
    }
}
