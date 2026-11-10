#!/usr/bin/env python3
"""release-manifest.py writes the [tool.release] grh pins, or refuses.

Run: python3 -I scripts/test_release_manifest.py (CI's `rust` job and
`make lint` do). Standard library only, like the script; Python 3.11 or later.
"""

import hashlib
import pathlib
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest

HERE = pathlib.Path(__file__).resolve().parent
SCRIPT = HERE / "release-manifest.py"
MANIFEST = HERE.parent / "greenharness.toml"
TARGETS = (
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-gnu",
)


class ReleaseManifest(unittest.TestCase):
    def setUp(self) -> None:
        self.dir = pathlib.Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.dir)
        self.version = tomllib.loads(MANIFEST.read_text())["extension"]["version"]
        self.dist = self.dir / "dist"
        self.dist.mkdir()
        self.expected = {}
        for target in TARGETS:
            body = f"tarball for {target}".encode()
            (self.dist / f"greengate-{self.version}-{target}.tar.gz").write_bytes(body)
            self.expected[target] = hashlib.sha256(body).hexdigest()
        # Present in a real dist/ and never part of [tool.release].
        (self.dist / f"greengate-{self.version}-x86_64-pc-windows-msvc.zip").write_bytes(b"zip")
        (self.dist / f"amont-{self.version}-x86_64-apple-darwin.tar.gz").write_bytes(b"old name")
        self.out = self.dist / "greenharness.toml"

    def run_script(self, version: str, manifest: pathlib.Path = MANIFEST) -> subprocess.CompletedProcess:
        return subprocess.run(
            [sys.executable, "-I", str(SCRIPT), version, str(self.dist), str(manifest), str(self.out)],
            capture_output=True,
            text=True,
        )

    def test_the_written_manifest_carries_the_tarballs_sums(self) -> None:
        run = self.run_script(f"v{self.version}")
        self.assertEqual(run.returncode, 0, run.stderr)
        written = tomllib.loads(self.out.read_text())
        self.assertEqual(
            written["tool"]["release"],
            {"repo": "greenharness/greengate", "version": self.version, "sha256": self.expected},
        )
        # Everything else is the committed manifest, unchanged.
        committed = tomllib.loads(MANIFEST.read_text())
        del written["tool"]["release"]
        self.assertEqual(written, committed)
        self.assertEqual(written["extension"]["name"], "greengate")
        self.assertEqual(written["tool"]["bin"], "greengate")

    def test_a_version_other_than_the_manifests_is_refused(self) -> None:
        run = self.run_script("v9.9.9")
        self.assertEqual(run.returncode, 1)
        self.assertIn("declares [extension] version", run.stderr)
        self.assertFalse(self.out.exists())

    def test_a_missing_tarball_is_refused(self) -> None:
        (self.dist / f"greengate-{self.version}-aarch64-unknown-linux-gnu.tar.gz").unlink()
        run = self.run_script(self.version)
        self.assertEqual(run.returncode, 1)
        self.assertIn("aarch64-unknown-linux-gnu", run.stderr)
        self.assertFalse(self.out.exists())

    def test_a_committed_manifest_with_sums_is_refused(self) -> None:
        with_sums = self.dir / "with-sums.toml"
        with_sums.write_text(
            MANIFEST.read_text() + '\n[tool.release]\nrepo = "x"\nversion = "1"\n'
        )
        run = self.run_script(self.version, with_sums)
        self.assertEqual(run.returncode, 1)
        self.assertIn("already carries [tool.release]", run.stderr)


if __name__ == "__main__":
    unittest.main()
