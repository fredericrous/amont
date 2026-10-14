#!/bin/sh
# Which gates may the `sign` job sign, from the results the jobs uploaded?
#
#   ci-attest-sign-plan.sh <results-dir>
#
# A gate is signed only when its result file says success, names THIS job's
# tree (a job that checked out something else proves nothing about it), and
# names the platform the workflow declares for that gate. Results are data
# written by a job that ran the pull request's code: nothing here executes
# them, and a field that does not match is a gate that is not signed.
#
# Writes `linux`, `macos` and `windows` (space-separated gate lists, one sign
# call per platform) to $GITHUB_OUTPUT, stdout when it is unset. Exit 0 always
# once it could read git: nothing to sign is not an error.
set -eu

dir=${1:?usage: ci-attest-sign-plan.sh <results-dir>}
out=${GITHUB_OUTPUT:-/dev/stdout}

tree=$(git rev-parse 'HEAD^{tree}')

# The platform each gate runs on, declared once, here. Keep in step with ci.yaml
# and with ATTEST_GATES in the Makefile (a test compares the names).
expected() {
  case $1 in
    ci-rust | ci-msrv | ci-deps | ci-hooks-ubuntu) echo x86_64-linux ;;
    ci-hooks-macos) echo aarch64-macos ;;
    ci-windows-test) echo x86_64-windows ;;
    *) return 1 ;;
  esac
}

linux=
macos=
windows=

if [ -d "$dir" ]; then
  # Sorted, so the output does not depend on directory order.
  for f in $(find "$dir" -name 'attest-result-*.json' | sort); do
    gate=$(jq -r '.gate // empty' "$f" 2> /dev/null) || gate=
    if [ -z "$gate" ] || ! want=$(expected "$gate"); then
      echo "ci-attest-sign-plan: $f: not a gate this workflow signs, ignored"
      continue
    fi
    outcome=$(jq -r '.outcome // empty' "$f")
    rtree=$(jq -r '.tree // empty' "$f")
    rplat=$(jq -r '.platform // empty' "$f")
    if [ "$outcome" != success ]; then
      echo "ci-attest-sign-plan: $gate: outcome '$outcome', not signed"
    elif [ "$rtree" != "$tree" ]; then
      echo "ci-attest-sign-plan: $gate: result is for tree $rtree, this job is on $tree, not signed"
    elif [ "$rplat" != "$want" ]; then
      echo "ci-attest-sign-plan: $gate: ran on '$rplat', the workflow declares '$want', not signed"
    else
      echo "ci-attest-sign-plan: $gate: signing for $want"
      case $want in
        x86_64-linux) linux="$linux $gate" ;;
        aarch64-macos) macos="$macos $gate" ;;
        x86_64-windows) windows="$windows $gate" ;;
      esac
    fi
  done
fi

{
  echo "linux=${linux# }"
  echo "macos=${macos# }"
  echo "windows=${windows# }"
} >> "$out"
