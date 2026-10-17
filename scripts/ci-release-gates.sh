#!/bin/sh
# Is every platform proven for the tree being released?
#
#   ci-release-gates.sh [--wait SECONDS]
#
# A pull request proves Linux; macOS, Windows and the load scenarios are
# proven by the push to main (decisions plan
# 2026-10-17-amont-ci-proves-platforms-once). So nothing may be released
# until every gate CI signs is attested for this exact tree, each on its own
# platform. This asks attest's verifier, polling every 60 s up to --wait
# (default 0: ask once), because the release PR's merge starts main's CI,
# which signs them, while the tag's release is already running.
#
# Exit 0 when every gate is covered. Exit 1 naming each one still missing.
# GIT_ATTEST names the git-attest binary (default: on PATH).
set -eu

wait=0
while [ $# -gt 0 ]; do
  case $1 in
    --wait) wait=${2:?--wait wants seconds}; shift 2 ;;
    *) echo "ci-release-gates: unknown argument $1" >&2; exit 2 ;;
  esac
done
bin=${GIT_ATTEST:-git-attest}

# <platform> <gate>[|<alternative>]... : every gate the release needs, the
# platform it must have been proven on, and what else counts as the same
# proof (a Windows machine's own signature, as ci-attest-verify-plan.sh
# accepts).
required='x86_64-linux ci-rust
x86_64-linux ci-msrv
x86_64-linux ci-deps
x86_64-linux ci-hooks-ubuntu
x86_64-linux ci-load-ubuntu
aarch64-macos ci-hooks-macos
aarch64-macos ci-load-macos
x86_64-windows ci-windows-test|pre-push-cargo-test'

# The gates a verified attestation covers on <platform>, one per line.
covered() {
  "$bin" covered --platform "$1" 2> /dev/null | tr ' ' '\n' | sed '/^$/d' || true
}

check() {
  missing=
  for p in x86_64-linux aarch64-macos x86_64-windows; do
    have=$(covered "$p")
    while read -r plat gates; do
      [ "$plat" = "$p" ] || continue
      ok=false
      for g in $(printf '%s' "$gates" | tr '|' ' '); do
        if printf '%s\n' "$have" | grep -qx "$g"; then ok=true; fi
      done
      [ "$ok" = true ] || missing="$missing $gates@$p"
    done << EOF
$required
EOF
  done
  [ -z "$missing" ]
}

started=$(date +%s)
while :; do
  if check; then
    echo "ci-release-gates: every gate is attested for this tree, on its platform"
    exit 0
  fi
  elapsed=$(( $(date +%s) - started ))
  if [ "$elapsed" -ge "$wait" ]; then
    echo "ci-release-gates: not proven for this tree after ${elapsed}s:" >&2
    for m in $missing; do echo "  $m" >&2; done
    echo "ci-release-gates: let main's CI finish on this commit (or re-run it), then re-run this release" >&2
    exit 1
  fi
  echo "ci-release-gates: waiting for$missing (${elapsed}s of ${wait}s)"
  sleep 60
done
