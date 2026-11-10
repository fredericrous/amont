#!/bin/sh
# Publish the workspace's crates to crates.io, in dependency order, resuming.
#
#   VERSION=3.0.0 scripts/publish-crates.sh
#
# ORDER IS A DEPENDENCY ORDER: crates.io resolves a dependency by VERSION, so
# each crate's dependencies must be in the index before it is accepted.
# greengate-runtime depends on greenharness-quiet and greenharness-slots;
# greengate and greengate-fleet depend on greengate-runtime. `cargo publish`
# waits for each upload to become available in the index before it returns,
# which is the wait between dependent publishes.
#
# RESUMING: a re-run of a release after a partial failure must not fail on a
# crate that is already up. Each crate is first probed on the registry API
# (`/api/v1/crates/<name>/<version>`): a 200 means that exact version is
# published, and it is skipped, the version read from the registry, never
# from an error text. Anything else (404, a rate limit, no network) publishes,
# and cargo's own "already uploaded" answer is still tolerated, because the
# API rate-limits datacenter IPs and a 429 is not an answer about the crate.
#
# For the test, the probe and cargo are commands from the environment:
# PUBLISH_CRATES_PROBE <name> <version> prints an HTTP status, and
# PUBLISH_CRATES_CARGO stands in for cargo.
set -eu

: "${VERSION:?set VERSION to the release version, without v}"
CRATES="greenharness-quiet greenharness-slots greengate-runtime greengate greengate-fleet"

probe() {
    if [ -n "${PUBLISH_CRATES_PROBE:-}" ]; then
        "$PUBLISH_CRATES_PROBE" "$1" "$2"
        return
    fi
    # crates.io answers 403 to a request without a User-Agent.
    curl -s -o /dev/null -w '%{http_code}' --max-time 20 \
        -A "greengate-release (github.com/greenharness/greengate)" \
        "https://crates.io/api/v1/crates/$1/$2" || echo 000
}

cargo_cmd=${PUBLISH_CRATES_CARGO:-cargo}

for crate in $CRATES; do
    status=$(probe "$crate" "$VERSION")
    if [ "$status" = "200" ]; then
        echo "$crate $VERSION is already on crates.io, skipped"
        continue
    fi
    echo "publishing $crate $VERSION (registry answered $status)"
    if out=$("$cargo_cmd" publish -p "$crate" 2>&1); then
        printf '%s\n' "$out"
    else
        printf '%s\n' "$out"
        if printf '%s' "$out" | grep -qiE "already (been )?uploaded|already exists"; then
            echo "  $crate $VERSION was already published, continuing"
        else
            echo "::error::publishing $crate failed"
            exit 1
        fi
    fi
done
