//! The hook binary — argument handling only.
//!
//! Everything it does lives in `amont-runtime`. Git invokes exactly five
//! hook names; the shim passes its own filename through, and the registry maps
//! that name to a handler.
//!
//! ## Why the parsing is one total function
//!
//! This file used to walk the arguments with an ad-hoc loop that took the first
//! non-flag token as the hook name and swept everything after it into `rest` —
//! and then asked, seven times over, `rest.first() == "install"`,
//! `rest.first() == "run"`, and so on. `rest` is where GIT'S OWN ARGUMENTS to
//! the hook live.
//!
//! So `amont --hooks-dir d pre-push <remote> <url>`, which is exactly what
//! the pre-push shim execs, dispatched on the REMOTE NAME. A remote called
//! `install` ran the full installer — copying the binary, populating the
//! template directory, baking shims — in the middle of a push. A remote called
//! `run`, `list`, `trust`, `restore` or `uninstall` was worse in the quiet way:
//! the branch fired, did something unrelated or nothing at all, and exited 0.
//! A push with zero checks, reported as a pass. `git remote add install <url>`
//! is a strange thing to type but not an impossible one, and nothing in the
//! shim, the hook, or the output would have hinted at the cause.
//!
//! The rule that fixes it is one line long: **only position 0 is ever tested
//! against the subcommand table.** Everything after it is data. That is not
//! something a reader can verify by scanning a chain of `if`s, so the parse is
//! a single function returning an [`Invocation`], and the property is a test
//! (`a_hook_argument_never_names_a_subcommand`) over every spelling the table
//! holds.

use std::ffi::OsString;
use std::path::PathBuf;

use amont_runtime::check::Stage;
use amont_runtime::{pushrefs, registry, ListOptions};

/// What `--help` prints, and what a usage error prints after saying why.
///
/// Lifted out of the module doc because it was only ever a doc comment: the
/// binary's actual `--help` was one line naming `--hooks-dir` and none of the
/// eight subcommands, printed to stderr, exiting 2. A user asking a program
/// what it does should not be told they used it wrong.
const USAGE: &str = "\
usage: amont <subcommand> | amont --hooks-dir <dir> <hook-name> [args…]

  list           what would run in this repository, and why not
                 [--json] [--stage pre-commit|pre-push] [--pushed]
  install        turn hooks on here: copy the binary if needed, bake the shims
                 [--force] replaces a hook that is not ours
  init           wire up THIS repository only — the verb a package manager
                 calls from `prepare`; never copies a binary, never prompts
  uninstall      remove OUR five shims and nothing else [--binary: the binary too]
  setup          the commit-style questions, current values as defaults
                 [--local|--global] [--dry-run]
  trust          show what amont.conf declares, and accept it
                 [--show: what is trusted] [--revoke: forget it]
  run            rehearse checks without committing: the pre-commit stage,
                 `run pre-push` for the push gate, or one check by name
                 [--all-files] [--hooks-dir <dir>]
  restore        bring back unstaged changes a killed pre-commit left parked
  agents-md      write the agent-guidance block into AGENTS.md, plus the
                 CLAUDE.md signpost pointing at it (both generated, both
                 checked) [--check: report drift only] [--path <file>]
  enroll         the machine-level standing grant: every future clone gets
                 the hooks [--conventions declared|everywhere]
  attest         `attest covered` — print the gate names a VALID signed
                 attestation covers for the tree checked out here (CI's
                 one-liner; prints nothing and exits 0 on any failure)
                 [--signers <file>] [--principal <id>]
                 [--platform <arch-os>|any: which leg is asking; defaults
                 to this machine's, so a matrix leg only skips work that
                 really ran on ITS platform]

  check          what is wrong with these FILES — no index, no staging, no
                 commit. The content checks only (ban-terms, secrets,
                 merge-conflict, large-files), reported as
                 `file:line:col: severity: message [check]`, which every
                 editor's error parser and every modern terminal already
                 understand. Reads a path list, or one unsaved buffer on
                 stdin. Exits 1 if anything blocking was found.
                 [<path>…] [--stdin-filename <path>] [--format text|json]

  --help         this text        --version   the binary's version

Hook mode (what the shims call): amont --hooks-dir <dir> <hook-name> [args…]
The full manual: https://fredericrous.github.io/amont/
";

/// The verbs. Named as a type so the dispatch cannot grow an eighth spelling
/// that only exists inside an `if`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sub {
    List,
    Setup,
    Install,
    Init,
    Uninstall,
    Run,
    Trust,
    Restore,
    AgentsMd,
    Enroll,
    Attest,
    Check,
}

impl Sub {
    /// This variant's position in [`SUBCOMMANDS`].
    ///
    /// The match is exhaustive, so adding a variant will not compile until it
    /// is given a position, and `the_subcommand_table_is_exhaustive` will not
    /// pass until that position exists in the table. Together that is the
    /// forcing function: a new verb cannot reach dispatch without appearing in
    /// the one table that `subcommand` reads.
    ///
    /// `cfg(test)` because dispatch itself has no use for a position — it is
    /// the exhaustiveness of the `match` that does the work, and that check
    /// happens whenever the test build compiles, which is every CI run.
    #[cfg(test)]
    const fn index(self) -> usize {
        match self {
            Sub::List => 0,
            Sub::Setup => 1,
            Sub::Install => 2,
            Sub::Init => 3,
            Sub::Uninstall => 4,
            Sub::Run => 5,
            Sub::Trust => 6,
            Sub::Restore => 7,
            Sub::AgentsMd => 8,
            Sub::Enroll => 9,
            Sub::Attest => 10,
            Sub::Check => 11,
        }
    }
}

/// THE table. One list, read by [`subcommand`] and walked by the tests.
///
/// There were previously seven independent string comparisons scattered down
/// `main`, each asked twice (once of `hook`, once of `rest.first()`), which is
/// fourteen places for the set of verbs to be. This is one.
const SUBCOMMANDS: [(&str, Sub); 12] = [
    ("list", Sub::List),
    ("setup", Sub::Setup),
    ("install", Sub::Install),
    ("init", Sub::Init),
    ("uninstall", Sub::Uninstall),
    ("run", Sub::Run),
    ("trust", Sub::Trust),
    ("restore", Sub::Restore),
    ("agents-md", Sub::AgentsMd),
    ("enroll", Sub::Enroll),
    ("attest", Sub::Attest),
    ("check", Sub::Check),
];

/// The only place a string is compared against the verb set.
fn subcommand(arg: &str) -> Option<Sub> {
    SUBCOMMANDS
        .iter()
        .find(|(name, _)| *name == arg)
        .map(|(_, sub)| *sub)
}

/// What the command line asked for. Total: every argv maps to exactly one of
/// these, including the empty one.
#[derive(Debug, PartialEq, Eq)]
enum Invocation {
    Sub {
        name: Sub,
        args: Vec<OsString>,
    },
    Hook {
        hooks_dir: PathBuf,
        name: String,
        args: Vec<OsString>,
    },
    Help,
    Version,
    /// Used wrongly, with the reason. Exit 2 — the binary was invoked wrongly,
    /// not a hook deciding something.
    Usage(String),
}

/// Decide what `argv` (already skipping argv[0]) asks for.
///
/// The rules, in order, and the order is the whole design:
///
/// 1. nothing at all ⇒ usage error;
/// 2. `--help`/`-h` first ⇒ help;
/// 3. **argv[0] names a subcommand ⇒ that subcommand, with argv[1..] passed
///    through VERBATIM.** Nothing after position 0 is tested against the table
///    again, ever. That single restriction is what makes a remote named
///    `install` an ordinary string;
/// 4. otherwise hook mode. `--hooks-dir <dir>` is consumed only while no hook
///    name has been taken; the first remaining token is the hook name; and
///    every token after it goes to the hook untouched.
///
/// Rule 4's "only before the name" clause fixes a second, quieter bug: the old
/// loop matched `--hooks-dir` anywhere, so `amont pre-push --hooks-dir X`
/// — or a hook invoked with a git-supplied argument that happened to be
/// `--hooks-dir` — silently ate two of the hook's own arguments and handed the
/// check a short list. After the name, `--hooks-dir` is just a word.
fn parse(argv: Vec<OsString>) -> Invocation {
    let Some(first) = argv.first() else {
        return Invocation::Usage("no arguments".to_string());
    };
    if first == "--help" || first == "-h" {
        return Invocation::Help;
    }
    if first == "--version" || first == "-V" {
        return Invocation::Version;
    }
    if let Some(sub) = first.to_str().and_then(subcommand) {
        return Invocation::Sub {
            name: sub,
            args: argv[1..].to_vec(),
        };
    }

    let mut hooks_dir: Option<PathBuf> = None;
    let mut name: Option<String> = None;
    let mut args: Vec<OsString> = Vec::new();
    let mut it = argv.into_iter();
    while let Some(a) = it.next() {
        if name.is_some() {
            args.push(a);
            continue;
        }
        if a == "--hooks-dir" {
            let Some(v) = it.next() else {
                return Invocation::Usage("--hooks-dir requires a value".to_string());
            };
            hooks_dir = Some(PathBuf::from(v));
            continue;
        }
        let Some(s) = a.to_str() else {
            return Invocation::Usage(format!("hook name is not valid UTF-8: {a:?}"));
        };
        name = Some(s.to_owned());
    }

    match (hooks_dir, name) {
        (Some(hooks_dir), Some(name)) => Invocation::Hook {
            hooks_dir,
            name,
            args,
        },
        (None, Some(name)) => Invocation::Usage(format!(
            "{name:?} is not a subcommand, and hook mode needs --hooks-dir <dir> before the hook name"
        )),
        _ => Invocation::Usage("no hook name".to_string()),
    }
}

/// Die quietly when the reader hangs up, like every other Unix filter.
///
/// Rust ignores SIGPIPE at startup, so `amont list | head` panicked with a
/// backtrace the moment `head` closed the pipe — `println!` hit EPIPE and
/// EPIPE is a panic. Restoring SIGPIPE's default disposition makes the exit
/// what a shell user expects (killed by the signal, status 141), and it is
/// safe here because nothing in this binary writes to a pipe it must outlive.
/// `extern "C"` rather than a crate: std already links libc, and the commit
/// path takes no dependencies. Windows has no SIGPIPE; there the pipe-closed
/// write fails an `Err` path instead of raising a signal, and this is a no-op.
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

fn main() {
    die_on_sigpipe();
    match parse(std::env::args_os().skip(1).collect()) {
        Invocation::Help => {
            print!("{USAGE}");
            std::process::exit(0);
        }
        Invocation::Version => {
            println!("amont {}", env!("CARGO_PKG_VERSION"));
            std::process::exit(0);
        }
        Invocation::Usage(why) => {
            eprintln!("amont: {why}");
            eprint!("{USAGE}");
            std::process::exit(2);
        }
        Invocation::Sub { name, args } => std::process::exit(run_sub(name, &args)),
        Invocation::Hook {
            hooks_dir,
            name,
            args,
        } => std::process::exit(run_hook(&hooks_dir, &name, &args)),
    }
}

/// The flags each verb takes — `flags` stand alone, `valued` consume the next
/// token. THE allowlist: [`run_sub`] rejects anything else, because a silently
/// ignored flag does the opposite of what was asked at the worst possible
/// verb — `amont trust --revok` used to fall through the `--revoke` test and
/// GRANT trust.
fn known_flags(sub: Sub) -> (&'static [&'static str], &'static [&'static str]) {
    match sub {
        Sub::List => (&["--json", "--pushed"], &["--stage"]),
        Sub::Setup => (&["--local", "--global", "--dry-run"], &[]),
        Sub::Install => (&["--force"], &[]),
        Sub::Init | Sub::Restore => (&[], &[]),
        Sub::Uninstall => (&["--binary"], &[]),
        Sub::Run => (&["--all-files"], &["--hooks-dir"]),
        Sub::Trust => (&["--show", "--revoke"], &[]),
        Sub::AgentsMd => (&["--check"], &["--path"]),
        Sub::Enroll => (&[], &["--conventions"]),
        Sub::Attest => (&[], &["--signers", "--principal", "--platform"]),
        Sub::Check => (&[], &["--stdin-filename", "--format"]),
    }
}

/// The first `--flag` the allowlist does not know, skipping the values of the
/// flags that take one. Positional words are the verb's own business.
fn unknown_flag(args: &[OsString], flags: &[&str], valued: &[&str]) -> Option<String> {
    let mut skip_value = false;
    for a in args {
        if skip_value {
            skip_value = false;
            continue;
        }
        let Some(a) = a.to_str() else { continue };
        if !a.starts_with('-') {
            continue;
        }
        if valued.contains(&a) {
            skip_value = true;
            continue;
        }
        if !flags.contains(&a) {
            return Some(a.to_string());
        }
    }
    None
}

/// This verb's spelling in [`SUBCOMMANDS`] — for error messages.
fn verb_name(sub: Sub) -> &'static str {
    SUBCOMMANDS
        .iter()
        .find(|(_, s)| *s == sub)
        .map(|(n, _)| *n)
        .unwrap_or("?")
}

/// Run a subcommand and return its exit code.
///
/// One `match` over the enum rather than seven `if`s, so the compiler is the
/// thing that notices an unhandled verb.
fn run_sub(sub: Sub, args: &[OsString]) -> i32 {
    // A help or version request is inert in ANY position, before anything
    // else looks at the arguments. `amont install --help` used to RUN THE
    // INSTALLER — copy the binary, populate the template dir, offer trust —
    // because parse() tested `--help` only at argv[0] and install scanned
    // only for `--force`. Asking a program what it does must never be a
    // mutating action.
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return 0;
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("amont {}", env!("CARGO_PKG_VERSION"));
        return 0;
    }
    let (flags, valued) = known_flags(sub);
    if let Some(bad) = unknown_flag(args, flags, valued) {
        eprintln!("amont: unknown flag {bad:?} for `{}`", verb_name(sub));
        eprint!("{USAGE}");
        return 2;
    }
    match sub {
        // `amont list` — what would run here, and why. Lives in the HOOK
        // binary rather than the fleet tool because this is the binary
        // installed everywhere, and the question is asked about the repo you
        // are standing in.
        Sub::List => {
            let stage = match stage_flag(args) {
                Ok(s) => s,
                Err(msg) => {
                    eprintln!("amont: {msg}");
                    return 2;
                }
            };
            amont_runtime::list_checks(ListOptions {
                json: args.iter().any(|a| a == "--json"),
                stage,
                pushed: args.iter().any(|a| a == "--pushed"),
            })
        }
        // `amont install` — was a Makefile recipe. It lives here so the
        // guard that decides whether a directory may be emptied has ONE
        // implementation, tested on every platform, rather than one in `make`
        // and another in PowerShell for the Windows users who have no `make`.
        Sub::Install => report(amont_runtime::install::run(
            args.iter().any(|a| a == "--force"),
        )),
        // `amont init` — the verb a package manager calls. Deliberately
        // flagless: it runs unattended, from a `prepare` script, so there is
        // nobody to have typed an option and nothing it should be asked to do
        // beyond wiring this one repository.
        Sub::Init => report(amont_runtime::install::init()),
        Sub::Uninstall => report(amont_runtime::install::uninstall(
            args.iter().any(|a| a == "--binary"),
        )),
        // `amont setup` — the four commit-style keys, asked once. A separate
        // verb rather than a prompt inside `install`, which has to stay
        // answerable by nobody: it runs under `amont-fleet install`, in
        // provisioning scripts, and never at all for the `init.templateDir`
        // users whose hooks arrive with a clone.
        Sub::Setup => report(amont_runtime::setup::command(args)),
        Sub::Restore => report(amont_runtime::staged_only::restore_command()),
        Sub::Trust => report(amont_runtime::trust::command(args)),
        Sub::Run => run_mode(args),
        Sub::AgentsMd => agents_md(args),
        // `amont enroll` — the machine-level standing grant: template dir +
        // `init.templateDir`, optionally scoping the conventions to declared
        // repositories. One command in the onboarding doc instead of one
        // `amont init` per clone per person.
        Sub::Enroll => {
            let conventions = match flag_value(args, "--conventions") {
                Ok(v) => v,
                Err(msg) => {
                    eprintln!("amont: {msg}");
                    return 2;
                }
            };
            report(amont_runtime::install::enroll(conventions.as_deref()))
        }
        // `amont attest covered` — CI's verifying one-liner. Prints the gate
        // names a VALID attestation covers for the tree checked out here, or
        // nothing. Exits 0 either way: fail-open is the contract, not an
        // option a workflow author might forget to pass. The only non-zero
        // exit is usage (a subaction this verb does not have).
        // `amont check <paths…>` — what is wrong with these FILES.
        //
        // Deliberately NOT a flag on `run`. `run` rehearses a commit: it is
        // index-aware, it holds unstaged work aside, and `restore` exists to
        // undo that. This answers a question about content, which may not be
        // staged and — via `--stdin-filename` — may never have been saved.
        // Sharing a verb would drag all of that machinery into a read-only
        // lookup, and an editor asking about a buffer would inherit a stash.
        Sub::Check => {
            let format = match flag_value(args, "--format") {
                Ok(f) => f,
                Err(msg) => {
                    eprintln!("amont: {msg}");
                    return 2;
                }
            };
            let json = match format.as_deref() {
                None | Some("text") => false,
                Some("json") => true,
                Some(other) => {
                    eprintln!("amont: unknown --format {other:?} (text or json)");
                    return 2;
                }
            };
            let stdin_name = match flag_value(args, "--stdin-filename") {
                Ok(n) => n,
                Err(msg) => {
                    eprintln!("amont: {msg}");
                    return 2;
                }
            };
            // Positional words only: the flags and their values are the
            // allowlist's business and were validated above.
            let mut paths: Vec<String> = Vec::new();
            let mut skip = false;
            for a in args {
                if skip {
                    skip = false;
                    continue;
                }
                let Some(a) = a.to_str() else { continue };
                if a == "--format" || a == "--stdin-filename" {
                    skip = true;
                    continue;
                }
                if !a.starts_with("--") {
                    paths.push(a.to_string());
                }
            }

            let mut findings = Vec::new();
            if let Some(name) = &stdin_name {
                use std::io::Read;
                let mut buf = Vec::new();
                if std::io::stdin().read_to_end(&mut buf).is_err() {
                    eprintln!("amont: could not read stdin");
                    return 2;
                }
                findings.extend(amont_runtime::content::scan(name, &buf));
            } else if paths.is_empty() {
                eprintln!("amont: check needs a path, or --stdin-filename <path>");
                eprint!("{USAGE}");
                return 2;
            }
            for p in &paths {
                match std::fs::read(p) {
                    Ok(bytes) => findings.extend(amont_runtime::content::scan(p, &bytes)),
                    // Named but unreadable is the caller's mistake, not a
                    // finding about the file — say so on stderr and keep
                    // going, so one bad path does not hide the other results.
                    Err(e) => eprintln!("amont: {p}: {e}"),
                }
            }

            if json {
                println!("{}", amont_runtime::finding::to_json(&findings));
            } else {
                for f in &findings {
                    println!("{}", f.render());
                }
            }
            // 1 for a blocking finding, the convention every linter follows.
            // A warning is not a failure: `large-files` warns about a
            // deliberate asset, and an editor asking what is here must not
            // get a non-zero exit for an answer it did not treat as fatal.
            i32::from(
                findings
                    .iter()
                    .any(|f| f.severity == amont_runtime::check::Severity::Block),
            )
        }
        Sub::Attest => {
            if args.first().map(|a| a != "covered").unwrap_or(true) {
                eprintln!("amont: attest takes the subaction `covered`");
                eprint!("{USAGE}");
                return 2;
            }
            let (signers, principal, platform) = match (
                flag_value(args, "--signers"),
                flag_value(args, "--principal"),
                flag_value(args, "--platform"),
            ) {
                (Ok(s), Ok(p), Ok(pl)) => (s, p, pl),
                (Err(msg), _, _) | (_, Err(msg), _) | (_, _, Err(msg)) => {
                    eprintln!("amont: {msg}");
                    return 2;
                }
            };
            let Some(signers) = signers
                .map(PathBuf::from)
                .or_else(amont_runtime::attest::default_signers)
            else {
                return 0; // no allowed_signers anywhere: nothing is covered
            };
            let Some(principal) =
                principal.or_else(|| amont_runtime::attest::first_principal(&signers))
            else {
                return 0; // an empty signers file names nobody to verify as
            };
            // Default: this machine's platform. A matrix leg therefore skips
            // only work that really ran on ITS platform, with no per-leg
            // configuration — the ubuntu and windows legs of a Rust matrix
            // simply find nothing covering them and run. `any` is the
            // deliberate, committed statement that a suite's result does not
            // depend on where it ran.
            let want = match platform.as_deref() {
                Some("any") => None,
                Some(explicit) => Some(explicit.to_string()),
                None => Some(amont_runtime::attest::platform()),
            };
            if let Some(gates) =
                amont_runtime::attest::covered(&signers, &principal, want.as_deref())
            {
                println!("{gates}");
            }
            0
        }
    }
}

/// The value following `--<flag>`, when present.
fn flag_value(rest: &[OsString], flag: &str) -> Result<Option<String>, String> {
    let mut iter = rest.iter();
    while let Some(a) = iter.next() {
        if a == flag {
            return iter
                .next()
                .and_then(|v| v.to_str())
                .map(|v| Some(v.to_string()))
                .ok_or_else(|| format!("{flag} requires a value"));
        }
    }
    Ok(None)
}

/// `Result<(), String>` → an exit code, printing the error. The shape four of
/// the subcommands share.
fn report(r: Result<(), String>) -> i32 {
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// `amont run [<check>] [--all-files] [--hooks-dir <dir>]`.
///
/// Scans its OWN arguments, which is the same thing it did before — but they
/// are now unambiguously its own, so the `*a != "run"` clause that used to
/// stop the verb itself being read as a check name is gone with the ambiguity
/// that made it necessary.
fn run_mode(args: &[OsString]) -> i32 {
    let all_files = args.iter().any(|a| a == "--all-files");
    // A named check runs alone; otherwise the whole pre-commit stage.
    let named = args
        .iter()
        .filter_map(|a| a.to_str())
        .find(|a| !a.starts_with("--"))
        .map(str::to_owned);
    // Loaded ahead of the push-ref decision below, which asks the manifest
    // whether a named check is a pre-push one.
    let manifest = amont_runtime::manifest::load(std::path::Path::new(
        &amont_runtime::hooks::common::repo_root(),
    ));
    // INVARIANT: the policy is installed immediately after EVERY
    // manifest::load, before any config read in the process — check_timeout
    // and friends cache on first read.
    amont_runtime::policy::install(manifest.policy.clone());
    // Resolve a short name to its full id BEFORE the push-ref decision:
    // `run pytest` must synthesize refs, and an ambiguous `run branch-pattern`
    // must say so rather than fail on an upstream it was never going to use.
    let named = match named {
        Some(n) if amont_runtime::registry::lookup(&n, &manifest).is_none() => {
            match amont_runtime::dispatch::resolve_check_name(&n, &manifest) {
                amont_runtime::dispatch::Named2::Resolved(id) => Some(id),
                amont_runtime::dispatch::Named2::Unknown => {
                    eprintln!("amont: unknown check {n:?} — try `amont list`");
                    return 2;
                }
                amont_runtime::dispatch::Named2::Ambiguous(ids) => {
                    eprintln!(
                        "amont: {n:?} names more than one check — pick one: {}",
                        ids.join(", ")
                    );
                    return 2;
                }
            }
        }
        other => other,
    };
    let push = if named
        .as_deref()
        .is_some_and(|n| needs_synthetic_push_refs(n, &manifest))
    {
        // A pre-push check invoked standalone has no real push on the other
        // end of stdin to read refs from. Left as `::default()`, `.get()`
        // would either block on a TTY that has nothing coming, or (stdin
        // redirected from `/dev/null`) read an empty ref list — which every
        // pre-push check treats as "nothing to check" and passes, silently.
        // Synthesize the one ref a standalone run can still answer for: what
        // HEAD would push.
        match pushrefs::synthetic_from_upstream() {
            Ok(r) => pushrefs::PushRefs::preloaded(vec![r]),
            Err(e) => {
                eprintln!("amont: {e}");
                return 2;
            }
        }
    } else {
        pushrefs::PushRefs::default()
    };
    let hooks_dir = match hooks_dir_flag(args) {
        Ok(Some(d)) => d,
        Ok(None) => PathBuf::from(".git/hooks"),
        Err(e) => {
            eprintln!("amont: {e}");
            return 2;
        }
    };
    let name = named.clone().unwrap_or_else(|| "pre-commit".to_string());
    let ctx = registry::Ctx {
        name: &name,
        args: &[],
        hooks_dir: &hooks_dir,
        push: &push,
        manifest: &manifest,
    };
    let verdict = match named {
        // `run_named` lives in the runtime so `registry::lookup` — and the
        // decision about whether a named check takes the index-fidelity hold —
        // stay in one place rather than being re-derived here.
        Some(check) => match amont_runtime::dispatch::run_named(&ctx, &check, all_files) {
            amont_runtime::dispatch::Named::Ran(v) => v,
            amont_runtime::dispatch::Named::Unknown => {
                eprintln!("amont: unknown check {check:?} — try `amont list`");
                return 2;
            }
            amont_runtime::dispatch::Named::Ambiguous(ids) => {
                eprintln!(
                    "amont: {check:?} names more than one check — pick one: {}",
                    ids.join(", ")
                );
                return 2;
            }
        },
        None => amont_runtime::dispatch::run_all(&ctx, all_files),
    };
    verdict.exit_code()
}

/// `amont agents-md [--check] [--path <file>]`.
fn agents_md(args: &[OsString]) -> i32 {
    let check_only = args.iter().any(|a| a == "--check");
    let path = match path_flag(args) {
        Ok(Some(p)) => p,
        // Outside a repository there is no root to write into. `repo_root()`
        // answered "." here, so `amont agents-md` typed from `~` wrote
        // `./AGENTS.md` into the user's home directory and reported success.
        Ok(None) => match amont_runtime::hooks::common::repo_root_checked() {
            Ok(root) => PathBuf::from(root).join("AGENTS.md"),
            Err(e) => {
                eprintln!("amont: {e}");
                return 2;
            }
        },
        Err(e) => {
            eprintln!("amont: {e}");
            return 2;
        }
    };
    if check_only {
        // Both files are generated, so both are checked; the worst verdict
        // wins. A signpost left behind by an older amont is exactly the
        // silent rot this pair exists to make impossible.
        let pointer_code = pointer_path(&path).map_or(0, |p| {
            match amont_runtime::agents_md::check_pointer(&p) {
                Ok(amont_runtime::agents_md::CheckResult::Drifted) => {
                    eprintln!(
                        "{}: signpost drifted from the generated one — run `amont agents-md`",
                        p.display()
                    );
                    1
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
                // NotPresent is not drift: the signpost is opt-in, exactly
                // as the block it points at is.
                Ok(_) => 0,
            }
        });
        let block_code = match amont_runtime::agents_md::check(&path) {
            Ok(amont_runtime::agents_md::CheckResult::NotPresent) => {
                println!(
                    "{}: not present (opt-in — run without --check to add it)",
                    path.display()
                );
                0
            }
            Ok(amont_runtime::agents_md::CheckResult::MatchesGenerated) => {
                println!("{}: up to date", path.display());
                0
            }
            Ok(amont_runtime::agents_md::CheckResult::Drifted) => {
                eprintln!(
                    "{}: drifted from the generated block — run `amont agents-md`",
                    path.display()
                );
                1
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        };
        block_code.max(pointer_code)
    } else {
        match amont_runtime::agents_md::write(&path) {
            Ok(()) => {
                println!("wrote {}", path.display());
                pointer_path(&path).map_or(0, |p| {
                    match amont_runtime::agents_md::write_pointer(&p) {
                        Ok(()) => {
                            println!("wrote {} (signpost)", p.display());
                            0
                        }
                        Err(e) => {
                            eprintln!("{e}");
                            1
                        }
                    }
                })
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        }
    }
}

/// Where the CLAUDE.md signpost goes, given where the block went.
///
/// `None` when the block IS `CLAUDE.md` — writing a signpost to the file
/// that already holds the guidance would replace it with a pointer to
/// itself. Sits beside the block rather than at the repo root so
/// `--path docs/AGENTS.md` keeps the pair together and the relative
/// `[AGENTS.md](AGENTS.md)` link in the signpost stays correct.
fn pointer_path(block: &std::path::Path) -> Option<PathBuf> {
    if block.file_name()? == "CLAUDE.md" {
        return None;
    }
    Some(block.with_file_name("CLAUDE.md"))
}

/// Dispatch a git-invoked hook. `args` is whatever git passed, verbatim.
fn run_hook(hooks_dir: &std::path::Path, hook: &str, args: &[OsString]) -> i32 {
    let push = pushrefs::PushRefs::default();
    // The manifest is parsed ONCE, here, at the process boundary — the same
    // shape as the push refs above it: owned by the entrypoint, lent to
    // everything downstream through the Ctx. git invokes hooks with the
    // repository as the working directory, so `repo_root()`'s answer is the
    // repository this hook is about.
    let manifest = amont_runtime::manifest::load(std::path::Path::new(
        &amont_runtime::hooks::common::repo_root(),
    ));
    // INVARIANT: the policy is installed immediately after EVERY
    // manifest::load, before any config read in the process — check_timeout
    // and friends cache on first read.
    amont_runtime::policy::install(manifest.policy.clone());
    let ctx = registry::Ctx {
        name: hook,
        args,
        hooks_dir,
        push: &push,
        manifest: &manifest,
    };
    // THE process boundary: the one place a hook result becomes a number.
    // Everything above speaks `Verdict`. An unknown name is NOT the usage
    // error it looks like: nobody hand-types hook mode — the name is a
    // shim passing its own filename, so a name this binary has never heard
    // is a shim from a NEWER template. That is age, not misuse, and it
    // arrives on every commit of every fresh repository, so it must not
    // fail and must not nag — see `skew`.
    match registry::lookup(hook, &manifest) {
        Some(run_hook) => run_hook(&ctx).exit_code(),
        None => amont_runtime::skew::absorb_newer_hook(hook),
    }
}

/// `--stage <pre-commit|pre-push>` for `list`, ad hoc scanned like every other
/// flag here — no parsing library, matching `trust.rs`'s `let flag = |f: &str|
/// args.iter().any(|a| a == f);` idiom.
fn stage_flag(rest: &[OsString]) -> Result<Option<Stage>, String> {
    let mut iter = rest.iter();
    while let Some(a) = iter.next() {
        if a == "--stage" {
            let v = iter
                .next()
                .and_then(|v| v.to_str())
                .ok_or_else(|| "--stage requires a value".to_string())?;
            return match v {
                "pre-commit" => Ok(Some(Stage::PreCommit)),
                "pre-push" => Ok(Some(Stage::PrePush)),
                other => Err(format!(
                    "--stage must be `pre-commit` or `pre-push`, got {other:?}"
                )),
            };
        }
    }
    Ok(None)
}

/// `--path <file>` for `agents-md`.
fn path_flag(rest: &[OsString]) -> Result<Option<PathBuf>, String> {
    value_flag(rest, "--path")
}

/// `--hooks-dir <dir>` for `run`, which is the one subcommand that takes it.
fn hooks_dir_flag(rest: &[OsString]) -> Result<Option<PathBuf>, String> {
    value_flag(rest, "--hooks-dir")
}

fn value_flag(rest: &[OsString], flag: &str) -> Result<Option<PathBuf>, String> {
    let mut iter = rest.iter();
    while let Some(a) = iter.next() {
        if a == flag {
            let v = iter
                .next()
                .ok_or_else(|| format!("{flag} requires a value"))?;
            return Ok(Some(PathBuf::from(v)));
        }
    }
    Ok(None)
}

/// Whether `amont run <name>` needs a synthetic push ref list — `name` is
/// either the `pre-push` entrypoint itself, or an individual check whose own
/// declared stage is pre-push. Checked here, rather than left to `.get()` to
/// discover empty, because empty and "nothing to check" are indistinguishable
/// once a check has already started running.
fn needs_synthetic_push_refs(name: &str, manifest: &amont_runtime::manifest::Manifest) -> bool {
    if name == "pre-push" {
        return true;
    }
    if let Some(c) = registry::one_named(name, manifest) {
        return c.stage() == Stage::PrePush;
    }
    // A SHORT name resolves later, in `run_named` — but the refs decision is
    // made here, first. Match the same way it will: if any check the name
    // reaches is a pre-push one, synthesize. For an ambiguous name this may
    // synthesize refs that go unused; run_named then reports the ambiguity.
    registry::CHECKS
        .iter()
        .any(|c| c.stage == Stage::PrePush && amont_runtime::skip_suppresses(c.name, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }

    /// The bug, stated as a property over the whole verb set: a hook argument
    /// is data, and no amount of it can name a subcommand.
    ///
    /// `amont --hooks-dir d pre-push <remote> <url>` is literally what the
    /// pre-push shim execs. With a remote named `install` the old dispatch ran
    /// the FULL INSTALLER mid-push; with one named `run`, `list`, `trust`,
    /// `restore` or `uninstall` the hook became a no-op that exited 0 — a push
    /// with no checks, reported as a pass.
    #[test]
    fn a_hook_argument_never_names_a_subcommand() {
        for (spelling, _) in SUBCOMMANDS {
            let got = parse(argv(&[
                "--hooks-dir",
                "/d",
                "pre-push",
                spelling,
                "https://example.test/r.git",
            ]));
            assert_eq!(
                got,
                Invocation::Hook {
                    hooks_dir: PathBuf::from("/d"),
                    name: "pre-push".to_string(),
                    args: argv(&[spelling, "https://example.test/r.git"]),
                },
                "a remote named {spelling:?} hijacked dispatch"
            );

            // And in the first argument position too — `commit-msg` is handed
            // a path, `prepare-commit-msg` a path and a source word.
            let got = parse(argv(&["--hooks-dir", "/d", "commit-msg", spelling]));
            assert_eq!(
                got,
                Invocation::Hook {
                    hooks_dir: PathBuf::from("/d"),
                    name: "commit-msg".to_string(),
                    args: argv(&[spelling]),
                },
                "a commit-message file named {spelling:?} hijacked dispatch"
            );
        }
    }

    /// Every variant round-trips through the one table, at its own position.
    /// Adding a verb will not compile without a position in `Sub::index`, and
    /// will not pass this without an entry in `SUBCOMMANDS` — which is the
    /// only place `subcommand` looks.
    #[test]
    fn the_subcommand_table_is_exhaustive() {
        for (i, (spelling, sub)) in SUBCOMMANDS.iter().enumerate() {
            assert_eq!(sub.index(), i, "{spelling} is at the wrong position");
            assert_eq!(
                subcommand(spelling),
                Some(*sub),
                "{spelling} does not round-trip"
            );
        }
        // Distinct spellings, or two verbs share a position and one of them is
        // unreachable.
        let mut names: Vec<&str> = SUBCOMMANDS.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "a spelling appears twice");
    }

    /// A subcommand takes everything after it verbatim — including tokens that
    /// are themselves verb spellings, and including flags it does not know.
    #[test]
    fn a_subcommand_passes_its_arguments_through_untouched() {
        assert_eq!(
            parse(argv(&["run", "pre-commit-prettier", "--all-files"])),
            Invocation::Sub {
                name: Sub::Run,
                args: argv(&["pre-commit-prettier", "--all-files"]),
            }
        );
        assert_eq!(
            parse(argv(&["agents-md", "--path", "install"])),
            Invocation::Sub {
                name: Sub::AgentsMd,
                args: argv(&["--path", "install"]),
            }
        );
    }

    /// `--hooks-dir` is consumed only BEFORE the hook name. After it, it is a
    /// word git handed us — the old loop matched it anywhere and swallowed two
    /// of the hook's own arguments.
    #[test]
    fn hooks_dir_after_the_hook_name_is_an_argument_not_a_flag() {
        assert_eq!(
            parse(argv(&[
                "--hooks-dir",
                "/d",
                "pre-push",
                "--hooks-dir",
                "origin"
            ])),
            Invocation::Hook {
                hooks_dir: PathBuf::from("/d"),
                name: "pre-push".to_string(),
                args: argv(&["--hooks-dir", "origin"]),
            }
        );
    }

    #[test]
    fn help_and_emptiness_are_their_own_answers() {
        assert_eq!(parse(argv(&["--help"])), Invocation::Help);
        assert_eq!(parse(argv(&["-h"])), Invocation::Help);
        assert!(matches!(parse(argv(&[])), Invocation::Usage(_)));
        assert!(matches!(
            parse(argv(&["--hooks-dir"])),
            Invocation::Usage(_)
        ));
        // A hook name with no --hooks-dir is a usage error that says so,
        // rather than a hook run against a directory nobody named.
        assert!(matches!(parse(argv(&["pre-push"])), Invocation::Usage(_)));
    }

    /// The help text has to actually describe the program. It used to be one
    /// line naming `--hooks-dir` and none of the verbs.
    #[test]
    fn the_usage_block_names_every_subcommand() {
        for (spelling, _) in SUBCOMMANDS {
            assert!(USAGE.contains(spelling), "usage never mentions {spelling}");
        }
        assert!(USAGE.contains("--hooks-dir"));
        assert!(USAGE.contains("--stage"));
    }
}
