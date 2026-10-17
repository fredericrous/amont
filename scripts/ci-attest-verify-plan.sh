#!/bin/sh
# Decide, from what attest's verifier reported, which CI work is already
# covered by a signature on this tree's declared inputs.
#
#   LINUX='["ci-rust"]' MACOS='[]' WINDOWS='[]' EVENT=pull_request \
#     PLATFORM_CODE=false FULL_CI=false ci-attest-verify-plan.sh
#
# Inputs are the verifier's `gates` outputs (JSON arrays, empty or unset when a
# step did not run). Prints one plain line per gate (`<gate>: skipped, ...` or
# `<gate>: running, ...`), appends the same to $GITHUB_STEP_SUMMARY, and writes
# these to $GITHUB_OUTPUT (stdout when it is unset):
#
#   rust_covered msrv_covered deps_covered windows_test_covered   true|false
#   windows_test_run  true when the Windows suite runs here
#   test_matrix   JSON for `strategy.matrix.include`, the legs NOT covered
#   test_any      true when test_matrix is not empty
#   load_matrix   the same for the real-clock load scenarios (`make test-load`)
#   load_any      true when load_matrix is not empty
#
# Fail-open: anything it cannot read means "not covered", which runs the work.
# `schedule` covers nothing: that run exists to catch drift no push made.
#
# A pull request proves Linux. macOS, Windows and the load scenarios are
# DEFERRED to the push to main, which proves them for the same tree and
# which the release waits for, unless the pull request changes something
# platform-specific (PLATFORM_CODE=true, from ci-platform-touched.sh) or
# carries the `full-ci` label (FULL_CI=true). A deferred gate says so; it is
# never a silent grey icon (decisions plan
# 2026-10-17-amont-ci-proves-platforms-once).
set -eu

event=${EVENT:-}
linux=${LINUX:-[]}
macos=${MACOS:-[]}
windows=${WINDOWS:-[]}
platform_code=${PLATFORM_CODE:-true}
full_ci=${FULL_CI:-false}

# Deferred: a pull request that touches nothing platform-specific and did
# not ask for everything.
deferred=false
if [ "$event" = pull_request ] && [ "$platform_code" != true ] && [ "$full_ci" != true ]; then
  deferred=true
fi

# readable <platform-array>: is it the JSON array the verifier promises? An
# empty or unset one is "nothing covered", not unreadable (the step may not have
# run). Anything else that is not an array is reported as such, never as a
# plain absence of a signature.
readable() {
  [ -z "$1" ] || [ "$1" = '[]' ] || printf '%s' "$1" | jq -e 'type == "array"' > /dev/null 2>&1
}

# covered <platform-array> <gate>
covered() {
  [ "$event" != schedule ] || return 1
  readable "$1" || return 1
  printf '%s' "$1" | jq -e --arg g "$2" 'index($g) != null' > /dev/null 2>&1
}

out=${GITHUB_OUTPUT:-/dev/stdout}
summary=${GITHUB_STEP_SUMMARY:-/dev/null}

say() {
  echo "$1"
  echo "$1" >> "$summary"
}

# report <gate> <covered?> <platform-array> [deferrable]: the log line a
# person reads instead of a grey icon.
report() {
  if [ "$2" = true ]; then
    say "$1: skipped, a signature covers this tree's declared inputs (ADR-0024)"
  elif [ "${4:-}" = deferrable ] && [ "$deferred" = true ]; then
    say "$1: deferred to the push to main, which proves it for this tree before any release (label full-ci runs it here)"
  elif [ "$event" = schedule ]; then
    say "$1: running, a scheduled run covers nothing"
  elif ! readable "$3"; then
    say "$1: running, the verifier's output was unreadable"
  else
    say "$1: running, no valid signature"
  fi
}

flag() {
  if covered "$1" "$2"; then echo true; else echo false; fi
}

rust=$(flag "$linux" ci-rust)
msrv=$(flag "$linux" ci-msrv)
deps=$(flag "$linux" ci-deps)
win=$(flag "$windows" ci-windows-test)
# A Windows machine's own local signature counts too, as it always has.
[ "$win" = true ] || win=$(flag "$windows" pre-push-cargo-test)
ubuntu=$(flag "$linux" ci-hooks-ubuntu)
mac=$(flag "$macos" ci-hooks-macos)
load_ubuntu=$(flag "$linux" ci-load-ubuntu)
load_mac=$(flag "$macos" ci-load-macos)

report ci-rust "$rust" "$linux"
report ci-msrv "$msrv" "$linux"
report ci-deps "$deps" "$linux"
report ci-windows-test "$win" "$windows" deferrable
report ci-hooks-ubuntu "$ubuntu" "$linux"
report ci-hooks-macos "$mac" "$macos" deferrable
report ci-load-ubuntu "$load_ubuntu" "$linux" deferrable
report ci-load-macos "$load_mac" "$macos" deferrable

# Runs here: not covered, and not deferred.
win_run=false
if [ "$win" != true ] && [ "$deferred" != true ]; then win_run=true; fi

matrix='['
sep=
if [ "$ubuntu" != true ]; then
  matrix="$matrix${sep}{\"os\":\"ubuntu-latest\",\"gate\":\"ci-hooks-ubuntu\"}"
  sep=,
fi
if [ "$mac" != true ] && [ "$deferred" != true ]; then
  matrix="$matrix${sep}{\"os\":\"macos-latest\",\"gate\":\"ci-hooks-macos\"}"
  sep=,
fi
matrix="$matrix]"
any=true
[ "$matrix" != '[]' ] || any=false

load='['
sep=
if [ "$load_ubuntu" != true ] && [ "$deferred" != true ]; then
  load="$load${sep}{\"os\":\"ubuntu-latest\",\"gate\":\"ci-load-ubuntu\"}"
  sep=,
fi
if [ "$load_mac" != true ] && [ "$deferred" != true ]; then
  load="$load${sep}{\"os\":\"macos-latest\",\"gate\":\"ci-load-macos\"}"
  sep=,
fi
load="$load]"
load_any=true
[ "$load" != '[]' ] || load_any=false

{
  echo "rust_covered=$rust"
  echo "msrv_covered=$msrv"
  echo "deps_covered=$deps"
  echo "windows_test_covered=$win"
  echo "windows_test_run=$win_run"
  echo "test_matrix=$matrix"
  echo "test_any=$any"
  echo "load_matrix=$load"
  echo "load_any=$load_any"
} >> "$out"
