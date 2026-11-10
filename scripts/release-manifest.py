#!/usr/bin/env python3
"""Write the release's copy of greenharness.toml, with [tool.release].

grh pins the manifest a release publishes (grh docs/extension-manifest.md,
"Shipping a manifest"). The committed greenharness.toml carries no sums: the
tarballs do not exist when it is committed. This script, run by the release
after the archives are built and BEFORE SHA256SUMS is computed, writes a copy
that adds

    [tool.release]
    repo = "greenharness/greengate"
    version = "<version>"
    [tool.release.sha256]
    <target> = "<sha256 of greengate-<version>-<target>.tar.gz>"

for each unix target grh installs, so the published manifest is checksummed
and attested with the archives it describes.

It refuses, with exit 1, anything that would publish a manifest grh then
refuses: a version that is not the manifest's [extension] version, a
manifest that already carries [tool.release], a missing tarball, or an
output that does not read back with the same values.

Usage: release-manifest.py <version> <dist-dir> <greenharness.toml> <out>
Python 3.11 or later (tomllib); run it as `python3 -I`.
"""

import hashlib
import pathlib
import sys
import tomllib

REPO = "greenharness/greengate"
# The targets grh downloads: the unix tarballs. Windows ships a .zip that
# grh does not install.
TARGETS = (
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-gnu",
)


def fail(message: str) -> None:
    print(f"release-manifest: {message}", file=sys.stderr)
    sys.exit(1)


def sha256_of(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main() -> None:
    if len(sys.argv) != 5:
        print(__doc__.strip().splitlines()[-2], file=sys.stderr)
        sys.exit(2)
    version = sys.argv[1].removeprefix("v")
    dist = pathlib.Path(sys.argv[2])
    source = pathlib.Path(sys.argv[3])
    out = pathlib.Path(sys.argv[4])

    text = source.read_text()
    manifest = tomllib.loads(text)
    declared = manifest.get("extension", {}).get("version")
    if declared != version:
        fail(f"{source} declares [extension] version {declared!r}, the release is {version!r}")
    if "release" in manifest.get("tool", {}):
        fail(f"{source} already carries [tool.release]; the committed manifest holds no sums")

    sums = {}
    for target in TARGETS:
        tarball = dist / f"greengate-{version}-{target}.tar.gz"
        if not tarball.is_file():
            fail(f"no {tarball.name} in {dist}")
        sums[target] = sha256_of(tarball)

    block = [
        "",
        "# Written by scripts/release-manifest.py from the release's own tarballs.",
        "[tool.release]",
        f'repo = "{REPO}"',
        f'version = "{version}"',
        "[tool.release.sha256]",
    ]
    block += [f'{target} = "{sums[target]}"' for target in TARGETS]
    written = text.rstrip("\n") + "\n" + "\n".join(block) + "\n"

    back = tomllib.loads(written).get("tool", {}).get("release", {})
    if back != {"repo": REPO, "version": version, "sha256": sums}:
        fail(f"the written manifest does not read back as written: {back!r}")

    out.write_text(written)
    print(f"release-manifest: {out} for {version} ({len(sums)} targets)")


if __name__ == "__main__":
    main()
