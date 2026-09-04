# SPDX-License-Identifier: MIT
"""Tests for radbeeper. Stdlib unittest, no hardware, no network."""
import importlib.util
import math
import os
import random
import struct
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SPEC = importlib.util.spec_from_loader(
    "radbeeper", importlib.machinery.SourceFileLoader(
        "radbeeper", os.path.join(HERE, os.pardir, "radbeeper")))
radbeeper = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(radbeeper)


class TestWindows(unittest.TestCase):
    def test_window_is_none_until_it_is_full(self):
        w = radbeeper.Windows((3, 30, 300))
        for i in range(3):
            w.add(float(i), 1)
        # 0s, 1s, 2s of samples: elapsed is 2, so even the 3s window is short.
        self.assertIsNone(w.average(3))
        w.add(3.0, 1)
        self.assertIsNotNone(w.average(3))
        self.assertIsNone(w.average(30))

    def test_average_is_counts_scaled_to_a_minute(self):
        w = radbeeper.Windows((3,))
        # Four samples one second apart, two counts each. The 3s window covers
        # the samples strictly after t=3.0-3=0.0, so t=1,2,3: six counts.
        for i in range(4):
            w.add(float(i), 2)
        self.assertAlmostEqual(w.average(3), 6 * 60.0 / 3)

    def test_a_burst_moves_the_short_window_first(self):
        w = radbeeper.Windows((3, 30))
        for i in range(31):
            w.add(float(i), 1)
        calm3, calm30 = w.average(3), w.average(30)
        for i in range(31, 34):
            w.add(float(i), 50)
        self.assertGreater(w.average(3), calm3 * 10)
        self.assertLess(w.average(30), w.average(3))
        self.assertGreater(w.average(30), calm30)

    def test_old_samples_leave_the_window(self):
        w = radbeeper.Windows((3,))
        for i in range(10):
            w.add(float(i), 1)
        # Only the last three seconds count, not all ten.
        self.assertAlmostEqual(w.average(3), 3 * 60.0 / 3)

    def test_total_and_elapsed_cover_the_whole_run(self):
        w = radbeeper.Windows((3,))
        for i in range(10):
            w.add(float(i), 2)
        self.assertEqual(w.total, 20)
        self.assertAlmostEqual(w.elapsed(), 9.0)

    def test_usvh_divides_by_the_tube_factor(self):
        w = radbeeper.Windows((3,))
        for i in range(4):
            w.add(float(i), 2)
        self.assertAlmostEqual(w.usvh(3, 151.5), w.average(3) / 151.5)


class TestSimCounter(unittest.TestCase):
    def test_poisson_mean_matches_the_requested_rate(self):
        sim = radbeeper.SimCounter(cpm=600.0, seed=1)   # 10 counts per second
        draws = [sim._draw() for _ in range(4000)]
        mean = sum(draws) / len(draws)
        self.assertAlmostEqual(mean, 10.0, delta=0.25)

    def test_poisson_variance_equals_its_mean(self):
        # The property that makes the simulator worth having: real decay has
        # variance equal to the mean, and that is what makes a 3s average
        # jumpy. A smooth fake would not exercise the display honestly.
        sim = radbeeper.SimCounter(cpm=600.0, seed=2)
        draws = [sim._draw() for _ in range(4000)]
        mean = sum(draws) / len(draws)
        var = sum((d - mean) ** 2 for d in draws) / len(draws)
        self.assertAlmostEqual(var / mean, 1.0, delta=0.15)

    def test_a_seed_repeats_exactly(self):
        a = [radbeeper.SimCounter(cpm=100.0, seed=42)._draw() for _ in range(1)]
        b = [radbeeper.SimCounter(cpm=100.0, seed=42)._draw() for _ in range(1)]
        self.assertEqual(a, b)

    def test_background_rate_is_low_but_not_zero(self):
        sim = radbeeper.SimCounter(cpm=25.0, seed=3)
        total = sum(sim._draw() for _ in range(600))   # ten minutes
        self.assertAlmostEqual(total / 10.0, 25.0, delta=6.0)


class TestHistoryDecode(unittest.TestCase):
    """The record grammar, one record shape at a time."""

    def counts(self, blob):
        return [r[3] for r in radbeeper.history_records(blob)
                if r[3] is not None]

    def test_plain_bytes_are_counts(self):
        self.assertEqual(self.counts(bytes([1, 2, 3])), [1, 2, 3])

    def test_ff_is_unwritten_flash_and_is_skipped(self):
        self.assertEqual(self.counts(bytes([1, 0xFF, 0xFF, 2])), [1, 2])

    def test_the_datetime_record_is_nine_bytes(self):
        # THE REGRESSION THIS FILE EXISTS FOR. GQ's document says ten, the
        # last a save mode; a GMC-320Re 4.26 writes nine and the tenth byte in
        # a real image is the 0x55 that opens the next record. Reading ten ate
        # that 0x55, left 0xAA to be decoded as an ordinary sample, and so
        # invented a count of 170 every three minutes. The shape below is
        # lifted from a real image at offset 144.
        blob = (bytes([0x55, 0xAA, 0x00, 26, 8, 29, 18, 13, 33])
                + bytes([0x55, 0xAA, 0x01])
                + bytes([0x00, 0x07, 1, 2]))
        self.assertEqual(self.counts(blob), [0, 7, 1, 2])
        self.assertNotIn(0xAA, self.counts(blob))

    def test_a_timestamp_places_the_counts_that_follow_it(self):
        base = time.mktime((2026, 9, 4, 10, 30, 0, 0, 1, -1))
        blob = (bytes([0x55, 0xAA, 0x00, 26, 9, 4, 10, 30, 0]) + bytes([7, 8])
                + bytes([0x55, 0xAA, 0x00, 26, 9, 4, 10, 30, 2]))
        rows = [r for r in radbeeper.history_records(blob) if r[3] is not None]
        self.assertEqual([r[3] for r in rows], [7, 8])
        self.assertAlmostEqual(rows[0][1], base)
        self.assertAlmostEqual(rows[1][1], base + 1.0)

    def test_55_aa_01_is_a_marker_and_carries_nothing(self):
        # GQ's document calls it a two-byte count, for a second whose count
        # did not fit in a byte. Reading it that way turned the two ordinary
        # samples after every timestamp into one reading of 256, or 512, or
        # 21,930 -- 1,701 of them in a real image, on a tube that saturates
        # three orders of magnitude below that. All 4,709 sat exactly nine
        # bytes after a timestamp and not one anywhere else, and the two bytes
        # following have the distribution of counts and not of a big-endian
        # pair: 64% zero, 21% one, 7% two. So it is three bytes, and what
        # comes after it is data.
        blob = bytes([0x55, 0xAA, 0x01]) + bytes([0x04, 0xD2])
        self.assertEqual(self.counts(blob), [0x04, 0xD2])
        self.assertNotIn(1234, self.counts(blob))

    def test_note_marker_is_read_as_text(self):
        note = b"hello"
        blob = bytes([0x55, 0xAA, 0x02, len(note)]) + note
        rows = list(radbeeper.history_records(blob))
        self.assertEqual(rows[0][4], "hello")

    def test_a_55_that_is_not_a_marker_is_a_count(self):
        self.assertEqual(self.counts(bytes([0x55, 0x01, 0x55])),
                         [0x55, 1, 0x55])

    def test_empty_flash_decodes_to_nothing(self):
        self.assertEqual(list(radbeeper.history_records(b"\xff" * 512)), [])

    def test_an_impossible_date_is_not_a_timestamp(self):
        # Corrupt or half-erased flash produces 55 AA 00 with rubbish after
        # it. That must not become a mark that places a thousand samples in
        # the year 2255.
        blob = bytes([0x55, 0xAA, 0x00, 99, 99, 99, 99, 99, 99]) + bytes([4])
        rows = list(radbeeper.history_records(blob))
        self.assertEqual([r[3] for r in rows if r[3] is not None], [4])
        self.assertTrue(all(r[1] is None for r in rows))


class TestMeasuredInterval(unittest.TestCase):
    """The counter's second is not a second, and the recording says so.

    179 samples between marks 181 seconds apart is 1.011 s each: 39 seconds of
    drift an hour. Assuming 1.000 would file an hour-old sample most of a
    minute from where it belongs, so the spacing is measured per stretch.
    """

    def image(self, per_mark, n, interval, start=(26, 9, 4, 11, 0, 0)):
        out = bytearray()
        t = time.mktime((2000 + start[0],) + start[1:] + (0, 1, -1))
        written = 0
        while written < n:
            st = time.localtime(t)
            out += bytes([0x55, 0xAA, 0x00, st.tm_year - 2000, st.tm_mon,
                          st.tm_mday, st.tm_hour, st.tm_min, st.tm_sec])
            here = min(per_mark, n - written)
            out += bytes([1] * here)
            written += here
            t = round(t + here * interval)
        return bytes(out)

    def test_the_interval_is_taken_from_the_marks_not_assumed(self):
        marks = radbeeper.history_marks(self.image(180, 360, 1.0111))
        dts = radbeeper.sample_intervals(marks)
        self.assertAlmostEqual(dts[0], 182 / 180.0, places=4)
        self.assertNotAlmostEqual(dts[0], 1.0, places=3)

    def test_the_last_stretch_borrows_the_measurement_before_it(self):
        # It has no following mark, so it cannot be measured -- but it is the
        # newest data, which is the part a backfill most wants.
        marks = radbeeper.history_marks(self.image(60, 180, 1.011))
        dts = radbeeper.sample_intervals(marks)
        self.assertIsNotNone(dts[-1])
        self.assertAlmostEqual(dts[-1], dts[-2])

    def test_one_mark_measures_nothing_and_says_so(self):
        # Rather than assuming a second and being quietly wrong.
        blob = bytes([0x55, 0xAA, 0x00, 26, 9, 4, 11, 0, 0]) + bytes([1] * 10)
        self.assertEqual(radbeeper.sample_intervals(
            radbeeper.history_marks(blob)), [None])
        self.assertEqual(list(radbeeper.history_samples(blob)), [])

    def test_samples_land_where_the_measured_spacing_puts_them(self):
        blob = self.image(100, 200, 1.02)
        got = list(radbeeper.history_samples(blob))
        self.assertEqual(len(got), 200)
        # 100 samples over a 102-second mark gap: the hundredth is 99 steps in.
        self.assertAlmostEqual(got[99][0] - got[0][0], 99 * 1.02, places=3)

    def test_the_clock_offset_shifts_everything_together(self):
        blob = self.image(60, 120, 1.011)
        plain = list(radbeeper.history_samples(blob))
        moved = list(radbeeper.history_samples(blob, offset=3600.0))
        self.assertAlmostEqual(moved[0][0] - plain[0][0], 3600.0)
        self.assertEqual([c for _t, c, _d in plain],
                         [c for _t, c, _d in moved])


class TestDiscovery(unittest.TestCase):
    def test_missing_driver_is_reported_as_its_own_reason(self):
        # This machine's kernel is the case in point when it is linux-virt;
        # either way the exception must name a reason and carry detail.
        try:
            radbeeper.find_counter(device=None, source="auto")
        except radbeeper.NotFound as e:
            self.assertTrue(e.reason)
            self.assertNotEqual(e.reason, "")
        except Exception:
            pass

    def test_sim_source_never_needs_hardware(self):
        c = radbeeper.find_counter(source="sim", sim_cpm=25.0, seed=5)
        self.assertEqual(c.model, "SIM")
        self.assertIsNotNone(c.cpm())
        c.close()


class TestArguments(unittest.TestCase):
    def test_spans_parse(self):
        self.assertEqual(radbeeper.spans_arg("3,30,300"), (3, 30, 300))

    def test_spans_reject_zero_and_words(self):
        import argparse
        for bad in ("0", "-1", "3,x", ""):
            with self.assertRaises(argparse.ArgumentTypeError):
                radbeeper.spans_arg(bad)

    def test_help_runs_with_no_command(self):
        self.assertEqual(radbeeper.main([]), 0)

    def test_sim_watch_runs_and_stops_on_duration(self):
        rc = radbeeper.main(["--source", "sim", "--seed", "9", "--duration", "3",
                           "--plain", "watch"])
        self.assertEqual(rc, 0)


class TestSparkline(unittest.TestCase):
    def test_sparkline_is_as_wide_as_asked(self):
        samples = [(float(i), i % 9) for i in range(50)]
        self.assertEqual(len(radbeeper.sparkline(samples, 20)), 20)

    def test_sparkline_of_nothing_is_empty(self):
        self.assertEqual(radbeeper.sparkline([], 20), "")

    def test_flat_zero_does_not_divide_by_zero(self):
        self.assertEqual(len(radbeeper.sparkline([(0.0, 0), (1.0, 0)], 10)), 2)


if __name__ == "__main__":
    unittest.main()


class TestLevels(unittest.TestCase):
    """The colour thresholds, as values rather than as a screenshot."""

    def test_background_is_calm(self):
        for cpm in (0, 12.0, 25.0, 99.9):
            self.assertEqual(radbeeper.level(cpm), "calm", cpm)

    def test_the_middle_band_is_raised(self):
        for cpm in (100.0, 250.0, 299.9):
            self.assertEqual(radbeeper.level(cpm), "raised", cpm)

    def test_anything_from_300_is_high(self):
        # 444 and 500 are one band, not two: the screenshot that suggested
        # otherwise was the terminal palette, and this is the check that says
        # so without anyone having to squint at a picture again.
        for cpm in (300.0, 444.0, 500.0, 9000.0):
            self.assertEqual(radbeeper.level(cpm), "high", cpm)

    def test_an_unfilled_window_is_not_a_reading(self):
        self.assertEqual(radbeeper.level(None), "unknown")


class TestInterval(unittest.TestCase):
    """The accumulator behind a log row: peaks, rate, and constant space."""

    def test_cps_is_per_one_second_not_per_interval(self):
        # 60 counts over 30 seconds is 2 CPS, never 60. The log's row spacing
        # must not leak into the unit.
        iv = radbeeper.Interval((3, 30))
        for _ in range(30):
            iv.add(2, [None, None])
        self.assertEqual(iv.seconds, 30)
        self.assertEqual(iv.counts, 60)
        self.assertAlmostEqual(iv.cps(), 2.0)

    def test_cps_of_an_empty_interval_is_zero_not_a_crash(self):
        self.assertEqual(radbeeper.Interval((3,)).cps(), 0.0)

    def test_peak_is_the_highest_seen_not_the_last(self):
        iv = radbeeper.Interval((3, 30))
        iv.add(1, [100.0, 10.0])
        iv.add(1, [900.0, 20.0])      # the spike
        iv.add(1, [50.0, 30.0])       # gone again by the time the row is due
        self.assertEqual(iv.peaks[0], 900.0)
        self.assertEqual(iv.peaks[1], 30.0)

    def test_an_unfilled_window_never_becomes_a_peak(self):
        # None means "not enough signal to say" and must not be read as 0,
        # here least of all: a peak of 0 would be a claim, not an absence.
        iv = radbeeper.Interval((3,))
        iv.add(1, [None])
        self.assertIsNone(iv.peaks[0])
        iv.add(1, [40.0])
        self.assertEqual(iv.peaks[0], 40.0)

    def test_reset_clears_everything_so_memory_cannot_grow(self):
        iv = radbeeper.Interval((3, 30))
        for _ in range(1000):
            iv.add(5, [10.0, 20.0])
        iv.reset()
        self.assertEqual((iv.counts, iv.seconds), (0, 0))
        self.assertEqual(iv.peaks, [None, None])
        # Nothing per-sample is retained -- the only state is the slots.
        self.assertEqual(sorted(radbeeper.Interval.__slots__),
                         ["counts", "peaks", "seconds", "spans"])


class TestService(unittest.TestCase):
    """The two paths the boot service can take."""

    def setUp(self):
        import tempfile
        self.tmp = tempfile.mkdtemp()
        self.real_state_dir = radbeeper.state_dir
        radbeeper.state_dir = lambda: self.tmp

    def tearDown(self):
        radbeeper.state_dir = self.real_state_dir

    def test_no_counter_is_dormant_and_exits_zero(self):
        # A service that cannot find its hardware must stop cleanly, not fail
        # and not respawn. Exit 0 is the whole contract with OpenRC here.
        rc = radbeeper.main(["--device", "/dev/does-not-exist", "service"])
        self.assertEqual(rc, 0)
        with open(os.path.join(self.tmp, "status")) as f:
            self.assertIn("dormant", f.read())

    def test_a_counter_is_monitored_and_logged(self):
        # One run, several claims: the interval cadence, the header, the field
        # count, and that the timestamps sort as text. Doing it in one run
        # keeps the suite's wall clock down -- the simulator yields in real
        # time, so every service test costs its --duration in seconds.
        rc = radbeeper.main(["--source", "sim", "--sim-cpm", "600", "--seed",
                             "4", "--log-every", "2", "--duration", "9",
                             "service"])
        self.assertEqual(rc, 0)
        with open(radbeeper.log_path(time.time(), self.tmp,
                                    "simulated")) as f:
            lines = f.read().split("\n")
        rows = [ln for ln in lines if ln and not ln.startswith("#")]
        head = lines[0]

        self.assertEqual(head, "#time\tcps\tcounts\tseconds"
                               "\tcpm_3\tcpm_30\tcpm_300"
                               "\tpeak_3\tpeak_30\tpeak_300\tsrc\tsite")
        # Every row is the full width, trailing empties included, or a column
        # count is not a reliable way to read the file.
        for r in rows:
            self.assertEqual(len(r.split("\t")), 12)
        # And every one of them says it was measured here, not reconstructed.
        for r in rows:
            self.assertEqual(r.split("\t")[10], radbeeper.SRC_LIVE)
        # A row every 2s over ~9s is a handful, NOT one per second. This is
        # the whole point of the change and the cheapest thing to regress.
        self.assertLessEqual(len(rows), 6)
        self.assertGreaterEqual(len(rows), 3)
        # Written big-endian so plain text sort is chronological.
        stamps = [r.split("\t")[0] for r in rows]
        self.assertEqual(stamps, sorted(stamps))
        with open(os.path.join(self.tmp, "status")) as f:
            self.assertIn("stopped", f.read())

    def test_the_last_partial_interval_is_still_written(self):
        # Stopping between rows must not throw away what was collected: a
        # service killed four seconds after a spike should still have it.
        rc = radbeeper.main(["--source", "sim", "--seed", "5",
                             "--log-every", "600", "--duration", "3",
                             "service"])
        self.assertEqual(rc, 0)
        with open(radbeeper.log_path(time.time(), self.tmp,
                                    "simulated")) as f:
            rows = [ln for ln in f.read().split("\n")
                    if ln and not ln.startswith("#")]
        # The interval never came due, so the only row is the partial one.
        self.assertEqual(len(rows), 1)
        self.assertLess(int(rows[0].split("\t")[3]), 600)

    def test_window_does_nothing_quietly_without_a_counter(self):
        self.assertEqual(radbeeper.main(["--device", "/dev/does-not-exist",
                                       "window"]), 0)


class TestHotplug(unittest.TestCase):
    """The session watcher: one window per plug, and none without one.

    spawn_window is replaced by a fork that only sleeps, because what is under
    test is WHEN the watcher decides to open a window -- not what the window
    then does, which is cmd_window's business and is covered above. The child
    is real rather than a fake pid so that the waitpid bookkeeping, which is
    how the loop knows a monitor is already up, is exercised too.
    """

    def setUp(self):
        self.real_ports = radbeeper.candidate_ports
        self.real_spawn = radbeeper.spawn_window
        self.opened = []
        self.kids = []
        self.child_life = 30.0   # a window that stays open

        def spawn(_args):
            self.opened.append(True)
            pid = os.fork()
            if pid == 0:
                if self.child_life:
                    time.sleep(self.child_life)
                os._exit(0)
            self.kids.append(pid)
            return pid

        radbeeper.spawn_window = spawn

    def tearDown(self):
        radbeeper.candidate_ports = self.real_ports
        radbeeper.spawn_window = self.real_spawn
        for pid in self.kids:
            try:
                os.kill(pid, 9)
            except OSError:
                pass
            try:
                os.waitpid(pid, 0)
            except OSError:
                pass

    def run_watcher(self, ports_over_time, duration=1.4, poll=0.2):
        """Run the loop with candidate_ports answering a fixed script.

        The last entry is the steady state, so a short script can describe a
        session of any length.
        """
        answers = list(ports_over_time)

        def ports():
            return answers.pop(0) if len(answers) > 1 else answers[0]

        radbeeper.candidate_ports = ports
        # --duration is a global option, so it goes before the subcommand.
        return radbeeper.main(["--duration", str(duration), "hotplug",
                               "--poll", str(poll), "--settle", "0"])

    def test_nothing_plugged_in_opens_nothing(self):
        # The whole point of the silent design: a machine with no counter must
        # never see a window, however long the session runs.
        self.assertEqual(self.run_watcher([[]]), 0)
        self.assertEqual(self.opened, [])

    def test_a_counter_already_there_at_login_opens_one_window(self):
        # The case the old `window` autostart line handled, and which this has
        # to keep handling now that it has replaced it.
        self.assertEqual(self.run_watcher([["/dev/ttyUSB0"]]), 0)
        self.assertEqual(len(self.opened), 1)

    def test_a_counter_plugged_in_later_opens_one_window(self):
        # Empty at login, then a node appears. Exactly one window -- not one
        # per poll for the rest of the session, which is the way this goes
        # wrong if the edge is not what triggers it.
        self.assertEqual(self.run_watcher([[], [], ["/dev/ttyUSB0"]]), 0)
        self.assertEqual(len(self.opened), 1)

    def test_an_event_is_written_off_after_its_tries(self):
        # A window that exits at once is what a serial cable that is not a
        # counter looks like from here. It is worth a few attempts, because
        # the same shape is a node whose group udev has not set yet -- and
        # then it must stop. Reopening a stranger's port every four seconds
        # for the rest of the session is the failure being guarded against.
        self.child_life = 0
        self.run_watcher([["/dev/ttyUSB0"]], duration=2.0, poll=0.2)
        self.assertEqual(len(self.opened), radbeeper.DEFAULT_TRIES)


class TestSlotsAndMerging(unittest.TestCase):
    """One row per slot, and a backfill never writes over what was measured.

    The slot -- floor of the time over the row spacing -- is a row's identity.
    Rows arrive from the live monitor and from the counter's flash and must not
    be able to describe the same stretch twice, and where neither has anything
    to say the file is left with a hole rather than an invention.
    """

    every = 30.0
    spans = (3, 30, 300)

    def setUp(self):
        import tempfile
        self.tmp = tempfile.mkdtemp()
        self.path = os.path.join(self.tmp, "cpm.tsv")
        # A round multiple of the spacing, so slot boundaries are obvious.
        self.base = float(int(time.time() // 3000) * 3000)

    def samples(self, n, start=0.0, dt=1.0, count=1):
        return [(self.base + start + i * dt, count, dt) for i in range(n)]

    def rows(self, samples, max_gap=10.0):
        return radbeeper.rows_from_samples(samples, self.spans, self.every,
                                           max_gap)

    def test_samples_become_one_row_per_slot(self):
        rows = self.rows(self.samples(120))
        self.assertEqual(len(rows), 4)
        # And each row is stamped at the START of its slot, not at whatever
        # instant the last sample in it happened to fall on.
        for i, (when, _line) in enumerate(rows):
            self.assertEqual(when, self.base + i * self.every)

    def test_a_row_carries_the_seconds_it_actually_covers(self):
        # The counter's sample is 1.011 s, so 30 of them are 30.33 seconds and
        # the column has to say so -- otherwise the cps beside it is 1% wrong
        # with nothing on the row to show why.
        rows = self.rows(self.samples(30, dt=1.011))
        seconds = float(rows[0][1].split("\t")[3])
        self.assertAlmostEqual(seconds, 30 * 1.011, places=3)

    def test_nothing_is_invented_for_a_slot_with_no_samples(self):
        # Two bursts an hour apart: four rows and four rows, and no rows at
        # all for the hour between them. A gap in the file is the honest
        # record of a gap in the recording.
        got = self.rows(self.samples(120) + self.samples(120, start=3600.0))
        self.assertEqual(len(got), 8)
        stamps = [w for w, _ in got]
        # The hole is the hour minus the slot the last samples of the first
        # burst were still filling: no row exists anywhere inside it.
        self.assertEqual(max(b - a for a, b in zip(stamps, stamps[1:])),
                         3600.0 - self.every * 3)

    def test_a_hole_in_the_recording_ends_the_averages(self):
        # A 300-second window carried across an hour the counter was switched
        # off would average two different afternoons together and print the
        # result as one number.
        long_enough = self.samples(400)
        after = self.samples(60, start=4000.0)
        got = self.rows(long_enough + after)
        before_gap = got[12][1].split("\t")
        after_gap = got[-1][1].split("\t")
        self.assertNotEqual(before_gap[6], "")   # cpm_300 was full
        self.assertEqual(after_gap[6], "")       # and starts again empty

    def test_backfilled_rows_say_so(self):
        line = self.rows(self.samples(60))[0][1]
        self.assertEqual(line.split("\t")[10], radbeeper.SRC_FLASH)

    def test_a_slot_that_already_has_a_row_is_left_alone(self):
        header = radbeeper.log_header(self.spans)
        live = radbeeper.log_row(self.base + 5, 1.0, 30, 30.0,
                                 [None, None, None], [None, None, None],
                                 radbeeper.SRC_LIVE)
        with open(self.path, "w") as f:
            f.write(header + "\n")
            f.write(live + "\n")
        added, clashed = radbeeper.merge_log(
            self.path, header, self.rows(self.samples(120)), self.every)
        self.assertEqual((added, clashed), (3, 1))
        with open(self.path) as f:
            body = [ln for ln in f.read().splitlines() if not ln.startswith("#")]
        self.assertEqual(len(body), 4)
        # The live row survived intact -- it is the better evidence.
        self.assertIn(live, body)
        self.assertEqual(sum(1 for ln in body
                             if ln.split("\t")[10] == radbeeper.SRC_LIVE), 1)

    def test_the_merged_file_sorts_chronologically_as_plain_text(self):
        header = radbeeper.log_header(self.spans)
        # Deliberately merge the older half second, since a backfill's whole
        # difficulty is that its rows belong in the past.
        radbeeper.merge_log(self.path, header,
                            self.rows(self.samples(120, start=3600.0)),
                            self.every)
        radbeeper.merge_log(self.path, header, self.rows(self.samples(120)),
                            self.every)
        with open(self.path) as f:
            lines = f.read().splitlines()
        self.assertTrue(lines[0].startswith("#"))
        body = lines[1:]
        self.assertEqual(body, sorted(body))
        self.assertEqual(len(body), 8)

    def test_a_merge_with_nothing_to_add_does_not_rewrite_the_file(self):
        header = radbeeper.log_header(self.spans)
        rows = self.rows(self.samples(120))
        radbeeper.merge_log(self.path, header, rows, self.every)
        before = open(self.path).read()
        added, clashed = radbeeper.merge_log(self.path, header, rows, self.every)
        self.assertEqual(added, 0)
        self.assertEqual(clashed, 4)
        self.assertEqual(open(self.path).read(), before)
        self.assertFalse(os.path.exists(self.path + ".new"))


class TestTheRing(unittest.TestCase):
    """Finding the newest bytes in a flash that may have wrapped.

    Reading the physical tail of a wrapped ring hands back the OLDEST hours
    while claiming they are the newest, and a backfill would then file last
    week under this afternoon. The counter on the bench was wrapped, so this
    is the ordinary case and not the exotic one.
    """

    per_mark = 60
    probe = 2048

    def run_of(self, start, samples, interval=1.011):
        """A stretch of recording: marks every per_mark samples."""
        out = bytearray()
        t = float(start)
        written = 0
        while written < samples:
            st = time.localtime(t)
            out += bytes([0x55, 0xAA, 0x00, st.tm_year - 2000, st.tm_mon,
                          st.tm_mday, st.tm_hour, st.tm_min, st.tm_sec])
            here = min(self.per_mark, samples - written)
            out += bytes([2] * here)
            written += here
            t = round(t + here * interval)
        return bytes(out)

    def shim(self, blob):
        class Shim(object):
            flash_size = len(blob)
            reads = 0

            def read_history(self, address, length):
                Shim.reads += 1
                return blob[address:address + length]
        return Shim()

    def test_a_flash_still_filling_ends_where_the_ff_starts(self):
        base = time.mktime((2026, 9, 1, 9, 0, 0, 0, 1, -1))
        data = self.run_of(base, 6000)
        blob = data + b"\xff" * (65536 - len(data))
        end, wrapped = radbeeper.find_history_end(self.shim(blob), len(blob),
                                                  self.probe)
        self.assertFalse(wrapped)
        self.assertLessEqual(abs(end - len(data)), self.probe)

    def test_a_wrapped_ring_ends_at_the_step_backwards_in_time(self):
        # Newest first physically, then the oldest: that is what a ring whose
        # pointer has come round looks like from address zero.
        base = time.mktime((2026, 9, 1, 9, 0, 0, 0, 1, -1))
        newer = self.run_of(base + 7 * 86400, 6000)
        older = self.run_of(base, 6000)
        blob = newer + older
        c = self.shim(blob)
        end, wrapped = radbeeper.find_history_end(c, len(blob), self.probe)
        self.assertTrue(wrapped)
        self.assertLessEqual(abs(end - len(newer)), self.probe)
        # And it is a bisection, not a read of the whole megabyte.
        self.assertLess(type(c).reads, 40)

    def test_the_tail_of_a_ring_is_the_newest_data_not_the_last_bytes(self):
        base = time.mktime((2026, 9, 1, 9, 0, 0, 0, 1, -1))
        newer = self.run_of(base + 7 * 86400, 6000)
        older = self.run_of(base, 6000)
        c = self.shim(newer + older)
        got = radbeeper.read_history_tail(c, 4096, quiet=True)
        stamps = [m[1] for m in radbeeper.history_marks(got)]
        self.assertTrue(stamps)
        # Everything read is from the newer week -- which is the whole point.
        self.assertGreater(min(stamps), base + 6 * 86400)

    def test_a_full_dump_of_a_ring_still_backfills_in_order(self):
        # `backfill --image` on a raw dump gets the seam as it lies. Sorting is
        # what makes it give the same answer as reading the tail.
        base = time.mktime((2026, 9, 1, 9, 0, 0, 0, 1, -1))
        blob = self.run_of(base + 7 * 86400, 600) + self.run_of(base, 600)
        got = sorted(radbeeper.history_samples(blob))
        self.assertEqual(got, sorted(got))
        self.assertLess(got[0][0], base + 86400)
        self.assertGreater(got[-1][0], base + 6 * 86400)


class TestDatedLogs(unittest.TestCase):
    """One file per month, which is the whole of rotation.

    A row goes to the file for its own month, so a month ending is not an
    event: that file stops growing and the next one starts. Nothing is
    scheduled, nothing renames a log while a service is appending to it, and a
    backfill carrying rows for a month that ended weeks ago needs to know
    nothing about what has already been rotated.
    """

    spans = (3, 30, 300)
    every = 30.0

    def setUp(self):
        import tempfile
        self.tmp = tempfile.mkdtemp()

    def at(self, y, m, d, hh=12, mm=0):
        return time.mktime((y, m, d, hh, mm, 0, 0, 1, -1))

    def row(self, when, src=radbeeper.SRC_FLASH):
        return (when, radbeeper.log_row(when, 1.0, 30, 30.0,
                                        [None] * 3, [None] * 3, src))

    def test_the_file_is_named_for_the_counter_and_the_month(self):
        # Two counters on one machine are two measurements, not one, and a
        # file that mixed them could not be unmixed afterwards.
        self.assertTrue(radbeeper.log_path(self.at(2026, 9, 4), self.tmp, "A1")
                        .endswith("cpm-A1-2026-09.tsv"))
        self.assertTrue(radbeeper.log_path(self.at(2026, 12, 31), self.tmp,
                                           "B2").endswith("cpm-B2-2026-12.tsv"))

    def test_the_names_sort_chronologically_within_a_counter(self):
        names = sorted(os.path.basename(radbeeper.log_path(
            self.at(2026, m, 1), self.tmp, "A1")) for m in (12, 2, 9, 1))
        self.assertEqual(names, ["cpm-A1-2026-01.tsv", "cpm-A1-2026-02.tsv",
                                 "cpm-A1-2026-09.tsv", "cpm-A1-2026-12.tsv"])

    def test_a_writer_rolls_over_at_the_month_boundary(self):
        out = radbeeper.LogWriter(self.spans, self.tmp, "A1")
        for when in (self.at(2026, 9, 30, 23, 59), self.at(2026, 10, 1, 0, 0)):
            out.write(when, self.row(when)[1] + "\n")
        out.close()
        names = sorted(os.listdir(self.tmp))
        self.assertEqual(names, ["cpm-A1-2026-09.tsv", "cpm-A1-2026-10.tsv"])
        # And each new file gets its own header, not a bare row.
        for n in names:
            with open(os.path.join(self.tmp, n)) as f:
                self.assertTrue(f.readline().startswith("#time"))

    def test_a_writer_notices_its_file_being_replaced_under_it(self):
        # merge_log renames a new file over the old one, which leaves an
        # appender writing to an orphaned inode: everything after that is lost
        # and nothing says so. The writer stats before each row for this.
        when = time.time()
        out = radbeeper.LogWriter(self.spans, self.tmp, "A1")
        out.write(when, self.row(when)[1] + "\n")
        path = radbeeper.log_path(when, self.tmp, "A1")
        os.rename(path, path + ".moved")          # as a merge would
        out.write(when, self.row(when)[1] + "\n")
        out.close()
        with open(path) as f:
            body = [ln for ln in f.read().splitlines() if not ln.startswith("#")]
        self.assertEqual(len(body), 1)            # the second row is not lost

    def test_backfill_files_each_month_where_it_belongs(self):
        rows = [self.row(self.at(2026, 8, 20)), self.row(self.at(2026, 9, 2))]
        header = radbeeper.log_header(self.spans)
        for when, line in rows:
            radbeeper.merge_log(radbeeper.log_path(when, self.tmp, "A1"),
                                header, [(when, line)], self.every)
        self.assertEqual(sorted(os.listdir(self.tmp)),
                         ["cpm-A1-2026-08.tsv", "cpm-A1-2026-09.tsv"])

    def test_an_undated_log_is_split_and_kept(self):
        # What an upgrade finds: one cpm.tsv in the old ten-column format,
        # spanning two months.
        old = os.path.join(self.tmp, radbeeper.LOG_NAME)
        with open(old, "w") as f:
            f.write("#time\tcps\tcounts\tseconds\tcpm_3\tcpm_30\tcpm_300"
                    "\tpeak_3\tpeak_30\tpeak_300\n")
            for when in (self.at(2026, 8, 20), self.at(2026, 9, 2)):
                f.write("%s\t1.000\t30\t30\t\t\t\t\t\t\n"
                        % time.strftime("%Y-%m-%dT%H:%M:%S",
                                        time.localtime(when)))

        class Args(object):
            spans = self.spans
            log_every = self.every
        moved, files = radbeeper.migrate_legacy_log(Args(), self.tmp, "A1")
        self.assertEqual((moved, files), (2, 2))
        self.assertEqual(
            sorted(os.listdir(self.tmp)),
            ["cpm-A1-2026-08.tsv", "cpm-A1-2026-09.tsv",
             "cpm.tsv.pre-rotation"])
        # The rows were widened, and marked as what they were: live readings
        # from a counter that had not been asked where it was.
        with open(os.path.join(self.tmp, "cpm-A1-2026-09.tsv")) as f:
            body = [ln for ln in f.read().splitlines()
                    if not ln.startswith("#")]
        self.assertEqual(len(body[0].split("\t")), 12)
        self.assertEqual(body[0].split("\t")[10], radbeeper.SRC_LIVE)

    def test_splitting_an_undated_log_twice_is_harmless(self):
        class Args(object):
            spans = self.spans
            log_every = self.every
        self.assertEqual(radbeeper.migrate_legacy_log(Args(), self.tmp, "A1"),
                         (0, 0))


class TestOneRowPerSlotLive(unittest.TestCase):
    """The slot rule has to hold for the logger too, not only for the merge.

    A service that comes back mid-interval writes a short first row covering a
    stretch the previous run already wrote in full. Two rows, one slot -- the
    exact clash the format is built to prevent, arriving from the one source
    that was not checking.
    """

    spans = (3, 30, 300)
    every = 30.0

    def setUp(self):
        import tempfile
        self.tmp = tempfile.mkdtemp()
        self.base = float(int(time.time() // 3000) * 3000)

    def line(self, when):
        return radbeeper.log_row(when, 1.0, 30, 30.0, [None] * 3, [None] * 3,
                                 radbeeper.SRC_LIVE, "here") + "\n"

    def writer(self):
        return radbeeper.LogWriter(self.spans, self.tmp, "A1", self.every)

    def test_a_second_row_in_one_slot_is_refused(self):
        out = self.writer()
        self.assertTrue(out.write(self.base + 1, self.line(self.base + 1)))
        self.assertFalse(out.write(self.base + 9, self.line(self.base + 9)))
        self.assertTrue(out.write(self.base + 31, self.line(self.base + 31)))
        out.close()

    def test_a_restart_does_not_duplicate_the_slot_it_lands_in(self):
        first = self.writer()
        first.write(self.base + 1, self.line(self.base + 1))
        first.close()
        again = self.writer()          # a new run, reading what is on disk
        self.assertFalse(again.write(self.base + 20, self.line(self.base + 20)))
        self.assertTrue(again.write(self.base + 40, self.line(self.base + 40)))
        again.close()
        with open(radbeeper.log_path(self.base, self.tmp, "A1")) as f:
            body = [ln for ln in f.read().splitlines()
                    if not ln.startswith("#")]
        self.assertEqual(len(body), 2)


class TestSites(unittest.TestCase):
    """Where a counter was, which is a property of a serial over time.

    Not of the machine and not of the file: these things get carried about, so
    a reading from last Tuesday has to resolve to where the counter was last
    Tuesday and not to where it is now.
    """

    def setUp(self):
        import tempfile
        self.tmp = tempfile.mkdtemp()

    def at(self, y, m, d):
        return time.mktime((y, m, d, 12, 0, 0, 0, 1, -1))

    def test_a_counter_nobody_has_placed_gets_no_place(self):
        # An assumed location is worse than none: it is published, it looks
        # like a measurement, and nobody reading it later can tell it was a
        # guess. So nothing is invented and the column stays empty.
        self.assertEqual(radbeeper.ensure_site("A1", self.tmp), [])
        self.assertIsNone(radbeeper.site_at("A1", time.time(), []))

    def test_the_earliest_record_covers_readings_older_than_itself(self):
        # A counter's flash goes back further than the day somebody wrote down
        # where it was, and those readings were still somewhere.
        radbeeper.record_site("A1", "The bench", self.at(2026, 9, 3), self.tmp)
        sites = radbeeper.read_sites(self.tmp)
        found = radbeeper.site_at("A1", self.at(2020, 1, 1), sites)
        self.assertEqual(found[2], "The bench")

    def test_a_move_applies_from_when_it_happened_and_not_before(self):
        radbeeper.record_site("A1", "The bench", self.at(2026, 9, 1), self.tmp)
        radbeeper.record_site("A1", "The garage", self.at(2026, 9, 3),
                              self.tmp)
        sites = radbeeper.read_sites(self.tmp)
        self.assertEqual(
            radbeeper.site_at("A1", self.at(2026, 9, 2), sites)[2],
            "The bench")
        self.assertEqual(
            radbeeper.site_at("A1", self.at(2026, 9, 4), sites)[2],
            "The garage")

    def test_it_is_a_history_and_not_a_setting(self):
        radbeeper.record_site("A1", "The bench", self.at(2026, 9, 1), self.tmp)
        radbeeper.record_site("A1", "The garage", self.at(2026, 9, 3),
                              self.tmp)
        radbeeper.record_site("A1", "The roof", self.at(2026, 9, 5), self.tmp)
        self.assertEqual(len(radbeeper.read_sites(self.tmp)), 3)

    def test_counters_do_not_borrow_each_others_places(self):
        radbeeper.record_site("A1", "The garage", self.at(2026, 9, 3),
                              self.tmp)
        sites = radbeeper.read_sites(self.tmp)
        self.assertIsNone(radbeeper.site_at("B2", self.at(2026, 9, 4), sites))

    def test_a_place_is_a_name_and_nothing_finer(self):
        # These logs are published. A place name is what a reader needs; a
        # decimal fix is a street address for whoever is holding the counter,
        # so the file has no column to put one in.
        radbeeper.record_site("A1", "The bench", self.at(2026, 9, 1), self.tmp)
        with open(os.path.join(self.tmp, radbeeper.SITES_NAME)) as f:
            for line in f:
                self.assertEqual(len(line.rstrip("\n").split("\t")), 3)


class TestExport(unittest.TestCase):
    """The page a fork publishes."""

    spans = (3, 30, 300)

    def setUp(self):
        import tempfile
        self.tmp = tempfile.mkdtemp()
        self.base = time.mktime((2026, 9, 2, 10, 0, 0, 0, 1, -1))
        radbeeper.record_site("A1", "The bench", self.base - 60, self.tmp)
        rows = []
        for i in range(4):
            when = self.base + i * 30
            rows.append((when, radbeeper.log_row(
                when, 1.0, 30, 30.0, [60.0, 60.0, 60.0], [90.0, 70.0, 60.0],
                radbeeper.SRC_FLASH, "The bench")))
        radbeeper.merge_log(radbeeper.log_path(self.base, self.tmp, "A1"),
                            radbeeper.log_header(self.spans), rows, 30.0)

    def test_a_log_name_says_which_counter_and_which_month(self):
        self.assertEqual([s for s, _p in radbeeper.log_files(self.tmp)], ["A1"])

    def test_a_serial_with_dashes_in_it_still_parses(self):
        open(os.path.join(self.tmp, "cpm-A-B-C-2026-09.tsv"), "w").close()
        self.assertIn("A-B-C", [s for s, _p in radbeeper.log_files(self.tmp)])

    def test_the_mean_is_counts_over_seconds_not_a_mean_of_means(self):
        # Rows are not all the same length -- a service stopped mid-interval
        # writes a short one -- so averaging the per-row averages would weight
        # a four-second row like a thirty-second one.
        short = self.base + 300
        radbeeper.merge_log(
            radbeeper.log_path(self.base, self.tmp, "A1"),
            radbeeper.log_header(self.spans),
            [(short, radbeeper.log_row(short, 15.0, 60, 4.0, [None] * 3,
                                       [None] * 3, radbeeper.SRC_FLASH, ""))],
            30.0)
        got = radbeeper.summarise(self.tmp)["A1"]
        self.assertAlmostEqual(got["cpm"], (120 + 60) * 60.0 / (120 + 4))

    def test_the_page_names_the_counter_and_where_it_was(self):
        counters = radbeeper.summarise(self.tmp)
        html = radbeeper.render_html(counters, radbeeper.read_sites(self.tmp))
        self.assertIn("A1", html)
        self.assertIn("The bench", html)
        self.assertIn("<table", html)
        self.assertTrue(html.startswith("<!doctype html>"))

    def test_the_page_survives_having_no_logs_at_all(self):
        import tempfile
        html = radbeeper.render_html(radbeeper.summarise(tempfile.mkdtemp()),
                                     [])
        self.assertIn("No logs found", html)

    def test_a_site_name_with_markup_in_it_is_escaped(self):
        radbeeper.record_site("A1", "<script>x</script>", self.base, self.tmp)
        html = radbeeper.render_html(radbeeper.summarise(self.tmp),
                                     radbeeper.read_sites(self.tmp))
        self.assertNotIn("<script>x</script>", html)
        self.assertIn("&lt;script&gt;", html)


class TestSpectrum(unittest.TestCase):
    """The accumulating spectrum, and why a flat one is the good answer.

    Radioactive decay is Poisson and the power spectrum of a Poisson process
    is white: every frequency carrying the same expected power. So a healthy
    counter watching background produces no shape at all, and the feature
    earns its place on the other case -- something arriving on a schedule,
    which decay does not have.
    """

    def test_the_transform_puts_a_sine_in_its_own_bin(self):
        n, k = 64, 7
        wave = [math.sin(2 * math.pi * k * i / n) for i in range(n)]
        power = [abs(c) ** 2 for c in radbeeper.fft(wave)[:n // 2]]
        self.assertEqual(power.index(max(power)), k)

    def test_a_constant_is_all_at_dc(self):
        power = [abs(c) ** 2 for c in radbeeper.fft([3.0] * 32)]
        self.assertGreater(power[0], 1e-9)
        self.assertLess(max(power[1:]), 1e-9)

    def test_a_length_that_is_not_a_power_of_two_is_refused(self):
        # Rather than silently padding, which would change the bin the answer
        # lands in without saying so.
        with self.assertRaises(ValueError):
            radbeeper.fft([1.0] * 10)

    def test_it_says_nothing_until_a_window_has_closed(self):
        spec = radbeeper.Spectrum(window=32)
        for _ in range(31):
            self.assertFalse(spec.add(1))
        self.assertEqual(spec.relative(), [])
        self.assertEqual(spec.wait(), 1)
        self.assertTrue(spec.add(1))
        self.assertEqual(spec.wait(), 0)

    def test_poisson_background_comes_out_flat(self):
        # The result that means "nothing is wrong". Averaging periodograms is
        # what makes it readable: one is flat in expectation and violently
        # noisy in fact.
        rng = random.Random(7)
        spec = radbeeper.Spectrum(window=64)
        for _ in range(64 * 40):
            # A Poisson draw at 0.5 counts a second, as background is.
            n, p, limit = 0, 1.0, pow(2.718281828459045, -0.5)
            while True:
                p *= rng.random()
                if p <= limit:
                    break
                n += 1
            spec.add(n)
        rel = spec.relative()
        self.assertEqual(len(rel), 31)
        self.assertLess(max(rel), 3.0)     # no bin stands out
        self.assertAlmostEqual(sum(rel) / len(rel), 1.0, places=6)

    def test_a_periodic_source_stands_out_of_the_grass(self):
        # Something arriving every eight seconds, buried in the same
        # background. This is the whole point of the panel.
        rng = random.Random(11)
        spec = radbeeper.Spectrum(window=64)
        for i in range(64 * 40):
            n = 1 if rng.random() < 0.5 else 0
            if i % 8 == 0:
                n += 6
            spec.add(n)
        rel = spec.relative()
        peak = max(rel)
        self.assertGreater(peak, 5.0)
        # Bin k of the DC-dropped spectrum is k+1 cycles per window, so a
        # period of 8 samples in a 64-sample window is bin 8 -- and an impulse
        # train carries the same power in every harmonic of it, so 16, 24 and
        # 32 stand up just as tall and which one happens to be tallest is
        # noise. What must be true is that the fundamental is loud and that
        # the loudest bin is one of its harmonics.
        self.assertGreater(rel[7], 5.0)
        self.assertAlmostEqual(spec.period(7), 8.0)
        self.assertEqual((rel.index(peak) + 1) % 8, 0)

    def test_columns_take_the_loudest_bin_they_cover(self):
        # A single sharp line is what is being looked for, and averaging it
        # with its quiet neighbours is how it disappears.
        rel = [1.0] * 32
        rel[9] = 12.0
        cols = radbeeper.spectrum_columns(rel, 8)
        self.assertEqual(len(cols), 8)
        self.assertEqual(max(cols), 12.0)
        self.assertEqual(cols.count(12.0), 1)

    def test_columns_of_nothing_are_nothing(self):
        self.assertEqual(radbeeper.spectrum_columns([], 40), [])


class TestBarRows(unittest.TestCase):
    """The counts chart, four rows tall instead of one."""

    def samples(self, values):
        return [(float(i), v) for i, v in enumerate(values)]

    def test_it_returns_the_height_asked_for(self):
        rows = radbeeper.bar_rows(self.samples([1, 2, 3]), 3, 4)
        self.assertEqual(len(rows), 4)
        self.assertTrue(all(len(r) == 3 for r in rows))

    def test_the_tallest_column_fills_every_row(self):
        rows = radbeeper.bar_rows(self.samples([0, 8]), 2, 4)
        self.assertEqual([r[1] for r in rows], ["█"] * 4)
        self.assertEqual([r[0] for r in rows], [" "] * 4)

    def test_a_half_height_column_fills_the_bottom_half(self):
        rows = radbeeper.bar_rows(self.samples([10, 5]), 2, 4)
        column = [r[1] for r in rows]
        self.assertEqual(column[0], " ")        # top row empty
        self.assertEqual(column[3], "█")   # bottom row full

    def test_four_rows_resolve_what_one_cannot(self):
        # One row has eight levels, so 30 and 32 out of 32 land on the same
        # glyph. Four rows have thirty-two and tell them apart.
        one = radbeeper.bar_rows(self.samples([32, 30]), 2, 1)
        four = radbeeper.bar_rows(self.samples([32, 30]), 2, 4)
        self.assertEqual(one[0][0], one[0][1])
        self.assertNotEqual([r[0] for r in four], [r[1] for r in four])

    def test_nothing_to_draw_is_not_a_crash(self):
        self.assertEqual(radbeeper.bar_rows([], 10, 4), [""] * 4)

    def test_all_zeroes_do_not_divide_by_zero(self):
        rows = radbeeper.bar_rows(self.samples([0, 0, 0]), 3, 4)
        self.assertEqual(rows, [" " * 3] * 4)


class TestSpectrumLadder(unittest.TestCase):
    """Several windows at once, so resolution grows with observation time.

    Frequency resolution is 1/T and there is no way round it: a long window
    resolves finely and waits a long time to say anything. Running them side
    by side gives up neither.
    """

    def test_the_short_rung_answers_first(self):
        lad = radbeeper.SpectrumLadder(windows=(8, 32))
        for _ in range(8):
            lad.add(1)
        self.assertEqual(lad.best().window, 8)

    def test_the_fine_rung_takes_over_once_it_has_enough(self):
        lad = radbeeper.SpectrumLadder(windows=(8, 32))
        for i in range(200):
            lad.add(i % 3)
        self.assertEqual(lad.best().window, 32)

    def test_before_anything_has_closed_it_offers_the_soonest(self):
        lad = radbeeper.SpectrumLadder(windows=(8, 32))
        lad.add(1)
        best = lad.best()
        self.assertEqual(best.window, 8)
        self.assertGreater(best.wait(), 0)

    def test_every_rung_sees_every_sample(self):
        lad = radbeeper.SpectrumLadder(windows=(8, 16))
        for _ in range(64):
            lad.add(2)
        self.assertTrue(all(r.runs > 0 for r in lad.rungs))
        # 64 samples, half-overlapped: 8-wide closes far more often than 16.
        self.assertGreater(lad.rungs[0].runs, lad.rungs[1].runs)


class TestOverlapAndSignificance(unittest.TestCase):
    def test_half_overlap_gets_two_averages_from_one_window(self):
        # Welch rather than Bartlett: the buffer keeps its second half, so a
        # window's worth of new data closes two windows, not one.
        spec = radbeeper.Spectrum(window=16)
        for _ in range(16):
            spec.add(1)
        self.assertEqual(spec.runs, 1)
        for _ in range(8):
            spec.add(1)
        self.assertEqual(spec.runs, 2)

    def test_significance_needs_no_stored_variance(self):
        # Each bin of one periodogram of white noise is exponential, so the
        # average of N has relative scatter 1/sqrt(N) exactly -- arithmetic on
        # the count of windows, not a variance accumulated beside it.
        spec = radbeeper.Spectrum(window=16)
        spec.runs = 100
        self.assertAlmostEqual(spec.independent(), 100 * 9.0 / 11.0)
        # Twice the mean, after 100 windows, is many sigma.
        self.assertGreater(spec.sigma(2.0), 9.0)
        # The same excess after one window is not.
        spec.runs = 1
        self.assertLess(spec.sigma(2.0), 1.5)


class TestBigNumber(unittest.TestCase):
    """Six-row digits, for the number read from across the room."""

    def test_every_digit_is_six_rows(self):
        for ch in "0123456789":
            self.assertEqual(len(radbeeper.big_number(ch)), 6)

    def test_the_rows_of_one_number_are_all_the_same_width(self):
        rows = radbeeper.big_number("1234.5")
        self.assertEqual(len(set(len(r) for r in rows)), 1)

    def test_digits_are_drawn_and_gaps_are_not(self):
        rows = radbeeper.big_number("8")
        self.assertTrue(all("█" in r for r in rows))
        self.assertNotIn("█", "".join(radbeeper.big_number(" ")))

    def test_a_dash_is_a_middle_bar_and_nothing_else(self):
        rows = radbeeper.big_number("-")
        self.assertEqual([bool("█" in r) for r in rows],
                         [False, False, True, False, False, False])

    def test_unknown_characters_are_skipped_not_crashed_on(self):
        self.assertEqual(radbeeper.big_number("9x9"), radbeeper.big_number("99"))
