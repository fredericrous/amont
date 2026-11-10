#!/usr/bin/env python3
"""Rewrite the homebrew formula for a release — asserts, not hope.

The tap bump was the release's last manual step, done by hand four times in
three days: copy a version, copy four sha256 lines, push. A transcription
error there ships a formula brew refuses (checksum mismatch) or, worse,
accepts. This script is the closed loop: it reads the release's own
SHA256SUMS, rewrites exactly the url/sha pairs the formula declares, and
refuses loudly on ANY surprise — a checksum file for the wrong version, a
target the formula wants that the release did not build, a rewrite count
other than what the formula carries, a stale version string surviving.

Idempotent on purpose: re-run against a formula already at the version it
produces byte-identical output, so the workflow's "nothing to commit" path
is how a resumed release run no-ops.

Since 3.0.0 the archives are `greengate-<v>-<target>`; a release also ships
old-name `amont-<v>-<target>` archives for 2.x installers until the census,
and the formula names only the `greengate-` ones. A formula still on the old
prefix (the 2.3.1 one) moves to the new.

Usage: bump-tap.py <version> <SHA256SUMS> <Formula/greengate.rb>
"""

import pathlib
import re
import sys

# Where the release assets live. The formula may still name the old owner
# (`fredericrous/amont`); the patterns below accept either and write this one.
REPO = "greenharness/greengate"


def main() -> None:
    version, sums_path, formula_path = sys.argv[1:4]
    version = version.lstrip("v")

    sums: dict[str, tuple[str, str]] = {}
    for line in pathlib.Path(sums_path).read_text().splitlines():
        parts = line.split()
        if len(parts) != 2:
            continue
        sha, name = parts
        # Only the new name: the old-name archives are for 2.x installers.
        m = re.fullmatch(r"greengate-([0-9.]+)-(.+?)\.(?:tar\.gz|zip)", name)
        if not m:
            continue
        assert m.group(1) == version, (
            f"SHA256SUMS is for {m.group(1)}, not {version} — wrong release?"
        )
        sums[m.group(2)] = (sha, name)
    assert sums, f"no greengate checksums parsed from {sums_path}"

    formula = pathlib.Path(formula_path)
    text = formula.read_text()
    text, n_version = re.subn(
        r'version "[0-9.]+"', f'version "{version}"', text, count=1
    )
    assert n_version == 1, "the formula carries no version line"

    def rewrite(m: "re.Match[str]") -> str:
        target = m.group("target")
        assert target in sums, (
            f"the formula wants {target} and this release did not publish it"
        )
        sha, name = sums[target]
        return (
            f'url "https://github.com/{REPO}/releases/'
            f'download/v{version}/{name}"'
            f'{m.group("mid")}sha256 "{sha}"'
        )

    pair = re.compile(
        r'url "https://github\.com/(?:fredericrous/amont|greenharness/greengate)/releases/'
        r'download/v[0-9.]+/(?:amont|greengate)-[0-9.]+-(?P<target>[a-z0-9_-]+)\.tar\.gz"'
        r'(?P<mid>\s+)sha256 "[0-9a-f]{64}"'
    )
    text, n = pair.subn(rewrite, text)
    assert n == 4, f"expected 4 url/sha pairs in the formula, rewrote {n}"

    # The repository moved (2026-11-08); a formula still on the old owner's
    # homepage is pointed at the new one, and a current one is left as it is.
    text, n_home = re.subn(
        r'homepage "https://github\.com/(?:fredericrous/amont|greenharness/greengate)"',
        f'homepage "https://github.com/{REPO}"',
        text,
        count=1,
    )
    assert n_home == 1, "the formula carries no homepage line naming fredericrous/amont or greenharness/greengate"

    stale = set(re.findall(r"(?:amont|greengate)-([0-9.]+)-", text)) - {version}
    assert not stale, f"stale release versions survived the rewrite: {stale}"

    formula.write_text(text)
    print(f"formula -> {version} ({n} targets)")


if __name__ == "__main__":
    main()
