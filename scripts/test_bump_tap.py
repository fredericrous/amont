#!/usr/bin/env python3
"""bump-tap.py rewrites the formula onto the repository's new home.

The repository moved from fredericrous/amont to greenharness/greengate
(2026-11-08). A formula written before the move still names the old owner
in its four `url` lines and its `homepage`; the next release must move all
five, and a formula already on the new owner must stay there.

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


def formula(owner_repo: str, version: str = "2.3.0") -> str:
    pairs = "".join(
        f'  url "https://github.com/{owner_repo}/releases/download/'
        f'v{version}/amont-{version}-{t}.tar.gz"\n  sha256 "{"0" * 64}"\n'
        for t in TARGETS
    )
    return (
        "class Amont < Formula\n"
        f'  homepage "https://github.com/{owner_repo}"\n'
        f'  version "{version}"\n{pairs}end\n'
    )


class BumpTap(unittest.TestCase):
    def setUp(self) -> None:
        self.dir = pathlib.Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.dir)
        self.formula = self.dir / "amont.rb"
        self.sums = self.dir / "SHA256SUMS"
        self.sums.write_text(
            "".join(f"{'a' * 64}  amont-2.3.1-{t}.tar.gz\n" for t in TARGETS)
        )

    def bump(self) -> subprocess.CompletedProcess:
        return subprocess.run(
            [sys.executable, "-I", str(SCRIPT), "v2.3.1", str(self.sums), str(self.formula)],
            capture_output=True,
            text=True,
        )

    def assert_on_new_home(self, text: str) -> None:
        self.assertEqual(text.count("greenharness/greengate"), 5, text)
        self.assertNotIn("fredericrous/amont", text)
        self.assertIn('homepage "https://github.com/greenharness/greengate"', text)
        self.assertIn('version "2.3.1"', text)
        self.assertEqual(text.count(f'sha256 "{"a" * 64}"'), 4)

    def test_an_old_owner_formula_moves_to_the_new_home(self) -> None:
        self.formula.write_text(formula("fredericrous/amont"))
        result = self.bump()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_on_new_home(self.formula.read_text())

    def test_a_new_owner_formula_stays_on_the_new_home(self) -> None:
        self.formula.write_text(formula("greenharness/greengate"))
        result = self.bump()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_on_new_home(self.formula.read_text())

    def test_a_second_run_changes_nothing(self) -> None:
        self.formula.write_text(formula("fredericrous/amont"))
        self.assertEqual(self.bump().returncode, 0)
        once = self.formula.read_text()
        self.assertEqual(self.bump().returncode, 0)
        self.assertEqual(self.formula.read_text(), once)

    def test_a_formula_without_a_homepage_is_refused(self) -> None:
        self.formula.write_text(
            formula("fredericrous/amont").replace(
                '  homepage "https://github.com/fredericrous/amont"\n', ""
            )
        )
        result = self.bump()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no homepage line", result.stderr)


if __name__ == "__main__":
    unittest.main()
