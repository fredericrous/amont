//! The three Rust hooks: `cargo fmt --check`, `cargo clippy`, `cargo test`.
//!
//! Scoped like every other language hook — they fire only when the commit (or
//! push) touches Rust, and only in a directory that actually has a `Cargo.toml`.
//! A Python repo never invokes cargo.
//!
//! Split across the two dispatchers by COST, matching what the other languages
//! already do: `fmt` and `clippy` are pre-commit (as ruff and pyright are),
//! `test` is pre-push (as `run-tests-js` is). Nobody wants to wait for a
//! workspace test run on every commit.
//!
//! Each is a separate check rather than one "rust" hook, so `hook.skip` can
//! disable them individually — `git config hook.skip clippy` when you are
//! mid-refactor, without losing the formatting gate.

use super::common::{
    fail, fixing_enabled, hl, ok, repo_root, restage, run as run_tool, staged_files, warn, which,
    Restaged,
};
use crate::check::Outcome;
use crate::git;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Files that mean "this commit touches Rust". `Cargo.toml` and `Cargo.lock`
/// count: a dependency bump compiles differently without a single `.rs` edit,
/// and that is exactly when clippy earns its keep.
///
/// Exported for the registry's drift guard: clippy and cargo-test consume this
/// whole set while declaring only `.rs` plus a `Cargo.toml` opt-in.
pub const RUST_PATHS: &[&str] = &[
    ".rs",
    "Cargo.toml",
    "Cargo.lock",
    "rustfmt.toml",
    "clippy.toml",
    // A pin bump changes which clippy judges the same source.
    "rust-toolchain.toml",
    "rust-toolchain",
];

/// What `cargo fmt` is handed. Exported so `registry.rs` declares the scope
/// from the same constant — see `lint_json_yaml::EXTS`.
pub const EXTS: &[&str] = &[".rs"];

fn is_rust_path(f: &str) -> bool {
    let name = f.rsplit('/').next().unwrap_or(f);
    RUST_PATHS.iter().any(|pattern| {
        if pattern.starts_with('.') {
            name.ends_with(pattern)
        } else {
            name == *pattern
        }
    })
}

/// The nearest ancestor of `file` holding a `Cargo.toml`, bounded by the repo.
///
/// Not simply "the repo root": plenty of repos keep a Rust component in a
/// subdirectory next to services in other languages, and cargo must run where
/// the manifest is. `--workspace` then covers every member from that point, so
/// one invocation per manifest root is enough.
fn cargo_root_for(root: &str, file: &str) -> Option<PathBuf> {
    let mut dir = Path::new(root).join(file);
    dir.pop();
    loop {
        if dir.join("Cargo.toml").is_file() {
            return Some(dir);
        }
        if dir == Path::new(root) || !dir.starts_with(root) {
            return None;
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn cargo_roots<'a>(root: &str, files: impl Iterator<Item = &'a str>) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    for f in files.filter(|f| is_rust_path(f)) {
        if let Some(d) = cargo_root_for(root, f) {
            seen.insert(d);
        }
    }
    seen.into_iter().collect()
}

/// True when `cargo <component> --version` works.
///
/// Only for separately-installable COMPONENTS — rustfmt and clippy, which a
/// toolchain can legitimately lack. A missing one must warn and pass, never
/// fail a commit for a tool the developer never chose.
///
/// Do NOT probe a BUILT-IN subcommand this way: `cargo test --version` is
/// "unexpected argument '--version'", so the probe reports test as unavailable
/// and the gate silently passes — it would never have run a test anywhere.
fn component_available(cargo: &[String], dir: &Path, sub: &str) -> bool {
    let Some((program, rest)) = cargo.split_first() else {
        return false;
    };
    Command::new(program)
        .args(rest)
        .arg(sub)
        .arg("--version")
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// The toolchain `dir` (or an ancestor) pins, with the file that pins it:
/// `rust-toolchain.toml`'s `[toolchain] channel`, or the legacy one-line
/// `rust-toolchain`. The walk is rustup's own — it reads the nearest pin
/// above the working directory — so what this finds is what rustup applies.
fn pinned_toolchain(dir: &Path) -> Option<(PathBuf, String)> {
    let mut d = Some(dir);
    while let Some(cur) = d {
        let toml = cur.join("rust-toolchain.toml");
        if let Ok(text) = std::fs::read_to_string(&toml) {
            if let Some(ch) = parse_toolchain_toml(&text) {
                return Some((toml, ch));
            }
        }
        let legacy = cur.join("rust-toolchain");
        if let Ok(text) = std::fs::read_to_string(&legacy) {
            let ch = text.lines().next().unwrap_or("").trim();
            if !ch.is_empty() {
                return Some((legacy, ch.to_string()));
            }
        }
        d = cur.parent();
    }
    None
}

/// `channel = "1.94.1"` out of a `rust-toolchain.toml`, ignoring comments.
fn parse_toolchain_toml(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| {
            let rest = l
                .strip_prefix("channel")?
                .trim_start()
                .strip_prefix('=')?
                .trim();
            let rest = rest.split('#').next()?.trim();
            let v = rest.trim_matches(|c| c == '"' || c == '\'');
            (!v.is_empty()).then(|| v.to_string())
        })
}

/// A pin we can hold a `cargo --version` against: `1.94` or `1.94.1`. A
/// channel name (`stable`, `nightly-2026-01-01`) is a moving target and is
/// left to rustup.
fn version_pin(pin: &str) -> Option<&str> {
    let parts: Vec<&str> = pin.split('.').collect();
    let numeric = (2..=3).contains(&parts.len())
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    numeric.then_some(pin)
}

/// `1.94.1` out of `cargo 1.94.1 (29ea6fb6a 2026-03-24)`, run where the pin
/// applies so a rustup shim answers for that directory.
fn cargo_version(cargo: &[String], dir: &Path) -> Option<String> {
    let (program, rest) = cargo.split_first()?;
    let out = Command::new(program)
        .args(rest)
        .arg("--version")
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let v = text.split_whitespace().nth(1)?;
    Some(v.to_string())
}

/// `1.94` matches `1.94.1`; `1.94.1` matches only itself.
fn version_matches(pin: &str, version: &str) -> bool {
    version == pin
        || version
            .strip_prefix(pin)
            .is_some_and(|rest| rest.starts_with('.'))
}

/// The cargo invocation a directory means, as an argv. With a pin and a
/// rustup: `rustup run <pin> cargo`, so the pin is applied by rustup AND
/// exported (`RUSTUP_TOOLCHAIN`, PATH) to every `rustc` cargo spawns. Without
/// either: the first `cargo` on PATH, as before.
///
/// Why not `which`: on a machine with a second cargo ahead of the rustup shim
/// (Homebrew's `rust` formula in /usr/local/bin, measured 2026-09-21) it finds
/// the wrong one and the pin is silently ignored — clippy then judges with
/// lints CI never sees.
///
/// Why not the toolchain's cargo binary by path (`rustup which cargo`, the
/// first shape of this fix): run directly, that cargo spawns each `rustc`
/// through the PATH proxy with cwd set to the crate being compiled, and the
/// proxy re-resolves the toolchain from THERE. A dependency that ships its own
/// `rust-toolchain` — `convert_case` 0.10 pins 1.83.0 — was built by 1.83.0
/// inside a 1.94.1 build: `E0514: crate compiled by an incompatible version
/// of rustc`, on the release commit of the change itself. `rustup run` is
/// the invocation that keeps one toolchain for the whole build.
fn resolve_cargo(dir: &Path) -> Option<Vec<String>> {
    if let Some((_, pin)) = pinned_toolchain(dir) {
        if let Some(rustup) = which("rustup") {
            return Some(vec![rustup, "run".into(), pin, "cargo".into()]);
        }
    }
    which("cargo").map(|c| vec![c])
}

/// Resolve cargo, hold it against the pin, verify the component.
///
/// `Err(Outcome::Unavailable)` when there is no cargo or no component (warned,
/// never failed: a tool the developer never chose cannot block a commit).
/// `Err(Outcome::Failed)` when the cargo that would run is NOT the pinned
/// toolchain: a green verdict from the wrong clippy is worth less than none,
/// and a red one wastes time on lints that are not CI's.
///
/// Split out from `each_root` because `fmt` now runs TWO passes — a `--check`
/// and, when repairing, a write — and the second must not re-probe rustfmt
/// (a second `cargo fmt --version` per manifest root) nor duplicate the
/// resolution it would have to get identical.
fn cargo_for(
    roots: &[PathBuf],
    component: Option<&str>,
    missing: &str,
) -> Result<Vec<String>, Outcome> {
    let first = roots
        .first()
        .map(PathBuf::as_path)
        .unwrap_or(Path::new("."));
    let Some(cargo) = resolve_cargo(first) else {
        warn(missing);
        return Err(Outcome::Unavailable);
    };
    for dir in roots {
        let Some((file, pin)) = pinned_toolchain(dir) else {
            continue;
        };
        let Some(got) = cargo_version(&cargo, dir) else {
            // `rustup run <pin>` with nothing installed under that name
            // answers nothing: say which toolchain to install rather than
            // let the real run fail on rustup's own message.
            fail(&format!(
                "{} pins `{pin}` and no cargo answers for it. {}.",
                file.display(),
                hl(&format!("rustup toolchain install {pin}"))
            ));
            return Err(Outcome::Failed);
        };
        let Some(want) = version_pin(&pin) else {
            continue;
        };
        if !version_matches(want, &got) {
            fail(&format!(
                "cargo is {got} but {} pins {want}, so this check would judge with a \
                 toolchain CI never runs. Put the rustup shim first on PATH — a second \
                 cargo ahead of it (Homebrew's {} in /usr/local/bin) is the usual cause — \
                 or {}.",
                file.display(),
                hl("rust"),
                hl(&format!("rustup toolchain install {want}"))
            ));
            return Err(Outcome::Failed);
        }
    }
    if let Some(c) = component {
        for dir in roots {
            if !component_available(&cargo, dir, c) {
                warn(missing);
                return Err(Outcome::Unavailable);
            }
        }
    }
    Ok(cargo)
}

/// Run one cargo invocation in every manifest root. True when all succeeded.
fn run_in_roots(
    settings: &crate::config::Settings,
    roots: &[PathBuf],
    argv: &[String],
    args: &[&str],
) -> bool {
    let extra: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    let mut all_ok = true;
    for dir in roots {
        let d = dir.to_string_lossy().into_owned();
        if !run_tool(settings, &d, argv, &extra) {
            all_ok = false;
        }
    }
    all_ok
}

/// Shared shape: find the manifest roots, verify the component if there is one
/// to verify, run the command in each, report once.
///
/// `component` is `Some` only for rustfmt/clippy; `None` means a built-in
/// subcommand where cargo's own presence is the whole requirement.
fn each_root(
    settings: &crate::config::Settings,
    roots: &[PathBuf],
    component: Option<&str>,
    args: &[&str],
    missing: &str,
) -> Result<bool, Outcome> {
    let argv = cargo_for(roots, component, missing)?;
    Ok(run_in_roots(settings, roots, &argv, args))
}

pub fn fmt(settings: &crate::config::Settings, _args: &[std::ffi::OsString]) -> Outcome {
    let files = staged_files(EXTS);
    if files.is_empty() {
        return Outcome::Passed;
    }
    let root = repo_root();
    let roots = cargo_roots(&root, files.iter().map(String::as_str));
    if roots.is_empty() {
        return Outcome::Passed;
    }
    const MISSING: &str =
        "Rust staged but rustfmt is not installed. `rustup component add rustfmt`.";
    let argv = match cargo_for(&roots, Some("fmt"), MISSING) {
        Ok(argv) => argv,
        Err(outcome) => return outcome,
    };

    // `--all -- --check` per the project convention. It inspects the working
    // TREE rather than the index — and that is now correct, because the
    // pre-commit stage holds the unstaged changes aside for the duration, so
    // the tree IS the staged content. This comment used to call that "the same
    // trade-off cargo fmt gives everyone", which was true of the observation
    // and wrong about the conclusion: see `staged_only`.
    if run_in_roots(settings, &roots, &argv, &["fmt", "--all", "--", "--check"]) {
        ok(settings, "Rust formatting is clean");
        return Outcome::Passed;
    }

    // The registry has always declared `Fix::Rewrite` for this check, and
    // `amont list --json` reported `"fix":"rewrite"` — which `agents_md`
    // explicitly tells agents to trust — while no fixing code existed
    // anywhere. Only prettier and the manifest's externals ever called
    // `restage`. Rather than downgrade the declaration, the fixing is now
    // real.
    if fixing_enabled(settings) && run_in_roots(settings, &roots, &argv, &["fmt", "--all"]) {
        // The non-obvious guard: `cargo fmt --all` formats the WHOLE
        // workspace, not just the staged files — but `restage` is handed the
        // staged `.rs` list, so nothing the author did not stage is staged
        // here. The formatter's other edits stay in the working tree, exactly
        // as an unrelated unstaged change would.
        match restage(&files) {
            Restaged::Staged => {
                ok(settings, "Rust reformatted and re-staged");
                return Outcome::Fixed;
            }
            Restaged::Failed(stuck) => {
                fail(&format!(
                    "cargo fmt rewrote these files but {} failed — the index still holds the \
                     UNFORMATTED content: {}",
                    hl("git add"),
                    stuck.join(", ")
                ));
                return Outcome::Failed;
            }
            // Nothing staged differed, so whatever `--check` objected to was
            // outside the staged set. Fall through and report it.
            Restaged::Nothing => {}
        }
    }

    fail(&format!("Unformatted Rust. Run {}.", hl("cargo fmt --all")));
    Outcome::Failed
}

pub fn clippy(settings: &crate::config::Settings, _args: &[std::ffi::OsString]) -> Outcome {
    // `staged_files` matches by suffix, which would also accept
    // `vendor/NotCargo.toml`. `is_rust_path` compares the basename, so let it
    // be the only filter rather than keeping two that disagree.
    let files: Vec<String> = staged_files(&[])
        .into_iter()
        .filter(|f| is_rust_path(f))
        .collect();
    if files.is_empty() {
        return Outcome::Passed;
    }
    let root = repo_root();
    let roots = cargo_roots(&root, files.iter().map(String::as_str));
    if roots.is_empty() {
        return Outcome::Passed;
    }
    match each_root(
        settings,
        &roots,
        Some("clippy"),
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--",
            "-D",
            "warnings",
        ],
        "Rust staged but clippy is not installed. `rustup component add clippy`.",
    ) {
        Err(outcome) => outcome,
        Ok(true) => {
            ok(settings, "Clippy passed");
            Outcome::Passed
        }
        Ok(false) => {
            fail(&format!(
                "Clippy warnings. Fix them or run {}.",
                hl("cargo clippy --fix")
            ));
            Outcome::Failed
        }
    }
}

/// pre-push. Mirrors `run-tests-js`: the range that is actually being pushed
/// decides whether the suite runs, so a docs-only push costs nothing.
///
/// PER REF, not once for the whole push: `git push origin a b` carries two
/// tips, and a single worktree checked out to the first one would run the
/// second ref's tests against the first ref's tree — a real failure in the
/// untested branch reported as a pass because nothing actually ran against
/// it. Each ref that touches Rust gets its own worktree and its own verdict.
pub fn test(
    settings: &crate::config::Settings,
    refs: &[crate::pushrefs::PushRef],
    gate: &str,
) -> Outcome {
    // `Unavailable`, never `Passed`, when git will not answer — same argument
    // and same wording as run-tests-js: a gate must not report green having
    // asked nothing.
    let Some(root) = git::stdout(&["rev-parse", "--show-toplevel"]) else {
        super::common::warn("cargo-test: git would not answer — the gate did NOT run");
        return Outcome::Unavailable;
    };
    let zero = git::stdout(&["hash-object", "--stdin"])
        .map(|h| "0".repeat(h.len()))
        .unwrap_or_else(|| "0".repeat(40));
    let mut ran_any = false;
    for r in refs {
        let changed = crate::pushrefs::changed_files_for(r, &zero);
        let roots = cargo_roots(&root, changed.iter().map(String::as_str));
        if roots.is_empty() {
            continue;
        }
        // Where THIS ref's suite runs decides what it is answering about.
        // `_guard` owns the checkout for the length of this ref's run;
        // dropping it removes the worktree before the next ref's begins.
        let (where_, _guard) =
            crate::pushed_tree::where_to_run(settings, &r.local_oid, &root, gate);
        let roots: Vec<PathBuf> = roots
            .iter()
            .map(|rt| {
                rt.strip_prefix(&root)
                    .map(|rel| where_.join(rel))
                    .unwrap_or_else(|_| rt.clone())
            })
            .collect();
        match each_root(
            settings,
            &roots,
            None,
            &["test", "--workspace", "--all-features"],
            "Rust changed but cargo is not installed.",
        ) {
            Err(outcome) => return outcome,
            Ok(true) => ran_any = true,
            Ok(false) => {
                fail("Rust tests failed. Push aborted.");
                return Outcome::Failed;
            }
        }
    }
    if ran_any {
        ok(settings, "Rust tests passed");
    }
    Outcome::Passed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_rust_paths() {
        assert!(is_rust_path("src/main.rs"));
        assert!(is_rust_path("Cargo.toml"));
        assert!(is_rust_path("crates/a/Cargo.lock"));
        assert!(is_rust_path("rustfmt.toml"));
        assert!(!is_rust_path("README.md"));
        assert!(!is_rust_path("src/main.rsx"));
        // A file merely CONTAINING the name is not the manifest.
        assert!(!is_rust_path("docs/Cargo.toml.md"));
        assert!(!is_rust_path("vendor/NotCargo.toml"));
    }

    #[test]
    fn finds_the_nearest_manifest_not_the_repo_root() {
        let tmp = std::env::temp_dir().join("amont-cargo-roots");
        let _ = std::fs::remove_dir_all(&tmp);
        let nested = tmp.join("services/engine");
        std::fs::create_dir_all(nested.join("src")).unwrap();
        std::fs::write(nested.join("Cargo.toml"), "[package]\n").unwrap();
        let root = tmp.to_string_lossy().into_owned();

        let got = cargo_roots(&root, ["services/engine/src/main.rs"].into_iter());
        assert_eq!(got, vec![nested.clone()], "should find the nested manifest");

        // A Rust file with no manifest anywhere above it is not a cargo project.
        std::fs::create_dir_all(tmp.join("scripts")).unwrap();
        let none = cargo_roots(&root, ["scripts/loose.rs"].into_iter());
        assert!(none.is_empty(), "no manifest above it: {none:?}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn several_files_in_one_crate_yield_one_root() {
        let tmp = std::env::temp_dir().join("amont-cargo-dedupe");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("src")).unwrap();
        std::fs::write(tmp.join("Cargo.toml"), "[package]\n").unwrap();
        let root = tmp.to_string_lossy().into_owned();
        let got = cargo_roots(&root, ["src/a.rs", "src/b.rs", "Cargo.toml"].into_iter());
        assert_eq!(got.len(), 1, "one cargo invocation, not three: {got:?}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn non_rust_files_select_nothing() {
        let got = cargo_roots("/tmp", ["README.md", "a.py"].into_iter());
        assert!(got.is_empty());
    }

    #[test]
    fn reads_the_channel_out_of_the_toml() {
        let text = "# pinned\n[toolchain]\nchannel = \"1.94.1\" # why\ncomponents = [\"clippy\"]\n";
        assert_eq!(parse_toolchain_toml(text).as_deref(), Some("1.94.1"));
        assert_eq!(
            parse_toolchain_toml("[toolchain]\nchannel = 'stable'\n").as_deref(),
            Some("stable")
        );
        assert_eq!(parse_toolchain_toml("[toolchain]\ncomponents = []\n"), None);
        assert_eq!(parse_toolchain_toml("# channel = \"1.0\"\n"), None);
    }

    #[test]
    fn only_a_version_is_held_against_cargo() {
        assert_eq!(version_pin("1.94.1"), Some("1.94.1"));
        assert_eq!(version_pin("1.94"), Some("1.94"));
        assert_eq!(version_pin("stable"), None);
        assert_eq!(version_pin("nightly-2026-01-01"), None);
        assert_eq!(version_pin("1"), None);
        assert!(version_matches("1.94.1", "1.94.1"));
        assert!(version_matches("1.94", "1.94.1"));
        assert!(!version_matches("1.94.1", "1.98.0"));
        assert!(!version_matches("1.9", "1.94.1"));
    }

    #[test]
    fn the_nearest_pin_above_the_manifest_wins() {
        let tmp = std::env::temp_dir().join("amont-toolchain-pin");
        let _ = std::fs::remove_dir_all(&tmp);
        let nested = tmp.join("services/engine");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(tmp.join("rust-toolchain"), "1.85.0\n").unwrap();
        let (file, pin) = pinned_toolchain(&nested).unwrap();
        assert_eq!((file, pin.as_str()), (tmp.join("rust-toolchain"), "1.85.0"));
        std::fs::write(
            nested.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"1.94.1\"\n",
        )
        .unwrap();
        let (file, pin) = pinned_toolchain(&nested).unwrap();
        assert_eq!(
            (file, pin.as_str()),
            (nested.join("rust-toolchain.toml"), "1.94.1")
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
