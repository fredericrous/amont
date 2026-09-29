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

// --- Busy is not stuck (ADR-0008, `hooks.liveness`) --------------------------
//
// A silent check whose process tree is measurably working is not killed by
// the silence budget; it answers to the ceiling. Every fixture below is built
// so that NOTHING depends on amont returning: the root stays alive for the
// whole run (it is what amont waits on), every worker stops itself within
// twenty seconds, the scripts kill their own group on exit, and the harness
// runs amont in a process group of its own under a watchdog that SIGKILLs
// all of it and fails the test at sixty seconds.
//
// Budgets: idle 4 s, ceiling 12 s. A check the silence budget wrongly kills
// dies at ~4 s; one kept alive by its CPU dies at the ceiling, ~12 s. Four
// seconds, not two: in a full workspace run a fork can stall for a second or
// more on a loaded machine, and a process blocked in exec burns no CPU — the
// tree then legitimately looks idle, and a two-second budget has no slack.
// The fixtures run as `sh ./x.sh`, not `./x.sh`: macOS can hold a freshly
// written executable in execve for seconds while it is assessed, most of all
// right after a test run has created hundreds of new files. Such a process
// has done no work — idle, correctly — so exec'ing the script directly made
// these tests fail intermittently with the check killed before its first
// line (the sampler saw one root, under 1 ms of CPU, no children). Running
// the system `sh` and reading the script as data takes the fresh file out of
// the exec path.
// Each busy fixture also prints one line once its burn is running, so its
// own start-up is not counted as silence, and runs under AMONT_CPU_TRACE so
// a failure shows what the sampler saw.

#[cfg(any(target_os = "linux", target_os = "macos"))]
const WATCHDOG: std::time::Duration = std::time::Duration::from_secs(60);

/// A busy loop in `sh` that stops itself after `secs`: a burst of pure
/// arithmetic between clock checks, so it spends its time on CPU rather
/// than spawning. It says `burning` once, when the loop is about to start.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn burn(secs: u32) -> String {
    format!(
        "end=$(( $(date +%s) + {secs} ))\n\
         echo burning\n\
         while :; do i=0; while [ $i -lt 20000 ]; do i=$((i+1)); done; \
         [ \"$(date +%s)\" -ge \"$end\" ] && break; done\n"
    )
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn script(r: &Repo, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    r.stage(name, &format!("#!/bin/sh\ntrap 'kill 0' EXIT\n{body}"));
    std::fs::set_permissions(r.path(name), std::fs::Permissions::from_mode(0o755)).expect("chmod");
    r.git(&["add", name]);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn busy_budgets(r: &Repo) {
    r.git(&["config", "amont.idleTimeout", "4"]);
    r.git(&["config", "amont.timeout", "12"]);
}

/// A fresh trace file for one test.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn trace_file(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("amont-cpu-trace-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn killed_at_the_ceiling(
    run: &common::HookRun,
    took: std::time::Duration,
    trace: &std::path::Path,
) {
    let seen = std::fs::read_to_string(trace).unwrap_or_default();
    let _ = std::fs::remove_file(trace);
    assert!(!run.passed(), "a killed check must fail:\n{}", run.output());
    assert!(
        took >= std::time::Duration::from_secs(11),
        "killed after {took:?}: the silence budget fired on a busy check:\n{}\n\
         what the sampler saw (pid start ppid cpu_ns):\n{seen}",
        run.output()
    );
    assert!(
        run.says("timed out") && run.says("kept its CPU busy"),
        "must be the ceiling, naming the busy tree:\n{}\nsampler:\n{seen}",
        run.output()
    );
}

/// The case this exists for: silent, and working in the root itself.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_silent_busy_check_answers_to_the_ceiling_not_the_silence_budget() {
    let _alone = alone();
    let r = Repo::new();
    script(&r, "busy.sh", &burn(24));
    manifest(&r, "pre-commit  busy  *  block  sh ./busy.sh\n");
    busy_budgets(&r);
    let trace = trace_file("busy");
    let (run, took) = r.hook_watched(
        "pre-commit",
        &[("AMONT_CPU_TRACE", trace.as_os_str())],
        WATCHDOG,
    );
    killed_at_the_ceiling(&run, took, &trace);
}

/// Work done in short-lived children the root forks and reaps one after
/// another — a test runner's worker per file. A sum over LIVE processes
/// misses all of it; the parent's reaped-children time does not.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn fork_per_file_work_keeps_a_silent_check_alive() {
    let _alone = alone();
    let r = Repo::new();
    let body = "end=$(( $(date +%s) + 24 ))\n\
                echo burning\n\
                while [ \"$(date +%s)\" -lt \"$end\" ]; do \
                sh -c 'i=0; while [ $i -lt 3000 ]; do i=$((i+1)); done'; done\n";
    script(&r, "churn.sh", body);
    manifest(&r, "pre-commit  churn  *  block  sh ./churn.sh\n");
    busy_budgets(&r);
    let trace = trace_file("churn");
    let (run, took) = r.hook_watched(
        "pre-commit",
        &[("AMONT_CPU_TRACE", trace.as_os_str())],
        WATCHDOG,
    );
    killed_at_the_ceiling(&run, took, &trace);
}

/// A worker whose parent exits is reparented away from the tree. Once the
/// sampler has SEEN it, it keeps counting. The intermediate waits until the
/// worker's pid appears in `AMONT_CPU_TRACE` before it exits, so the test
/// never depends on winning the discovery race — which the guarantee does
/// not cover (a worker orphaned before it was ever seen is invisible). The
/// root then idles in `sleep`, so the only CPU left is the orphan's.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_seen_worker_keeps_counting_after_its_parent_exits() {
    let _alone = alone();
    let r = Repo::new();
    let trace = trace_file("orphan");
    let worker = burn(24).trim_end().replace('\n', "; ");
    let body = format!(
        "sh -c '({worker}) & w=$!; n=0; \
         while [ $n -lt 200 ]; do grep -q \"^$w \" \"$AMONT_CPU_TRACE\" 2>/dev/null && break; \
         n=$((n+1)); sleep 0.05; done'\n\
         sleep 24\n"
    );
    script(&r, "orphan.sh", &body);
    manifest(&r, "pre-commit  orphan  *  block  sh ./orphan.sh\n");
    busy_budgets(&r);
    let (run, took) = r.hook_watched(
        "pre-commit",
        &[("AMONT_CPU_TRACE", trace.as_os_str())],
        WATCHDOG,
    );
    killed_at_the_ceiling(&run, took, &trace);
}

/// A tool blocked on nothing — the hang the silence budget exists for —
/// still dies at the budget, and the message now says the CPU was measured
/// idle, naming the span the claim rests on.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn an_idle_silent_check_still_dies_at_the_budget_and_says_it_was_measured() {
    let _alone = alone();
    let r = Repo::new();
    // `exec`: sleep IS the root. A child sleep would outlive the kill and hold
    // amont's output pipe until it ended — the grandchild limitation this
    // change does not address.
    script(&r, "idle.sh", "exec sleep 30\n");
    manifest(&r, "pre-commit  idle  *  block  ./idle.sh\n");
    r.git(&["config", "amont.idleTimeout", "3"]);
    r.git(&["config", "amont.timeout", "0"]);
    let (run, took) = r.hook_watched("pre-commit", &[], WATCHDOG);
    assert!(!run.passed(), "{}", run.output());
    assert!(
        took < std::time::Duration::from_secs(10),
        "took {took:?}:\n{}",
        run.output()
    );
    assert!(
        run.says("printed nothing") && run.says("did no measurable CPU work"),
        "must say it was silent AND measured idle:\n{}",
        run.output()
    );
}

/// `amont.idleCpuCredit false` restores the silence-only rule: the same busy
/// check dies at the budget, and the message makes no CPU claim.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn with_cpu_credit_off_a_busy_silent_check_dies_at_the_budget() {
    let _alone = alone();
    let r = Repo::new();
    script(&r, "busy.sh", &burn(20));
    manifest(&r, "pre-commit  busy  *  block  sh ./busy.sh\n");
    busy_budgets(&r);
    r.git(&["config", "amont.idleCpuCredit", "false"]);
    let (run, took) = r.hook_watched("pre-commit", &[], WATCHDOG);
    assert!(!run.passed(), "{}", run.output());
    assert!(
        took < std::time::Duration::from_secs(7),
        "took {took:?}:\n{}",
        run.output()
    );
    assert!(
        run.says("printed nothing for 4s and was killed"),
        "{}",
        run.output()
    );
    assert!(
        !run.says("CPU"),
        "no CPU claim when CPU is not sampled:\n{}",
        run.output()
    );
}
