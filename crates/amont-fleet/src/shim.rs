//! Classifying an installed hook file against what it should be.
//!
//! The trap here is symmetrical and both halves are silent:
//!
//! - Compare an installed shim against the RAW tracked template and every one
//!   of the 96 repos reports as drifted, because installation deliberately
//!   substitutes the binary path. A tool that cries wolf on the whole fleet is
//!   a tool nobody reads.
//! - Compare too loosely — say, on length or on one marker line — and genuine
//!   drift is never seen, which is worse, because the report still looks calm.
//!
//! So classification is exact: recover the baked path from the installed file,
//! re-render the template with it, and require byte equality. Recovery IS the
//! comparison; there is no fuzzy middle.
//!
//! Note the placeholder appears three times in the template, one of them inside
//! a comment, because `make install` seds globally. Any comparison that assumed
//! a single substitution would misclassify every baked shim.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// The shim, the placeholder and the hook names come from the RUNTIME, which is
/// what installs them.
///
/// They used to be a second `include_str!` and a second copy of the constants
/// here. That was survivable while nothing else baked shims, and stopped being
/// so the moment `amont install` existed: drift detection compares a repo's
/// shim against `render(path)`, so if the installer and the dashboard disagreed
/// by one byte, every correctly installed repo in the fleet would report as
/// drifted. Re-exported rather than re-declared so they cannot.
pub use amont_runtime::install::{DISPATCHERS, PLACEHOLDER, SHIM as TEMPLATE};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ShimState {
    /// Byte-identical to the template rendered with `baked`.
    Ok {
        baked: String,
    },
    /// Present, readable text, but not the template under any substitution.
    Drifted,
    Missing,
    /// A symlink, pointing anywhere — including at one of our own shims.
    ///
    /// This used to collapse into `Ok`/`Drifted`, because `read_to_string`
    /// follows a link and reports the TARGET's bytes. A dispatcher that is a
    /// link to a tracked file in the working tree therefore read as a perfectly
    /// healthy shim, and `fix --apply` then wrote through it — `fs::write`
    /// follows links too — and rewrote the tracked file. That is the verified
    /// incident `hookfile` exists to end, and this variant is how the dashboard
    /// can see it before the write does.
    Symlink {
        target: Option<PathBuf>,
    },
    /// It exists and we could not establish what it is: not valid UTF-8 (a
    /// compiled hook), unreadable, a directory, a device, a hard link with other
    /// names. Every one of these used to become `Drifted` or `Missing` — the
    /// latter being the dangerous one, since "missing" is what makes `fix`
    /// decide to WRITE.
    Unreadable {
        why: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "bake", rename_all = "snake_case")]
pub enum BakeState {
    /// Points at the binary we would install.
    Current,
    /// Points into a `node_modules` tree: the REPOSITORY ships its own binary,
    /// as a dev dependency, and `amont init` baked it from the `prepare`
    /// script. Deliberate, correct, and none of the fleet's business.
    ///
    /// Without this it read as [`BakeState::Stale`] — "points somewhere else" is
    /// literally true — and `fix --apply` would helpfully rewrite all four
    /// shims to `~/.local/bin/amont`. On this machine that is churn: the fleet
    /// re-bakes one way, the next `npm install` re-bakes the other, forever.
    ///
    /// On a TEAMMATE's machine it is worse than churn. The entire point of the
    /// npm route is that the binary travels with the repository and nothing has
    /// to be installed first — so `~/.local/bin/amont` is exactly the path that
    /// does not exist there, and a fleet-wide repair would leave every one of
    /// those repositories with shims that resolve nothing.
    SelfManaged { path: String },
    /// Points somewhere else — the GUI-client failure mode, since a hook
    /// launched without an interactive PATH resolves nothing.
    Stale { path: String },
    /// The placeholder survived; resolution falls through to $HOME/.local/bin.
    Unbaked,
    /// The shims disagree with each other about where the binary is.
    Mixed,
    /// Nothing installed to have an opinion about.
    None,
}

pub fn render(binary: &str) -> String {
    amont_runtime::install::bake(TEMPLATE, binary)
}

/// Recover the substituted path, or `None` if this file is not the template
/// under ANY substitution.
///
/// Solved from the template's literal segments rather than from a marker line.
/// An earlier version anchored on `BIN="…"` and silently recovered the WRONG
/// value: the template assigns `BIN="$GIT_HOOKS_BIN"` on an earlier line for
/// the escape hatch, so the first match was never the baked path. Every
/// correctly installed shim in the fleet would have reported as drifted.
///
/// Splitting on the placeholder cannot pick the wrong line, because the segments
/// around each occurrence are fixed text. The candidate is still only accepted
/// once re-rendering reproduces the file byte for byte, so a wrong guess is
/// rejected rather than believed.
pub fn recover_baked(installed: &str) -> Option<String> {
    let head = TEMPLATE.split(PLACEHOLDER).next()?;
    let rest = installed.strip_prefix(head)?;
    // The text that follows the first placeholder is fixed; whatever precedes
    // it is the substitution.
    let tail = TEMPLATE.split(PLACEHOLDER).nth(1)?;
    let end = if tail.is_empty() {
        rest.len()
    } else {
        rest.find(tail)?
    };
    let candidate = rest[..end].to_string();
    (render(&candidate) == installed).then_some(candidate)
}

/// What is installed at one dispatcher path.
///
/// Two questions, asked in this order and by two different owners:
///
/// 1. WHAT IS THERE — a link, a directory, a binary, an unreadable file, a
///    regular readable file. [`amont_runtime::hookfile::classify`] owns that
///    one, because it is the same question `install` and `uninstall` ask and
///    three separate one-liners used to answer it differently. It never follows
///    a link and never guesses.
/// 2. IS IT OUR TEMPLATE, byte for byte — which only [`recover_baked`] can
///    answer, since `hookfile` knows about our marker but not about the exact
///    substitution.
///
/// The body used to be `read_to_string(path)` with `Err` ⇒ `Missing`, which
/// answered neither question honestly: a compiled hook, a directory and a
/// permissions error all read as "nothing installed", and "nothing installed"
/// is what makes `fix` decide to write one.
pub fn classify(path: &Path) -> ShimState {
    use amont_runtime::hookfile::{ForeignWhy, HookFile};
    match amont_runtime::hookfile::classify(path) {
        HookFile::Absent => ShimState::Missing,
        HookFile::Symlink { target } => ShimState::Symlink { target },
        HookFile::NotARegularFile => ShimState::Unreadable {
            why: "not a regular file (a directory, a fifo, a device)".to_string(),
        },
        HookFile::Unknown { why } => ShimState::Unreadable { why },
        HookFile::Foreign(ForeignWhy::HandWritten) | HookFile::Ours => {
            // A regular, readable, UTF-8 file: now the byte-exact question.
            match std::fs::read_to_string(path)
                .ok()
                .and_then(|c| recover_baked(&c))
            {
                Some(baked) => ShimState::Ok { baked },
                None => ShimState::Drifted,
            }
        }
        HookFile::Foreign(why) => ShimState::Unreadable {
            why: why.describe(),
        },
    }
}

/// Whether a baked path is one a package manager owns.
///
/// The test is a `node_modules` PATH SEGMENT, not a substring: `/srv/my
/// node_modules-backup/bin/amont` is somebody's directory, not a dependency
/// tree. Both separators, because a shim baked on Windows carries `C:\…\`.
///
/// Deliberately not "is it inside THIS repository's node_modules". Hooks are
/// shared across worktrees while `node_modules` is not, so the owning checkout
/// of a hooks directory is frequently not the one whose install last ran — and
/// answering "no" there would put us straight back to rewriting a path the
/// package manager is maintaining.
fn is_package_managed(path: &str) -> bool {
    path.replace('\\', "/")
        .split('/')
        .any(|part| part == "node_modules")
}

/// Collapse the four shims' baked paths into one verdict.
pub fn bake_state(shims: &[ShimState], installed_binary: &str) -> BakeState {
    let mut baked: Vec<&str> = shims
        .iter()
        .filter_map(|s| match s {
            ShimState::Ok { baked } => Some(baked.as_str()),
            _ => None,
        })
        .collect();
    baked.sort_unstable();
    baked.dedup();

    match baked.as_slice() {
        [] => BakeState::None,
        [one] if *one == PLACEHOLDER => BakeState::Unbaked,
        [one] if *one == installed_binary => BakeState::Current,
        // After the exact match, so a machine that genuinely installed its
        // binary inside a `node_modules` still reads as Current.
        [one] if is_package_managed(one) => BakeState::SelfManaged {
            path: (*one).to_string(),
        },
        [one] => BakeState::Stale {
            path: (*one).to_string(),
        },
        _ => BakeState::Mixed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shim baked inside a `node_modules` tree belongs to the repository's
    /// package manager, not to us.
    ///
    /// The `Stale` reading it used to get is what makes `fix --apply` rewrite
    /// it to our binary — churn here, and on a machine that installed nothing
    /// (the entire premise of the npm route) a rewrite to `~/.local/bin/amont`
    /// points at a file that does not exist.
    #[test]
    fn a_bake_inside_node_modules_is_the_package_managers_business() {
        let npm = "/repo/node_modules/@amont-hooks/darwin-arm64/bin/amont";
        assert_eq!(
            bake_state(&[ok(npm)], "/home/me/.local/bin/amont"),
            BakeState::SelfManaged {
                path: npm.to_string()
            }
        );
        // The pre-1.20 unscoped layout, still on machines that installed
        // then. `is_package_managed` looks for a `node_modules` component and
        // never at the package name, which is what makes the scope move a
        // non-event here — but a test that only knew the new shape would stop
        // proving that.
        let legacy = "/repo/node_modules/amont-darwin-arm64/bin/amont";
        assert!(matches!(
            bake_state(&[ok(legacy)], "/home/me/.local/bin/amont"),
            BakeState::SelfManaged { .. }
        ));
        // pnpm's real layout, which is where this actually bites: the store
        // path carries the version, so it changes on every bump. Scoped
        // packages add a directory level to it as well.
        let pnpm = "/repo/node_modules/.pnpm/@amont-hooks+darwin-x64@1.20.0/node_modules/@amont-hooks/darwin-x64/bin/amont";
        assert!(matches!(
            bake_state(&[ok(pnpm)], "/home/me/.local/bin/amont"),
            BakeState::SelfManaged { .. }
        ));
        // Windows separators reach this from a shim baked by `amont init`
        // there; splitting only on `/` would call it stale.
        let win = r"C:\repo\node_modules\@amont-hooks\win32-x64\bin\amont.exe";
        assert!(matches!(
            bake_state(&[ok(win)], "/home/me/.local/bin/amont"),
            BakeState::SelfManaged { .. }
        ));
    }

    /// SEGMENT, not substring. Somebody's `node_modules-backup` directory is
    /// not a dependency tree, and treating it as one would exempt a genuinely
    /// stale bake from the repair that fixes it.
    #[test]
    fn only_a_real_path_segment_counts_as_package_managed() {
        for path in [
            "/srv/node_modules-backup/bin/amont",
            "/srv/my-node_modules/bin/amont",
            "/opt/amont/bin/amont",
        ] {
            assert_eq!(
                bake_state(&[ok(path)], "/bin/amont"),
                BakeState::Stale {
                    path: path.to_string()
                },
                "{path} should still be stale"
            );
        }
    }

    /// The exact match wins first, so a machine that deliberately keeps its
    /// installed binary inside a node_modules still reads as Current rather
    /// than being quietly exempted from repair.
    #[test]
    fn an_exact_match_outranks_the_node_modules_rule() {
        let p = "/repo/node_modules/.bin/amont";
        assert_eq!(bake_state(&[ok(p)], p), BakeState::Current);
    }

    #[test]
    fn templates_are_still_one_blob() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../templates/hooks");
        for name in DISPATCHERS {
            let p = std::path::Path::new(dir).join(name);
            let got = std::fs::read_to_string(&p).expect("template");
            assert_eq!(
                got, TEMPLATE,
                "{name} differs from the embedded template; embedding one blob \
                 is no longer valid and classification would be wrong for it"
            );
        }
    }

    /// EVERY occurrence, whatever the count. It used to assert exactly three,
    /// which is a fact about today's shim text rather than about the renderer —
    /// rewording a comment that mentioned the token broke it while nothing was
    /// wrong. What matters is that the installer and this agree, which the
    /// shared constants now make structural, and that none survives.
    #[test]
    fn every_occurrence_is_substituted() {
        let expected = TEMPLATE.matches(PLACEHOLDER).count();
        assert!(expected > 0, "the template lost its placeholder entirely");
        let out = render("/opt/amont");
        assert!(!out.contains(PLACEHOLDER), "a placeholder survived");
        assert_eq!(out.matches("/opt/amont").count(), expected);
    }

    /// The dashboard and the installer must produce the same bytes, or a
    /// correctly installed repo reads as drifted.
    #[test]
    fn the_dashboard_renders_what_the_installer_writes() {
        assert_eq!(
            render("/opt/amont"),
            amont_runtime::install::bake(amont_runtime::install::SHIM, "/opt/amont")
        );
    }

    #[test]
    fn a_correctly_baked_shim_is_not_drift() {
        let installed = render("/Users/me/.local/bin/amont");
        assert_eq!(
            recover_baked(&installed).as_deref(),
            Some("/Users/me/.local/bin/amont"),
            "the whole fleet would read as drifted"
        );
    }

    #[test]
    fn an_unbaked_shim_recovers_the_placeholder() {
        assert_eq!(recover_baked(TEMPLATE).as_deref(), Some(PLACEHOLDER));
    }

    /// The other half of the trap: real drift must not be waved through.
    #[test]
    fn genuine_drift_is_detected() {
        let mut edited = render("/opt/amont");
        edited.push_str("\n# someone added this\n");
        assert_eq!(recover_baked(&edited), None);

        let hand_written = "#!/bin/sh\nBIN=\"/opt/amont\"\nexec \"$BIN\" \"$@\"\n";
        assert_eq!(
            recover_baked(hand_written),
            None,
            "a plausible-looking file that is not our template must not pass"
        );
    }

    /// A candidate that reproduces the file is required — finding `BIN="…"` is
    /// not enough on its own.
    #[test]
    fn the_anchor_alone_does_not_satisfy_it() {
        let faked = render("/opt/a").replace("exec", "# exec");
        assert!(faked.contains("BAKED=\"/opt/a\""));
        assert_eq!(recover_baked(&faked), None);
    }

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fleet-shim-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("mkdir");
        d
    }

    /// The verified incident, at the level of one classification. A dispatcher
    /// that is a LINK to a healthy shim used to read as `Ok` — `read_to_string`
    /// follows links — so nothing in the dashboard or the plan could see that a
    /// write here lands somewhere else entirely.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_dispatcher_is_a_symlink_not_a_healthy_shim() {
        let d = tmpdir("symlink");
        let real = d.join("shared-pre-commit");
        std::fs::write(&real, render("/bin/gh")).unwrap();
        let link = d.join("pre-commit");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        assert_eq!(
            classify(&link),
            ShimState::Symlink {
                target: Some(real.clone())
            },
            "a link to a perfect shim is still a link"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A compiled hook is not "missing", and the difference matters: `Missing`
    /// is the state that makes `fix` decide to WRITE one.
    #[test]
    fn a_binary_hook_is_unreadable_not_missing() {
        let d = tmpdir("binary");
        let p = d.join("pre-commit");
        std::fs::write(&p, [0x7f, b'E', b'L', b'F', 0x02, 0x01, 0xff, 0xfe]).unwrap();
        assert!(
            matches!(classify(&p), ShimState::Unreadable { .. }),
            "classified as {:?}",
            classify(&p)
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    fn ok(p: &str) -> ShimState {
        ShimState::Ok { baked: p.into() }
    }

    #[test]
    fn bake_states() {
        assert_eq!(bake_state(&[ok("/bin/gh")], "/bin/gh"), BakeState::Current);
        assert_eq!(
            bake_state(&[ok("/old/gh")], "/bin/gh"),
            BakeState::Stale {
                path: "/old/gh".into()
            }
        );
        assert_eq!(
            bake_state(&[ok(PLACEHOLDER)], "/bin/gh"),
            BakeState::Unbaked
        );
        assert_eq!(
            bake_state(&[ok("/a"), ok("/b")], "/bin/gh"),
            BakeState::Mixed
        );
        assert_eq!(
            bake_state(&[ShimState::Missing], "/bin/gh"),
            BakeState::None
        );
        // Four identical shims are Current, not Mixed.
        assert_eq!(
            bake_state(
                &[ok("/bin/gh"), ok("/bin/gh"), ok("/bin/gh"), ok("/bin/gh")],
                "/bin/gh"
            ),
            BakeState::Current
        );
    }
}
