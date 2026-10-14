#!/bin/sh
# Record that a CI gate passed on the tree this job checked out.
#
#   ci-attest-result.sh <gate>
#
# Writes $RUNNER_TEMP/attest-result-<gate>.json and prints one line. Every
# heavy job calls this as its last step, only when it succeeded, and uploads the
# file as an artifact; the `sign` job (ci.yaml) is the one writer of signatures
# and signs a gate only from a result that names the tree it is itself standing
# on. The job that RAN the tests never holds the signing key: it executes the
# pull request's code.
#
# Plain printf, no jq: this also runs under Git Bash on the Windows leg.
set -eu

gate=${1:?usage: ci-attest-result.sh <gate>}
out=${RUNNER_TEMP:-/tmp}/attest-result-$gate.json

tree=$(git rev-parse 'HEAD^{tree}')
sha=$(git rev-parse HEAD)

# <arch>-<os>, the names attest uses (Rust's std::env::consts).
arch=$(uname -m)
case $arch in
  arm64) arch=aarch64 ;;
  amd64) arch=x86_64 ;;
esac
case $(uname -s) in
  Linux) os=linux ;;
  Darwin) os=macos ;;
  MINGW* | MSYS* | CYGWIN*) os=windows ;;
  *) os=$(uname -s | tr '[:upper:]' '[:lower:]') ;;
esac

printf '{"gate":"%s","platform":"%s-%s","tree":"%s","sha":"%s","outcome":"success"}\n' \
  "$gate" "$arch" "$os" "$tree" "$sha" > "$out"
echo "ci-attest-result: $gate passed on $arch-$os for tree $tree"
