#!/bin/sh
# Install the greengate binaries (and their old names, amont and
# amont-fleet, as copies). Nothing else.
#
#   curl -fsSL https://raw.githubusercontent.com/greenharness/greengate/main/install/install.sh | sh
#
# This script deliberately does NOT turn any hooks on. It downloads a verified
# binary, puts it somewhere your shims can find it, and tells you what to run
# next. That restraint is the point: this project's posture is that nothing
# runs in a repository you did not ask, and an installer that quietly enabled
# hooks — or worse, set `init.templateDir` so every future clone got them —
# would contradict the guarantee on its first contact with your machine.
#
# POSIX sh, no bashisms, because the shims are POSIX sh for the same reason:
# this has to run wherever git does.
set -eu

REPO="greenharness/greengate"
# `$HOME/.local/bin` by default, and not arbitrarily: it is candidate 3 in the
# shim's own resolution order, so a binary here is found even by a shim whose
# path was never baked.
BIN_DIR="${AMONT_BIN_DIR:-$HOME/.local/bin}"
VERSION="${AMONT_VERSION:-latest}"

RED='\033[31m'; GREEN='\033[32m'; YELLOW='\033[33m'; OFF='\033[0m'
if [ -n "${NO_COLOR:-}" ] || [ ! -t 1 ]; then RED=''; GREEN=''; YELLOW=''; OFF=''; fi

say()  { printf '  %s\n' "$1"; }
ok()   { printf "  ${GREEN}✓${OFF} %s\n" "$1"; }
warn() { printf "  ${YELLOW}!${OFF} %s\n" "$1"; }
die()  { printf "  ${RED}✗${OFF} %s\n" "$1" >&2; exit 1; }

need() { command -v "$1" > /dev/null 2>&1 || die "$1 is required and was not found"; }

need uname
need tar

# curl or wget, whichever is here.
if command -v curl > /dev/null 2>&1; then
    fetch() { curl -fsSL "$1"; }
    fetch_to() { curl -fsSL "$1" -o "$2"; }
elif command -v wget > /dev/null 2>&1; then
    fetch() { wget -qO- "$1"; }
    fetch_to() { wget -qO "$2" "$1"; }
else
    die "neither curl nor wget is available"
fi

target() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$os" in
        Linux)
            # musl when there is no glibc: the static build runs on distros
            # older than whatever the release was built against, which is the
            # usual reason a "linux" binary fails for somebody.
            if ldd --version 2>&1 | grep -qi musl; then libc=musl; else libc=gnu; fi
            case "$arch" in
                x86_64|amd64)  echo "x86_64-unknown-linux-$libc" ;;
                aarch64|arm64) [ "$libc" = "musl" ] && die "no aarch64 musl build yet — build from source with cargo install greengate"
                               echo "aarch64-unknown-linux-gnu" ;;
                *) die "unsupported architecture: $arch" ;;
            esac
            ;;
        Darwin)
            case "$arch" in
                x86_64) echo "x86_64-apple-darwin" ;;
                arm64)  echo "aarch64-apple-darwin" ;;
                *) die "unsupported architecture: $arch" ;;
            esac
            ;;
        MINGW*|MSYS*|CYGWIN*)
            # Reachable: this runs under Git Bash, which every Git for Windows
            # install ships. Point at the PowerShell installer rather than the
            # releases page, because there IS a one-liner for this platform.
            die "on Windows, use PowerShell:
    irm https://raw.githubusercontent.com/$REPO/main/install/install.ps1 | iex
  or download the .zip from https://github.com/$REPO/releases"
            ;;
        *) die "unsupported OS: $os" ;;
    esac
}

resolve_version() {
    if [ "$VERSION" != "latest" ]; then
        echo "${VERSION#v}"
        return
    fi
    # The API rather than the /releases/latest redirect, so a rate-limited or
    # offline run fails LOUDLY here instead of downloading a 404 page and
    # handing you a tarball full of HTML.
    tag=$(fetch "https://api.github.com/repos/$REPO/releases/latest" \
        | sed -n 's/.*"tag_name" *: *"\([^"]*\)".*/\1/p' | head -n 1)
    [ -n "$tag" ] || die "could not determine the latest release (rate limited? set AMONT_VERSION=vX.Y.Z)"
    echo "${tag#v}"
}

main() {
    printf '\n  greengate installer\n\n'

    t=$(target)
    v=$(resolve_version)
    # Releases before 3.0.0 shipped `amont-<v>-<t>` with binaries named
    # amont and amont-fleet; a pinned rollback (AMONT_VERSION=2.3.1) must
    # still find them, and installs them under both names like any other.
    case "$v" in
        [012].*) prefix=amont; hook=amont; fleet=amont-fleet ;;
        *)       prefix=greengate; hook=greengate; fleet=greengate-fleet ;;
    esac
    name="${prefix}-${v}-${t}"
    base="https://github.com/$REPO/releases/download/v${v}"

    say "version:  $v"
    say "platform: $t"
    say "into:     $BIN_DIR"
    printf '\n'

    tmp=$(mktemp -d)
    # Clean up on the way out however we leave, a half-copied temporary in
    # $BIN_DIR included; an interrupt exits 130 (143 on TERM), not the status
    # of whatever it cut short.
    trap 'rm -rf "$tmp"; rm -f "$BIN_DIR"/.*.tmp.$$' EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM

    say "downloading…"
    fetch_to "$base/${name}.tar.gz" "$tmp/${name}.tar.gz" \
        || die "download failed: $base/${name}.tar.gz"

    # Checksums are not optional here. This binary runs on every commit with
    # your credentials and reads every staged file; verifying what it is before
    # putting it in that position is the whole argument the project makes about
    # its own dependencies, applied to itself.
    if fetch_to "$base/SHA256SUMS" "$tmp/SHA256SUMS" 2> /dev/null; then
        if command -v sha256sum > /dev/null 2>&1; then
            got=$(sha256sum "$tmp/${name}.tar.gz" | cut -d' ' -f1)
        elif command -v shasum > /dev/null 2>&1; then
            got=$(shasum -a 256 "$tmp/${name}.tar.gz" | cut -d' ' -f1)
        else
            got=""
        fi
        if [ -n "$got" ]; then
            want=$(grep " ${name}.tar.gz\$" "$tmp/SHA256SUMS" | cut -d' ' -f1 | head -n 1)
            [ -n "$want" ] || die "SHA256SUMS has no entry for ${name}.tar.gz"
            [ "$got" = "$want" ] || die "checksum mismatch — refusing to install
    expected $want
    got      $got"
            ok "checksum verified"
        else
            warn "no sha256 tool found — the download was NOT verified"
        fi
    else
        warn "no SHA256SUMS published for this release — the download was NOT verified"
    fi

    tar xzf "$tmp/${name}.tar.gz" -C "$tmp"
    # Whether this is an upgrade decides what to say at the end: the
    # first-install steps are wrong advice the second time.
    upgrading=0
    { [ -x "$BIN_DIR/greengate" ] || [ -x "$BIN_DIR/amont" ]; } && upgrading=1
    mkdir -p "$BIN_DIR"
    # Each binary, then its old name as a COPY of it: shims a 2.x install
    # baked to $BIN_DIR/amont keep running the new build. A copy, never a
    # link: a 2.x `amont install` copies onto its target, and through a link
    # it would overwrite greengate itself.
    for pair in "$hook:greengate:amont" "$fleet:greengate-fleet:amont-fleet"; do
        src=${pair%%:*}
        rest=${pair#*:}
        b=${rest%%:*}
        old=${rest#*:}
        if [ -f "$tmp/$name/$src" ]; then
            for dest in "$b" "$old"; do
                # Write to a temporary name and rename over the destination:
                # replacing a RUNNING binary in place fails on some platforms,
                # and rename is atomic, so a half-copied binary never exists.
                cp "$tmp/$name/$src" "$BIN_DIR/.$dest.tmp.$$"
                chmod 755 "$BIN_DIR/.$dest.tmp.$$"
                mv "$BIN_DIR/.$dest.tmp.$$" "$BIN_DIR/$dest"
            done
            ok "installed $BIN_DIR/$b (and $old, its old name)"
        fi
    done

    printf '\n'
    case ":$PATH:" in
        *":$BIN_DIR:"*) ;;
        *) warn "$BIN_DIR is not on your PATH — add it, or the shims will still find the binary but you will not" ;;
    esac

    if [ "$upgrading" -eq 1 ]; then
        printf '  Upgraded. Hooks already run this binary — nothing per repository\n'
        printf '  needs redoing for the checks themselves. What does not update on its\n'
        printf '  own: a repository'"'"'s hook shims when a release changes them, and the\n'
        printf '  generated block in AGENTS.md/CLAUDE.md. One command sees both:\n\n'
        printf '    greengate-fleet fix --root ~/Developer                   # dry run\n'
        printf '    greengate-fleet fix --root ~/Developer --apply --agents-md\n\n'
        printf '  or, in one repository:  greengate install && greengate agents-md\n\n'
        return
    fi
    printf '  Nothing is enabled yet, on purpose. To turn the hooks on:\n\n'
    printf '    cd <your-repo> && greengate install # this repository only\n'
    printf '    greengate list                      # what would run here\n'
    printf '    greengate uninstall                 # and back out again\n\n'
    printf '  Across many repositories at once:  greengate-fleet install --root ~/Developer\n\n'
}

main "$@"
