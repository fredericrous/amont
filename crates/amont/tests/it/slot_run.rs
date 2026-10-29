//! `amont slot run` and `amont slots` (ADR-0014): any command queues for a
//! host slot, and the queue says who holds it.
//!
//! Unpinned, like `busy_defers.rs`: `.cargo/config.toml` sets
//! `AMONT_HOST_SLOT=held` for the test process, which would make every
//! wrapper nested. Each test removes it and uses a slot directory of its own
//! with one fixed slot, so no load test or ramp is involved.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

fn slot_dir(tag: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("amont-slotrun-{}-{tag}-{seq}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

fn amont(slots: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_amont"));
    cmd.args(args)
        .env_remove("AMONT_HOST_SLOT")
        .env("AMONT_SLOT_DIR", slots)
        .env("AMONT_HOST_SLOTS", "1");
    cmd
}

fn spawn(slots: &Path, args: &[&str]) -> Child {
    amont(slots, args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn amont")
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Wait until `amont slots --json` shows a holder, so a test never races the
/// first wrapper's admission with a sleep.
fn until_held(slots: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let out = amont(slots, &["slots", "--json"])
            .output()
            .expect("amont slots");
        let json = String::from_utf8_lossy(&out.stdout).into_owned();
        if json.contains("\"held\":1") {
            return json;
        }
        assert!(Instant::now() < deadline, "no holder appeared: {json}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// One slot, two wrapped commands: the second waits, says whom it waits
/// behind, and runs after the first; each exit status passes through.
#[test]
fn a_second_command_queues_behind_the_first_and_names_it() {
    let slots = slot_dir("queue");
    let first = spawn(
        &slots,
        &["slot", "run", "--", "sh", "-c", "sleep 3; exit 3"],
    );
    let json = until_held(&slots);
    assert!(json.contains("\"tool\":\"amont slot run\""), "{json}");
    assert!(json.contains("sleep 3"), "{json}");
    let second = amont(
        &slots,
        &["slot", "run", "--", "sh", "-c", "echo second-ran"],
    )
    .output()
    .expect("second");
    let out = text(&second);
    assert_eq!(second.status.code(), Some(0), "{out}");
    assert!(
        out.contains("queued behind amont slot run: sh -c sleep 3"),
        "{out}"
    );
    assert!(out.contains("second-ran"), "{out}");
    let first = first.wait_with_output().expect("first");
    assert_eq!(first.status.code(), Some(3), "{}", text(&first));
    let waits = std::fs::read_to_string(slots.join("waits.jsonl")).unwrap_or_default();
    assert_eq!(
        waits.matches("\"outcome\":\"admitted\"").count(),
        2,
        "{waits}"
    );
    let _ = std::fs::remove_dir_all(&slots); // best-effort: scratch
}

/// A wait past `--max-wait` exits 75 and runs nothing; `--on-timeout run`
/// runs it unqueued, and says so.
#[test]
fn a_timed_out_wait_exits_75_without_running_anything() {
    let slots = slot_dir("timeout");
    let mut holder = spawn(&slots, &["slot", "run", "--", "sleep", "4"]);
    until_held(&slots);
    let marker = slots.join("ran");
    let out = amont(
        &slots,
        &[
            "slot",
            "run",
            "--max-wait",
            "1",
            "--",
            "touch",
            marker.to_str().unwrap(),
        ],
    )
    .output()
    .expect("timed out");
    assert_eq!(out.status.code(), Some(75), "{}", text(&out));
    assert!(!marker.exists(), "nothing may run on a timeout");
    let out = amont(
        &slots,
        &[
            "slot",
            "run",
            "--max-wait",
            "1",
            "--on-timeout",
            "run",
            "--",
            "touch",
            marker.to_str().unwrap(),
        ],
    )
    .output()
    .expect("unqueued");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(marker.exists(), "--on-timeout run runs it");
    assert!(text(&out).contains("running unqueued"), "{}", text(&out));
    let _ = holder.wait(); // best-effort: reap the holder
    let _ = std::fs::remove_dir_all(&slots); // best-effort: scratch
}

/// A wrapper under a held slot does not queue for the slot its parent holds,
/// and the wrapped command's own flags are its own.
#[test]
fn a_nested_wrapper_runs_at_once_and_flags_after_dashes_pass_through() {
    let slots = slot_dir("nested");
    let mut holder = spawn(&slots, &["slot", "run", "--", "sleep", "3"]);
    until_held(&slots);
    let started = Instant::now();
    let out = amont(
        &slots,
        &[
            "slot",
            "run",
            "--",
            "sh",
            "-c",
            "echo got:\"$1\"",
            "x",
            "--help",
        ],
    )
    .env("AMONT_HOST_SLOT", "held")
    .output()
    .expect("nested");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(text(&out).contains("got:--help"), "{}", text(&out));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "a nested wrapper queued"
    );
    let _ = holder.wait(); // best-effort: reap the holder
    let _ = std::fs::remove_dir_all(&slots); // best-effort: scratch
}

/// The wrapper alone holds the slot: a daemon the command leaves behind
/// (`setsid`, detached) does not keep it once the command exits.
#[test]
fn a_detached_daemon_does_not_keep_the_slot() {
    let slots = slot_dir("daemon");
    let out = amont(
        &slots,
        &[
            "slot",
            "run",
            "--",
            "sh",
            "-c",
            "(sleep 20 >/dev/null 2>&1 &); exit 0",
        ],
    )
    .output()
    .expect("daemon");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let started = Instant::now();
    let next = amont(&slots, &["slot", "run", "--max-wait", "5", "--", "true"])
        .output()
        .expect("next");
    assert_eq!(next.status.code(), Some(0), "{}", text(&next));
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "the daemon held the slot"
    );
    let _ = std::fs::remove_dir_all(&slots); // best-effort: scratch
}
