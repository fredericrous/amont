//! The checks whose assertions are about ELAPSED TIME.
//!
//! `amont` runs a check under two clocks: a silence budget (`amont.idleTimeout`
//! — a tool that has printed nothing for N seconds is presumed stuck) and a
//! total ceiling (`amont.timeout`). The tests below prove the wiring by
//! actually spending the seconds, which makes them the only tests in this
//! suite that can fail because of what ELSE was running.
//!
//! # Why they are their own binary
//!
//! They used to live in `external.rs`. `a_chatty_check_outlives_the_idle_budget`
//! ticks every 0.2s for fifteen seconds against a ten-second budget, and its
//! neighbours there held `sleep 300` — so it competed for a scheduler slot with
//! twenty-odd tests each spawning `amont`, `git` and shell subprocesses. It
//! failed roughly two runs in three, and adding just two unrelated tests to
//! that file took it to three failures in three.
//!
//! `cargo` runs test BINARIES sequentially, so a file of their own hands them
//! the machine. The `SEQUENTIAL` lock then keeps them from competing with each
//! other, which matters because two of them park a `sleep 300` while a third is
//! trying to be scheduled every 200ms.
//!
//! Neither half is a retry and neither weakens an assertion: the budgets, the
//! tick rate and every expectation are exactly as they were. What changed is
//! only how much else is happening on the machine while the clock is read.
//!
//! The *decisions* these tests exercise are pinned to the second, without any
//! sleeping, by `common::tests::the_clocks_judge_silence_and_ceiling_separately`
//! in the runtime. These prove the wiring end to end.

mod common;

use common::Repo;
use std::process::Command;
use std::sync::{Mutex, MutexGuard};

/// One timing test at a time.
///
/// `PoisonError` is unwrapped through deliberately: a panicking test poisons
/// the lock, and turning every subsequent test into a second failure would
/// bury the first one — which is the report that actually says what broke.
static SEQUENTIAL: Mutex<()> = Mutex::new(());

fn alone() -> MutexGuard<'static, ()> {
    SEQUENTIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Trust is a separate decision, so every test about what a declared check
/// does has to make it first — otherwise they would all be testing the trust
/// gate instead.
fn manifest(r: &Repo, body: &str) {
    r.stage("amont.conf", body);
    let out = Command::new(env!("CARGO_BIN_EXE_amont"))
        .arg("trust")
        .current_dir(r.path(""))
        .output()
        .expect("amont trust");
    assert!(out.status.success(), "could not trust the manifest");
}

/// One hung tool must not hold the commit — and the parked unstaged work —
/// hostage. The budget kills it and the check FAILS, loudly, with the config
/// key that raises the budget named.
#[cfg(unix)]
#[test]
fn a_check_that_outlives_the_budget_is_killed_and_fails() {
    let _alone = alone();
    let r = Repo::new();
    // `exec`, so the sleep IS the spawned process rather than a grandchild:
    // the kill only reaches the direct child, and a grandchild inheriting the
    // harness's output pipe would hold this TEST hostage the way no real git
    // invocation can (git lends hooks its own stdio, it does not read a pipe).
    let body = "#!/bin/sh\nexec sleep 300\n";
    r.stage("slow.sh", body);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(r.path("slow.sh"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
    r.git(&["add", "slow.sh"]);
    manifest(&r, "pre-commit  slowpoke  *  block  ./slow.sh\n");
    r.git(&["config", "amont.timeout", "1"]);

    let started = std::time::Instant::now();
    let run = r.hook("pre-commit", &[]);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(60),
        "the deadline never fired"
    );
    assert!(!run.passed(), "a killed check must fail:\n{}", run.output());
    assert!(
        run.says("timed out") && run.says("amont.timeout"),
        "must say what happened and how to change the budget:\n{}",
        run.output()
    );
}

/// The other clock. A tool that goes quiet is stuck; the silence budget
/// kills it, the check fails, and the message names the silence — not the
/// wall clock, which is off here and must not be blamed.
#[cfg(unix)]
#[test]
fn a_silent_check_is_killed_by_the_idle_budget() {
    let _alone = alone();
    let r = Repo::new();
    let body = "#!/bin/sh\nexec sleep 300\n";
    r.stage("quiet.sh", body);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(r.path("quiet.sh"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
    r.git(&["add", "quiet.sh"]);
    manifest(&r, "pre-commit  quiet  *  block  ./quiet.sh\n");
    r.git(&["config", "amont.timeout", "0"]);
    r.git(&["config", "amont.idleTimeout", "1"]);

    let started = std::time::Instant::now();
    let run = r.hook("pre-commit", &[]);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(60),
        "the silence budget never fired"
    );
    assert!(!run.passed(), "a killed check must fail:\n{}", run.output());
    assert!(
        run.says("printed nothing") && run.says("amont.idleTimeout"),
        "must blame the silence and name its key:\n{}",
        run.output()
    );
    assert!(
        !run.says("amont.timeout <secs>"),
        "the ceiling was off and must not be blamed:\n{}",
        run.output()
    );
}

/// The point of the second clock: a tool that keeps talking is slow, not
/// stuck, and outlives a silence budget shorter than its total run.
#[cfg(unix)]
#[test]
fn a_chatty_check_outlives_the_idle_budget() {
    let _alone = alone();
    let r = Repo::new();
    let body =
        "#!/bin/sh\ni=0\nwhile [ $i -lt 75 ]; do i=$((i+1)); echo tick $i; sleep 0.2; done\n";
    r.stage("chatty.sh", body);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(r.path("chatty.sh"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
    r.git(&["add", "chatty.sh"]);
    manifest(&r, "pre-commit  chatty  *  block  ./chatty.sh\n");
    // Fifteen seconds of ticks every 0.2s against a ten-second budget. The
    // 50x margin is for a machine running the whole suite at once, where a
    // `sleep 0.2` has been seen to take 25 times that — never for the tool
    // itself, which is the thing being measured. The decision itself is
    // pinned to the second by `common::tests::the_clocks_judge_silence_and_
    // ceiling_separately`; this test only proves the wiring end to end.
    r.git(&["config", "amont.idleTimeout", "10"]);

    let run = r.hook("pre-commit", &[]);
    assert!(
        run.passed(),
        "fifteen seconds of steady output must not trip a ten-second silence budget:\n{}",
        run.output()
    );
    assert!(!run.says("killed"), "{}", run.output());
}

/// THE interleave regression: two concurrent checks, each printing around a
/// deliberate pause, must come out as two CONTIGUOUS blocks. Against the
/// pre-capture code this fails almost every run — both probes are mid-sleep
/// together and their second lines land across each other's.
#[cfg(unix)]
#[test]
fn concurrent_checks_emit_contiguous_blocks() {
    let _alone = alone();
    use std::os::unix::fs::PermissionsExt;
    let r = Repo::new();
    for name in ["alpha", "beta"] {
        let file = format!("{name}.sh");
        r.stage(
            &file,
            &format!("#!/bin/sh\necho {name}-first\nsleep 1\necho {name}-second\nexit 0\n"),
        );
        std::fs::set_permissions(r.path(&file), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
        r.git(&["add", &file]);
    }
    manifest(
        &r,
        "pre-commit  alpha  *  warn  ./alpha.sh\n\
         pre-commit  beta  *  warn  ./beta.sh\n",
    );

    let run = r.hook("pre-commit", &[]);
    assert!(run.passed(), "{}", run.output());
    let out = run.output();
    for (name, other) in [("alpha", "beta"), ("beta", "alpha")] {
        let first = out
            .find(&format!("{name}-first"))
            .unwrap_or_else(|| panic!("{name}-first missing:\n{out}"));
        let second = out
            .find(&format!("{name}-second"))
            .unwrap_or_else(|| panic!("{name}-second missing:\n{out}"));
        assert!(first < second, "{name}'s lines arrived reversed:\n{out}");
        assert!(
            !out[first..second].contains(other),
            "{name}'s block was interleaved with {other}'s:\n{out}"
        );
    }
}
