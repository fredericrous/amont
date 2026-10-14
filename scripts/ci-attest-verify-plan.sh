#!/bin/sh
# Decide, from what attest's verifier reported, which CI work is already
# covered by a signature on this tree's declared inputs.
#
#   LINUX='["ci-rust"]' MACOS='[]' WINDOWS='[]' EVENT=pull_request \
#     ci-attest-verify-plan.sh
#
# Inputs are the verifier's `gates` outputs (JSON arrays, empty or unset when a
# step did not run). Prints one plain line per gate (`<gate>: skipped, ...` or
# `<gate>: running, ...`), appends the same to $GITHUB_STEP_SUMMARY, and writes
# these to $GITHUB_OUTPUT (stdout when it is unset):
#
#   rust_covered msrv_covered deps_covered windows_test_covered   true|false
#   test_matrix   JSON for `strategy.matrix.include`, the legs NOT covered
#   test_any      true when test_matrix is not empty
#
# Fail-open: anything it cannot read means "not covered", which runs the work.
# `schedule` covers nothing: that run exists to catch drift no push made.
set -eu

event=${EVENT:-}
linux=${LINUX:-[]}
macos=${MACOS:-[]}
windows=${WINDOWS:-[]}

# covered <platform-array> <gate>
covered() {
  [ "$event" != schedule ] || return 1
  printf '%s' "$1" | jq -e --arg g "$2" 'index($g) != null' > /dev/null 2>&1
}

out=${GITHUB_OUTPUT:-/dev/stdout}
summary=${GITHUB_STEP_SUMMARY:-/dev/null}

say() {
  echo "$1"
  echo "$1" >> "$summary"
}

# report <gate> <covered?>: the log line a person reads instead of a grey icon.
report() {
  if [ "$2" = true ]; then
    say "$1: skipped, a signature covers this tree's declared inputs (ADR-0024)"
  elif [ "$event" = schedule ]; then
    say "$1: running, a scheduled run covers nothing"
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

report ci-rust "$rust"
report ci-msrv "$msrv"
report ci-deps "$deps"
report ci-windows-test "$win"
report ci-hooks-ubuntu "$ubuntu"
report ci-hooks-macos "$mac"

matrix='['
sep=
if [ "$ubuntu" != true ]; then
  matrix="$matrix${sep}{\"os\":\"ubuntu-latest\",\"gate\":\"ci-hooks-ubuntu\"}"
  sep=,
fi
if [ "$mac" != true ]; then
  matrix="$matrix${sep}{\"os\":\"macos-latest\",\"gate\":\"ci-hooks-macos\"}"
  sep=,
fi
matrix="$matrix]"
any=true
[ "$matrix" != '[]' ] || any=false

{
  echo "rust_covered=$rust"
  echo "msrv_covered=$msrv"
  echo "deps_covered=$deps"
  echo "windows_test_covered=$win"
  echo "test_matrix=$matrix"
  echo "test_any=$any"
} >> "$out"
