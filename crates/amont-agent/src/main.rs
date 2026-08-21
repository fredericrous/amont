//! `amont-agent` — a guard that inspects a shell command before Claude Code
//! runs it.
//!
//! Amont gates `git commit` and `git push`. Neither can see the command string
//! itself, and some defects live only there: `git push … | tail -5` reports the
//! pipe's exit status, so a rejected push reads as success and no git hook is
//! ever in a position to notice.
//!
//! This binary is not on the commit path and takes dependencies. It is a
//! sibling of `amont-fleet` in that respect: opt-in, installed separately, and
//! free to buy features with crates.
//!
//! ## argv
//!
//! The parse follows `crates/amont/src/main.rs` exactly, including its
//! hardest-won rule: **only position 0 is ever tested against the subcommand
//! table.** Everything after it is data. In `amont` that rule exists because a
//! git remote named `install` once ran the installer mid-push; the same class
//! of accident is available to anything that scans argv for a verb.

mod backtest;
mod civil;
mod corpus;
mod decision;
mod doctor;
mod graduate;
mod hook;
mod journal;
mod payload;
mod rules;
mod settings;
mod shell;
mod stance;
mod transcript;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
usage: amont-agent <command>

  hook                  read a Claude Code payload on stdin, decide
  install [--write]     add the hook to settings.json (prints it by default)
  uninstall [--write]   remove exactly what install added
  status                every rule, its stance, and what it has seen
  doctor                is the guard installed, runnable, and actually firing?
  corpus check          replay every reviewed judgement through the rules
  graduate <rule> --to advise|deny
  demote <rule>         back to observing, no questions asked
  backtest [flags]      replay your transcripts through the rules
  explain <rule>        every match for one rule, for review
  check '<command>'     run the rules over one command, no stdin
  rules                 every rule, its default stance and its evidence

install/uninstall flags:
  --write               actually edit the file
  --reformat            accept a normalised file when we cannot match its style
  --project             .claude/settings.json instead of ~/.claude
  --local               .claude/settings.local.json

backtest/explain flags:
  --transcripts <dir>   where the .jsonl transcripts live
  --since <YYYY-MM-DD>  ignore entries before this day
  --rule <id>           restrict to one rule (repeatable)
  --sample <n>          sample matches to print per rule
  --format cases        emit matches as reviewable case lines (explain only)
  --json                machine-readable output
";

enum Sub {
    Hook,
    Install,
    Uninstall,
    Status,
    Doctor,
    Corpus,
    Graduate,
    Demote,
    Backtest,
    Explain,
    Check,
    Rules,
}

const SUBCOMMANDS: [(&str, Sub); 12] = [
    ("hook", Sub::Hook),
    ("install", Sub::Install),
    ("uninstall", Sub::Uninstall),
    ("status", Sub::Status),
    ("doctor", Sub::Doctor),
    ("corpus", Sub::Corpus),
    ("graduate", Sub::Graduate),
    ("demote", Sub::Demote),
    ("backtest", Sub::Backtest),
    ("explain", Sub::Explain),
    ("check", Sub::Check),
    ("rules", Sub::Rules),
];

enum Invocation {
    Sub { name: Sub, args: Vec<OsString> },
    Help,
    Version,
    Usage(String),
}

fn parse(argv: Vec<OsString>) -> Invocation {
    let Some(first) = argv.first() else {
        return Invocation::Usage("no command given".into());
    };
    let head = first.to_string_lossy().into_owned();
    if head == "--help" || head == "-h" || head == "help" {
        return Invocation::Help;
    }
    if head == "--version" || head == "-V" {
        return Invocation::Version;
    }
    for (name, sub) in SUBCOMMANDS {
        if head == name {
            return Invocation::Sub {
                name: sub,
                args: argv[1..].to_vec(),
            };
        }
    }
    Invocation::Usage(format!("unknown command `{head}`"))
}

fn main() -> ExitCode {
    // Claude Code does not spawn us from git, but a session started inside a
    // git hook still carries GIT_DIR / GIT_WORK_TREE, and those beat
    // `current_dir` for every git command we run. `amont_runtime::git` builds
    // its own Commands and has no seam to pass an environment through, so the
    // scrub has to happen here. This is the same failure the test harness's
    // `strip_git_env` exists to prevent; it committed to the wrong repository
    // once.
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            std::env::remove_var(&key);
        }
    }

    match parse(std::env::args_os().skip(1).collect()) {
        Invocation::Help => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Invocation::Version => {
            println!("amont-agent {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Invocation::Usage(why) => {
            eprintln!("amont-agent: {why}\n");
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
        Invocation::Sub { name, args } => match name {
            // `hook` takes no flags: everything it needs arrives on stdin.
            // Claude Code may pass `--event <name>` for readability, and it is
            // accepted and ignored — the payload names its own event, and
            // trusting argv over the payload would let the two disagree.
            Sub::Hook => {
                let _ = args;
                hook::run()
            }
            Sub::Install => run_install(&args, true),
            Sub::Uninstall => run_install(&args, false),
            Sub::Status => run_status(),
            Sub::Doctor => {
                // Non-zero when the guard is not doing anything, so this is
                // usable from a cron job or a SessionStart check without
                // anybody having to read the text.
                if doctor::report(&doctor::run()) {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                }
            }
            Sub::Corpus => run_corpus(&args),
            Sub::Graduate => run_graduate(&args, true),
            Sub::Demote => run_graduate(&args, false),
            Sub::Backtest => run_backtest(&args, false),
            Sub::Explain => run_backtest(&args, true),
            Sub::Check => run_check(&args),
            Sub::Rules => run_rules(),
        },
    }
}

#[derive(Default)]
struct Flags {
    write: bool,
    reformat: bool,
    project: bool,
    local: bool,
    transcripts: Vec<PathBuf>,
    since: Option<civil::Day>,
    only: Vec<String>,
    sample: Option<usize>,
    json: bool,
    format: Option<String>,
    to: Option<String>,
    force: bool,
    rest: Vec<String>,
}

fn flags(args: &[OsString]) -> Result<Flags, String> {
    let mut f = Flags::default();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].to_string_lossy().into_owned();
        let value = |i: &mut usize, what: &str| -> Result<String, String> {
            *i += 1;
            args.get(*i)
                .map(|v| v.to_string_lossy().into_owned())
                .ok_or_else(|| format!("{what} needs a value"))
        };
        match a.as_str() {
            "--transcripts" => f.transcripts.push(PathBuf::from(value(&mut i, &a)?)),
            "--since" => {
                let v = value(&mut i, &a)?;
                f.since =
                    Some(civil::day_of(&v).ok_or_else(|| format!("--since: `{v}` is not a date"))?);
            }
            "--rule" => f.only.push(value(&mut i, &a)?),
            "--sample" => {
                let v = value(&mut i, &a)?;
                f.sample = Some(
                    v.parse()
                        .map_err(|_| format!("--sample: `{v}` is not a number"))?,
                );
            }
            "--json" => f.json = true,
            "--format" => f.format = Some(value(&mut i, &a)?),
            "--to" => f.to = Some(value(&mut i, &a)?),
            "--force" => f.force = true,
            "--write" => f.write = true,
            "--reformat" => f.reformat = true,
            "--project" => f.project = true,
            "--local" => f.local = true,
            other if other.starts_with('-') => return Err(format!("unknown flag `{other}`")),
            other => f.rest.push(other.to_string()),
        }
        i += 1;
    }
    Ok(f)
}

fn selected(only: &[String]) -> Result<Vec<&'static rules::Rule>, String> {
    if only.is_empty() {
        return Ok(rules::RULES.iter().collect());
    }
    only.iter()
        .map(|id| {
            rules::by_id(id).ok_or_else(|| {
                let known: Vec<&str> = rules::RULES.iter().map(|r| r.id).collect();
                format!("no rule `{id}` — known rules: {}", known.join(", "))
            })
        })
        .collect()
}

fn run_backtest(args: &[OsString], explain: bool) -> ExitCode {
    let mut f = match flags(args) {
        Ok(f) => f,
        Err(why) => {
            eprintln!("amont-agent: {why}");
            return ExitCode::from(2);
        }
    };
    if explain {
        match f.rest.len() {
            1 => f.only.push(f.rest[0].clone()),
            _ => {
                eprintln!("amont-agent: explain needs exactly one rule");
                return ExitCode::from(2);
            }
        }
    }
    let chosen = match selected(&f.only) {
        Ok(c) => c,
        Err(why) => {
            eprintln!("amont-agent: {why}");
            return ExitCode::from(2);
        }
    };
    let roots = match transcript::roots(&f.transcripts) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("amont-agent: {}", e.explain());
            return ExitCode::from(2);
        }
    };
    let scan = transcript::Scan {
        roots: &roots,
        tool: "Bash",
        since: f.since,
    };
    let as_cases = explain && f.format.as_deref() == Some("cases");
    if let Some(other) = f.format.as_deref() {
        if other != "cases" {
            eprintln!("amont-agent: --format takes `cases`, not `{other}`");
            return ExitCode::from(2);
        }
    }
    // A review dump wants everything, not a sample: the point is to look at
    // each match once and decide.
    let samples = f.sample.unwrap_or(if as_cases {
        usize::MAX
    } else if explain {
        40
    } else {
        2
    });
    match backtest::run(&scan, &chosen, samples) {
        Ok(report) => {
            if as_cases {
                // Every line starts unreviewed. The file this is appended to is
                // the same format the reviewer edits in place — one format, so
                // the review is a single pass with no export step to forget.
                println!("{}", corpus::HEADER);
                let mut seen = std::collections::BTreeSet::new();
                for group in &report.samples {
                    for sample in group {
                        if seen.insert(sample.command.clone()) {
                            print!(
                                "{}",
                                corpus::line_for(corpus::Verdict::Unreviewed, &sample.command)
                            );
                        }
                    }
                }
            } else if f.json {
                println!("{}", report.to_json(&roots));
            } else {
                print!("{}", report.render(&roots));
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("amont-agent: {}", e.explain());
            ExitCode::from(2)
        }
    }
}

fn run_check(args: &[OsString]) -> ExitCode {
    let f = match flags(args) {
        Ok(f) => f,
        Err(why) => {
            eprintln!("amont-agent: {why}");
            return ExitCode::from(2);
        }
    };
    if f.rest.len() != 1 {
        eprintln!("amont-agent: check needs exactly one command, quoted");
        return ExitCode::from(2);
    }
    let src = &f.rest[0];
    let parsed = shell::lex(src);
    if let shell::Parsed::Opaque(why) = &parsed {
        println!("no opinion: {}", why.why());
        return ExitCode::SUCCESS;
    }
    let found = rules::examine_all(&parsed);
    if found.is_empty() {
        println!("no rule fires");
        return ExitCode::SUCCESS;
    }
    for (rule, finding) in found {
        println!("{} [{}]", rule.id, rule.default_stance.as_str());
        println!("  {}", finding.reason);
        println!("  → {}", finding.remedy);
        println!(
            "  ▸ {}",
            backtest::excerpt(src, finding.span.start, finding.span.end)
        );
    }
    ExitCode::SUCCESS
}

fn run_rules() -> ExitCode {
    for r in rules::RULES {
        println!(
            "{:<18} {:<8} {:>6.1}/1000  measured {}",
            r.id,
            r.default_stance.as_str(),
            r.evidence.per_1000,
            r.evidence.measured
        );
    }
    ExitCode::SUCCESS
}

fn run_install(args: &[OsString], adding: bool) -> ExitCode {
    let f = match flags(args) {
        Ok(f) => f,
        Err(why) => {
            eprintln!("amont-agent: {why}");
            return ExitCode::from(2);
        }
    };
    let scope = if f.local {
        settings::Scope::ProjectLocal
    } else if f.project {
        settings::Scope::Project
    } else {
        settings::Scope::User
    };
    let project = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let Some(path) = scope.path(&project) else {
        eprintln!("amont-agent: cannot find your Claude Code settings directory");
        return ExitCode::from(2);
    };
    // The absolute path of THIS binary. A `PATH`-resolved command exits 127
    // into Claude Code's non-blocking bucket the moment PATH differs, which
    // disables the guard with nothing to notice it.
    let bin = match std::env::current_exe() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("amont-agent: cannot resolve my own path: {e}");
            return ExitCode::from(2);
        }
    };

    let planned = if adding {
        settings::plan_install(&path, &bin, f.reformat)
    } else {
        settings::plan_uninstall(&path, f.reformat)
    };
    let plan = match planned {
        Ok(p) => p,
        Err(e) => {
            eprintln!("amont-agent: {}", e.explain());
            if adding {
                eprintln!("\n{}", settings::snippet(&bin));
            }
            return ExitCode::from(2);
        }
    };

    if !f.write {
        println!("{}", plan.change.describe(&plan.path));
        println!("Nothing written. Re-run with --write to apply:\n");
        print!("{}", plan.after);
        return ExitCode::SUCCESS;
    }
    if matches!(plan.change, settings::Change::WouldReformat) {
        eprintln!("{}\n", plan.change.describe(&plan.path));
        eprint!("{}", settings::snippet(&bin));
        return ExitCode::from(2);
    }
    match settings::apply(&plan) {
        Ok(()) => {
            println!("{}", plan.change.describe(&plan.path));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("amont-agent: could not write {}: {e}", plan.path.display());
            ExitCode::from(2)
        }
    }
}

fn run_status() -> ExitCode {
    println!("{:<18}{:<10}{:<10}  evidence", "rule", "ships as", "now");
    for r in rules::RULES {
        let now = stance::resolve(r);
        println!(
            "{:<18}{:<10}{:<10}  {:.1}/1000 measured {}",
            r.id,
            r.default_stance.as_str(),
            now.as_str(),
            r.evidence.per_1000,
            r.evidence.measured
        );
    }
    if let Some(p) = journal::path() {
        println!("\njournal: {}", p.display());
    }
    ExitCode::SUCCESS
}

fn run_corpus(args: &[OsString]) -> ExitCode {
    let f = match flags(args) {
        Ok(f) => f,
        Err(why) => {
            eprintln!("amont-agent: {why}");
            return ExitCode::from(2);
        }
    };
    match f.rest.first().map(String::as_str) {
        Some("check") | None => {}
        Some(other) => {
            eprintln!("amont-agent: corpus takes `check`, not `{other}`");
            return ExitCode::from(2);
        }
    }
    let mut healthy = true;
    for rule in rules::RULES {
        let score = corpus::score(rule);
        if score.reviewed == 0 && score.unreviewed == 0 {
            println!("{:<18} no cases yet", rule.id);
            continue;
        }
        let precision = match score.precision() {
            Some(p) => format!("{:.0}%", p * 100.0),
            None => "unmeasured".to_string(),
        };
        println!(
            "{:<18} {} reviewed ({} negative), {} unreviewed, precision {precision}",
            rule.id, score.reviewed, score.negatives, score.unreviewed
        );
        for d in &score.disagreements {
            healthy = false;
            println!(
                "  {}:{} expected {} — {}",
                corpus::path_for(rule.id).display(),
                d.line,
                d.expected.as_str(),
                corpus::escape(&d.command)
            );
        }
    }
    if healthy {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn run_graduate(args: &[OsString], promoting: bool) -> ExitCode {
    let f = match flags(args) {
        Ok(f) => f,
        Err(why) => {
            eprintln!("amont-agent: {why}");
            return ExitCode::from(2);
        }
    };
    let Some(id) = f.rest.first() else {
        eprintln!("amont-agent: name a rule");
        return ExitCode::from(2);
    };
    let Some(rule) = rules::by_id(id) else {
        let known: Vec<&str> = rules::RULES.iter().map(|r| r.id).collect();
        eprintln!("amont-agent: no rule `{id}` — known: {}", known.join(", "));
        return ExitCode::from(2);
    };
    let to = if promoting {
        let Some(to) = f.to.as_deref().and_then(rules::Stance::parse) else {
            eprintln!("amont-agent: graduate needs --to advise|deny");
            return ExitCode::from(2);
        };
        to
    } else {
        rules::Stance::Observe
    };

    let verdict = graduate::assess(rule, to);
    for (ok, line) in &verdict.lines {
        println!(
            "  {} {line}",
            if *ok {
                amont_runtime::ui::valid_sign()
            } else {
                amont_runtime::ui::error_sign()
            }
        );
    }
    if !verdict.allowed && !f.force {
        eprintln!(
            "\nrefusing to move {} to {}: not enough evidence.\n\
             Review its matches first:\n  \
             amont-agent explain {} --format cases >> {}\n\
             then label each line and re-run. `--force` overrides and is recorded as forced.",
            rule.id,
            to.as_str(),
            rule.id,
            corpus::path_for(rule.id).display()
        );
        return ExitCode::from(2);
    }
    match graduate::set(rule, to) {
        Ok(()) => {
            println!(
                "{} is now `{}`{}",
                rule.id,
                to.as_str(),
                if verdict.allowed { "" } else { " (forced)" }
            );
            ExitCode::SUCCESS
        }
        Err(why) => {
            eprintln!("amont-agent: could not record the stance: {why}");
            ExitCode::from(2)
        }
    }
}
