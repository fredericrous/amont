# Git-templates Makefile
# Author: https://github.com/fredericrous
MAKEFILE_PATH      := $(abspath $(lastword $(MAKEFILE_LIST)))
MAKEFILE_DIR       := $(dir $(MAKEFILE_PATH))
XDG_CONFIG_HOME	   ?= ~/.config
SRC_CTRL_HOOKS     := $(MAKEFILE_DIR)templates/hooks/*

# Windows produces amont.exe; everywhere else, amont. Detected from the
# ARTIFACT rather than by sniffing the OS: Git Bash, MSYS2 and Cygwin report
# three different `uname` strings and all build the same .exe. Recursive `=`,
# not `:=`, so it is evaluated when a recipe line runs — i.e. after the
# `cargo build` above it, when the file exists to be detected.
EXE                 = $(shell [ -f "$(MAKEFILE_DIR)target/release/amont.exe" ] \
                        || [ -f "$(MAKEFILE_DIR)target/debug/amont.exe" ] && echo .exe)
# Where installed binaries go. Mirrors `install::bin_dir()` in the runtime,
# which honours $AMONT_BIN_DIR and falls back to ~/.local/bin — so `make
# install-fleet` lands beside whatever `amont install` wrote.
#
# This was dropped in #62 when installation moved into the binary and the
# Makefile's own install recipe went with it. `install-fleet` still referenced
# it, so it expanded to nothing and the recipe ran `install … /amont-fleet`
# — an attempt to write to the filesystem root.
INSTALL_BIN_DIR    ?= $(if $(AMONT_BIN_DIR),$(AMONT_BIN_DIR),$(HOME)/.local/bin)

DEBUG_BIN           = $(MAKEFILE_DIR)target/debug/amont$(EXE)
RELEASE_BIN         = $(MAKEFILE_DIR)target/release/amont$(EXE)

# `check`, not `test`. A bare `make` used to run `test` alone, which is exactly
# the set CI's `rust` job does NOT gate on — so the local default could be green
# while CI was red on fmt or clippy without a single line of behaviour being
# wrong. See `lint` below.
all: check

# +x only the scripts, never data files. Keyed on the shebang rather than the
# extension, so the extensionless git entrypoints (commit-msg, pre-commit,
# pre-push, prepare-commit-msg) get +x. POSIX sh — no zsh needed.
#
# The loop used to also walk crates/amont/tests/*. Those are `.rs` files with
# no shebang, so every iteration read a first line and did nothing: a leftover
# from when the suites were executable zsh scripts. The comment likewise still
# excused "never data files like package.json" — there has been no package.json
# in templates/hooks since the shims replaced it. Only the four shims are
# executable artifacts now, so only they are walked.
chmodx:
	@for f in $(SRC_CTRL_HOOKS); do \
		[ -f "$$f" ] || continue; \
		if head -1 "$$f" | grep -q '^#!'; then chmod +x "$$f"; fi; \
	done

build:
	@cargo build --quiet

# `--show-output` so a case that SKIPPED for a missing tool is visible: cargo
# hides stdout for passing tests, and a silent skip is indistinguishable from a
# pass — the trap the zsh suites already guarded against.
#
# RUN=<filter> still works: `make test RUN=branch_pattern`.
test: chmodx
	@./scripts/check-no-deps.sh
	@cargo test $(if $(RUN),--test $(RUN)) -- --show-output

# EXACTLY what CI's `rust` job runs, in the same order, with the same flags.
# Keep them identical: the point of this target is that a contributor can
# reproduce a red CI locally, and any drift here silently removes that.
#
# It did not exist. `make test` ran check-no-deps + `cargo test`, the README
# told contributors "`make test` runs the tests", and CI additionally gated on
# these two — so a commit that was fmt-dirty or tripped a clippy lint passed
# every local target the repo offered and went red on push.
#
# WARNING — there is a THIRD clippy invocation, and it is stricter than this
# one. `amont`' own `pre-commit-clippy` (crates/amont-runtime/src/hooks/
# rust_tools.rs) runs `cargo clippy --workspace --all-targets --all-features --
# -D warnings` on every Rust commit in this repo. CI's is `--all-targets` only:
# no `--workspace`, no `--all-features`. So the hook can reject a commit that
# this target and CI both accept. Unifying them is a DECISION about which is
# authoritative — the hook's wider net, or CI's cheaper one — not a quiet edit
# to whichever line you happened to be looking at. Left divergent on purpose,
# and stated here so the next person finds it before it surprises them.
lint:
	@cargo fmt --check
	@cargo clippy --all-targets -- -D warnings

# The full local gate: everything CI's `rust` job and `hooks` job assert.
# `test` stays lint-free so the inner loop is a `cargo test` and nothing else.
check: lint test

# Installation lives in the BINARY, not here: `amont install`.
#
# The routine has to decide whether a directory may be emptied —
# ~/.config/git/git-templates is commonly a symlink to a checkout, so
# "installing" there means deleting TRACKED files. That guard was got wrong
# twice. A Makefile can only be tested on the platforms that have `make`, and
# Git for Windows ships bash and coreutils but NOT make, so the shell version
# would have needed a PowerShell twin: two implementations of one dangerous
# guard. It is Rust now, with the worktree case that caused the second incident
# under test on every platform.
install: chmodx
	@cargo build --release --quiet
	@$(RELEASE_BIN) install

# The dashboard, installed SEPARATELY and on purpose.
#
# `make install` ships only the hook binary. That one runs on every commit in
# every repo, and its dependency-free posture is the entire reason the Rust
# migration happened; amont-fleet pulls ratatui, crossterm and serde. Keeping
# the two installs apart is what stops "I wanted the dashboard" from becoming
# "every commit now depends on a TUI library".
#
# Nothing is deleted here — it writes one binary — so this needs none of the
# fail-closed machinery `install` carries.
install-fleet:
	@cargo build --release --quiet -p amont-fleet
	@mkdir -p $(INSTALL_BIN_DIR)
	@install -m 0755 $(MAKEFILE_DIR)target/release/amont-fleet $(INSTALL_BIN_DIR)/amont-fleet
	@echo "installed $(INSTALL_BIN_DIR)/amont-fleet"
	@echo "  amont-fleet          report the fleet"
	@echo "  amont-fleet tui      the dashboard"
	@echo "  amont-fleet fix      what would change (dry run; --apply to write)"

# Push the shim SET to every repo. Only needed when a hook is added, removed
# or renamed — ordinary binary fixes reach all repos via `make install`, since
# every shim points at the one binary.
#
# This was a shell script until a Rust plan was proved to remove exactly the
# same set on a fixture covering every actionable case. Dry-run by default;
# APPLY=1 to write.
propagate:
	@cargo run -q -p amont-fleet -- fix $(if $(APPLY),--apply,)

# The commit path must stay dependency-free; see the script.
deps:
	@./scripts/check-no-deps.sh

.PHONY: all chmodx build test lint check install install-fleet propagate deps
