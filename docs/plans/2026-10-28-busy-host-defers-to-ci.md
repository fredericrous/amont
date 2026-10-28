---
status: active
branch: feat/busy-host-defers-to-ci
repos: [amont, relais]
adrs: [ADR-0009, ADR-0013]
---
# A busy host defers heavy gates to CI

## Review panel

📄 Full reviews: [2026-10-28-busy-host-defers-to-ci.reviews.md](2026-10-28-busy-host-defers-to-ci.reviews.md)
👉 **Decide:** none — approve if the bet holds that most of the load comes from amont gates, which hold slots. Load from outside the hooks is a non-goal; the 14-day measure checks it.
📍 amont, relais · plan reviewed, nothing built · next: the ADR, then the worktree. Panel: architect, backend, rust, po, tui, unix (sonnet-5-5), codex.
**Changed by review:** lazy slot; deferral recorded beside the check so its own error handling cannot turn it into a failure; race-safe held count; push twin of a commit-deferred gate defers too, via `PairVerdict::Deferred`, never `Gated`; deferral kept outside the 64-run history.
**Verdicts:** 6/6 approve-with-changes in round 1. Final: backend approve, rust approve. Codex approve-with-changes. 4 carried into implementation: rehearsal reporting "passed" when every gate deferred (codex #3); hand review of `PairVerdict` matches; `before_spawn` refuses at once once a deferral is recorded; raise `HELD` as soon as the flock is won.

## Context

Heavy git-hook gates (cargo test, the relais plugin suite) push this Intel Mac to load 40–400 when several sessions commit at once. Under that load, gates fail on their own timeouts: relais plugin-test hit 5 s test timeouts four commits in a row, and the person had to approve a "proven-alone" skip by hand. On GitHub-hosted repos, CI has capacity of its own and already re-runs every gate the signed attestation does not name. Running the suite locally under contention buys nothing there.

What amont already has, read from the source:
- A machine-wide flock semaphore: `crates/amont-runtime/src/host_slots.rs`, ADR-0009 `hooks.host-concurrency`. It has `amont.hostSlots` slots (cores/4 by default) under `/tmp/amont-slots-<uid>`, acquired lazily in `before_spawn` (`host_slots.rs:216`).
- Per-gate attestations. attest's `gates` output lists only the gates that passed, so CI already runs what is missing (`attest/SPEC.md:152-165`). **No change to attest or git-attest is needed.**

What is missing:
1. **Declared gates never take a slot.** `weight_of` (`host_slots.rs:57`) matches only the built-in names in `HEAVY`. amont's own `pre-commit cargo-test` line (`amont.conf:38`) and relais's plugin-test are declared, so they run unqueued, and they are the worst offenders.
2. **A slot that never frees runs the gate anyway**, after up to `amont.timeout` (3600 s). Nothing ever defers.
3. **No `Deferred` outcome.** `Outcome` is `Passed|Failed|Warned|Fixed|Unavailable|Inert` (`check.rs:285`).

Decided with the person: deferral is declared per repository in `amont.conf` (`set onBusy defer`), not inferred from the remote.

Non-goals: load from processes outside amont's hooks (agent builds, relais workers) holds no slot and does not cause a deferral. A gate that gets a slot and then times out still fails as today. If the slot logs show the plugin-test failures happened while slots were free, CPU-load sampling becomes a separate plan.

## Design

**Weight of a declared gate.** A declared check is Heavy when:
- (a) it moves a built-in Heavy gate. Its short name equals the short name of a `HEAVY` entry, as `cargo-test` does `pre-push-cargo-test`'s; or
- (b) its line carries a new `heavy` flag token, parsed like `files` and `docs-skip` (`manifest.rs` `Declared`, `parse_lines` ~1156).

`enter_check` takes the weight from the dispatcher instead of deriving it from the name alone. `HEAVY` stays the source for built-ins.

**Busy policy.** Two new `SETTABLE` keys in `manifest.rs`, parsed as strictly as `planExempt` (exact value, once; a bad or repeated value is a broken line, as for `planExempt`, `manifest.rs:1285-1301`):
- `set onBusy wait|defer`. The default is `wait`, which is today's behaviour unchanged.
- `set slotWait <seconds>`. Default 60; it applies only under `defer`.
- Precedence, stated once in `docs/configuration.md`: `AMONT_ON_BUSY` in the environment, for one run (`AMONT_ON_BUSY=wait git push`), then `amont.conf`, then the default `wait`.

`hostSlots` stays a host key. The repository declares only what to do when the host is full.

**Deferral path.** The slot stays lazy for every Heavy check, built-in or declared (`host_slots.rs:12-14`): it is taken at the check's first tool spawn, so a check with nothing to do never waits and never defers. Under `defer`:
- A new `host_slots::settle(settings, budget) -> Acquired` holds the nested and disabled checks that live only in `before_spawn` today (`host_slots.rs:209-228`); `acquire` alone never returns `Nested`. `before_spawn` calls it with `slotWait` under `defer` and `amont.timeout` under `wait`. `enter_check` takes the weight from the dispatcher, because it resets `CURRENT`.
- While waiting, the check keeps today's queue row (`stage.queue`) and `(waited … for a host slot)` line (`host_slots.rs:234-253`), so a deferring hook is never silent.
- On a deferral, `before_spawn` refuses the spawn and records it in the thread-local `CURRENT` (`deferred: Option<Duration>`). The two spawn sites (`hooks/common.rs:964,993`) return an `io::Error` only to stop the spawn. The dispatcher reads the recorded deferral through `CheckGuard` before it drops, and that deferral overrides whatever outcome the check returned, as `Outcome::Deferred`, so a check's own `Err` handling (`Unavailable`/`Failed`) cannot swallow it.
- `TimedOut` means the check is not run and gets `Outcome::Deferred`, but only when this amont process holds no slot. That is known from a process-wide `static HELD: AtomicUsize`, raised when a `Slot` is created and lowered in its `Drop`, because `CURRENT` is thread-local (`host_slots.rs:166`). A waiter reads it (`SeqCst`) when its `slotWait` runs out; if it is non-zero, it keeps waiting up to `amont.timeout`; if it is zero, it makes one more `try_lock` pass before deferring. `Drop for Slot` releases the file (held as `Option<File>`, `take()`n) before `fetch_sub`, so the count never reads zero while the flock is still held. A sibling of a check holding a slot in the same stage waits as under `wait`, so a commit never defers behind its own checks.
- `TimedOut` under `defer` replaces the "running it anyway" note (`host_slots.rs:281-283`) with the deferral line.
- `Disabled`, `Nested`, `Unsupported`, `DirUnsafe` and `Io` mean there is no queue to be busy, so the check runs as today. Deferring only ever follows a measured wait.

This applies to the concurrent pre-commit threads (`dispatch.rs:304-337`) and to the serial pre-push loop (`dispatch.rs:1160-1318`). In the serial loop, once one Heavy check defers on `TimedOut`, the later Heavy checks of that stage defer without waiting again, so a push waits at most one `slotWait`.

**`Outcome::Deferred`.**
- It never blocks, whatever the severity.
- It writes no stamp token: `add_tokens` sees only passed gates.
- `attestable` (`dispatch.rs:1506`) leaves it out, because it did not pass, so CI runs it.
- It adds a `RunOutcome::Deferred` evidence line (`gate_stamp.rs:139`). Because this line now controls push behavior, retain the latest pre-commit deferred state per gate independently of the shared `MAX_RUNS` history limit; a subsequent execution of that gate supersedes its retained state. Retained deferral state never becomes a passing token. Add an integration test that appends more than 64 unrelated run records and verifies that the deferred twin still stays deferred.
- It is announced in a roll-up line in `announce`, in the same form as `could not run` (`warning_sign()`, no new glyph, 80 columns or fewer), and it prints in quiet mode too:
  `! 1 check(s) deferred to CI (no host slot in 60s): cargo-test`
  followed by `  run now: cargo test --locked · stop deferring: set onBusy wait`

Exhaustive `match`es on `Outcome` are found by the compiler. Every `_ =>` arm and `matches!` on `Outcome` is reviewed by hand (`Outcome::Unavailable` appears 57 times across 20 files).

**The chain to CI.** The commit's evidence note carries a `run … deferred` line for the gate. Pre-push resolves the twin through the existing pairing traversal: inspect every relevant commit across every pushed ref, reading each commit's tree note. Historical deferral permits skipping the twin only when every relevant commit has either valid passing proof or an explicit deferral for the paired gate; an unchecked commit retains the existing unstamped behavior. When any relevant commit is covered only by a deferral, the pairing returns a new `PairVerdict::Deferred(n)`, never `Gated`: the dispatcher maps it to `Outcome::Deferred` and does not add the twin to `passed` (`dispatch.rs:1122-1128`), so the signed `gates` line cannot name the twin and CI runs it (`ci.yaml:119-120`). Preserve the existing scope-coverage checks. Integration tests cover deferred code followed by documentation, and a multi-ref push containing both a deferred branch and an unchecked branch. This is the first skip decision that reads a `run` line, so the ADR amends `gate_stamp.rs:59-61`: a `deferred` line can only move a gate to CI, never stamp it, so a forged line cannot pass a gate. `AMONT_ON_BUSY=wait` overrides it:
- A pre-commit gate deferred at commit leaves no stamp. Under `onBusy defer`, its built-in push twin (for example `pre-push-cargo-test`) also defers to CI and does not run: running the suite at push brings back the idle-connection reset that moved it to commit (`amont.conf:10-24`). A `--no-verify` commit (no deferral recorded) still brings the push gate back (`amont.conf:33`). The ADR names this trade.
- A Heavy push gate whose commit recorded no deferral (no deferred twin) waits up to `slotWait` at push; if the host is still busy, it defers and stays out of the attestation, so CI runs it. The twin rule above does not depend on the load at push.
- A declared gate with no push twin (relais plugin-test) goes straight to CI.

**Safety framing** (`general.no-disabled-safety`): a deferred gate is still required. It moves to CI, which is already the backstop (`docs/ci.md`), and the move is said out loud. A repository should declare `defer` only where its CI workflow runs each Heavy gate, and the ADR and the docs say so.

## Phases (one amont PR, then the rollout)

1. **ADR.** amont `adr/0013-a-busy-host-defers-to-ci.md`:
   - replaces ADR-0009's head of `hooks.host-concurrency`, to add declared weight and the busy policy;
   - adds the first decision on a new key, `hooks.busy-policy`, declared in `.adr.yaml`;
   - runs `aval heads --write`.

   No fleet ADR: `ci.attested-skip` is unchanged.
2. **Weight.** The `heavy` flag and the moved-gate rule. `weight_of` widens to take the check, and the registry test is kept.
3. **Policy and deferral.** The `onBusy` and `slotWait` keys, `host_slots::settle`, the typed spawn refusal, the process-wide held-slot count, `Outcome::Deferred` and `RunOutcome::Deferred`, announce and summary.
4. **Docs.**
   - `docs/configuration.md`: `onBusy`, `slotWait`, and the "host concurrency" section.
   - `docs/custom-checks.md`: the `heavy` flag.
   - `docs/ci.md`: deferral as a CI backstop.
   - `amont agents-md` regenerated.
5. **Release** amont (the `tag-release` skill), then `brew upgrade`, and check that `~/.local/bin/amont` does not shadow it.
6. **Rollout.** First confirm that every amont reading these `amont.conf` files (brew, `~/.local/bin`, CI, npm pins) is at the release: an older one runs `heavy make plugin` as the command (`manifest.rs:1397-1414`). Then add `set onBusy defer` to amont and relais, the two contention sources. For relais, also add `heavy` to plugin-test, since its GitHub workflow already runs `make plugin` every time (`.github/workflows/ci.yaml:167`) and runs `cargo test` unless the attestation lists `pre-push-cargo-test` (`ci.yaml:119-120`). Other GitHub repos follow, one line each, each after its workflow has been checked.

Implementation of phases 2–4 goes through relais (bounded, `cargo test` acceptance), per the routing rule.

## Verification

- **Unit:**
  - `weight_of` for a moved declared gate, a `heavy` line and a plain declared line.
  - `onBusy` and `slotWait` parsing: valid, duplicate, bad value.
  - `attestable` excludes `Deferred`.
  - A `block`-severity check that is `Deferred` gives a passing verdict.
- **Integration** (`crates/amont/tests/it`, `common::Repo`), with `AMONT_SLOT_DIR` set to a temp dir and `AMONT_HOST_SLOTS=1`, through `Repo::hook_watched_unpinned` under `alone()` (the pinned helpers set `AMONT_HOST_SLOT=held`, which is `Nested` and never defers; `common/mod.rs:142`). The test holds `slot-0` with its own flock.
  - `onBusy defer` and `slotWait 1`, with output piped and `NO_COLOR=1`: `git commit` succeeds, the output names the deferral (in quiet mode too), and `refs/notes/amont-gate` has no token for the gate.
  - The slot is then released. `git push` to a local bare remote under `onBusy defer` does not run the push twin: the output names the deferral and no push stamp is written. `AMONT_ON_BUSY=wait git push` runs the twin and stamps it.
  - A forged `run … deferred` line with attest on: the gate is left out of the signed `gates` line.
  - `onBusy wait` (the default): the existing behaviour, checked by the existing `tests/timing.rs:~610` test.
  - Two Heavy pre-commit checks, `AMONT_HOST_SLOTS=1`, no outside holder: neither defers behind the other.
  - A Heavy check with nothing to do while the slot is held: no wait and no deferral.
  - With the slot held and 3 Heavy push gates: `git push` waits about one `slotWait`, not three.
  - `amont.attest` on with the slot held at push: the signed `gates` line omits the deferred gate.
  - `amont.attest` on, a push of commit A (stamped for `cargo-test`) and commit B (deferred): the output names the deferral, not "gated at commit", and the signed `gates` line omits `pre-push-cargo-test`.
- **Locally:** `cargo test --locked` in the worktree → exit 0, with the new unit and integration tests named in the output.
- **Measured before choosing `slotWait`:** `amont.hostSlots` on this Mac, and the p50/p95 slot wait over a week. After rollout: the deferral rate per repo per day, from the `RunOutcome::Deferred` evidence lines, with a stated bar.
- **Live, after release:**
  - In relais under real load, commit a `.ts` change and see `deferred` instead of 5 s timeouts. Record how many of the `amont.hostSlots` slots were held each time plugin-test deferred or failed.
  - On the PR, read the attest step's `gates` output and see the test step run (step conclusion read explicitly).
- **After release (14 days, amont and relais):** Heavy gates deferred out of all Heavy gates run; plugin-test timeout failures against the four in a row before; how many deferred gates CI then failed. Roll back to `onBusy wait` in a repo if more than 50% of its Heavy gates defer or the timeout failures do not fall.

<!-- panel: repos=amont,relais reviewers=architect,backend,language,po,tui,unix,codex body-sha=91972bb3d0c2 -->
