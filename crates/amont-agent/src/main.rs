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
mod rules;
mod shell;
mod transcript;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
usage: amont-agent <command>

  backtest [flags]      replay your transcripts through the rules
  explain <rule>        every match for one rule, for review
  check '<command>'     run the rules over one command, no stdin
  rules                 every rule, its default stance and its evidence

backtest/explain flags:
  --transcripts <dir>   where the .jsonl transcripts live
  --since <YYYY-MM-DD>  ignore entries before this day
  --rule <id>           restrict to one rule (repeatable)
  --sample <n>          sample matches to print per rule
  --json                machine-readable output
";

enum Sub {
    Backtest,
    Explain,
    Check,
    Rules,
}

const SUBCOMMANDS: [(&str, Sub); 4] = [
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
            Sub::Backtest => run_backtest(&args, false),
            Sub::Explain => run_backtest(&args, true),
            Sub::Check => run_check(&args),
            Sub::Rules => run_rules(),
        },
    }
}

#[derive(Default)]
struct Flags {
    transcripts: Vec<PathBuf>,
    since: Option<civil::Day>,
    only: Vec<String>,
    sample: Option<usize>,
    json: bool,
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
    let samples = f.sample.unwrap_or(if explain { 40 } else { 2 });
    match backtest::run(&scan, &chosen, samples) {
        Ok(report) => {
            if f.json {
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
