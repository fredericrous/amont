#!/bin/sh
# Does this pull request change something that behaves per platform?
#
#   ci-platform-touched.sh <base-rev>
#
# Prints `true` or `false` on the last line of stdout; the reasons, one file
# each, before it. `true` makes the pull request run macOS and Windows as well
# as Linux (decisions plan 2026-10-17-amont-ci-proves-platforms-once);
# `false` defers them to the push to main, which the release waits for.
#
# Platform-specific: what CI and the release build from (the workflows, the
# CI scripts, the toolchain and the dependency tree), what ships per platform
# (the installers, the npm platform packages, the hook templates), and a
# product source file that compiles differently per platform (`cfg(windows)`,
# `cfg(unix)`, `cfg(target_os`, `cfg(target_family`, and their `not(`). A
# test that is merely `cfg(unix)` does not count: it gates where a test runs,
# not what the product does.
#
# Fail-open: a base it cannot diff against reads as `true`, which runs more.
set -eu

base=${1:?usage: ci-platform-touched.sh <base-rev>}

if ! changed=$(git diff --name-only "$base"...HEAD 2> /dev/null); then
  echo "cannot diff against $base"
  echo true
  exit 0
fi

touched=false
cfg='cfg(_attr)?\((not\()?(windows|unix|target_os|target_family)'
for f in $changed; do
  why=
  case $f in
    .github/workflows/* | scripts/ci-* | Cargo.toml | Cargo.lock | */Cargo.toml \
      | rust-toolchain.toml | .cargo/* | install/* | npm/* | templates/*)
      why="builds or ships per platform" ;;
    crates/*/src/*)
      if [ -f "$f" ] && grep -qE "$cfg" "$f"; then why="compiles per platform"; fi ;;
  esac
  if [ -n "$why" ]; then
    echo "$f: $why"
    touched=true
  fi
done
echo "$touched"
