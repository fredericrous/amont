//! The land journey: `amont land` from a branch with work on it to a branch
//! the remote is proven to hold.
//!
//! `land` pushes the current branch to its one push URL, then reads the
//! remote back and says whether the ref is where it pushed it. It neither
//! runs nor skips the gates: those are whatever `pre-push` the push runs. So
//! this journey uses the fixture of `land.rs` (a clone on `feat`, one commit
//! ahead of a pushed `main`, a bare `origin` inside its `.git`) without
//! installing amont, and where it needs a gate it writes the `pre-push` hook
//! by hand. A hook is a shell script, so those steps are unix-only, as they
//! were in `land.rs`.
//!
//! The journey, in order:
//! 1. a usage error is exit 2, with a status on stdout under `--json`;
//! 2. `--dry-run`: the refusals (a dirty tree, two push URLs, `main`) and,
//!    with none left, what a land would push and where, touching nothing;
//! 3. the same refusals for real, each writing a refused receipt, and a
//!    gate's refusal reaching the person with nothing moved;
//! 4. the land: proven, tracked, upstream set, one receipt, stdout the JSON
//!    document alone;
//! 5. a rewrite is refused until `--force`, which leases on what it read;
//! 6. the remote: unreachable is exit 3, and the push URL is the one proven;
//! 7. upstreams: a branch tracking `main` lands under its own name, a renamed
//!    one under its upstream's name, a long name fits the verdict line.
//!
//! The faults this journey does not meet (another writer, a remote that lies
//! after the push, the retry) stay in `land.rs`, each on its own fixture.

#[cfg(unix)]
use crate::land::hook;
use crate::land::{
    field, fixture, head, land, plain_git, receipt_field, receipts, status_of, tip, Fx,
};
use std::path::PathBuf;

fn remote_ref_exists(fx: &Fx, name: &str) -> bool {
    !plain_git(&fx.r.dir, &["for-each-ref", name]).is_empty()
}

fn on_bare(fx: &Fx, branch: &str) -> bool {
    plain_git(&fx.bare, &["branch", "--list", branch]).contains(branch)
}

fn bare_url(fx: &Fx) -> &str {
    fx.bare.to_str().expect("utf8")
}

/// The receipts `before` did not have, after checking there is exactly one
/// more of them: one receipt per land.
fn one_new_receipt(fx: &Fx, before: &[String], step: &str) -> String {
    let after = receipts(fx);
    assert_eq!(
        after.len(),
        before.len() + 1,
        "step `{step}`: one land, one receipt: {after:?}"
    );
    let new: Vec<&String> = after.iter().filter(|r| !before.contains(r)).collect();
    assert_eq!(new.len(), 1, "step `{step}`: {after:?}");
    new[0].clone()
}

/// One repository walks the whole journey.
///
/// Slower than a second: it is a flow, about thirty runs of `amont land` on
/// one repository, standing in for the eighteen fixtures its steps used to
/// build one by one.
#[test]
fn the_land_journey() {
    let fx = fixture("feat");

    // ---- 1. usage ---------------------------------------------------------

    let bogus = land(&fx, &["--bogus"]);
    step_eq!("usage", bogus.code, 2);
    let json = land(&fx, &["--bogus", "--json"]);
    step_eq!("usage", json.code, 2);
    step_eq!("usage", status_of(&json), "usage");
    let extra = land(&fx, &["origin", "--json"]);
    step_eq!("usage", extra.code, 2);
    step_eq!("usage", status_of(&extra), "usage");

    // ---- 2. the dry run ---------------------------------------------------

    // Refused on a dirty tree, two push URLs and `main`, and nothing written.
    fx.r.write("b.txt", "edited\n");
    let dirty = land(&fx, &["--dry-run", "--json"]);
    step_eq!("dry dirty", dirty.code, 1, "{}", dirty.stderr);
    step_eq!("dry dirty", status_of(&dirty), "refused");
    step!(
        "dry dirty",
        dirty.stderr.contains("git stash"),
        "{}",
        dirty.stderr
    );
    fx.r.git(&["checkout", "-q", "--", "b.txt"]);

    fx.r.git(&["config", "--add", "remote.origin.pushurl", bare_url(&fx)]);
    fx.r.git(&[
        "config",
        "--add",
        "remote.origin.pushurl",
        "/nowhere/else.git",
    ]);
    let two = land(&fx, &["--dry-run", "--json"]);
    step_eq!("dry two urls", two.code, 1, "{}", two.stderr);
    step_eq!("dry two urls", status_of(&two), "refused");
    step!(
        "dry two urls",
        two.stderr.contains("push destinations"),
        "{}",
        two.stderr
    );
    fx.r.git(&["config", "--unset-all", "remote.origin.pushurl"]);

    fx.r.git(&["switch", "-q", "main"]);
    let main_before = tip(&fx, "main");
    let on_main = land(&fx, &["--dry-run", "--json"]);
    step_eq!("dry main", on_main.code, 1, "{}", on_main.stderr);
    step_eq!("dry main", status_of(&on_main), "refused");
    step!(
        "dry main",
        on_main.stderr.contains("git switch -c"),
        "{}",
        on_main.stderr
    );
    let last = on_main.stderr.lines().last().expect("a verdict line");
    step!("dry main", last.starts_with("refused"), "{last}");
    step_eq!("dry main", tip(&fx, "main"), main_before);
    step!("dry main", !on_bare(&fx, "feat"));
    step!(
        "dry main",
        !remote_ref_exists(&fx, "refs/remotes/origin/feat")
    );
    step!(
        "dry main",
        receipts(&fx).is_empty(),
        "a dry run writes no receipt"
    );
    fx.r.git(&["switch", "-q", "feat"]);

    // Nothing to refuse: what a land would do, on a branch with no upstream.
    let ran = land(&fx, &["--dry-run", "--json"]);
    step_eq!("dry run", ran.code, 0, "{}", ran.stderr);
    step_eq!("dry run", status_of(&ran), "would-land");
    step_eq!("dry run", field(&ran, "dst"), "refs/heads/feat");
    step_eq!("dry run", field(&ran, "pushed"), head(&fx));
    step_eq!("dry run", field(&ran, "branch"), "feat");
    step_eq!("dry run", field(&ran, "remote"), "origin");
    step_eq!("dry run", field(&ran, "push_url"), bare_url(&fx));
    step!(
        "dry run",
        ran.stdout.contains("\"retried\":false"),
        "{}",
        ran.stdout
    );
    step!(
        "dry run",
        ran.stdout.contains("\"lease\":false"),
        "{}",
        ran.stdout
    );
    step!(
        "dry run",
        field(&ran, "receipt_dir").ends_with("amont/land")
            || field(&ran, "receipt_dir").ends_with("amont\\land"),
        "{}",
        ran.stdout
    );
    let last = ran.stderr.lines().last().expect("a verdict line");
    step!("dry run", last.starts_with("would-land"), "{last}");
    step!(
        "dry run",
        !on_bare(&fx, "feat"),
        "a dry run must not create the branch"
    );
    step!(
        "dry run",
        !remote_ref_exists(&fx, "refs/remotes/origin/feat")
    );
    step!(
        "dry run",
        receipts(&fx).is_empty(),
        "a dry run writes no receipt"
    );

    let forced = land(&fx, &["--dry-run", "--force", "--json"]);
    step_eq!("dry force", forced.code, 0, "{}", forced.stderr);
    step_eq!("dry force", status_of(&forced), "would-land");
    step!(
        "dry force",
        forced.stdout.contains("\"lease\":true"),
        "{}",
        forced.stdout
    );

    // ---- 3. the refusals, for real ----------------------------------------

    fx.r.write("b.txt", "edited\n");
    let dirty = land(&fx, &["--json"]);
    step_eq!("dirty", dirty.code, 1);
    step_eq!("dirty", status_of(&dirty), "refused");
    step!(
        "dirty",
        dirty.stderr.contains("git stash"),
        "{}",
        dirty.stderr
    );
    let all = receipts(&fx);
    step_eq!("dirty", all.len(), 1, "{all:?}");
    step_eq!("dirty", receipt_field(&all[0], "status"), "refused");
    step_eq!("dirty", receipt_field(&all[0], "branch"), "feat");
    fx.r.git(&["checkout", "-q", "--", "b.txt"]);

    fx.r.git(&["switch", "-q", "main"]);
    let on_main = land(&fx, &["--json"]);
    step_eq!("main", on_main.code, 1);
    step!(
        "main",
        on_main.stderr.contains("git switch -c"),
        "{}",
        on_main.stderr
    );
    fx.r.git(&["switch", "-q", "feat"]);

    fx.r.git(&["config", "--add", "remote.origin.pushurl", bare_url(&fx)]);
    fx.r.git(&[
        "config",
        "--add",
        "remote.origin.pushurl",
        "/nowhere/else.git",
    ]);
    let two = land(&fx, &["--json"]);
    step_eq!("two urls", two.code, 1, "{}", two.stderr);
    step_eq!("two urls", status_of(&two), "refused");
    step!(
        "two urls",
        two.stderr.contains("push destinations"),
        "{}",
        two.stderr
    );
    fx.r.git(&["config", "--unset-all", "remote.origin.pushurl"]);

    // The gate says no: its words reach the person, and nothing moves.
    #[cfg(unix)]
    {
        hook(&fx, "echo 'gate says no' >&2; exit 1");
        let ran = land(&fx, &["--json"]);
        step_eq!("gate refuses", ran.code, 1);
        step_eq!("gate refuses", status_of(&ran), "refused");
        step!(
            "gate refuses",
            ran.stderr.contains("gate says no"),
            "{}",
            ran.stderr
        );
        step!(
            "gate refuses",
            !on_bare(&fx, "feat"),
            "a refused push must not create the branch"
        );
        let tracking =
            fx.r.git(&["rev-parse", "--verify", "-q", "refs/remotes/origin/feat"]);
        step!(
            "gate refuses",
            !tracking.status.success(),
            "tracking ref must stay absent"
        );
        // From here the gate passes, and says so.
        hook(&fx, "echo 'hook says hello'");
    }

    // ---- 4. the land ------------------------------------------------------

    let before = receipts(&fx);
    let ran = land(&fx, &["--json"]);
    step_eq!("land", ran.code, 0, "{}", ran.stderr);
    step_eq!("land", status_of(&ran), "landed");
    step_eq!("land", tip(&fx, "feat"), head(&fx));
    step_eq!("land", field(&ran, "remote_after"), head(&fx));
    step_eq!("land", field(&ran, "pushed"), head(&fx));
    step_eq!("land", field(&ran, "remote_before"), "");
    // A URL push moves no tracking ref; land does, and sets the upstream.
    step_eq!(
        "land",
        plain_git(&fx.r.dir, &["rev-parse", "refs/remotes/origin/feat"]),
        head(&fx)
    );
    step_eq!(
        "land",
        plain_git(&fx.r.dir, &["rev-parse", "--abbrev-ref", "feat@{u}"]),
        "origin/feat"
    );
    #[cfg(unix)]
    {
        step!(
            "land",
            ran.stdout.trim().starts_with('{') && ran.stdout.trim().ends_with('}'),
            "{}",
            ran.stdout
        );
        step_eq!(
            "land",
            ran.stdout.trim().lines().count(),
            1,
            "{}",
            ran.stdout
        );
        step!(
            "land",
            ran.stderr.contains("hook says hello"),
            "hook output is never suppressed:\n{}",
            ran.stderr
        );
        let last = ran.stderr.lines().last().expect("a verdict line");
        step!("land", last.starts_with("landed"), "{last}");
    }
    let r = one_new_receipt(&fx, &before, "land");
    step_eq!("land", receipt_field(&r, "status"), "landed");
    step_eq!("land", receipt_field(&r, "pushed"), tip(&fx, "feat"));
    step_eq!("land", receipt_field(&r, "branch"), "feat");
    step_eq!("land", receipt_field(&r, "dst"), "refs/heads/feat");
    step!("land", r.contains("\"retried\":false"), "{r}");
    step!(
        "land",
        r.contains("\"started\":") && r.contains("\"ended\":"),
        "{r}"
    );
    // The dry run's receipt_dir is where that receipt lives.
    let dry = land(&fx, &["--dry-run", "--json"]);
    let dir = PathBuf::from(field(&dry, "receipt_dir"));
    let names: Vec<_> = std::fs::read_dir(&dir).expect("dir").flatten().collect();
    step_eq!("land", names.len(), receipts(&fx).len());
    step!(
        "land",
        names
            .iter()
            .any(|e| std::fs::read_to_string(e.path()).is_ok_and(|c| c == r)),
        "the landed receipt is not in {}",
        dir.display()
    );

    // ---- 5. a rewrite -----------------------------------------------------

    fx.r.git(&[
        "commit",
        "-q",
        "--no-verify",
        "--amend",
        "-m",
        "work, reworded",
    ]);
    let refused = land(&fx, &["--json"]);
    step_eq!("rewrite", refused.code, 1, "{}", refused.stderr);
    step_eq!("rewrite", status_of(&refused), "refused");
    step!(
        "rewrite",
        tip(&fx, "feat") != head(&fx),
        "a plain land replaced the ref"
    );

    let forced = land(&fx, &["--force", "--json"]);
    step_eq!("force", forced.code, 0, "{}", forced.stderr);
    step_eq!("force", tip(&fx, "feat"), head(&fx));

    // ---- 6. the remote ----------------------------------------------------

    fx.r.git(&["remote", "set-url", "origin", "http://127.0.0.1:1/x.git"]);
    let started = std::time::Instant::now();
    let ran = land(&fx, &["--json"]);
    step_eq!("unreachable", ran.code, 3, "{}", ran.stderr);
    step_eq!("unreachable", status_of(&ran), "unreachable");
    step!(
        "unreachable",
        started.elapsed() < std::time::Duration::from_secs(20),
        "the probe outlived its budget: {:?}",
        started.elapsed()
    );

    // The fetch URL still points nowhere; only `pushurl` is real.
    fx.r.git(&["config", "remote.origin.pushurl", bare_url(&fx)]);
    fx.r.stage("c.txt", "more\n");
    fx.r.commit("more");
    let ran = land(&fx, &["--json"]);
    step_eq!("push url", ran.code, 0, "{}", ran.stderr);
    step_eq!("push url", field(&ran, "push_url"), bare_url(&fx));
    step_eq!("push url", tip(&fx, "feat"), head(&fx));
    fx.r.git(&["config", "--unset-all", "remote.origin.pushurl"]);
    fx.r.git(&["remote", "set-url", "origin", bare_url(&fx)]);

    // ---- 7. upstreams -----------------------------------------------------

    // `git worktree add -b <b> origin/main` leaves the branch tracking `main`:
    // its base, not its target. It lands under its own name, `main` is left
    // alone, and the upstream moves to the branch, like `git push -u`.
    fx.r.git(&["config", "branch.feat.remote", "origin"]);
    fx.r.git(&["config", "branch.feat.merge", "refs/heads/main"]);
    fx.r.stage("d.txt", "based on main\n");
    fx.r.commit("based on main");
    let main_before = tip(&fx, "main");
    let dry = land(&fx, &["--dry-run", "--json"]);
    step_eq!("tracks main", dry.code, 0, "{}", dry.stderr);
    step_eq!("tracks main", status_of(&dry), "would-land");
    step_eq!("tracks main", field(&dry, "dst"), "refs/heads/feat");
    let ran = land(&fx, &["--json"]);
    step_eq!("tracks main", ran.code, 0, "{}", ran.stderr);
    step_eq!("tracks main", tip(&fx, "feat"), head(&fx));
    step_eq!(
        "tracks main",
        tip(&fx, "main"),
        main_before,
        "main is never written"
    );
    step_eq!(
        "tracks main",
        plain_git(&fx.r.dir, &["rev-parse", "--abbrev-ref", "feat@{u}"]),
        "origin/feat"
    );

    // A renamed upstream decides the remote name: pushed, leased and tracked
    // under it, and the local name never reaches the remote.
    fx.r.git(&["switch", "-q", "-c", "topic"]);
    fx.r.stage("e.txt", "topic\n");
    fx.r.commit("topic");
    fx.r.git(&["config", "branch.topic.remote", "origin"]);
    fx.r.git(&["config", "branch.topic.merge", "refs/heads/feature-x"]);
    let before = receipts(&fx);
    let dry = land(&fx, &["--dry-run", "--json"]);
    step_eq!("renamed", dry.code, 0, "{}", dry.stderr);
    step_eq!("renamed", status_of(&dry), "would-land");
    step_eq!("renamed", field(&dry, "dst"), "refs/heads/feature-x");
    step!(
        "renamed",
        !remote_ref_exists(&fx, "refs/remotes/origin/feature-x")
    );
    step_eq!(
        "renamed",
        receipts(&fx),
        before,
        "a dry run writes no receipt"
    );
    let ran = land(&fx, &["--json"]);
    step_eq!("renamed", ran.code, 0, "{}", ran.stderr);
    step_eq!("renamed", tip(&fx, "feature-x"), head(&fx));
    step!(
        "renamed",
        !on_bare(&fx, "topic"),
        "the local name must not be created on the remote"
    );
    step_eq!(
        "renamed",
        plain_git(&fx.r.dir, &["rev-parse", "refs/remotes/origin/feature-x"]),
        head(&fx)
    );
    // A forced rewrite leases on the REMOTE name.
    fx.r.git(&["commit", "-q", "--no-verify", "--amend", "-m", "reworded"]);
    let forced = land(&fx, &["--force", "--json"]);
    step_eq!("renamed force", forced.code, 0, "{}", forced.stderr);
    step_eq!("renamed force", tip(&fx, "feature-x"), head(&fx));

    // A name longer than the line: the verdict still fits eighty columns.
    let long = format!("feature/{}", "x".repeat(112));
    fx.r.git(&["switch", "-q", "-c", &long]);
    let ran = land(&fx, &[]);
    step_eq!("long name", ran.code, 0, "{}", ran.stderr);
    let last = ran.stderr.lines().last().expect("a verdict line");
    step!("long name", last.chars().count() <= 80, "{last}");
    step!("long name", last.starts_with("landed"), "{last}");
}
