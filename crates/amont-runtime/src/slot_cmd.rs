//! `amont slot run` and `amont slots` (ADR-0014): the host slots for any
//! command, and who holds them.
//!
//! `amont slot run [--weight N] [--max-wait S] [--on-timeout run|exit] -- <cmd…>`
//! waits for a slot, runs the command with `AMONT_HOST_SLOT=held`, passes
//! its exit status through and frees the slot when the command exits. The
//! wrapper alone holds the slot: its descriptors are close-on-exec, so a
//! daemon the command leaves behind never keeps it. A wait longer than
//! `--max-wait` (default 90 s, under Claude Code's 120 s Bash timeout) exits
//! 75 without running anything, unless `--on-timeout run` says otherwise.

use std::ffi::OsString;
use std::process::Command;
use std::time::{Duration, Instant};

/// Exit status for "the host stayed busy; nothing ran" (EX_TEMPFAIL).
pub const BUSY_EXIT: i32 = 75;

/// The default `--max-wait`: under Claude Code's 120 s Bash timeout, so a
/// wrapped command is never killed while it is still queued.
pub const DEFAULT_MAX_WAIT: Duration = Duration::from_secs(90);

/// `amont slot <args>`.
pub fn slot_command(args: &[OsString]) -> i32 {
    let (own, cmd) = split_at_dashes(args);
    match own.first().and_then(|a| a.to_str()) {
        Some("run") => run(&own[1..], cmd),
        _ => {
            eprintln!("amont: usage: amont slot run [--weight N] [--max-wait S] [--on-timeout run|exit] -- <command…>");
            2
        }
    }
}

/// The arguments before `--`, and the command after it.
pub fn split_at_dashes(args: &[OsString]) -> (&[OsString], &[OsString]) {
    match args.iter().position(|a| a == "--") {
        Some(i) => (&args[..i], &args[i + 1..]),
        None => (args, &[]),
    }
}

/// The flags `slot run` takes, each with a value.
const VALUED: &[&str] = &["--weight", "--max-wait", "--on-timeout"];

/// Read `own` (the words before `--`) into flag values, as `--flag value`
/// or `--flag=value`. Anything else is a usage error.
fn parse(own: &[OsString]) -> Result<Vec<(&'static str, String)>, String> {
    let mut out = Vec::new();
    let mut words = own.iter();
    while let Some(word) = words.next() {
        let word = word.to_str().ok_or("an argument is not valid UTF-8")?;
        let (name, inline) = match word.split_once('=') {
            Some((n, v)) => (n, Some(v.to_string())),
            None => (word, None),
        };
        let Some(flag) = VALUED.iter().find(|f| **f == name) else {
            return Err(format!("unknown argument {word:?}"));
        };
        let value = match inline {
            Some(v) => v,
            None => words
                .next()
                .and_then(|v| v.to_str())
                .ok_or(format!("{flag} needs a value"))?
                .to_string(),
        };
        out.push((*flag, value));
    }
    Ok(out)
}

fn value<'a>(flags: &'a [(&'static str, String)], flag: &str) -> Option<&'a str> {
    flags
        .iter()
        .find(|(f, _)| *f == flag)
        .map(|(_, v)| v.as_str())
}

/// How the command runs: inside a held slot, or with none.
#[derive(Clone, Copy)]
enum Launch {
    /// A slot is held for it (here or by an ancestor): `AMONT_HOST_SLOT=held`.
    Held,
    /// No slot: off, unsupported, unavailable, or `--on-timeout run`.
    Unqueued,
}

fn run(own: &[OsString], cmd: &[OsString]) -> i32 {
    let own = match parse(own) {
        Ok(flags) => flags,
        Err(why) => {
            eprintln!("amont: slot run: {why}");
            return 2;
        }
    };
    let own = own.as_slice();
    let Some((program, rest)) = cmd.split_first() else {
        eprintln!("amont: `amont slot run` needs a command after `--`");
        return 2;
    };
    let weight = match value(own, "--weight").map(str::parse::<u32>) {
        None => 1,
        Some(Ok(n)) if n >= 1 => n,
        Some(_) => {
            eprintln!("amont: --weight is a whole number from 1");
            return 2;
        }
    };
    let max_wait = match value(own, "--max-wait").map(str::parse::<u64>) {
        None => DEFAULT_MAX_WAIT,
        Some(Ok(s)) => Duration::from_secs(s),
        Some(Err(_)) => {
            eprintln!("amont: --max-wait is a whole number of seconds");
            return 2;
        }
    };
    let run_on_timeout = match value(own, "--on-timeout") {
        None | Some("exit") => false,
        Some("run") => true,
        Some(other) => {
            eprintln!("amont: --on-timeout is `run` or `exit`, not {other:?}");
            return 2;
        }
    };
    let label: String = std::iter::once(program)
        .chain(rest.iter().take(2))
        .map(|a| a.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ");

    // Already inside a slot (a wrapped command running this one): no second
    // queue for the slot the parent holds.
    let nested = std::env::var_os(amont_slots::HELD_ENV).is_some_and(|v| !v.is_empty());
    let settings = crate::config::Settings::default();
    let sizing = crate::host_slots::sizing(&settings);
    let (Some(sizing), false, true) = (sizing, nested, amont_slots::SUPPORTED) else {
        return spawn(
            program,
            rest,
            if nested {
                Launch::Held
            } else {
                Launch::Unqueued
            },
        );
    };

    let dir = amont_slots::dir();
    let started = Instant::now();
    let mut waiter = match amont_slots::Waiter::new(
        sizing,
        weight,
        amont_slots::Holder::here("amont slot run", &label),
    ) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("amont: host slots unavailable ({e}); running unqueued");
            return spawn(program, rest, Launch::Unqueued);
        }
    };
    let mut last_said: Option<Instant> = None;
    let lease = loop {
        match waiter.poll() {
            Ok(amont_slots::Poll::Granted(lease)) => break Some(lease),
            Ok(amont_slots::Poll::Waiting(queue)) => {
                let due = last_said.map_or(true, |t| t.elapsed() >= Duration::from_secs(30));
                if due && (!queue.holders.is_empty() || queue.unknown > 0) {
                    eprintln!("amont: {}", crate::host_slots::behind(&queue));
                    last_said = Some(Instant::now());
                }
            }
            Err(e) => {
                eprintln!("amont: host slots unavailable ({e}); running unqueued");
                return spawn(program, rest, Launch::Unqueued);
            }
        }
        if started.elapsed() >= max_wait {
            break None;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    drop(waiter);
    let waited = started.elapsed().as_millis();
    let Some(lease) = lease else {
        if run_on_timeout {
            amont_slots::record_wait(&dir, "amont slot run", weight, waited, "unqueued");
            eprintln!(
                "amont: no host slot freed in {}s; running unqueued (--on-timeout run)",
                max_wait.as_secs()
            );
            return spawn(program, rest, Launch::Unqueued);
        }
        amont_slots::record_wait(&dir, "amont slot run", weight, waited, "timeout");
        eprintln!(
            "amont: no host slot freed in {}s; `{label}` did not run (exit {BUSY_EXIT}). \
             Retry the same command later, or let CI run it.",
            max_wait.as_secs()
        );
        return BUSY_EXIT;
    };
    amont_slots::record_wait(&dir, "amont slot run", weight, waited, "admitted");
    let code = spawn(program, rest, Launch::Held);
    drop(lease);
    code
}

/// Run the command, inheriting stdio, and pass its exit status through: the
/// code, or 128 + the signal that ended it.
fn spawn(program: &OsString, rest: &[OsString], launch: Launch) -> i32 {
    let mut cmd = Command::new(program);
    cmd.args(rest);
    match launch {
        Launch::Held => {
            cmd.env(amont_slots::HELD_ENV, "held");
        }
        Launch::Unqueued => {}
    }
    match cmd.status() {
        Ok(status) => exit_code(status),
        Err(e) => {
            eprintln!("amont: cannot run {}: {e}", program.to_string_lossy());
            127
        }
    }
}

fn exit_code(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }
    1
}

/// `amont slots [--json]`.
pub fn slots_command(args: &[OsString]) -> i32 {
    if let Some(bad) = args.iter().find(|a| *a != "--json") {
        eprintln!("amont: slots: unknown argument {:?}", bad.to_string_lossy());
        return 2;
    }
    let json = !args.is_empty();
    let settings = crate::config::Settings::default();
    let Some(sizing) = crate::host_slots::sizing(&settings) else {
        if json {
            println!("{{\"cap\":0,\"held\":0,\"holders\":[],\"unknown\":0,\"waiting\":0}}");
        } else {
            println!("host slots are off (amont.hostSlots 0)");
        }
        return 0;
    };
    let status = match amont_slots::status(sizing) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("amont: cannot read the host slots: {e}");
            return 1;
        }
    };
    let held = status.holders.len() + status.unknown;
    let mode = match sizing {
        amont_slots::Sizing::Auto => format!(
            "auto: load {}, {} cores",
            amont_slots::load1().map_or("?".to_string(), |l| format!("{l:.1}")),
            amont_slots::cores()
        ),
        amont_slots::Sizing::Fixed(n) => format!("amont.hostSlots {n}"),
    };
    if json {
        let holders: Vec<String> = status
            .holders
            .iter()
            .map(amont_slots::Holder::to_json)
            .collect();
        println!(
            "{{\"cap\":{},\"held\":{held},\"holders\":[{}],\"unknown\":{},\"waiting\":{}}}",
            status.cap,
            holders.join(","),
            status.unknown,
            status.waiting
        );
        return 0;
    }
    let now = amont_slots::now_ms();
    println!("host slots: {held} of {} held ({mode})", status.cap);
    for h in &status.holders {
        println!("  {}  pid {}", h.describe(now), h.pid);
    }
    if status.unknown > 0 {
        println!("  {} unknown (old client)", status.unknown);
    }
    println!("waiting: {}", status.waiting);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    #[test]
    fn flags_take_a_separate_or_an_inline_value_and_nothing_else() {
        let flags = parse(&os(&["--weight", "2", "--on-timeout=run"])).unwrap();
        assert_eq!(value(&flags, "--weight"), Some("2"));
        assert_eq!(value(&flags, "--on-timeout"), Some("run"));
        assert!(parse(&os(&["--wieght", "2"]))
            .unwrap_err()
            .contains("unknown"));
        assert!(parse(&os(&["extra"])).unwrap_err().contains("unknown"));
        assert!(parse(&os(&["--max-wait"]))
            .unwrap_err()
            .contains("needs a value"));
    }

    #[test]
    fn the_command_is_everything_after_the_first_dashes() {
        let args = os(&[
            "run",
            "--weight",
            "2",
            "--",
            "cargo",
            "test",
            "--",
            "--nocapture",
        ]);
        let (own, cmd) = split_at_dashes(&args);
        assert_eq!(own, &os(&["run", "--weight", "2"])[..]);
        assert_eq!(cmd, &os(&["cargo", "test", "--", "--nocapture"])[..]);
    }
}
