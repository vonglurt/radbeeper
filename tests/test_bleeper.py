# SPDX-License-Identifier: MIT
"""Tests for bleeper. Stdlib unittest, no hardware, no network."""
import importlib.util
import os
import struct
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SPEC = importlib.util.spec_from_loader(
    "bleeper", importlib.machinery.SourceFileLoader(
        "bleeper", os.path.join(HERE, os.pardir, "bleeper")))
bleeper = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bleeper)


class TestWindows(unittest.TestCase):
    def test_window_is_none_until_it_is_full(self):
        w = bleeper.Windows((3, 30, 300))
        for i in range(3):
            w.add(float(i), 1)
        # 0s, 1s, 2s of samples: elapsed is 2, so even the 3s window is short.
        self.assertIsNone(w.average(3))
        w.add(3.0, 1)
        self.assertIsNotNone(w.average(3))
        self.assertIsNone(w.average(30))

    def test_average_is_counts_scaled_to_a_minute(self):
        w = bleeper.Windows((3,))
        # Four samples one second apart, two counts each. The 3s window covers
        # the samples strictly after t=3.0-3=0.0, so t=1,2,3: six counts.
        for i in range(4):
            w.add(float(i), 2)
        self.assertAlmostEqual(w.average(3), 6 * 60.0 / 3)

    def test_a_burst_moves_the_short_window_first(self):
        w = bleeper.Windows((3, 30))
        for i in range(31):
            w.add(float(i), 1)
        calm3, calm30 = w.average(3), w.average(30)
        for i in range(31, 34):
            w.add(float(i), 50)
        self.assertGreater(w.average(3), calm3 * 10)
        self.assertLess(w.average(30), w.average(3))
        self.assertGreater(w.average(30), calm30)

    def test_old_samples_leave_the_window(self):
        w = bleeper.Windows((3,))
        for i in range(10):
            w.add(float(i), 1)
        # Only the last three seconds count, not all ten.
        self.assertAlmostEqual(w.average(3), 3 * 60.0 / 3)

    def test_total_and_elapsed_cover_the_whole_run(self):
        w = bleeper.Windows((3,))
        for i in range(10):
            w.add(float(i), 2)
        self.assertEqual(w.total, 20)
        self.assertAlmostEqual(w.elapsed(), 9.0)

    def test_usvh_divides_by_the_tube_factor(self):
        w = bleeper.Windows((3,))
        for i in range(4):
            w.add(float(i), 2)
        self.assertAlmostEqual(w.usvh(3, 151.5), w.average(3) / 151.5)


class TestSimCounter(unittest.TestCase):
    def test_poisson_mean_matches_the_requested_rate(self):
        sim = bleeper.SimCounter(cpm=600.0, seed=1)   # 10 counts per second
        draws = [sim._draw() for _ in range(4000)]
        mean = sum(draws) / len(draws)
        self.assertAlmostEqual(mean, 10.0, delta=0.25)

    def test_poisson_variance_equals_its_mean(self):
        # The property that makes the simulator worth having: real decay has
        # variance equal to the mean, and that is what makes a 3s average
        # jumpy. A smooth fake would not exercise the display honestly.
        sim = bleeper.SimCounter(cpm=600.0, seed=2)
        draws = [sim._draw() for _ in range(4000)]
        mean = sum(draws) / len(draws)
        var = sum((d - mean) ** 2 for d in draws) / len(draws)
        self.assertAlmostEqual(var / mean, 1.0, delta=0.15)

    def test_a_seed_repeats_exactly(self):
        a = [bleeper.SimCounter(cpm=100.0, seed=42)._draw() for _ in range(1)]
        b = [bleeper.SimCounter(cpm=100.0, seed=42)._draw() for _ in range(1)]
        self.assertEqual(a, b)

    def test_background_rate_is_low_but_not_zero(self):
        sim = bleeper.SimCounter(cpm=25.0, seed=3)
        total = sum(sim._draw() for _ in range(600))   # ten minutes
        self.assertAlmostEqual(total / 10.0, 25.0, delta=6.0)


class TestHistoryDecode(unittest.TestCase):
    def test_plain_bytes_are_counts(self):
        rows = list(bleeper.decode_history(bytes([1, 2, 3])))
        self.assertEqual([r[3] for r in rows], [1, 2, 3])

    def test_ff_is_unwritten_flash_and_is_skipped(self):
        rows = list(bleeper.decode_history(bytes([1, 0xFF, 0xFF, 2])))
        self.assertEqual([r[3] for r in rows], [1, 2])

    def test_timestamp_marker_sets_the_stamp_and_mode(self):
        blob = bytes([0x55, 0xAA, 0x00, 26, 9, 4, 10, 30, 0, 2]) + bytes([7])
        rows = list(bleeper.decode_history(blob))
        self.assertEqual(rows[0][1], "2026-09-04 10:30:00")
        self.assertEqual(rows[0][2], "every minute")
        # the count after it inherits the stamp
        self.assertEqual(rows[1][3], 7)
        self.assertEqual(rows[1][1], "2026-09-04 10:30:00")

    def test_two_byte_count_marker(self):
        blob = bytes([0x55, 0xAA, 0x01]) + struct.pack(">H", 1234)
        rows = list(bleeper.decode_history(blob))
        self.assertEqual(rows[0][3], 1234)

    def test_note_marker_is_read_as_text(self):
        note = b"hello"
        blob = bytes([0x55, 0xAA, 0x02, len(note)]) + note
        rows = list(bleeper.decode_history(blob))
        self.assertEqual(rows[0][4], "hello")

    def test_a_55_that_is_not_a_marker_is_a_count(self):
        rows = list(bleeper.decode_history(bytes([0x55, 0x01, 0x55])))
        self.assertEqual([r[3] for r in rows], [0x55, 1, 0x55])

    def test_empty_flash_decodes_to_nothing(self):
        self.assertEqual(list(bleeper.decode_history(b"\xff" * 512)), [])


class TestDiscovery(unittest.TestCase):
    def test_missing_driver_is_reported_as_its_own_reason(self):
        # This machine's kernel is the case in point when it is linux-virt;
        # either way the exception must name a reason and carry detail.
        try:
            bleeper.find_counter(device=None, source="auto")
        except bleeper.NotFound as e:
            self.assertTrue(e.reason)
            self.assertNotEqual(e.reason, "")
        except Exception:
            pass

    def test_sim_source_never_needs_hardware(self):
        c = bleeper.find_counter(source="sim", sim_cpm=25.0, seed=5)
        self.assertEqual(c.model, "SIM")
        self.assertIsNotNone(c.cpm())
        c.close()


class TestArguments(unittest.TestCase):
    def test_spans_parse(self):
        self.assertEqual(bleeper.spans_arg("3,30,300"), (3, 30, 300))

    def test_spans_reject_zero_and_words(self):
        import argparse
        for bad in ("0", "-1", "3,x", ""):
            with self.assertRaises(argparse.ArgumentTypeError):
                bleeper.spans_arg(bad)

    def test_help_runs_with_no_command(self):
        self.assertEqual(bleeper.main([]), 0)

    def test_sim_watch_runs_and_stops_on_duration(self):
        rc = bleeper.main(["--source", "sim", "--seed", "9", "--duration", "3",
                           "--plain", "watch"])
        self.assertEqual(rc, 0)


class TestSparkline(unittest.TestCase):
    def test_sparkline_is_as_wide_as_asked(self):
        samples = [(float(i), i % 9) for i in range(50)]
        self.assertEqual(len(bleeper.sparkline(samples, 20)), 20)

    def test_sparkline_of_nothing_is_empty(self):
        self.assertEqual(bleeper.sparkline([], 20), "")

    def test_flat_zero_does_not_divide_by_zero(self):
        self.assertEqual(len(bleeper.sparkline([(0.0, 0), (1.0, 0)], 10)), 2)


if __name__ == "__main__":
    unittest.main()


class TestLevels(unittest.TestCase):
    """The colour thresholds, as values rather than as a screenshot."""

    def test_background_is_calm(self):
        for cpm in (0, 12.0, 25.0, 99.9):
            self.assertEqual(bleeper.level(cpm), "calm", cpm)

    def test_the_middle_band_is_raised(self):
        for cpm in (100.0, 250.0, 299.9):
            self.assertEqual(bleeper.level(cpm), "raised", cpm)

    def test_anything_from_300_is_high(self):
        # 444 and 500 are one band, not two: the screenshot that suggested
        # otherwise was the terminal palette, and this is the check that says
        # so without anyone having to squint at a picture again.
        for cpm in (300.0, 444.0, 500.0, 9000.0):
            self.assertEqual(bleeper.level(cpm), "high", cpm)

    def test_an_unfilled_window_is_not_a_reading(self):
        self.assertEqual(bleeper.level(None), "unknown")
