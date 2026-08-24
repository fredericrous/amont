//! The guard itself: payload in, decision out.
//!
//! ## The contract
//!
//! **Every failure path exits 0 having written nothing.** Unreadable payload,
//! unknown event, a tool that is not Bash, a command this crate's lexer cannot
//! parse, a working directory that no longer exists, a rule that panics, a
//! journal that cannot be written — all of them are silence.
//!
//! That is not defensiveness, it is the only posture that keeps the guard
//! installed. A hook that fails toward refusing gets in the way of work the
//! author knew was correct, and the fix a person reaches for at that moment is
//! to delete the whole thing from `settings.json`, which switches off every
//! rule at once. A hook that fails toward silence loses one firing.
//!
//! ## The order is a cost decision
//!
//! `examine` runs first and touches nothing. Only if something fires do we pay
//! for `git config` (one process per key) or `confirm` (one process, or a
//! directory read). This runs before every shell command the model issues, so
//! the no-fire path is the path that has to be free.

use std::io::{IsTerminal, Read};
use std::process::ExitCode;

use crate::decision::{self, Decision};
use crate::journal;
use crate::payload::{self, Bash, Event, Session};
use crate::rules::{self, Confirmed, Context, Finding, Rule, Stance};
use crate::shell::{self, Parsed};

pub fn run() -> ExitCode {
    // A person typing `amont-agent hook` with no payload would otherwise block
    // on stdin forever with no indication why.
    if std::io::stdin().is_terminal() {
        eprintln!(
            "amont-agent: `hook` reads a Claude Code payload on stdin.\n\
             Try `amont-agent check '<command>'` to test a command by hand."
        );
        return ExitCode::from(2);
    }

    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        return Decision::Silent.emit();
    }

    // A panic anywhere below is a bug in this crate, and a bug in this crate
    // must not become a refused command or a broken session.
    let decided = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decide(&raw)));
    match decided {
        Ok(d) => d.emit(),
        Err(_) => {
            eprintln!("amont-agent: internal error; allowing the command through");
            Decision::Silent.emit()
        }
    }
}

fn decide(raw: &str) -> Decision {
    match payload::parse(raw) {
        Event::SessionStart(session) => {
            // The fact that we ran is what `doctor` uses to tell "no rule
            // fired" apart from "the guard is dead". Written first, so a slow
            // fetch below can never cost the heartbeat.
            heartbeat();
            on_session_start(&session)
        }
        Event::NotOurs => Decision::Silent,
        Event::PreBash(bash) => on_bash(&bash),
    }
}

/// Where the checkout stands against the remote, stated once per session.
///
/// Governed by the `stale-base` rule's stance, so one key silences both the
/// notice and the branch-creation rule: `observe` measures and journals but
/// says nothing; anything above it speaks. There is nothing to refuse at a
/// session opening, so `deny` speaks exactly like `advise` here.
fn on_session_start(session: &Session) -> Decision {
    if !session.cwd.is_dir() {
        return Decision::Silent;
    }
    let rule = &rules::stale_base::RULE;
    let stance = crate::stance::resolve(rule);
    let Some(drift) = crate::stale::measure(&session.cwd, "HEAD") else {
        return Decision::Silent;
    };
    if drift.behind == 0 {
        return Decision::Silent;
    }
    let outcome = match stance {
        Stance::Observe => "watched",
        Stance::Advise | Stance::Deny => "advised",
    };
    journal::record(&journal::Entry {
        rule: rule.id,
        stance: stance.as_str(),
        outcome,
        session: &session.session,
        repo: &drift.repo,
        mode: "-",
        excerpt: &format!("session start: {} behind {}", drift.behind, drift.base),
    });
    match stance {
        Stance::Observe => Decision::Silent,
        Stance::Advise | Stance::Deny => Decision::Context(format!(
            "amont-agent/{}: {}",
            rule.id,
            crate::stale::notice(&drift)
        )),
    }
}

fn on_bash(bash: &Bash) -> Decision {
    let parsed = shell::lex(&bash.command);
    if matches!(parsed, Parsed::Opaque(_)) {
        return Decision::Silent;
    }

    let fired = rules::examine_all(&parsed);
    if fired.is_empty() {
        // The whole no-fire path: one lex, no processes, no files.
        return Decision::Silent;
    }

    let mut deny: Vec<String> = Vec::new();
    let mut advise: Vec<String> = Vec::new();

    for (rule, finding) in &fired {
        let stance = crate::stance::resolve(rule);
        if !confirmed(rule, finding, bash, &parsed) {
            note(rule, "unconfirmed", "skipped", bash, finding);
            continue;
        }
        let text = decision::phrase(rule.id, &finding.reason, &finding.remedy);
        match stance {
            Stance::Observe => note(rule, "observe", "watched", bash, finding),
            Stance::Advise => {
                note(rule, "advise", "advised", bash, finding);
                advise.push(text);
            }
            Stance::Deny => {
                note(rule, "deny", "denied", bash, finding);
                deny.push(text);
            }
        }
    }

    // A refusal outranks advice: there is no point advising about a command
    // that is not going to run. The advisory findings are still journalled.
    if !deny.is_empty() {
        Decision::Deny(deny.join("\n\n"))
    } else if !advise.is_empty() {
        Decision::Advise(advise.join("\n\n"))
    } else {
        Decision::Silent
    }
}

/// A rule with no `confirm` is confirmed. A `confirm` that cannot answer is
/// NOT — failing to establish the fact is silence, like everything else here.
fn confirmed(rule: &Rule, finding: &Finding, bash: &Bash, parsed: &Parsed) -> bool {
    let Some(confirm) = rule.confirm else {
        return true;
    };
    if !bash.cwd.is_dir() {
        return false;
    }
    let ctx = Context {
        cwd: &bash.cwd,
        parsed,
    };
    matches!(confirm(&ctx, finding), Confirmed::Yes)
}

fn note(rule: &Rule, stance: &str, outcome: &str, bash: &Bash, finding: &Finding) {
    let excerpt = crate::backtest::excerpt(&bash.command, finding.span.start, finding.span.end);
    journal::record(&journal::Entry {
        rule: rule.id,
        stance,
        outcome,
        session: &bash.session,
        repo: &repo_name(&bash.cwd),
        mode: &bash.permission_mode,
        excerpt: &excerpt,
    });
}

/// The basename of the repository, not its path. Enough to group firings by
/// project without writing `/Users/<name>/…` into a log file.
fn repo_name(cwd: &std::path::Path) -> String {
    let mut dir = cwd;
    loop {
        if dir.join(".git").exists() {
            break;
        }
        match dir.parent() {
            Some(p) => dir = p,
            None => break,
        }
    }
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "-".to_string())
}

/// One line, rewritten by rename so it is always current and always whole.
/// `doctor` compares it against the newest transcript timestamp: transcripts
/// prove sessions happened, this proves the guard ran in one.
fn heartbeat() {
    let Some(dir) = journal::dir() else { return };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let tmp = dir.join("heartbeat.new");
    if std::fs::write(&tmp, format!("{now} {}\n", env!("CARGO_PKG_VERSION"))).is_ok() {
        let _ = std::fs::rename(&tmp, dir.join("heartbeat"));
    }
}
