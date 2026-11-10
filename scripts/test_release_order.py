#!/usr/bin/env python3
"""release_order.py keeps a resumed old release from moving a current pointer.

On 2026-11-10 a `workflow_dispatch` of v3.0.0, an hour after v3.0.1 shipped,
re-ran `publish-tap` and rewrote the homebrew formula back to 3.0.0: every
`brew upgrade` after it saw 3.0.0 as current. The same run would have moved
npm's `latest` dist-tag, and a first-time `gh release create` for an older
tag marks it the latest release. These tests pin the two answers the
release workflow asks before touching any of those.

Run: python3 -I scripts/test_release_order.py (CI's `rust` job and
`make lint` do). Standard library only, like the script.
"""

import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
SCRIPT = HERE / "release_order.py"


def run(*args: str, stdin: str = "") -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, "-I", str(SCRIPT), *args],
        input=stdin,
        capture_output=True,
        text=True,
    )


def newest(version: str, *tags: str) -> subprocess.CompletedProcess:
    """Tags on stdin, one per line, as the workflow pipes them."""
    return run("newest", version, stdin="".join(f"{t}\n" for t in tags))


class Newest(unittest.TestCase):
    def test_the_highest_release_is_newest(self) -> None:
        r = newest("3.0.1", "v2.3.1", "v3.0.0", "v3.0.1")
        self.assertEqual((r.returncode, r.stdout.strip()), (0, "true"), r.stderr)

    def test_a_resumed_older_release_is_not(self) -> None:
        r = newest("v3.0.0", "v2.3.1", "v3.0.0", "v3.0.1")
        self.assertEqual((r.returncode, r.stdout.strip()), (0, "false"), r.stderr)

    def test_versions_compare_as_numbers_not_text(self) -> None:
        # Text order puts 3.0.9 above 3.0.10.
        r = newest("3.0.10", "v3.0.9", "v3.0.10")
        self.assertEqual(r.stdout.strip(), "true")
        r = newest("3.0.9", "v3.0.9", "v3.0.10")
        self.assertEqual(r.stdout.strip(), "false")

    def test_a_prerelease_never_outranks_its_release(self) -> None:
        r = newest("3.1.0", "v3.1.0-rc.1", "v3.1.0")
        self.assertEqual(r.stdout.strip(), "true")
        r = newest("3.1.0-rc.1", "v3.1.0-rc.1", "v3.1.0")
        self.assertEqual(r.stdout.strip(), "false")

    def test_tags_that_are_not_versions_are_ignored(self) -> None:
        r = newest("3.0.1", "v3.0.1", "latest", "vNext", "v4")
        self.assertEqual(r.stdout.strip(), "true")

    def test_ls_remote_lines_are_read_as_their_tags(self) -> None:
        # What the workflow pipes: `<sha>\trefs/tags/<tag>`, one per line.
        lines = "".join(f"{'0' * 40}\trefs/tags/{t}\n" for t in ("v2.3.1", "v3.0.0", "v3.0.1"))
        self.assertEqual(run("newest", "3.0.0", stdin=lines).stdout.strip(), "false")
        self.assertEqual(run("newest", "3.0.1", stdin=lines).stdout.strip(), "true")

    def test_a_malformed_version_is_refused(self) -> None:
        r = newest("three", "v3.0.1")
        self.assertEqual(r.returncode, 1)
        self.assertIn("three", r.stderr)


class Tap(unittest.TestCase):
    def setUp(self) -> None:
        self.dir = pathlib.Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.dir)
        self.formula = self.dir / "greengate.rb"

    def tap(self, version: str, formula_version: str | None) -> subprocess.CompletedProcess:
        if formula_version is not None:
            self.formula.write_text(
                f'class Greengate < Formula\n  version "{formula_version}"\nend\n'
            )
        return run("tap", version, str(self.formula))

    def test_a_newer_release_moves_the_tap(self) -> None:
        r = self.tap("3.0.1", "3.0.0")
        self.assertEqual((r.returncode, r.stdout.strip()), (0, "move"), r.stderr)

    def test_the_same_release_rewrites_it_idempotently(self) -> None:
        r = self.tap("3.0.1", "3.0.1")
        self.assertEqual((r.returncode, r.stdout.strip()), (0, "move"), r.stderr)

    def test_an_older_release_leaves_a_newer_tap_alone(self) -> None:
        # The 2026-11-10 incident: v3.0.0 resumed over a 3.0.1 tap.
        r = self.tap("v3.0.0", "3.0.1")
        self.assertEqual((r.returncode, r.stdout.strip()), (0, "keep"), r.stderr)
        self.assertIn("3.0.1", r.stderr)

    def test_a_tap_without_the_formula_takes_it(self) -> None:
        r = self.tap("3.0.1", None)
        self.assertEqual((r.returncode, r.stdout.strip()), (0, "move"), r.stderr)

    def test_a_formula_without_a_version_line_is_refused(self) -> None:
        self.formula.write_text("class Greengate < Formula\nend\n")
        r = run("tap", "3.0.1", str(self.formula))
        self.assertEqual(r.returncode, 1)


if __name__ == "__main__":
    unittest.main()
