#!/usr/bin/env python3
"""bump-tap.py rewrites the formula onto the repository's home and its name.

The repository moved from fredericrous/amont to greenharness/greengate
(2026-11-08), and 3.0.0 renamed the archives from `amont-<v>-<target>` to
`greengate-<v>-<target>`, while still shipping old-name archives for 2.x
installers. A formula written before either change must come out naming the
new owner and the new archives; a formula already there must stay there.

Run: python3 scripts/test_bump_tap.py (CI's `rust` job and `make lint` do).
Standard library only, like the script.
"""

import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
SCRIPT = HERE / "bump-tap.py"

TARGETS = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-gnu",
]


def formula(owner_repo: str, version: str = "2.3.1", prefix: str = "amont") -> str:
    pairs = "".join(
        f'  url "https://github.com/{owner_repo}/releases/download/'
        f'v{version}/{prefix}-{version}-{t}.tar.gz"\n  sha256 "{"0" * 64}"\n'
        for t in TARGETS
    )
    return (
        "class Greengate < Formula\n"
        f'  homepage "https://github.com/{owner_repo}"\n'
        f'  version "{version}"\n{pairs}end\n'
    )


class BumpTap(unittest.TestCase):
    def setUp(self) -> None:
        self.dir = pathlib.Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.dir)
        self.formula = self.dir / "greengate.rb"
        self.sums = self.dir / "SHA256SUMS"
        # A 3.0.0 release: the new archives, and the old-name ones beside
        # them, each with its own digest.
        self.sums.write_text(
            "".join(f"{'a' * 64}  greengate-3.0.0-{t}.tar.gz\n" for t in TARGETS)
            + "".join(f"{'b' * 64}  amont-3.0.0-{t}.tar.gz\n" for t in TARGETS)
        )

    def bump(self, version: str = "v3.0.0") -> subprocess.CompletedProcess:
        return subprocess.run(
            [sys.executable, "-I", str(SCRIPT), version, str(self.sums), str(self.formula)],
            capture_output=True,
            text=True,
        )

    def assert_on_new_name(self, text: str) -> None:
        self.assertEqual(text.count("greenharness/greengate"), 5, text)
        self.assertNotIn("fredericrous/amont", text)
        self.assertIn('homepage "https://github.com/greenharness/greengate"', text)
        self.assertIn('version "3.0.0"', text)
        # Only the new archives, with their own digests.
        self.assertEqual(text.count("/greengate-3.0.0-"), 4, text)
        self.assertNotRegex(text, r"releases/download/v[0-9.]+/amont-")
        self.assertEqual(text.count(f'sha256 "{"a" * 64}"'), 4)
        self.assertNotIn("b" * 64, text)

    def test_an_old_name_formula_moves_to_greengate_archives(self) -> None:
        self.formula.write_text(formula("greenharness/greengate"))
        run = self.bump()
        self.assertEqual(run.returncode, 0, run.stderr)
        self.assert_on_new_name(self.formula.read_text())

    def test_an_old_owner_formula_moves_too(self) -> None:
        self.formula.write_text(formula("fredericrous/amont"))
        run = self.bump()
        self.assertEqual(run.returncode, 0, run.stderr)
        self.assert_on_new_name(self.formula.read_text())

    def test_a_greengate_formula_stays_on_greengate(self) -> None:
        self.formula.write_text(formula("greenharness/greengate", "3.0.0", "greengate"))
        run = self.bump()
        self.assertEqual(run.returncode, 0, run.stderr)
        self.assert_on_new_name(self.formula.read_text())

    def test_a_second_run_changes_nothing(self) -> None:
        self.formula.write_text(formula("fredericrous/amont"))
        self.assertEqual(self.bump().returncode, 0)
        once = self.formula.read_text()
        self.assertEqual(self.bump().returncode, 0)
        self.assertEqual(self.formula.read_text(), once)

    def test_sums_for_another_version_are_refused(self) -> None:
        self.formula.write_text(formula("greenharness/greengate", "3.0.0", "greengate"))
        run = self.bump("v3.0.1")
        self.assertNotEqual(run.returncode, 0)
        self.assertIn("SHA256SUMS is for 3.0.0, not 3.0.1", run.stderr)

    def test_a_stale_greengate_version_is_refused(self) -> None:
        # Five url lines, one of them a version the release does not touch:
        # only four pairs are rewritten and the fifth survives.
        text = formula("greenharness/greengate", "3.0.0", "greengate")
        text = text.replace(
            "end\n",
            '  # resource "old", url "https://x/greengate-2.9.0-aarch64-apple-darwin.tar.gz"\nend\n',
        )
        self.formula.write_text(text)
        run = self.bump()
        self.assertNotEqual(run.returncode, 0)
        self.assertIn("stale release versions survived", run.stderr)

    def test_old_name_sums_alone_are_refused(self) -> None:
        self.sums.write_text(
            "".join(f"{'b' * 64}  amont-3.0.0-{t}.tar.gz\n" for t in TARGETS)
        )
        self.formula.write_text(formula("greenharness/greengate"))
        run = self.bump()
        self.assertNotEqual(run.returncode, 0)
        self.assertIn("no greengate checksums parsed", run.stderr)

    def test_the_shipped_formula_bumps(self) -> None:
        # The file publish-tap actually copies into the tap, not a stand-in.
        shipped = HERE.parent / "packaging" / "homebrew" / "greengate.rb"
        self.formula.write_text(shipped.read_text(encoding="utf-8"), encoding="utf-8")
        run = self.bump()
        self.assertEqual(run.returncode, 0, run.stderr)
        self.assert_on_new_name(self.formula.read_text())

    def test_a_formula_without_a_homepage_is_refused(self) -> None:
        self.formula.write_text(
            formula("fredericrous/amont").replace(
                '  homepage "https://github.com/fredericrous/amont"\n', ""
            )
        )
        run = self.bump()
        self.assertNotEqual(run.returncode, 0)
        self.assertIn("no homepage line", run.stderr)


if __name__ == "__main__":
    unittest.main()
