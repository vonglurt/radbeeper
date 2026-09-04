# SPDX-License-Identifier: MIT
"""The serial path, against a fake GMC-320 on a pty.

These are the tests that cover the code --source sim skips entirely: termios
setup, exact-length reads on a real file descriptor, command framing, the
masked heartbeat stream and the chunked SPIR download.
"""
import importlib.machinery
import importlib.util
import os
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
SPEC = importlib.util.spec_from_loader(
    "radbeeper", importlib.machinery.SourceFileLoader(
        "radbeeper", os.path.join(HERE, os.pardir, "radbeeper")))
radbeeper = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(radbeeper)

from fake_gmc import FakeGMC, build_history  # noqa: E402


class SerialCase(unittest.TestCase):
    tick = 0.05
    cpm = 600.0

    def setUp(self):
        self.dev = FakeGMC(cpm=self.cpm, seed=11, tick=self.tick)
        self.dev.start()

    def tearDown(self):
        self.dev.stop()


class TestIdentify(SerialCase):
    def test_a_gmc_answers_and_is_identified(self):
        c = radbeeper.identify(self.dev.path, baud=115200)
        self.assertIsNotNone(c)
        self.assertEqual(c.version, "GMC-320Re 4.26")
        self.assertEqual(c.model, "GMC-320")
        self.assertEqual(c.serial_no, "123456789ABCDE")
        self.assertEqual(c.flash_size, 0x100000)
        c.close()

    def test_find_counter_uses_the_named_device(self):
        c = radbeeper.find_counter(device=self.dev.path)
        self.assertEqual(c.model, "GMC-320")
        c.close()

    def test_a_silent_port_is_not_a_counter(self):
        quiet = FakeGMC(tick=99)
        quiet.running = False          # never started, so it answers nothing
        try:
            self.assertIsNone(radbeeper.identify(quiet.path, baud=115200))
        finally:
            quiet.stop()

    def test_a_missing_device_raises_not_found(self):
        with self.assertRaises(radbeeper.NotFound):
            radbeeper.find_counter(device="/dev/definitely-not-here")


class TestQueries(SerialCase):
    def test_cpm_volt_and_clock(self):
        c = radbeeper.identify(self.dev.path, baud=115200)
        self.assertEqual(c.cpm(), 600)
        self.assertAlmostEqual(c.voltage(), 3.9, places=2)
        self.assertRegex(c.datetime(), r"^20\d\d-\d\d-\d\d \d\d:\d\d:\d\d$")
        c.close()

    def test_the_status_bits_are_masked_off_the_count(self):
        # The fake sets 0x8000 on every count, exactly as current firmware
        # does. Unmasked, a 10-count second reads as 32778 and every average
        # is nonsense.
        c = radbeeper.identify(self.dev.path, baud=115200)
        for _ in range(5):
            v = c.cps()
            self.assertIsNotNone(v)
            self.assertLess(v, 0x4000)
        c.close()

    def test_commands_are_framed_as_the_device_expects(self):
        c = radbeeper.identify(self.dev.path, baud=115200)
        c.cpm()
        c.close()
        self.assertIn(b"<GETVER>>", self.dev.commands)
        self.assertIn(b"<GETCPM>>", self.dev.commands)


class TestHeartbeat(SerialCase):
    def test_the_stream_yields_masked_counts_and_stops_cleanly(self):
        c = radbeeper.identify(self.dev.path, baud=115200)
        got = []
        for when, counts in c.samples():
            got.append(counts)
            self.assertLess(counts, 0x4000)
            if len(got) >= 5:
                break
        c.close()
        self.assertEqual(len(got), 5)
        self.assertIn(b"<HEARTBEAT1>>", self.dev.commands)
        self.assertIn(b"<HEARTBEAT0>>", self.dev.commands)

    def test_the_stream_ends_rather_than_blocking_when_the_device_goes_away(self):
        # Unplugging mid-read must end the generator, not wedge the monitor.
        c = radbeeper.identify(self.dev.path, baud=115200)
        it = c.samples()
        next(it)
        self.dev.heartbeat = False
        self.dev.running = False
        with self.assertRaises(StopIteration):
            next(it)
        c.close()

    def test_samples_feed_the_windows(self):
        c = radbeeper.identify(self.dev.path, baud=115200)
        w = radbeeper.Windows((3, 30, 300))
        n = 0
        for when, counts in c.samples():
            w.add(when, counts)
            n += 1
            if n >= 8:
                break
        c.close()
        self.assertEqual(len(w.samples), 8)
        self.assertGreater(w.total, 0)


class TestHistoryOverSerial(SerialCase):
    def test_spir_downloads_the_whole_image_in_chunks(self):
        blob = build_history(seconds=200, cpm=300.0, seed=7, size=4096)
        self.dev.history = blob
        c = radbeeper.identify(self.dev.path, baud=115200)
        got = c.read_history(0, len(blob))
        c.close()
        self.assertEqual(got, blob)
        # more than one SPIR, or the chunking is not being exercised
        spirs = [x for x in self.dev.commands if x.startswith(b"<SPIR")]
        self.assertGreater(len(spirs), 1)

    def test_the_downloaded_image_decodes_to_the_samples_put_in(self):
        blob = build_history(seconds=50, cpm=400.0, seed=9, size=2048)
        self.dev.history = blob
        c = radbeeper.identify(self.dev.path, baud=115200)
        got = c.read_history(0, len(blob))
        c.close()
        rows = list(radbeeper.decode_history(got))
        counts = [r[3] for r in rows if r[3] is not None]
        self.assertEqual(len(counts), 50)
        stamps = [r[1] for r in rows if r[1]]
        self.assertEqual(stamps[0], "2026-09-04 11:00:00")
        self.assertEqual([r[2] for r in rows if r[2]][0], "every minute")
        self.assertIn("note!", [r[4] for r in rows])


class TestCommandsEndToEnd(SerialCase):
    def test_probe_reports_the_device(self):
        rc = radbeeper.main(["-d", self.dev.path, "-b", "115200", "probe"])
        self.assertEqual(rc, 0)

    def test_cpm_command(self):
        self.assertEqual(radbeeper.main(["-d", self.dev.path, "cpm"]), 0)

    def test_watch_runs_against_the_wire(self):
        rc = radbeeper.main(["-d", self.dev.path, "--duration", "0.3",
                             "--plain", "watch"])
        self.assertEqual(rc, 0)

    def test_log_pull_writes_a_raw_image_and_a_csv(self):
        self.dev.history = build_history(seconds=30, cpm=200.0, size=1024)
        with tempfile.TemporaryDirectory() as d:
            stem = os.path.join(d, "hist")
            rc = radbeeper.main(["-d", self.dev.path, "log", "pull",
                                 "-o", stem, "--bytes", "1024"])
            self.assertEqual(rc, 0)
            self.assertEqual(os.path.getsize(stem + ".bin"), 1024)
            with open(stem + ".csv") as f:
                lines = f.read().strip().splitlines()
            self.assertEqual(lines[0], "offset,timestamp,save_mode,count,note")
            self.assertGreater(len(lines), 30)


if __name__ == "__main__":
    unittest.main()
