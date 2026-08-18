//! The large-file guard, driven through the real binary with the
//! thresholds turned down so the fixtures stay small.

mod common;
use common::Repo;

fn run_check(r: &Repo) -> (i32, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("--hooks-dir")
        .arg(r.path(".git/hooks"))
        .arg("pre-commit-large-files")
        .current_dir(&r.dir)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn stage_of_size(r: &Repo, name: &str, mb: usize) {
    r.stage(name, &"x".repeat(mb * 1024 * 1024));
}

/// Over the block threshold refuses, naming the file, the size, and the
/// remedy.
#[test]
fn an_oversized_file_blocks_with_the_remedy_named() {
    let r = Repo::new();
    r.git(&["config", "amont.largeFileBlock", "2"]);
    stage_of_size(&r, "dataset.bin", 3);
    let (code, out) = run_check(&r);
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("dataset.bin"), "{out}");
    assert!(out.contains("git-lfs"), "the remedy is named: {out}");
}

/// Between warn and block: named, never blocking — a large asset can be
/// deliberate, and this is the moment to decide.
#[test]
fn a_merely_large_file_warns_and_passes() {
    let r = Repo::new();
    r.git(&["config", "amont.largeFileWarn", "1"]);
    r.git(&["config", "amont.largeFileBlock", "100"]);
    stage_of_size(&r, "big.pdf", 2);
    let (code, out) = run_check(&r);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("big.pdf"), "{out}");
    assert!(out.contains("every future clone"), "{out}");
}

#[test]
fn ordinary_files_pass_quietly() {
    let r = Repo::new();
    r.stage("small.txt", "hello\n");
    let (code, out) = run_check(&r);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("No oversized files staged"), "{out}");
}
