#!/usr/bin/env python3
"""Whether a release may move a "current" pointer — the tap, npm, GitHub.

A release run can be resumed for an OLD tag (`gh workflow run release.yaml
--ref v3.0.0`, the documented way to finish a publish that failed). Every
job of that run then publishes again, and three of them move a pointer
every installer reads as "the current version": the homebrew formula,
npm's `latest` dist-tag, and GitHub's latest release. On 2026-11-10 such a
resume, an hour after v3.0.1 shipped, rewrote the formula back to 3.0.0.

This script answers the two questions the workflow asks before moving one:

    release_order.py newest <version> < tags
        prints `true` when <version> is at least every release tag read
        from stdin, one per line (`git ls-remote --tags` lines work too;
        tags that are not versions are ignored), else `false`. Stdin, not
        arguments: an unquoted `$tags` is one argument in zsh, every tag
        ignored, and every version "newest" (found 2026-11-10).

    release_order.py tap <version> <Formula/greengate.rb>
        prints `move` when the formula is absent or at <version> or older,
        `keep` (with the reason on stderr) when it already names a newer
        version, which a resumed old release must never lower.

Versions compare as numbers (3.0.10 above 3.0.9), and a prerelease ranks
below its release (3.1.0-rc.1 below 3.1.0). Exit 1 on a malformed input,
never a guess.
"""

import pathlib
import re
import sys
from typing import NoReturn

VERSION = re.compile(r"v?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?")


def fail(message: str) -> NoReturn:
    """Refuse, on stderr, exit 1. Not `assert`: `python -O` strips those."""
    print(f"release_order: {message}", file=sys.stderr)
    sys.exit(1)


def key(text: str) -> tuple | None:
    """A sort key for a version, or None when `text` is not one.

    A release sorts above every prerelease of it; prereleases among
    themselves compare by their identifiers, numbers as numbers.
    """
    m = VERSION.fullmatch(text.strip())
    if not m:
        return None
    major, minor, patch, pre = m.groups()
    if pre is None:
        rank: tuple = (1,)
    else:
        rank = (0, tuple((0, int(p), "") if p.isdigit() else (1, 0, p) for p in pre.split(".")))
    return (int(major), int(minor), int(patch), rank)


def version_key(text: str) -> tuple:
    k = key(text)
    if k is None:
        fail(f"not a version: {text!r}")
    return k


def newest(version: str, tags: list[str]) -> bool:
    mine = version_key(version)
    return all(mine >= k for k in (key(t) for t in tags) if k is not None)


def tap(version: str, formula: pathlib.Path) -> str:
    mine = version_key(version)
    if not formula.exists():
        return "move"
    m = re.search(r'^\s*version "([^"]+)"', formula.read_text(encoding="utf-8"), re.M)
    if not m:
        fail(f"{formula} carries no version line")
    current = m.group(1)
    if version_key(current) > mine:
        print(
            f"the tap already carries {current}, newer than {version.lstrip('v')}; left as it is",
            file=sys.stderr,
        )
        return "keep"
    return "move"


def main() -> None:
    args = sys.argv[1:]
    if len(args) == 2 and args[0] == "newest":
        tags = [line.rsplit("/", 1)[-1] for line in sys.stdin.read().split()]
        print("true" if newest(args[1], tags) else "false")
    elif len(args) == 3 and args[0] == "tap":
        print(tap(args[1], pathlib.Path(args[2])))
    else:
        fail("usage: release_order.py newest <version> < tags | tap <version> <formula>")


if __name__ == "__main__":
    main()
