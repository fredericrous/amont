//! The corpus that must never fire, and the traps that must.
//!
//! Precision is the product. Recall is recoverable — a rule that misses
//! something can be widened next week, and the backtester will say by how much.
//! A false positive is not recoverable in the same way: it refuses work the
//! author knew was correct, and the response to that is to delete the hook from
//! `settings.json`, which switches off every rule at once.
//!
//! So this file gets the negative cases, and they are real commands taken from
//! the transcripts, not invented ones. Every entry in [`BENIGN`] is a shape
//! that a naive implementation of one of these rules DID match while the design
//! was being measured: 501 of 2,477 matches — one in five — were wrong, and
//! these are the four families they fell into.
//!
//! Driving the real binary rather than calling the functions is deliberate: it
//! covers the argv parse and the output path too, and it is the same thing a
//! human would type to check a suspicious command.

use std::process::Command;

fn check(command: &str) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_amont-agent"))
        .args(["check", command])
        .output()
        .expect("the binary runs");
    assert!(out.status.success(), "check exited {:?}", out.status.code());
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn fires(command: &str) -> bool {
    !check(command).starts_with("no rule fires")
}

fn fires_rule(command: &str, rule: &str) -> bool {
    check(command).lines().any(|l| l.starts_with(rule))
}

/// Real commands that must produce silence. Each carries the family it belongs
/// to, because a future widening of any rule will break one of these first and
/// the failure should name what was traded away.
const BENIGN: &[(&str, &str)] = &[
    // --- a pattern inside a quoted string is text, not a command -----------
    (r#"pkill -f "git push origin v0.33.66""#, "quoted"),
    (
        r#"echo "=== first tag ===" && git log --oneline | head"#,
        "quoted",
    ),
    (
        r#"gh pr create --body "we could use --auto here""#,
        "quoted",
    ),
    (r#"git commit -m "add --no-verify to the docs""#, "quoted"),
    (r#"rg "git add -A" docs/"#, "quoted"),
    // --- the verb and the pipe are in different commands --------------------
    (
        "kustomize build k/ >/dev/null && echo done | tail -1",
        "clause",
    ),
    (
        "git tag -d v1.9.0; git show HEAD:Cargo.toml | grep version",
        "clause",
    ),
    (
        "kubectl delete pod x --wait=false; kubectl get pods | head",
        "clause",
    ),
    // --- reading, not mutating ---------------------------------------------
    ("git tag --sort=-v:refname | head -5", "listing"),
    ("git tag --contains 9dd54d2 | head", "listing"),
    ("git tag -l 'v1.*' | tail -3", "listing"),
    ("git stash list | grep wip", "listing"),
    ("git log --oneline | head -20", "listing"),
    // --- explicitly a dry run ----------------------------------------------
    (
        "kubectl apply --dry-run=client -f x.yaml 2>&1 | tail -20",
        "dry-run",
    ),
    ("git push --dry-run origin main | tail -3", "dry-run"),
    ("helm upgrade r ./chart --dry-run | tail -5", "dry-run"),
    ("npm publish --dry-run | tail -2", "dry-run"),
    // --- the mutating command is the SINK, so its status is the pipeline's --
    ("echo msg | git commit -F -", "sink"),
    ("cat manifest.yaml | kubectl apply -f -", "sink"),
    // --- scoped, deliberate forms ------------------------------------------
    ("git add -A packages/dbt-duckdb/", "scoped"),
    ("git add -p", "scoped"),
    ("git add src/main.rs src/lib.rs", "scoped"),
    ("git add -- -A", "scoped"),
    (r#"git stash pop "stash@{2}""#, "scoped"),
    ("git stash apply refs/stash@{1}", "scoped"),
    // --- a branch started from the remote is the remedy, not the mistake ---
    (
        "git fetch origin -q && git worktree add ../x-wt-y -b feat/y origin/main",
        "remote-base",
    ),
    (
        "git fetch forgejo main -q && git checkout -B fix/x forgejo/main 2>&1 | tail -1",
        "remote-base",
    ),
    ("git switch -c feat/y upstream/main", "remote-base"),
    ("git checkout -t origin/feat/y", "remote-base"),
    ("git worktree add ../x feat/existing", "remote-base"),
    ("git worktree add --detach ../x", "remote-base"),
    // --- ordinary work that touches none of the rules ----------------------
    ("cargo test --workspace", "ordinary"),
    ("npm run build && npm run test", "ordinary"),
    ("git status --short", "ordinary"),
    ("git push origin main", "ordinary"),
    ("git commit -m 'feat: a thing'", "ordinary"),
];

#[test]
fn the_benign_corpus_is_silent() {
    let mut wrong = Vec::new();
    for (command, family) in BENIGN {
        let said = check(command);
        if !said.starts_with("no rule fires") {
            wrong.push(format!(
                "[{family}] {command}\n    fired: {}",
                said.lines().next().unwrap_or_default()
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} benign commands fired:\n{}",
        wrong.len(),
        BENIGN.len(),
        wrong.join("\n")
    );
}

/// The named trap. A regex written for "force-pushing a tag" matched this on
/// its first run against real data — a perfectly ordinary branch push with a
/// lease. Argv-level matching is what makes it structurally impossible, and
/// this test is here so that stays true.
#[test]
fn force_with_lease_on_a_branch_is_not_a_force_pushed_tag() {
    assert!(!fires("git push --force-with-lease origin feat/x"));
    assert!(!fires(
        "git push --force-with-lease --no-verify=x origin feat/x"
    ));
}

#[test]
fn a_mutating_command_piped_into_a_trimmer_is_caught() {
    for command in [
        "git push origin main 2>&1 | tail -5",
        "git push -u origin feat/x 2>&1 | head -12",
        "git commit -m 'x' 2>&1 | tail -8",
        "git tag v1.2.3 && git push origin v1.2.3 2>&1 | tail -3",
        "kubectl apply -f x.yaml | tail -1",
        "npm publish | grep -i error",
    ] {
        assert!(fires_rule(command, "pipe-to-tail"), "missed: {command}");
    }
}

/// The regression for the bug that hid 89 true positives: blanking a heredoc
/// from the `<<TAG` operator rather than from the following newline erases
/// `2>&1 | tail -8` from the operator's own line, and the command reads clean.
#[test]
fn a_heredoc_does_not_swallow_the_rest_of_its_own_line() {
    let command = "git commit -F- <<'MSG' 2>&1 | tail -8\nfeat: a subject\n\nbody\nMSG\n";
    assert!(fires_rule(command, "pipe-to-tail"));
}

/// A construct we cannot read must produce silence, not a guess.
#[test]
fn what_cannot_be_read_gets_no_opinion() {
    for command in [
        "eval \"$deploy\"",
        "sh -c 'git push origin main | tail -1'",
        "git push \"origin",
        "git commit -F- <<'MSG'\nno terminator here\n",
    ] {
        let said = check(command);
        assert!(
            said.starts_with("no opinion"),
            "expected no opinion for {command:?}, got {said}"
        );
    }
}

#[test]
fn the_smaller_rules_catch_their_own_shapes() {
    assert!(fires_rule("git stash pop", "bare-stash-pop"));
    assert!(fires_rule("git stash apply", "bare-stash-pop"));
    assert!(fires_rule(
        "gh pr merge 381 --squash --auto",
        "gh-pr-merge-auto"
    ));
    assert!(fires_rule("git commit --no-verify -m x", "no-verify"));
    assert!(fires_rule("git add -A", "git-add-broad"));
    assert!(fires_rule("git add .", "git-add-broad"));
    assert!(fires_rule("git worktree add ../x -b feat/y", "stale-base"));
    assert!(fires_rule("git worktree add ../x", "stale-base"));
    assert!(fires_rule("git checkout -b feat/y", "stale-base"));
    assert!(fires_rule("git switch -c feat/y main", "stale-base"));
}

/// `-n` means `--no-verify` on `git commit` and `--dry-run` on `git push`.
/// A flat flag list reports a dry-run push as a bypassed gate, which is the
/// exact opposite of what it is.
#[test]
fn the_short_flag_means_different_things_per_subcommand() {
    assert!(fires_rule("git commit -n -m x", "no-verify"));
    assert!(!fires_rule("git push -n origin main", "no-verify"));
}

/// Every rule must say what to do instead. A rule that only says what is wrong
/// cannot be obeyed, and an unobeyable rule is noise the reader learns to skip.
#[test]
fn every_finding_carries_a_remedy() {
    for command in [
        "git push origin main | tail -5",
        "git stash pop",
        "git checkout -b feat/y",
        "gh pr merge 1 --auto",
        "git commit --no-verify -m x",
        "git add -A",
    ] {
        let said = check(command);
        assert!(
            said.contains("→ "),
            "no remedy offered for {command:?}:\n{said}"
        );
    }
}
