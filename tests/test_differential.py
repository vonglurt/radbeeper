# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Paul Richeson
"""The Python and the Rust write the same characters, or this fails.

The log format is owned by the Python program and is being ported to Rust one
piece at a time. The whole risk in doing that is a second dialect: two
programs that both write something plausible and not quite the same, so that a
month of rows depends on which binary happened to be running. Nothing catches
that by reading the code.

So both are run on the same inputs and the output is compared byte for byte.
Not "equivalent" -- identical. A `%g` that rounds differently in the sixth
significant figure, a timestamp with a space instead of a T, an empty field
written as 0.0: each of those is a silent, permanent corruption of a file
whose one promise is that `sort` on it is chronological, and each of them is
one character.

Skipped, not failed, when there is no Rust toolchain: the Python suite has to
run on a machine with no network and nothing installed, which is the whole
reason the Python exists.
"""
import os
import subprocess
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import importlib.machinery
import importlib.util

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
_loader = importlib.machinery.SourceFileLoader(
    "radbeeper", os.path.join(ROOT, "radbeeper"))
_spec = importlib.util.spec_from_loader("radbeeper", _loader)
radbeeper = importlib.util.module_from_spec(_spec)
sys.modules["radbeeper"] = radbeeper
_loader.exec_module(radbeeper)

ORACLE = os.path.join(ROOT, "rust", "target", "release", "examples",
                      "format_oracle")


def build_oracle():
    """The oracle binary, rebuilt whenever there is a cargo to rebuild it.

    NOT "use it if it is there". A stale oracle is worse than no oracle: it
    passes, silently, against a Rust that has since changed -- which is
    exactly what this test exists to catch. cargo is incremental and a no-op
    build costs a fraction of a second. Only a machine with no toolchain at
    all falls back to whatever binary happens to be on disk.
    """
    if not any(os.access(os.path.join(p, "cargo"), os.X_OK)
               for p in os.environ.get("PATH", "").split(os.pathsep)):
        return ORACLE if os.path.exists(ORACLE) else None
    try:
        subprocess.run(
            ["cargo", "build", "--release", "--example", "format_oracle"],
            cwd=os.path.join(ROOT, "rust"), check=True,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=300)
    except (subprocess.SubprocessError, OSError):
        return None
    return ORACLE if os.path.exists(ORACLE) else None


ORACLE_PATH = build_oracle()


@unittest.skipIf(ORACLE_PATH is None, "no Rust toolchain; nothing to diff")
class TestSameBytes(unittest.TestCase):

    def rust(self, directives):
        out = subprocess.run([ORACLE_PATH], input="\n".join(directives) + "\n",
                             capture_output=True, text=True, check=True)
        return out.stdout.splitlines()

    def compare(self, cases):
        """cases: [(directive line, the string Python produces)]"""
        got = self.rust([d for d, _ in cases])
        self.assertEqual(len(got), len(cases))
        for (directive, want), mine in zip(cases, got):
            self.assertEqual(mine, want, "on %r" % directive)

    def test_percent_g_agrees_to_the_last_character(self):
        # %g formats the seconds column and every span in the header. Rust has
        # no such formatter, so this one is written out by hand, and this is
        # what says it was written out correctly.
        values = [3, 30, 300, 3000, 0.5, 1, 10, 60, 30.0, 30.1666666666,
                  29.16111111, 27.15, 4.0, 0.0, 2.5, 0.1, 100000.0,
                  1234567.0, 1000000.0, 123456789.0, 0.000123456789, 1e-5,
                  0.25, 7.5, 1e6 - 1, 999999.5, 0.0001, 0.00009]
        self.compare([("g\t%r" % float(v), "%g" % float(v)) for v in values])

    def test_the_header_is_the_same_header(self):
        for spans in ((3, 30, 300), (3, 30, 300, 3000), (1, 10), (0.5, 60)):
            want = radbeeper.log_header(spans)
            got = self.rust(["header\t" + ",".join(repr(float(s))
                                                   for s in spans)])
            self.assertEqual(got[0], want, "spans %r" % (spans,))

    def test_a_timestamp_is_the_same_second_written_the_same_way(self):
        import time
        cases = []
        for t in (0, 1_000_000, 1_788_600_000, 1_791_600_000, 2_000_000_000):
            want = time.strftime("%Y-%m-%dT%H:%M:%S", time.localtime(t))
            cases.append(("stamp\t%r" % float(t), want))
        self.compare(cases)

    def test_a_row_is_the_same_row(self):
        import time
        rows = [
            (1_788_600_000.0, 0.333, 10, 30.0, [20.0, None, None],
             [60.0, None, None], radbeeper.SRC_LIVE, ""),
            (1_788_600_030.0, 0.8, 24, 30.1666666666, [48.0, 40.4, 38.8],
             [120.0, 60.0, 39.2], radbeeper.SRC_FLASH, "The bench"),
            (1_788_600_060.0, 0.0, 0, 27.15, [None, None, None],
             [None, None, None], radbeeper.SRC_LIVE, "Lab 2"),
            (1_788_600_090.0, 15.0, 450, 30.0, [900.0, 880.5, 0.0],
             [1800.0, 0.0, 0.0], radbeeper.SRC_LIVE, "beside the source"),
        ]
        cases = []
        for when, cps, counts, seconds, avg, peak, src, site in rows:
            want = radbeeper.log_row(when, cps, counts, seconds, avg, peak,
                                     src, site)
            spec = "\t".join([
                "row", repr(when), repr(cps), str(counts), repr(seconds),
                ",".join("none" if a is None else repr(a) for a in avg),
                ",".join("none" if p is None else repr(p) for p in peak),
                src, site,
            ])
            cases.append((spec, want))
        self.compare(cases)

    def test_a_row_lands_in_the_same_file(self):
        cases = []
        for when in (1_788_600_000.0, 1_791_600_000.0, 0.0):
            want = os.path.basename(
                radbeeper.log_path(when, "", "F48824B8207F7E"))
            cases.append(("path\t%r\tF48824B8207F7E" % when, want))
        self.compare(cases)

    def test_a_slot_is_the_same_slot(self):
        cases = []
        for when in (0.0, 29.9, 30.0, 1_788_600_001.0, -1.0):
            cases.append(("slot\t%r\t30.0" % when,
                          str(radbeeper.slot_of(when, 30.0))))
        self.compare(cases)


if __name__ == "__main__":
    unittest.main()
