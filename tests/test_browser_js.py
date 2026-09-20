# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Paul Richeson
"""The frame browser's javascript: a second decoder, pinned to the first.

WHY THIS SUITE EXISTS AT ALL. src/browser.js carries its own copy of the
frame decoder and its own copy of the spectrum, forty lines and a hundred
that already exist in src/frames.js and src/analysis.rs. That duplication is
deliberate -- see the head of src/browser.js for why the browser page cannot
share the file that is byte-locked to the Python's embedded copy -- and the
condition of keeping it is that nothing here is allowed to drift:

  1. The two javascript decoders read the same fixture to the same frames.
     A change to one that the other does not get is a page that shows
     different bytes than the audit page shows, from the same file.

  2. The browser's spectrum is the reference implementation's spectrum. The
     monitor draws this while the counter is running and this page draws it
     from the record afterwards; a page that disagreed would be describing
     seconds that nothing else describes that way. The Python is the
     reference the Rust was ported from, so it is what the javascript is
     measured against here.

Node is a convenience, not a dependency of this project: every test that
needs it is skipped and not failed when it is absent.
"""
import importlib.machinery
import importlib.util
import json
import math
import os
import random
import re
import subprocess
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BROWSER = os.path.join(ROOT, "src", "browser.js")
VIEWER = os.path.join(ROOT, "src", "frames.js")
FIXTURE = os.path.join(ROOT, "tests", "fixtures", "frames.bin")

SPEC = importlib.util.spec_from_loader(
    "radbeeper", importlib.machinery.SourceFileLoader(
        "radbeeper", os.path.join(ROOT, "radbeeper")))
radbeeper = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(radbeeper)


def node():
    """The node binary, or None -- a convenience here, not a dependency."""
    for name in ("node", "nodejs"):
        try:
            subprocess.run([name, "--version"], capture_output=True, timeout=20,
                           check=True)
            return name
        except (OSError, subprocess.SubprocessError):
            continue
    return None


class TestTheBrowserRunsFromAScriptTag(unittest.TestCase):
    """No bundler, no network, and nothing that can end its own element."""

    def setUp(self):
        with open(BROWSER, encoding="utf-8") as f:
            self.source = f.read()

    def test_there_is_no_module_syntax(self):
        lines = self.source.split("\n")
        for pattern in (r"^\s*import\s", r"^\s*export\s", r"\bimport\s*\(",
                        r"^\s*(?:var|let|const)?\s*\w*\s*=?\s*require\s*\("):
            found = [n + 1 for n, line in enumerate(lines)
                     if re.search(pattern, line) and "typeof module" not in line]
            # The one hook is the test export at the bottom, guarded by a
            # `typeof module` check.
            self.assertEqual([n for n in found if n < len(lines) - 12], [],
                             "module syntax at line(s) %s -- the browser has to "
                             "run from a <script> tag off a file:// URL" % found)

    def test_nothing_can_close_the_script_element(self):
        self.assertNotIn("</script", self.source)

    def test_it_reaches_for_no_network(self):
        """A page saved to a disk browses its own frames with the wire out."""
        self.assertNotIn("http://", self.source)
        self.assertNotIn("https://", self.source)

    def test_the_page_embeds_this_file_and_not_a_copy_of_it(self):
        """The Rust compiles it in; nothing may paste a second copy."""
        with open(os.path.join(ROOT, "src", "browser.rs"), encoding="utf-8") as f:
            rust = f.read()
        self.assertIn('include_str!("browser.js")', rust)


class TestTheTwoDecodersAgree(unittest.TestCase):
    """src/frames.js and src/browser.js, on the same bytes."""

    @classmethod
    def setUpClass(cls):
        cls.NODE = node()
        if cls.NODE is None:
            raise unittest.SkipTest("no node to run the javascript against")
        if not os.path.exists(FIXTURE):
            raise unittest.SkipTest("no frame fixture -- "
                                    "RB_WRITE_FIXTURE=1 cargo test --test frames_fixture")

    def run_js(self, body):
        out = subprocess.run([self.NODE, "-e", body], capture_output=True,
                             text=True, timeout=60)
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
        return out.stdout.strip()

    def test_the_same_fixture_decodes_to_the_same_frames(self):
        got = self.run_js(
            "var a = require(%s), b = require(%s), fs = require('fs');\n"
            "var buf = new Uint8Array(fs.readFileSync(%s));\n"
            "function norm(f) { return {seq: f.seq, started: f.started,\n"
            "  suspect: f.suspect, key: f.key, link: f.link, samples: f.samples}; }\n"
            "console.log(JSON.stringify({viewer: a.readFrames(buf).map(norm),\n"
            "  browser: b.readFrames(buf).map(norm)}));"
            % (json.dumps(VIEWER), json.dumps(BROWSER), json.dumps(FIXTURE)))
        both = json.loads(got)
        self.assertTrue(both["viewer"], "the fixture decoded to nothing")
        self.assertEqual(
            both["browser"], both["viewer"],
            "src/browser.js and src/frames.js read the same file differently")

    def test_a_damaged_frame_costs_one_frame_here_too(self):
        """The scan-forward recovery is part of the format, not of one reader."""
        got = self.run_js(
            "var b = require(%s), fs = require('fs');\n"
            "var buf = new Uint8Array(fs.readFileSync(%s));\n"
            "var whole = b.readFrames(buf).length;\n"
            "var hurt = buf.slice(0); hurt[8] ^= 0xff;\n"
            "console.log(JSON.stringify([whole, b.readFrames(hurt).length]));"
            % (json.dumps(BROWSER), json.dumps(FIXTURE)))
        whole, hurt = json.loads(got)
        self.assertGreater(whole, 1)
        self.assertLess(hurt, whole, "damaging a frame cost nothing")
        self.assertGreaterEqual(hurt, whole - 2,
                                "one damaged frame took more than its neighbour")


class TestTheSpectrumIsTheReferenceSpectrum(unittest.TestCase):
    """The browser's FFT against the Python the Rust was ported from."""

    @classmethod
    def setUpClass(cls):
        cls.NODE = node()
        if cls.NODE is None:
            raise unittest.SkipTest("no node to run the javascript against")

    def counts(self, n, seed, period=None):
        """Deterministic arrivals, optionally with something periodic in them."""
        rng = random.Random(seed)
        out = []
        for i in range(n):
            lam = 4.0
            if period:
                lam += 3.0 * (1 if (i % period) < period // 2 else 0)
            # A small Poisson draw, by inversion -- the exact generator does
            # not matter, only that both sides see the same integers.
            k, p, target = 0, math.exp(-lam), rng.random()
            acc = p
            while acc < target and k < 40:
                k += 1
                p *= lam / k
                acc += p
            out.append(k)
        return out

    def from_js(self, counts):
        out = subprocess.run(
            [self.NODE, "-e",
             "var b = require(%s);\n"
             "var s = b.spectrumOf(%s);\n"
             "console.log(JSON.stringify(s && {window: s.window, runs: s.runs,\n"
             "  loudest: s.loudest, chanceMax: s.chanceMax, period: s.period,\n"
             "  suspect: s.suspect, relative: s.relative}));"
             % (json.dumps(BROWSER), json.dumps(counts))],
            capture_output=True, text=True, timeout=120)
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
        return json.loads(out.stdout.strip())

    def from_python(self, counts):
        ladder = radbeeper.SpectrumLadder()
        for c in counts:
            ladder.add(c)
        spec = ladder.best()
        if not spec.runs:
            return None
        rel = spec.relative()
        top, where = spec.loudest()
        return {"window": spec.window, "runs": spec.runs,
                "loudest": [top, where], "chanceMax": spec.chance_max(),
                "period": spec.period(where),
                "suspect": bool(top and top >= spec.chance_max() * 1.25),
                "relative": rel}

    def same(self, counts, why):
        js, py = self.from_js(counts), self.from_python(counts)
        self.assertIsNotNone(py, why)
        self.assertIsNotNone(js, why)
        self.assertEqual(js["window"], py["window"], why)
        self.assertEqual(js["runs"], py["runs"], why)
        self.assertEqual(js["loudest"][1], py["loudest"][1],
                         "%s: a different bin is the loudest" % why)
        self.assertEqual(js["suspect"], py["suspect"],
                         "%s: the two disagree about whether this is flat" % why)
        for name in ("chanceMax", "period"):
            self.assertAlmostEqual(js[name], py[name], places=9, msg=why)
        self.assertEqual(len(js["relative"]), len(py["relative"]), why)
        for i, (a, b) in enumerate(zip(js["relative"], py["relative"])):
            self.assertAlmostEqual(a, b, places=8,
                                   msg="%s: bin %d differs" % (why, i))

    def test_a_quiet_counter_is_flat_in_both(self):
        self.same(self.counts(600, seed=11), "600 seconds of ordinary decay")

    def test_a_period_in_the_arrivals_is_found_by_both(self):
        """The case the flag exists for: both must see the same line."""
        counts = self.counts(600, seed=12, period=64)
        self.same(counts, "600 seconds with a 64-second square wave in them")
        self.assertTrue(self.from_python(counts)["suspect"],
                        "the fixture for this test is not actually periodic")

    def test_too_few_seconds_says_so_rather_than_guessing(self):
        self.assertIsNone(self.from_js(self.counts(40, seed=13)))


class TestTheReadoutSaysWhatTheRecordHolds(unittest.TestCase):
    """The per-second readout, which is the same one the terminal prints."""

    @classmethod
    def setUpClass(cls):
        cls.NODE = node()
        if cls.NODE is None:
            raise unittest.SkipTest("no node to run the javascript against")

    def digits(self, samples, per_line=None):
        arg = "" if per_line is None else ", %d" % per_line
        out = subprocess.run(
            [self.NODE, "-e",
             "var b = require(%s);\n"
             "console.log(JSON.stringify(b.digits({samples: %s}%s)));"
             % (json.dumps(BROWSER), json.dumps(samples), arg)],
            capture_output=True, text=True, timeout=60)
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
        return json.loads(out.stdout.strip())

    def test_an_ordinary_second_is_one_character(self):
        got = self.digits([[0, 0], [1, 1], [1, 9], [1, 10], [1, 35]])
        self.assertEqual(got, ["019az"])

    def test_a_gap_is_drawn_as_a_gap_and_not_as_a_zero(self):
        """A second the counter was away is not a second it counted nothing."""
        got = self.digits([[0, 1], [4, 2]])
        self.assertEqual(got, ["1...2"])

    def test_a_busy_counter_gets_its_numbers_written_out(self):
        """Above 35 a character grid says the same thing about every second.

        This is the whole reason the readout has a second form: a counter on
        a real source put 150 in every second of the frames this was written
        against, and every one of them rendered as `+`.
        """
        got = self.digits([[0, 40], [1, 150], [1, 7]], per_line=60)
        self.assertEqual(len(got), 1)
        self.assertEqual(got[0].split(), ["40", "150", "7"])
