#!/usr/bin/env bash
# Lay out the seven npm packages from release artifacts already built.
#
#   scripts/npm-pack.sh <version> <artifact-dir> [out-dir]
#
# `<artifact-dir>` is the directory the release workflow downloads into: one
# `amont-<version>-<target>.tar.gz` (or `.zip`) per target, exactly as
# `release.yaml`'s `Archive` step produced them. Nothing is compiled here — the
# bytes published to npm are the SAME bytes published to the GitHub release and
# checksummed in `SHA256SUMS`, which is the property that makes the npm path
# worth trusting at all.
#
# Publishing is a separate step on purpose (`npm publish` in the workflow, or by
# hand from `<out-dir>`), so the layout can be inspected — and `npm pack`ed —
# before anything irreversible happens. npm versions are immutable.
set -euo pipefail

VERSION="${1:?usage: npm-pack.sh <version> <artifact-dir> [out-dir]}"
ARTIFACTS="${2:?usage: npm-pack.sh <version> <artifact-dir> [out-dir]}"
OUT="${3:-dist/npm}"

# shellcheck disable=SC1007  # `CDPATH= cd` is an assignment PREFIX scoped to
# that one command — the standard guard against a user's CDPATH making `cd`
# resolve elsewhere or print the directory. Not a mistyped assignment.
HERE=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

# target | npm package | os | cpu | libc
#
# The six `release.yaml` builds, and no more. aarch64-musl and aarch64-windows
# are deliberately absent: neither is built upstream, and a package that resolves
# to nothing is worse than one npm skips — it installs, then fails at the first
# commit. `install.sh` refuses the same pair for the same reason.
TARGETS=(
    "aarch64-apple-darwin|@amont-hooks/darwin-arm64|darwin|arm64|"
    "x86_64-apple-darwin|@amont-hooks/darwin-x64|darwin|x64|"
    "aarch64-unknown-linux-gnu|@amont-hooks/linux-arm64-gnu|linux|arm64|glibc"
    "x86_64-unknown-linux-gnu|@amont-hooks/linux-x64-gnu|linux|x64|glibc"
    "x86_64-unknown-linux-musl|@amont-hooks/linux-x64-musl|linux|x64|musl"
    "x86_64-pc-windows-msvc|@amont-hooks/win32-x64|win32|x64|"
)

say() { printf '  %s\n' "$1"; }
die() { printf '  ✗ %s\n' "$1" >&2; exit 1; }

command -v tar > /dev/null 2>&1 || die "tar is required"

rm -rf "$OUT"
mkdir -p "$OUT"

# --- the six platform packages -------------------------------------------

for row in "${TARGETS[@]}"; do
    IFS='|' read -r target pkg os cpu libc <<< "$row"

    name="amont-${VERSION}-${target}"
    # Every binary the archive carries. `npm/platform/package.json.in` lists
    # `bin/` as a DIRECTORY, so a package ships whatever is put in there and
    # this loop is the whole change needed to add one.
    suffix=
    [ "$os" = "win32" ] && suffix=.exe

    # Unpack into a scratch dir rather than reading the archive twice. A missing
    # archive is fatal: publishing five of six packages would leave `amont`'s
    # optionalDependencies naming a version that does not exist, and npm rejects
    # the whole install rather than skipping it.
    scratch="$OUT/.unpack/$target"
    mkdir -p "$scratch"
    if [ -f "$ARTIFACTS/$name.tar.gz" ]; then
        tar xzf "$ARTIFACTS/$name.tar.gz" -C "$scratch"
    elif [ -f "$ARTIFACTS/$name.zip" ]; then
        command -v unzip > /dev/null 2>&1 || die "unzip is required for $name.zip"
        unzip -q "$ARTIFACTS/$name.zip" -d "$scratch"
    else
        die "no archive for $target (looked for $ARTIFACTS/$name.tar.gz and .zip)"
    fi

    mkdir -p "$OUT/$pkg/bin"
    for b in amont amont-fleet; do
        src="$scratch/$name/$b$suffix"
        [ -f "$src" ] || die "$name archive holds no $b$suffix"
        cp "$src" "$OUT/$pkg/bin/$b$suffix"
        # npm preserves the mode of files in the tarball; a binary that arrives
        # without +x fails at the first commit with "permission denied".
        chmod 0755 "$OUT/$pkg/bin/$b$suffix"
    done

    # `libc` is a real npm/pnpm resolution field, and the ONLY thing separating
    # the two linux-x64 packages. Without it both match a linux/x64 host and the
    # resolver picks one at random — half the time the glibc build on Alpine,
    # which cannot exec at all.
    if [ -n "$libc" ]; then
        libc_block=$(printf '  "libc": [\n    "%s"\n  ],' "$libc")
    else
        libc_block=""
    fi

    python3 - "$HERE/npm/platform/package.json.in" "$OUT/$pkg/package.json" \
        "$pkg" "$VERSION" "$os" "$cpu" "$libc_block" <<'PY'
import sys
src, dst, pkg, version, os_, cpu, libc = sys.argv[1:8]
text = open(src).read()
for token, value in (("__PKG__", pkg), ("__VERSION__", version),
                     ("__OS__", os_), ("__CPU__", cpu)):
    text = text.replace(token, value)
# The libc line is whole-line: an empty substitution must take the newline with
# it, or the JSON keeps a blank line where a key was.
text = "\n".join(l for l in text.replace("__LIBC__", libc).split("\n")
                 if l.strip() != "")
open(dst, "w").write(text + "\n")
PY

    say "$pkg  ($target)"
done

rm -rf "$OUT/.unpack"

# --- the package people depend on ----------------------------------------

mkdir -p "$OUT/amont/bin"
# BOTH wrappers. `amont.js` is the bin entry and `native.js` is what it
# requires; shipping one without the other produces a package that installs
# cleanly and dies on first use with MODULE_NOT_FOUND.
#
# That is not hypothetical — it is what every published version from the
# introduction of native.js through 1.20.0 actually did. `files` in
# package.json.in named `bin/native.js` the whole time, but `files` cannot
# ship what was never staged here, so the manifest promised a file the packer
# had not copied and npm said nothing.
for f in amont.js native.js; do
    cp "$HERE/npm/amont/bin/$f" "$OUT/amont/bin/$f"
    chmod 0755 "$OUT/amont/bin/$f"
done
cp "$HERE/README.md" "$OUT/amont/README.md"
sed "s/__VERSION__/$VERSION/g" "$HERE/npm/amont/package.json.in" > "$OUT/amont/package.json"
say "amont"

# --- prove it before anybody publishes it --------------------------------
#
# Every failure below is one that cannot be fixed after the fact: npm versions
# are immutable, so a package published without its binary can only be
# superseded, never replaced.

for row in "${TARGETS[@]}"; do
    IFS='|' read -r _ pkg os _ _ <<< "$row"
    suffix=
    [ "$os" = "win32" ] && suffix=.exe
    for b in amont amont-fleet; do
        [ -x "$OUT/$pkg/bin/$b$suffix" ] || die "$pkg/bin/$b$suffix missing or not executable"
    done
    python3 -c "import json,sys; json.load(open(sys.argv[1]))" "$OUT/$pkg/package.json" \
        || die "$pkg/package.json is not valid JSON"
done
python3 -c "import json,sys; json.load(open(sys.argv[1]))" "$OUT/amont/package.json" \
    || die "amont/package.json is not valid JSON"

# Every file `files` names is actually here. The one failure this catches is
# the one that shipped for four releases: a manifest promising bin/native.js
# that the layout above never staged, producing a package that installs and
# then dies on first use.
python3 - "$OUT/amont" <<'PY'
import json, os, sys
root = sys.argv[1]
manifest = json.load(open(os.path.join(root, "package.json")))
missing = [f for f in manifest.get("files", [])
           if not os.path.exists(os.path.join(root, f))]
if missing:
    sys.exit("  \u2717 amont/package.json names files that were never staged: "
             + ", ".join(missing))
PY

# The dependency versions must match the packages actually laid out beside them.
python3 - "$OUT" "$VERSION" <<'PY'
import json, os, sys
out, version = sys.argv[1], sys.argv[2]
main = json.load(open(os.path.join(out, "amont", "package.json")))
missing = []
for dep, want in main.get("optionalDependencies", {}).items():
    path = os.path.join(out, dep, "package.json")
    if not os.path.exists(path):
        missing.append(f"{dep}: declared, not built")
        continue
    have = json.load(open(path))["version"]
    if have != want or want != version:
        missing.append(f"{dep}: declares {want}, package is {have}, release is {version}")
if missing:
    sys.exit("  ✗ " + "\n  ✗ ".join(missing))
PY

say ""
say "laid out $VERSION in $OUT — publish the six platform packages BEFORE amont,"
say "or npm rejects it for an optionalDependency version that does not exist yet."
