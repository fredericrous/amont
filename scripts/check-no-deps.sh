#!/bin/sh
# The hook binary must depend on nothing outside this workspace.
#
# NOT for the reason this comment used to give. It claimed the migration existed
# because a Python repo needed node and zsh installed to commit, so a dependency
# tree "would undo it". That conflates two different things: node and zsh were
# RUNTIME dependencies that had to exist on PATH in every repo, while a Rust
# crate is compiled in and statically linked. Adding `serde` would not make
# anyone install anything. The old justification did not hold.
#
# The reasons that do:
#
#   1. SUPPLY CHAIN, and this is the strong one. This binary runs on every
#      commit in 96 repos, with the developer's credentials, reading every
#      staged file, while nobody is watching. Every transitive crate is code
#      executing in that position. Zero dependencies means that code is std and
#      ours. The argument is about THIS binary; amont-fleet pulls ratatui and
#      a dozen crates quite happily, because it is opt-in and runs when asked.
#
#   2. OFFLINE REPRODUCIBILITY. A std-only crate builds indefinitely without a
#      registry. Real, though modest.
#
#   3. A FORCING FUNCTION. The guard prevents no specific harm so much as it
#      makes each dependency in the commit path an argued decision rather than a
#      default. It is something to argue WITH, not a wall: if the commit path
#      ever genuinely needs a parser, weigh that crate's tree against the code
#      it replaces and decide. Do not treat this file as a prohibition.
#
# A dependency reaches the commit path through amont-runtime as easily as
# directly, so the check is on the RESOLVED tree rather than on a manifest.
#
# Every step below fails CLOSED. This script used to send cargo's stderr to
# /dev/null and end the pipeline with `|| true`, so ANY failure to answer the
# question — cargo missing, registry unreachable, the package renamed, a
# manifest error, or simply being run from outside the workspace — produced an
# empty result, which read as "no external dependencies" and printed the
# reassuring line. The job named "hook binary has no external dependencies"
# would have stayed green through all of them. A guard that cannot fail is
# decoration.
set -eu

# Normal AND build edges, on EVERY target: a build script runs on the
# developer's machine as surely as the binary does, and a dependency declared
# under `[target.'cfg(windows)'.dependencies]` never shows on a macOS host's
# default query.
EDGES="--edges normal,build --target all"

# shellcheck disable=SC2086 # $EDGES is two flags, split on purpose
if ! tree_out=$(cargo tree -p amont $EDGES --prefix none); then
  echo "check-no-deps: cargo tree failed — the commit path is UNVERIFIED." >&2
  echo "That is a failure, not a pass: see the error above." >&2
  exit 1
fi

if [ -z "$tree_out" ]; then
  echo "check-no-deps: cargo tree produced no output — nothing was verified." >&2
  exit 1
fi

crates=$(printf '%s\n' "$tree_out" | sed 's/ (\*)$//' | awk '{print $1}' | sort -u)

# The root crate must be in its own tree. If it is not, we are reading
# something other than what we think we are, and an empty `ext` below would
# mean nothing.
if ! printf '%s\n' "$crates" | grep -qx 'amont'; then
  echo "check-no-deps: 'amont' is absent from its own dependency tree." >&2
  echo "Refusing to conclude anything from that. Output was:" >&2
  printf '%s\n' "$tree_out" | sed 's/^/  /' >&2
  exit 1
fi

# Two crates are allowed, each argued beside its declaration in
# crates/amont-runtime/Cargo.toml: git-attest (decisions ADR-0037), attest's
# std-only implementation of the attestation format, and amont-quiet
# (ADR-0039), the one implementation of the quiet-run active entry. Each is
# allowed by name, and only while its OWN tree is empty, checked below: a
# dependency it gains would reach the commit path through it.
ALLOWED="git-attest amont-quiet amont-slots"
#
# The one legitimate `|| true` in this file: `grep -v` exits 1 when nothing
# matches, and nothing matching IS the success case here.
ext=$(printf '%s\n' "$crates" | grep -vE '^(amont|amont-runtime|git-attest|amont-quiet|amont-slots)$' || true)

if [ -n "$ext" ]; then
  echo "amont gained external dependencies:" >&2
  echo "$ext" | sed 's/^/  /' >&2
  echo "" >&2
  echo "The commit path must stay dependency-free. Put this in amont-fleet." >&2
  exit 1
fi
for leaf in $ALLOWED; do
  # shellcheck disable=SC2086 # $EDGES is two flags, split on purpose
  if ! leaf_out=$(cargo tree -p "$leaf" $EDGES --prefix none); then
    echo "check-no-deps: cargo tree -p $leaf failed — the commit path is UNVERIFIED." >&2
    exit 1
  fi
  leaf_ext=$(printf '%s\n' "$leaf_out" | sed 's/ (\*)$//' | awk '{print $1}' | sort -u | grep -vx "$leaf" || true)
  if [ -n "$leaf_ext" ]; then
    echo "$leaf gained dependencies, which would reach the commit path:" >&2
    echo "$leaf_ext" | sed 's/^/  /' >&2
    exit 1
  fi
done
echo "amont: no external dependencies (git-attest, amont-quiet and amont-slots, std-only, are the allowed crates)"
