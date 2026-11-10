#!/bin/sh
# publish-crates.sh publishes in dependency order and skips what is up.
#
# Run: sh scripts/test_publish_crates.sh (CI's `rust` job and `make lint` do).
# The registry probe and cargo are stubs: no network, nothing published.
set -eu

HERE=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

fail() { printf 'FAIL: %s\n' "$1" >&2; exit 1; }

# The probe answers 200 for the crates listed in $tmp/up, 404 otherwise.
cat > "$tmp/probe" <<'EOF'
#!/bin/sh
if grep -qx "$1" "$PROBE_UP" 2>/dev/null; then echo 200; else echo 404; fi
EOF
# cargo records what it was asked to publish; it answers "already uploaded"
# for the crates listed in $tmp/conflict.
cat > "$tmp/cargo" <<'EOF'
#!/bin/sh
echo "$3" >> "$CARGO_LOG"
if grep -qx "$3" "$CARGO_CONFLICT" 2>/dev/null; then
    echo "error: crate version \`x\` is already uploaded" >&2
    exit 101
fi
echo "Published $3"
EOF
chmod 755 "$tmp/probe" "$tmp/cargo"

run() {
    : > "$tmp/log"
    VERSION=3.0.0 PUBLISH_CRATES_PROBE="$tmp/probe" PUBLISH_CRATES_CARGO="$tmp/cargo" \
        PROBE_UP="$tmp/up" CARGO_LOG="$tmp/log" CARGO_CONFLICT="$tmp/conflict" \
        sh "$HERE/publish-crates.sh" > "$tmp/out" 2>&1
}

# A first release: all five, in dependency order.
: > "$tmp/up"; : > "$tmp/conflict"
run || fail "a clean publish failed: $(cat "$tmp/out")"
printf '%s\n' greenharness-quiet greenharness-slots greengate-runtime greengate greengate-fleet > "$tmp/want"
cmp -s "$tmp/log" "$tmp/want" || fail "order: got $(tr '\n' ' ' < "$tmp/log")"

# A re-run after the first two published: they are skipped, the rest publish.
printf '%s\n' greenharness-quiet greenharness-slots > "$tmp/up"
run || fail "a resumed publish failed: $(cat "$tmp/out")"
printf '%s\n' greengate-runtime greengate greengate-fleet > "$tmp/want"
cmp -s "$tmp/log" "$tmp/want" || fail "resume: got $(tr '\n' ' ' < "$tmp/log")"
grep -q "greenharness-quiet 3.0.0 is already on crates.io, skipped" "$tmp/out" \
    || fail "the skip was not said"

# The probe could not tell (it said 404 here) but cargo says "already
# uploaded": tolerated, and the run goes on.
: > "$tmp/up"; echo greengate-runtime > "$tmp/conflict"
run || fail "an already-uploaded answer failed the run: $(cat "$tmp/out")"
grep -qx greengate-fleet "$tmp/log" || fail "the run stopped after an already-uploaded crate"

# Any other cargo failure stops the run, and later crates are not published.
cat > "$tmp/cargo" <<'EOF'
#!/bin/sh
echo "$3" >> "$CARGO_LOG"
if [ "$3" = greengate-runtime ]; then echo "error: 401 Unauthorized" >&2; exit 101; fi
echo "Published $3"
EOF
: > "$tmp/conflict"
if run; then fail "a real failure did not fail the run"; fi
grep -qx greengate "$tmp/log" && fail "a crate after the failure was published"

echo "publish-crates: order, skip, resume and failure checks pass"
