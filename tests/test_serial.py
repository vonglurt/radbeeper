# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Paul Richeson
"""The serial path, against a fake GMC-320 on a pty.

These are the tests that cover the code --source sim skips entirely: termios
setup, exact-length reads on a real file descriptor, command framing, the
masked heartbeat stream and the chunked SPIR download.
"""
import importlib.machinery
import importlib.util
import os
import struct
import sys
import tempfile
import time
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
        # 100 samples between marks, because the counter's RTC has
        # one-second resolution: over 20 samples a 1.1% drift rounds away to
        # nothing, and the interval is only resolvable over a long stretch.
        # The real device writes a mark every 180.
        blob = build_history(seconds=300, cpm=400.0, seed=9, size=8192,
                             per_mark=100)
        self.dev.history = blob
        c = radbeeper.identify(self.dev.path, baud=115200)
        got = c.read_history(0, len(blob))
        c.close()
        rows = list(radbeeper.history_records(got))
        counts = [r[3] for r in rows if r[3] is not None]
        self.assertEqual(len(counts), 300)
        placed = [r[1] for r in rows if r[1] is not None and r[3] is not None]
        self.assertEqual(placed[0],
                         time.mktime((2026, 9, 4, 11, 0, 0, 0, 1, -1)))
        # The fixture ticks at 1.011 s, so the decoder must not report 1.000.
        dt = [r[2] for r in rows if r[2] is not None][0]
        self.assertAlmostEqual(dt, 1.01, places=3)
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
            self.assertEqual(lines[0], "offset,time,interval_s,count,note")
            self.assertGreaterEqual(len(lines), 30)


if __name__ == "__main__":
    unittest.main()


class TestTheLock(SerialCase):
    """One reader at a time, because two readers is a wrong number.

    Two processes reading one tty each get a share of the bytes and neither is
    told, so a logger and a monitor running together would halve both their
    counts and look entirely plausible doing it. These are the tests for the
    flock that stops that -- and for the two different answers the program
    gives to a locked port, which is the whole reason the case is separate
    from "no counter at all".
    """

    def test_a_second_open_of_the_same_port_is_refused(self):
        first = radbeeper.Serial(self.dev.path, 115200, 1.0)
        try:
            with self.assertRaises(radbeeper.Busy):
                radbeeper.Serial(self.dev.path, 115200, 1.0)
        finally:
            first.close()

    def test_the_lock_goes_when_the_holder_closes(self):
        first = radbeeper.Serial(self.dev.path, 115200, 1.0)
        first.close()
        second = radbeeper.Serial(self.dev.path, 115200, 1.0)
        second.close()

    def test_a_busy_port_is_its_own_reason_not_a_missing_counter(self):
        # "No counter" and "the counter is busy" have different fixes, so they
        # must not arrive as the same message.
        held = radbeeper.Serial(self.dev.path, 115200, 1.0)
        try:
            with self.assertRaises(radbeeper.NotFound) as caught:
                radbeeper.find_counter(device=self.dev.path)
            self.assertTrue(caught.exception.busy)
            self.assertIn("rc-service radbeeper stop",
                          caught.exception.detail)
        finally:
            held.close()

    def test_the_window_opens_nothing_when_the_port_is_busy(self):
        # Already being read means already covered: no second window, and no
        # write to a status file that belongs to whoever holds the port.
        held = radbeeper.Serial(self.dev.path, 115200, 1.0)
        try:
            self.assertEqual(
                radbeeper.main(["--device", self.dev.path, "window"]), 0)
        finally:
            held.close()

    def test_the_service_waits_on_a_busy_port_instead_of_going_dormant(self):
        # The one place this program is allowed a retry loop. An absent device
        # will not appear because a daemon asked again; a locked one will, the
        # moment the person watching it closes their monitor.
        import tempfile
        tmp = tempfile.mkdtemp()
        real = radbeeper.state_dir
        radbeeper.state_dir = lambda: tmp
        held = radbeeper.Serial(self.dev.path, 115200, 1.0)
        try:
            rc = radbeeper.main(["--device", self.dev.path,
                                 "--duration", "1", "service"])
            self.assertEqual(rc, 0)
            with open(os.path.join(tmp, "status")) as f:
                status = f.read()
            self.assertIn("waiting", status)
            self.assertNotIn("dormant", status)
        finally:
            held.close()
            radbeeper.state_dir = real


class TestTheFullScreenMonitor(SerialCase):
    """run_curses, actually drawing.

    It only executes when stdout is a terminal, so every other test in this
    suite takes the plain path and never touches it -- which is how a
    NameError in its draw loop shipped: a local named `level` shadowed the
    module-level level() that band() calls, and the first coloured row raised.
    A pseudo-terminal is cheap and this would have caught it.
    """

    def test_it_draws_on_a_terminal_without_falling_over(self):
        import pty
        import subprocess
        import threading

        import fcntl
        import termios as tio

        master, slave = pty.openpty()
        # A WIDE, TALL terminal. The default 80x24 is too small for the
        # spectrum and the random line to be drawn at all, so the test that
        # exists to exercise the draw loop was skipping most of it -- and a
        # TypeError in the part it skipped shipped anyway.
        fcntl.ioctl(slave, tio.TIOCSWINSZ, struct.pack("HHHH", 46, 132, 0, 0))
        prog = os.path.join(HERE, os.pardir, "radbeeper")
        env = dict(os.environ, TERM="xterm-256color")
        proc = subprocess.Popen(
            [sys.executable, prog, "-d", self.dev.path, "--duration", "3",
             "watch"],
            stdin=slave, stdout=slave, stderr=subprocess.PIPE, env=env)
        os.close(slave)

        # Drain the terminal, or the child blocks once its buffer fills.
        drawn = []

        def drain():
            try:
                while True:
                    chunk = os.read(master, 4096)
                    if not chunk:
                        break
                    drawn.append(chunk)
            except OSError:
                pass

        reader = threading.Thread(target=drain)
        reader.daemon = True
        reader.start()
        err = proc.communicate(timeout=60)[1].decode("utf-8", "replace")
        os.close(master)
        reader.join(timeout=5)

        self.assertEqual(proc.returncode, 0, err)
        self.assertNotIn("Traceback", err)
        painted = b"".join(drawn).decode("utf-8", "replace")
        # There is no title row any more -- the program's name is the one
        # thing on that screen nobody needs telling. What must be there is
        # the counter's identity, the readout, and the footer.
        self.assertIn(self.dev.path, painted)
        self.assertIn("GMC-320Re", painted)
        self.assertIn("counts this second", painted)
        self.assertIn("q to quit", painted)
