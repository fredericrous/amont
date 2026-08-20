//! `amont setup` — choose what `commit-msg` enforces, once.
//!
//! The keys in [`crate::commit_style`] are the answer to "these defaults do not
//! suit me". This is the answer to "I did not know there were any". An adoption
//! feature is only half built if the dial exists and nobody finds it, so the
//! same four settings are offered here, each with its current value as the
//! default and one line saying what it is for.
//!
//! **Input is stdin, not `/dev/tty`.** `trust::confirm` opens `/dev/tty`
//! because it prompts from inside a *hook*, where git owns stdin and reading it
//! would consume a pre-push ref list. That reasoning does not reach here: this
//! is a subcommand somebody typed, and its stdin IS the terminal. The general
//! rule, worth stating once: `/dev/tty` for a prompt inside a hook, stdin for a
//! prompt inside a subcommand. Reading stdin also works on Windows — where
//! `confirm` is hard-coded to decline — and can be driven by a heredoc, which
//! is how this is tested without a pty.
//!
//! Nothing is written until every question has been answered, and what does get
//! written is printed as the `git config` commands that would produce it. A
//! wizard that reports "saved" has told you nothing you can check, paste into
//! your dotfiles, or hand to a teammate.

use crate::commit_style::{
    self, Gitmoji, Style, KEY_BODY_WRAP, KEY_DESCRIPTION_MAX, KEY_GITMOJI, KEY_SUBJECT_MAX,
};
use crate::git;
use crate::ui::highlight;
use std::io::{BufRead, IsTerminal, Write};

/// Where the answers are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Where {
    Local,
    Global,
}

impl Where {
    fn flag(self) -> &'static str {
        match self {
            Where::Local => "--local",
            Where::Global => "--global",
        }
    }
    fn word(self) -> &'static str {
        match self {
            Where::Local => "local",
            Where::Global => "global",
        }
    }
}

/// One decided setting: the key, and the value to write — or `None` to unset,
/// which is how a setting returns to the shipped default.
struct Answer {
    key: &'static str,
    value: Option<String>,
    changed: bool,
}

pub fn command(args: &[std::ffi::OsString]) -> Result<(), String> {
    // INVARIANT: policy installed immediately after every manifest::load —
    // setup DISPLAYS effective values, and without the policy it would show
    // pre-policy state as "current" and offer to re-write it. What setup
    // WRITES is --local/--global git config, so the ladder stays correct
    // either way; only the display needed the truth.
    if let Ok(root) = crate::hooks::common::repo_root_checked() {
        let manifest = crate::manifest::load(std::path::Path::new(&root));
        crate::policy::install(manifest.policy.clone());
    }
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let asked_local = args.iter().any(|a| a == "--local");
    let asked_global = args.iter().any(|a| a == "--global");
    if asked_local && asked_global {
        return Err("amont setup: --local and --global contradict each other".to_string());
    }
    if let Some(bad) = args.iter().find(|a| {
        !matches!(
            a.to_str(),
            Some("--dry-run") | Some("--local") | Some("--global")
        )
    }) {
        return Err(format!(
            "amont setup: unknown argument {:?}\nusage: amont setup [--local|--global] [--dry-run]",
            bad.to_string_lossy()
        ));
    }

    // `--local` needs a repository, and refusing beats falling back to `.` —
    // the same reasoning `trust::command` records: a fallback would write a
    // setting into whatever directory somebody happened to be standing in.
    if asked_local && git::stdout(&["rev-parse", "--show-toplevel"]).is_none() {
        return Err("amont setup --local: not inside a git repository".to_string());
    }

    let (style, _) = commit_style::describe();

    if !std::io::stdin().is_terminal() {
        return offer_the_commands(
            &style,
            if asked_local {
                Where::Local
            } else {
                Where::Global
            },
        );
    }

    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let forced = match (asked_local, asked_global) {
        (true, _) => Some(Where::Local),
        (_, true) => Some(Where::Global),
        _ => None,
    };
    match ask_all(&mut input, &style, forced)? {
        Some((scope, answers)) => apply(&answers, scope, dry_run),
        None => quit(),
    }
}

/// Every question, in order. `None` means the reader quit and nothing should be
/// written.
///
/// Takes its input as a `BufRead` rather than reaching for stdin itself, which
/// is what lets the whole flow — defaults, re-asking on a bad answer, quitting
/// — be tested without a pseudo-terminal.
fn ask_all(
    input: &mut impl BufRead,
    style: &Style,
    forced_scope: Option<Where>,
) -> Result<Option<(Where, Vec<Answer>)>, String> {
    println!("amont setup — what `commit-msg` enforces, and how it decorates.");
    println!("Nothing here disables a check; it changes what the check asks for.");
    println!("Enter keeps the current value. `q` quits without writing anything.");

    let scope = match forced_scope {
        Some(s) => s,
        None => match ask_scope(input)? {
            Some(s) => s,
            None => return Ok(None),
        },
    };

    let mut answers = Vec::new();
    match ask_gitmoji(input, style.gitmoji)? {
        Some(a) => answers.push(a),
        None => return Ok(None),
    }
    for (key, label, why, current, default) in [
        (
            KEY_SUBJECT_MAX,
            "Maximum length of the whole subject line",
            "72 is git's own convention, and what `git log --oneline` fits",
            style.subject_max,
            commit_style::DEFAULT_SUBJECT_MAX,
        ),
        (
            KEY_DESCRIPTION_MAX,
            "Maximum length of the description, after the `type: `",
            "50 is the strict end of the convention; 68 still fits a 72-column \
             subject with a short type and no scope",
            style.description_max,
            commit_style::DEFAULT_DESCRIPTION_MAX,
        ),
        (
            KEY_BODY_WRAP,
            "Hard-wrap the body at how many columns",
            "0 leaves the body exactly as written — what keeps a pasted stack \
             trace or a fenced code block intact",
            style.body_wrap,
            commit_style::DEFAULT_BODY_WRAP,
        ),
    ] {
        match ask_number(input, key, label, why, current, default)? {
            Some(a) => answers.push(a),
            None => return Ok(None),
        }
    }

    Ok(Some((scope, answers)))
}

fn quit() -> Result<(), String> {
    println!("\nnothing written.");
    Ok(())
}

/// Write the answers, and print exactly what was written.
fn apply(answers: &[Answer], scope: Where, dry_run: bool) -> Result<(), String> {
    let changed: Vec<&Answer> = answers.iter().filter(|a| a.changed).collect();
    println!();
    if changed.is_empty() {
        println!("nothing to change — every setting is already what you chose.");
    } else {
        println!(
            "{} ({}):",
            if dry_run { "would write" } else { "wrote" },
            scope.word()
        );
        for a in &changed {
            match &a.value {
                Some(v) => println!("  git config {} {} {v}", scope.flag(), a.key),
                // Unsetting is how a setting goes back to the shipped default,
                // which is a state `git config <key> <default>` cannot express:
                // one is "I chose this", the other is "I have no opinion".
                None => println!("  git config {} --unset {}", scope.flag(), a.key),
            }
        }
        if !dry_run {
            for a in &changed {
                let ok = match &a.value {
                    Some(v) => git::succeeds(&["config", scope.flag(), a.key, v]),
                    None => {
                        // Exit 5 is "nothing to unset", which is success here.
                        git::succeeds(&["config", scope.flag(), "--unset", a.key])
                            || matches!(
                                git::output(&["config", scope.flag(), "--get", a.key]),
                                Some(o) if o.code == 1
                            )
                    }
                };
                if !ok {
                    return Err(format!("amont setup: could not write {}", a.key));
                }
            }
        }
    }

    let unchanged: Vec<&Answer> = answers.iter().filter(|a| !a.changed).collect();
    if !unchanged.is_empty() {
        println!("\nunchanged:");
        for a in unchanged {
            println!("  {}", a.key);
        }
    }

    println!("\nTwo more, per repository rather than per person:");
    println!("  git config amont.fix true             # let a check fix what it finds");
    println!("  git config amont.testPushedTree true  # test what you push, not your tree");
    println!("\nRead it all back with:  amont list");
    Ok(())
}

/// Not a terminal: print the keys and their current values as commands, and
/// exit **0**.
///
/// `amont setup > setup.sh` in a provisioning script should produce
/// something usable rather than an error. The refusal goes to stderr so it does
/// not end up in that file.
fn offer_the_commands(style: &Style, scope: Where) -> Result<(), String> {
    eprintln!("amont setup: not a terminal — nothing to ask. The keys, with their current values:");
    println!(
        "git config {} {KEY_GITMOJI} {}",
        scope.flag(),
        style.gitmoji.as_str()
    );
    println!(
        "git config {} {KEY_SUBJECT_MAX} {}",
        scope.flag(),
        style.subject_max
    );
    println!(
        "git config {} {KEY_DESCRIPTION_MAX} {}",
        scope.flag(),
        style.description_max
    );
    println!(
        "git config {} {KEY_BODY_WRAP} {}",
        scope.flag(),
        style.body_wrap
    );
    Ok(())
}

/// Read one answer. `None` means quit — `q`, or end of input.
fn prompt(input: &mut impl BufRead, question: &str, why: &str, current: &str) -> Option<String> {
    println!("\n{question}");
    if !why.is_empty() {
        println!("  {why}");
    }
    print!("  [{}] > ", highlight(current));
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if input.read_line(&mut line).ok()? == 0 {
        return None; // EOF
    }
    let answer = line.trim().to_string();
    if answer.eq_ignore_ascii_case("q") {
        return None;
    }
    Some(answer)
}

fn ask_scope(input: &mut impl BufRead) -> Result<Option<Where>, String> {
    loop {
        let Some(answer) = prompt(
            input,
            "Where should these settings go?",
            "global is usually right — how you write commit messages is the same \
             statement in every repository you have",
            "global",
        ) else {
            return Ok(None);
        };
        match answer.as_str() {
            "" | "global" => return Ok(Some(Where::Global)),
            "local" => {
                if git::stdout(&["rev-parse", "--show-toplevel"]).is_none() {
                    println!("  not inside a git repository — `local` has nowhere to go.");
                    continue;
                }
                return Ok(Some(Where::Local));
            }
            other => println!("  {other:?} is neither `global` nor `local`."),
        }
    }
}

fn ask_gitmoji(input: &mut impl BufRead, current: Gitmoji) -> Result<Option<Answer>, String> {
    println!("\nWhere should the type's gitmoji go?");
    for g in Gitmoji::ALL {
        println!("  {:<9} {:<22} {}", g.as_str(), g.example(), g.explain());
    }
    loop {
        print!("  [{}] > ", highlight(current.as_str()));
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if input.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Ok(None);
        }
        let answer = line.trim();
        if answer.eq_ignore_ascii_case("q") {
            return Ok(None);
        }
        let chosen = if answer.is_empty() {
            current
        } else {
            match Gitmoji::parse(&answer.to_ascii_lowercase()) {
                Some(g) => g,
                None => {
                    println!("  {answer:?} is not one of the four above.");
                    continue;
                }
            }
        };
        return Ok(Some(answer_for(
            KEY_GITMOJI,
            chosen.as_str().to_string(),
            commit_style::DEFAULT_GITMOJI.as_str().to_string(),
            current.as_str().to_string(),
        )));
    }
}

fn ask_number(
    input: &mut impl BufRead,
    key: &'static str,
    label: &str,
    why: &str,
    current: usize,
    default: usize,
) -> Result<Option<Answer>, String> {
    loop {
        let Some(answer) = prompt(input, label, why, &current.to_string()) else {
            return Ok(None);
        };
        let chosen = if answer.is_empty() {
            current
        } else {
            match answer.parse::<usize>() {
                Ok(n) => n,
                Err(_) => {
                    println!("  {answer:?} is not a number.");
                    continue;
                }
            }
        };
        return Ok(Some(answer_for(
            key,
            chosen.to_string(),
            default.to_string(),
            current.to_string(),
        )));
    }
}

/// A chosen value, as a write instruction.
///
/// Choosing the shipped default writes an **unset**, not the value: the two
/// are different statements, and a config file full of keys set to what they
/// already were is noise somebody else has to read.
fn answer_for(key: &'static str, chosen: String, default: String, current: String) -> Answer {
    let changed = chosen != current;
    Answer {
        key,
        value: if chosen == default {
            None
        } else {
            Some(chosen)
        },
        changed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Choosing the default means the key goes away, not that it gets written
    /// with a value equal to the default.
    #[test]
    fn choosing_the_default_unsets_the_key() {
        let a = answer_for(KEY_SUBJECT_MAX, "72".into(), "72".into(), "100".into());
        assert!(a.value.is_none(), "should unset");
        assert!(a.changed);

        let b = answer_for(KEY_SUBJECT_MAX, "100".into(), "72".into(), "72".into());
        assert_eq!(b.value.as_deref(), Some("100"));
        assert!(b.changed);
    }

    /// Answering with what is already in effect is not a change, so the wizard
    /// writes nothing and says so. That is what makes re-running it safe.
    #[test]
    fn keeping_the_current_value_changes_nothing() {
        let a = answer_for(KEY_SUBJECT_MAX, "100".into(), "72".into(), "100".into());
        assert!(!a.changed);
    }

    #[test]
    fn the_two_scopes_spell_themselves_for_git() {
        assert_eq!(Where::Local.flag(), "--local");
        assert_eq!(Where::Global.flag(), "--global");
    }

    /// A quit at any prompt writes nothing — a half-applied wizard is the worst
    /// outcome available.
    #[test]
    fn q_and_eof_both_mean_quit() {
        let mut q = std::io::Cursor::new(b"q\n".to_vec());
        assert!(prompt(&mut q, "x", "", "d").is_none());
        let mut eof = std::io::Cursor::new(Vec::new());
        assert!(prompt(&mut eof, "x", "", "d").is_none());
        let mut enter = std::io::Cursor::new(b"\n".to_vec());
        assert_eq!(prompt(&mut enter, "x", "", "d").as_deref(), Some(""));
    }

    fn answers(input: &str, style: &Style) -> Option<(Where, Vec<Answer>)> {
        let mut cursor = std::io::Cursor::new(input.as_bytes().to_vec());
        ask_all(&mut cursor, style, None).expect("the flow does not fail")
    }

    fn value(list: &[Answer], key: &str) -> Option<String> {
        list.iter()
            .find(|a| a.key == key)
            .and_then(|a| a.value.clone())
    }

    /// The whole flow, answered.
    #[test]
    fn every_answer_reaches_its_key() {
        let (scope, list) = answers("global\nsuffix\n100\n68\n0\n", &Style::default()).unwrap();
        assert_eq!(scope, Where::Global);
        assert_eq!(value(&list, KEY_GITMOJI).as_deref(), Some("suffix"));
        assert_eq!(value(&list, KEY_SUBJECT_MAX).as_deref(), Some("100"));
        assert_eq!(value(&list, KEY_DESCRIPTION_MAX).as_deref(), Some("68"));
        assert_eq!(value(&list, KEY_BODY_WRAP).as_deref(), Some("0"));
    }

    /// PROPERTY: pressing Enter through the whole wizard writes nothing.
    ///
    /// That is what makes re-running it safe — the brackets show what is in
    /// effect, so accepting them all is by definition a no-op.
    #[test]
    fn accepting_every_default_changes_nothing() {
        let (_, list) = answers("\n\n\n\n\n", &Style::default()).unwrap();
        assert!(
            list.iter().all(|a| !a.changed),
            "something was marked changed: {:?}",
            list.iter().map(|a| a.key).collect::<Vec<_>>()
        );
    }

    /// The brackets show the CURRENT value, not the shipped one, so a reader
    /// who already configured something is not quietly offered a reset.
    #[test]
    fn the_offered_default_is_what_is_in_effect() {
        let configured = Style {
            gitmoji: Gitmoji::Prefix,
            subject_max: 100,
            ..Style::default()
        };
        let (_, list) = answers("\n\n\n\n\n", &configured).unwrap();
        assert!(list.iter().all(|a| !a.changed));
        // Keeping a non-default value keeps it WRITTEN, not unset.
        assert_eq!(value(&list, KEY_GITMOJI).as_deref(), Some("prefix"));
        assert_eq!(value(&list, KEY_SUBJECT_MAX).as_deref(), Some("100"));
    }

    /// Answering with the shipped default removes the key rather than pinning
    /// it — "I have no opinion" is a different statement from "I chose this".
    #[test]
    fn returning_to_the_default_unsets_rather_than_pins() {
        let configured = Style {
            subject_max: 100,
            ..Style::default()
        };
        let (_, list) = answers("\nnone\n72\n\n\n", &configured).unwrap();
        let subject = list
            .iter()
            .find(|a| a.key == KEY_SUBJECT_MAX)
            .expect("asked");
        assert!(subject.changed);
        assert!(subject.value.is_none(), "should unset, not write 72");
    }

    /// A bad answer re-asks rather than aborting or silently taking a default.
    #[test]
    fn an_unusable_answer_is_asked_again() {
        let (_, list) = answers(
            "sideways\nglobal\nnope\nsuffix\nlots\n80\n\n\n",
            &Style::default(),
        )
        .unwrap();
        assert_eq!(value(&list, KEY_GITMOJI).as_deref(), Some("suffix"));
        assert_eq!(value(&list, KEY_SUBJECT_MAX).as_deref(), Some("80"));
    }

    /// Quitting partway writes nothing at all.
    #[test]
    fn quitting_midway_discards_the_answers_already_given() {
        assert!(answers("global\nsuffix\nq\n", &Style::default()).is_none());
        // And end of input is the same thing.
        assert!(answers("global\nsuffix\n", &Style::default()).is_none());
    }
}
