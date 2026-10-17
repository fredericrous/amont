//! A guard for a setting no other test can see.
//!
//! Cargo does not apply `[profile.release]`'s `panic` key to test targets, so
//! `a_panicking_check_blocks_the_commit` and `a_thread_that_dies_leaves_a_failure_behind`
//! pass identically whether or not the shipped binary unwinds. They were
//! passing while the release build aborted, which made both of them assertions
//! about a configuration that was not the one being shipped.
//!
//! Reading the manifest is therefore not a workaround, it is the only place the
//! question can be asked. Same shape as `registry`'s
//! `the_shipped_shims_are_exactly_the_git_invoked_hooks`, which reads
//! `templates/hooks/` for the same reason.

/// The workspace manifest, found relative to this crate rather than to the
/// process's working directory.
fn workspace_manifest() -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// The lines of `[profile.release]`, up to the next section header.
fn release_profile(manifest: &str) -> Vec<&str> {
    manifest
        .lines()
        .skip_while(|l| l.trim() != "[profile.release]")
        .skip(1)
        .take_while(|l| !l.trim_start().starts_with('['))
        .collect()
}

/// `panic = "abort"` turns two designed-in safety nets into dead code, in the
/// build that is actually installed and nowhere else.
#[test]
fn the_release_profile_does_not_abort_on_panic() {
    let manifest = workspace_manifest();
    let profile = release_profile(&manifest);
    assert!(
        !profile.is_empty(),
        "no [profile.release] section found — has the manifest moved?"
    );
    for line in &profile {
        let code = line.split('#').next().unwrap_or("").trim();
        assert!(
            !code.starts_with("panic"),
            "[profile.release] sets {code:?}. `panic = \"abort\"` disables \
             dispatch's catch_unwind AND Drop for StagedOnly in the shipped \
             binary, and no cargo test can detect it — see the comment in \
             Cargo.toml."
        );
    }
}

/// The fixture above only means anything if it is reading the right section.
#[test]
fn the_release_profile_reader_finds_the_settings_that_are_there() {
    let manifest = workspace_manifest();
    let profile = release_profile(&manifest).join("\n");
    assert!(
        profile.contains("strip") && profile.contains("lto"),
        "the section reader did not find the known settings: {profile:?}"
    );
}
