#!/bin/sh
# Drives the README demo. Everything here is REAL: a real repository, the real
# release binary, real hook output, recorded in one pass. Nothing is re-enacted,
# and nothing is edited afterwards — which is the point, since the thing being
# demonstrated is that these checks behave the way the README says they do.
#
# To re-record (needs `asciinema` and `agg`):
#
#     cargo build --release
#     rm -rf /tmp/shop /tmp/shop-home
#     mkdir -p /tmp/shop-home/.local/bin /tmp/shop
#     cp target/release/amont /tmp/shop-home/.local/bin/amont
#     export HOME=/tmp/shop-home \
#            XDG_CONFIG_HOME=/tmp/shop-home/.config \
#            GIT_CONFIG_GLOBAL=/tmp/shop-home/.gitconfig \
#            GIT_CONFIG_SYSTEM=/dev/null
#     cd /tmp/shop && git init -q -b main .
#     git config user.email dev@example.com && git config user.name Dev
#     printf 'export const cart = [];\n' > cart.js
#     printf '{"items": []}\n'            > data.json
#     git add -A
#     /tmp/shop-home/.local/bin/amont install < /dev/null > /dev/null
#     DEMO_BIN=/tmp/shop-home/.local/bin/amont asciinema rec --overwrite \
#         --cols 96 --rows 30 --command "sh <repo>/docs/assets/demo.sh" \
#         <repo>/docs/assets/amont-demo.cast
#     agg --theme monokai --font-size 15 --idle-time-limit 2 \
#         <repo>/docs/assets/amont-demo.cast \
#         <repo>/docs/assets/amont-demo.gif
#
# The isolated HOME/XDG/GIT_CONFIG_* environment is not tidiness. Without it the
# recording prints the recorder's own checkout paths and their global
# `init.templateDir` — noise, and somebody's directory layout published to
# strangers.
#
# `greengate install` runs BEFORE the recording rather than inside it: it prompts
# about AGENTS.md on a tty, and a demo that hangs waiting for a keypress records
# the hang.
set -eu

BIN="$DEMO_BIN"
export AMONT_BIN="$BIN"

say() {
    printf '\033[32m$\033[0m %s\n' "$1"
    sleep 0.8
}

sleep 0.3

say 'greengate list --stage pre-push'
"$BIN" list --stage pre-push
sleep 3

printf '\n'
say 'git commit -m "Add to Cart"'
git commit -m "Add to Cart" || true
sleep 4

printf '\n'
say 'git commit -m "feat: a cart the checks agree with"'
git commit -m "feat: a cart the checks agree with" || true
sleep 3.5

printf '\n'
say 'greengate uninstall'
"$BIN" uninstall
sleep 3.5
