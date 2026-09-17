# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Paul Richeson
"""The two programs agree on what version they are.

Both write it into the HTML they export, so a drift shows up as a wall of
differential failures pointing at byte 4072 of index.html and saying nothing
about the cause. This says the cause.
"""
import os
import re
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def cargo_version():
    with open(os.path.join(ROOT, "Cargo.toml")) as f:
        for line in f:
            m = re.match(r'version = "([^"]+)"', line)
            if m:
                return m.group(1)
    raise AssertionError("no version in Cargo.toml")


def script_version():
    with open(os.path.join(ROOT, "radbeeper")) as f:
        for line in f:
            m = re.match(r'VERSION = "([^"]+)"', line)
            if m:
                return m.group(1)
    raise AssertionError("no VERSION in the reference script")


class TestTheVersionsAgree(unittest.TestCase):
    def test_the_manifest_and_the_script_say_the_same_version(self):
        self.assertEqual(
            cargo_version(), script_version(),
            "Cargo.toml and the reference script disagree -- bump both, or "
            "the exported pages differ and every byte-for-byte test fails")


if __name__ == "__main__":
    unittest.main()
