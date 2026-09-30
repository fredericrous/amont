//! Version skew withholds a tree gate's stamp (ADR-0024,
//! `ci.skip-needs-the-same-command`: the same command, run by the same tool).
//!
//! A tree gate may prove its command only with the tool CI would resolve.
//! Checked before the gate starts, never deciding the commit:
//!
//! - an `amont.conf` tool pin must match the version the GATE runs (read the
//!   way `tree_cache::tool_version` reads it, not `verify_tool_pins`'
//!   `<program> --version`, which may be another binary);
//! - npm: the installed tree (`node_modules/.package-lock.json`) must be the
//!   locked one (`package-lock.json`), both ways — every locked package this
//!   machine should have is installed at its locked version and integrity,
//!   and nothing is installed that the lock does not name. Link entries, and
//!   optional packages whose `os`/`cpu` exclude this machine, are skipped;
//!   pnpm: `node_modules/.pnpm/lock.yaml` (the lock pnpm installed) must be
//!   byte-identical to `pnpm-lock.yaml`. yarn and bun installs cannot be
//!   verified, so they withhold. The lockfile is looked up from the gate's
//!   directory to the repository root, so a workspace package finds its
//!   root's lock;
//! - uv: `uv sync --locked --check` must find nothing to do, for the default
//!   package set or for `--all-packages` (a workspace CI syncs whole). Both
//!   are EXACT checks, so extra installed packages count as drift. Neither
//!   ever syncs or touches the network.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::json_read::{parse, Value};
use crate::manifest::{ToolPin, TreeGate, TreeTool};

/// Why `gate` may not be proven with the tools installed here, or `None`.
pub fn skew(cwd: &Path, gate: &TreeGate, pins: &[ToolPin], version: &str) -> Option<String> {
    if let Some(pin) = pins.iter().find(|p| p.program == gate.tool.as_str()) {
        let have = version;
        if have != "pinned-in-command" && !have.contains(&pin.want) {
            return Some(format!(
                "{} is pinned to {} but the gate runs {have}",
                pin.program, pin.want
            ));
        }
    }
    if matches!(gate.tool, TreeTool::Eslint | TreeTool::Prettier) {
        if let Some(why) = js_drift(cwd) {
            return Some(why);
        }
    }
    if gate.command.split_whitespace().take(2).eq(["uv", "run"]) {
        let check = |extra: &[&str]| {
            Command::new("uv")
                .args(["sync", "--locked", "--check"])
                .args(extra)
                .current_dir(cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };
        if !check(&[]) && !check(&["--all-packages"]) {
            return Some("the uv environment is not synced to uv.lock".into());
        }
    }
    None
}

/// The nearest directory from `cwd` up to the repository root holding one of
/// the JS lockfiles, and which.
fn js_lock_dir(cwd: &Path) -> Option<(std::path::PathBuf, &'static str)> {
    let mut dir = cwd.to_path_buf();
    loop {
        for lock in [
            "package-lock.json",
            "pnpm-lock.yaml",
            "yarn.lock",
            "bun.lockb",
        ] {
            if dir.join(lock).is_file() {
                return Some((dir, lock));
            }
        }
        if dir.join(".git").exists() || !dir.pop() {
            return None;
        }
    }
}

/// Whether the installed JS tree differs from its lock (no lock: nothing to
/// compare, and nothing a linter could have resolved from it either).
pub fn js_drift(cwd: &Path) -> Option<String> {
    let (dir, lock) = js_lock_dir(cwd)?;
    match lock {
        "package-lock.json" => npm_drift(&dir),
        "pnpm-lock.yaml" => {
            let want = std::fs::read(dir.join(lock)).ok();
            let have = std::fs::read(dir.join("node_modules").join(".pnpm").join("lock.yaml")).ok();
            match (want, have) {
                (Some(w), Some(h)) if w == h => None,
                (_, None) => Some("node_modules was not installed from pnpm-lock.yaml".into()),
                _ => Some("node_modules was installed from another pnpm-lock.yaml".into()),
            }
        }
        other => Some(format!("an install from {other} cannot be verified")),
    }
}

/// npm's names for this machine's platform, as a lockfile's `os`/`cpu` use.
fn npm_platform() -> (&'static str, &'static str) {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let cpu = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    };
    (os, cpu)
}

/// Whether a lockfile `os`/`cpu` list admits `this` (npm's grammar: plain
/// names allow, `!name` excludes; an absent list admits everything).
fn admits(list: Option<&Value>, this: &str) -> bool {
    let Some(list) = list else { return true };
    let names: Vec<&str> = list.items().iter().filter_map(Value::as_str).collect();
    if names.iter().any(|n| n.strip_prefix('!') == Some(this)) {
        return false;
    }
    let allows: Vec<&&str> = names.iter().filter(|n| !n.starts_with('!')).collect();
    allows.is_empty() || allows.iter().any(|n| **n == this)
}

/// Whether the installed tree differs from the lock, and how.
pub fn npm_drift(cwd: &Path) -> Option<String> {
    let lock = std::fs::read_to_string(cwd.join("package-lock.json")).ok();
    let hidden = std::fs::read_to_string(cwd.join("node_modules").join(".package-lock.json")).ok();
    let (Some(lock), Some(hidden)) = (lock, hidden) else {
        return Some("node_modules was not installed from package-lock.json".into());
    };
    let (Some(lock), Some(hidden)) = (parse(&lock), parse(&hidden)) else {
        return Some("a lockfile could not be read".into());
    };
    drift(
        lock.get("packages").map(Value::members).unwrap_or(&[]),
        hidden.get("packages").map(Value::members).unwrap_or(&[]),
        npm_platform(),
    )
}

fn drift(
    locked: &[(String, Value)],
    installed: &[(String, Value)],
    (os, cpu): (&str, &str),
) -> Option<String> {
    let relevant = |(path, e): &&(String, Value)| {
        !path.is_empty() && !e.get("link").is_some_and(Value::is_true) && e.get("version").is_some()
    };
    for (path, e) in locked.iter().filter(relevant) {
        let optional = e.get("optional").is_some_and(Value::is_true)
            || e.get("devOptional").is_some_and(Value::is_true);
        if optional && !(admits(e.get("os"), os) && admits(e.get("cpu"), cpu)) {
            continue;
        }
        let Some((_, have)) = installed.iter().find(|(p, _)| p == path) else {
            if optional {
                continue; // an optional install may fail without breaking npm
            }
            return Some(format!("{path} is locked but not installed"));
        };
        let want_v = e.get("version").and_then(Value::as_str);
        if have.get("version").and_then(Value::as_str) != want_v {
            return Some(format!(
                "{path} is installed at another version than locked"
            ));
        }
        if let Some(want_i) = e.get("integrity").and_then(Value::as_str) {
            if have.get("integrity").and_then(Value::as_str) != Some(want_i) {
                return Some(format!(
                    "{path} is installed with another integrity than locked"
                ));
            }
        }
    }
    for (path, _) in installed.iter().filter(relevant) {
        if !locked.iter().any(|(p, _)| p == path) {
            return Some(format!("{path} is installed but not locked"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pk(text: &str) -> Vec<(String, Value)> {
        parse(text).unwrap().members().to_vec()
    }

    const MAC: (&str, &str) = ("darwin", "x64");

    #[test]
    fn tree_skew_in_sync_is_none() {
        let lock =
            pk(r#"{"":{"name":"x"},"node_modules/a":{"version":"1.0.0","integrity":"sha512-A"}}"#);
        let inst = pk(r#"{"node_modules/a":{"version":"1.0.0","integrity":"sha512-A"}}"#);
        assert_eq!(drift(&lock, &inst, MAC), None);
    }

    #[test]
    fn tree_skew_other_platform_optional_is_skipped() {
        let lock = pk(r#"{"node_modules/a":{"version":"1.0.0"},
            "node_modules/@esbuild/linux-arm64":{"version":"0.2.0","optional":true,"os":["linux"],"cpu":["arm64"]},
            "node_modules/fsevents":{"version":"2.3.3","optional":true,"os":["darwin"]}}"#);
        let inst = pk(
            r#"{"node_modules/a":{"version":"1.0.0"},"node_modules/fsevents":{"version":"2.3.3"}}"#,
        );
        assert_eq!(drift(&lock, &inst, MAC), None);
    }

    #[test]
    fn tree_skew_link_entries_are_skipped() {
        let lock = pk(
            r#"{"node_modules/pkg":{"resolved":"packages/pkg","link":true},"packages/pkg":{"version":"0.0.0"}}"#,
        );
        let inst = pk(
            r#"{"node_modules/pkg":{"resolved":"packages/pkg","link":true},"packages/pkg":{"version":"0.0.0"}}"#,
        );
        assert_eq!(drift(&lock, &inst, MAC), None);
    }

    #[test]
    fn tree_skew_wrong_version_withholds() {
        let lock = pk(r#"{"node_modules/eslint-plugin-x":{"version":"2.0.0"}}"#);
        let inst = pk(r#"{"node_modules/eslint-plugin-x":{"version":"1.9.0"}}"#);
        assert!(drift(&lock, &inst, MAC)
            .unwrap()
            .contains("another version"));
    }

    #[test]
    fn tree_skew_missing_withholds() {
        let lock = pk(r#"{"node_modules/a":{"version":"1.0.0"}}"#);
        assert!(drift(&lock, &[], MAC).unwrap().contains("not installed"));
    }

    #[test]
    fn tree_skew_extra_install_withholds_the_other_way() {
        let lock = pk(r#"{"node_modules/a":{"version":"1.0.0"}}"#);
        let inst = pk(
            r#"{"node_modules/a":{"version":"1.0.0"},"node_modules/sneaky-plugin":{"version":"0.1.0"}}"#,
        );
        assert!(drift(&lock, &inst, MAC)
            .unwrap()
            .contains("installed but not locked"));
    }

    #[test]
    fn tree_skew_integrity_compared_only_when_locked() {
        let lock = pk(r#"{"node_modules/a":{"version":"1.0.0"}}"#);
        let inst = pk(r#"{"node_modules/a":{"version":"1.0.0","integrity":"sha512-B"}}"#);
        assert_eq!(drift(&lock, &inst, MAC), None);
        let lock = pk(r#"{"node_modules/a":{"version":"1.0.0","integrity":"sha512-A"}}"#);
        assert!(drift(&lock, &inst, MAC).unwrap().contains("integrity"));
    }

    /// Point `AMONT_NPM_DRIFT_DIR` at a real npm project to see what the
    /// comparison says about its installed tree.
    #[test]
    #[ignore]
    fn tree_skew_probe_a_real_project() {
        let dir = std::env::var("AMONT_NPM_DRIFT_DIR").expect("AMONT_NPM_DRIFT_DIR");
        eprintln!("npm_drift: {:?}", npm_drift(Path::new(&dir)));
    }

    fn project(files: &[(&str, &str)]) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "amont-skew-{}-{}",
            std::process::id(),
            files.len() * 7 + files.first().map(|f| f.1.len()).unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(".git")).unwrap();
        for (path, body) in files {
            let p = d.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        d
    }

    #[test]
    fn tree_skew_pnpm_identical_lock_is_in_sync() {
        let d = project(&[
            ("pnpm-lock.yaml", "lockfileVersion: '9.0'\n"),
            ("node_modules/.pnpm/lock.yaml", "lockfileVersion: '9.0'\n"),
            ("web/src/a.ts", ""),
        ]);
        assert_eq!(js_drift(&d.join("web")), None);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn tree_skew_pnpm_stale_install_withholds() {
        let d = project(&[
            ("pnpm-lock.yaml", "lockfileVersion: '9.0'\noverrides: {}\n"),
            ("node_modules/.pnpm/lock.yaml", "lockfileVersion: '9.0'\n"),
        ]);
        assert!(js_drift(&d).unwrap().contains("another pnpm-lock.yaml"));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn tree_skew_yarn_cannot_be_verified() {
        let d = project(&[("yarn.lock", "# yarn\n"), ("x/y.txt", "")]);
        assert!(js_drift(&d).unwrap().contains("cannot be verified"));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn tree_skew_negated_os_excludes() {
        let v = parse(r#"["!win32"]"#).unwrap();
        assert!(admits(Some(&v), "darwin"));
        assert!(!admits(Some(&v), "win32"));
    }
}
