# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Paul Richeson
"""The counter's thermometer, asked in the middle of the stream.

The service reads two bytes a second from a stream with no framing, and now
and then writes a question into the middle of it. The whole risk is in that
sentence: four bytes of answer read as two seconds of counts, or a second of
counts swallowed as part of an answer, and either way a log with a number in
it that the tube never counted.

So the fake counter writes down every count it sends, and the test is that
what the service published is those counts, in that order, with none missing
and none added -- while temperatures were being asked once a second, with a
second of counts pushed in ahead of every answer, with a thermometer that has
not read yet, and with a firmware that never answers at all.

Skipped where there is no Rust build: the thermometer is in the Rust program
only.
"""
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)

from fake_gmc import FakeGMC  # noqa: E402


def _binary():
    if any(os.access(os.path.join(p, "cargo"), os.X_OK)
           for p in os.environ.get("PATH", "").split(os.pathsep)):
        try:
            subprocess.run(["cargo", "build", "--release"], cwd=ROOT,
                           check=True, capture_output=True, timeout=900)
        except (subprocess.SubprocessError, OSError):
            return None
    path = os.path.join(ROOT, "target", "release", "radbeeper")
    return path if os.access(path, os.X_OK) else None


BINARY = _binary()


@unittest.skipIf(BINARY is None, "no Rust build; the thermometer is Rust only")
class TempCase(unittest.TestCase):
    tick = 0.2
    # The first question goes out two seconds after the stream starts.
    seconds = 9.0

    def setUp(self):
        # /tmp, not the default: a unix socket's path has a length limit and
        # a temporary directory on a Mac is most of it.
        self.dir = tempfile.mkdtemp(prefix="rbt", dir="/tmp")
        self.dev = FakeGMC(cpm=600.0, seed=17, tick=self.tick)

    def tearDown(self):
        self.dev.stop()
        shutil.rmtree(self.dir, ignore_errors=True)

    def serve(self, *extra, seconds=None):
        """Run a service against the fake and return what it published:
        (counts, temperatures, its stdout and stderr)."""
        self.dev.start()
        proc = subprocess.Popen(
            [BINARY, "-d", self.dev.path, "--logs", self.dir, "--no-backfill",
             "--no-frames", "--duration", str(seconds or self.seconds),
             *extra, "service"],
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        sock = None
        deadline = time.time() + 10
        while time.time() < deadline and sock is None:
            try:
                s = socket.socket(socket.AF_UNIX)
                s.connect(os.path.join(self.dir, "sock"))
                sock = s
            except OSError:
                s.close()
                time.sleep(0.05)
        self.assertIsNotNone(sock, "the service never served its stream")
        data = b""
        sock.settimeout(20)
        while True:
            try:
                chunk = sock.recv(65536)
            except OSError:
                break
            if not chunk:
                break
            data += chunk
        sock.close()
        said = proc.communicate(timeout=20)[0]
        counts, temps = [], []
        for line in data.decode().splitlines():
            f = line.split("\t")
            if f[0] == "s":
                counts.append(int(f[3]))
            elif f[0] == "t":
                temps.append(float(f[3]))
        return counts, temps, said

    def assert_every_second_is_the_counters(self, counts):
        self.assertGreater(len(counts), 10, "the service read almost nothing")
        sent = list(self.dev.sent)
        self.assertEqual(
            counts, sent[:len(counts)],
            "what was published is not what the counter sent, second for second")

    def temp_rows(self):
        names = [n for n in os.listdir(self.dir) if n.startswith("temp-")]
        if not names:
            return []
        self.assertEqual(len(names), 1)
        with open(os.path.join(self.dir, names[0])) as f:
            lines = f.read().splitlines()
        self.assertEqual(lines[0], "#time\ttemp_c")
        return [float(l.split("\t")[1]) for l in lines[1:]]


class TestTheThermometer(TempCase):
    def test_it_is_read_and_logged_and_no_second_is_lost(self):
        counts, temps, said = self.serve("--temp-every", "1")
        self.assert_every_second_is_the_counters(counts)
        self.assertGreaterEqual(len(temps), 3, said)
        self.assertEqual(set(temps), {26.1})
        self.assertEqual(self.temp_rows(), temps)
        self.assertIn("reads 26.1 C", said)

    def test_below_freezing_keeps_its_sign(self):
        self.dev.temp = -3.5
        counts, temps, _ = self.serve("--temp-every", "1")
        self.assert_every_second_is_the_counters(counts)
        self.assertEqual(set(temps), {-3.5})

    def test_a_second_that_gets_in_ahead_of_the_answer_is_kept(self):
        # The worst order the wire can deliver: counts, then the answer.
        self.dev.interleave = True
        counts, temps, said = self.serve("--temp-every", "1")
        self.assert_every_second_is_the_counters(counts)
        self.assertGreaterEqual(len(temps), 3, said)
        self.assertEqual(set(temps), {26.1})

    def test_eighty_five_is_waited_out_and_never_written(self):
        self.dev.temp_cold = 2
        counts, temps, said = self.serve("--temp-every", "1", seconds=12)
        self.assert_every_second_is_the_counters(counts)
        self.assertGreaterEqual(len(temps), 1, said)
        self.assertEqual(set(temps), {26.1})
        self.assertNotIn(85.0, self.temp_rows())

    def test_the_count_log_is_the_same_file_it_always_was(self):
        self.serve("--temp-every", "1", "--log-every", "2")
        names = [n for n in os.listdir(self.dir) if n.startswith("cpm-")]
        self.assertEqual(len(names), 1)
        with open(os.path.join(self.dir, names[0])) as f:
            head = f.readline().rstrip("\n")
        self.assertNotIn("temp", head)
        self.assertTrue(head.startswith("#time\tcps\tcounts\tseconds\t"))


class TestNotAsking(TempCase):
    def test_no_temp_asks_nothing(self):
        counts, temps, said = self.serve("--no-temp")
        self.assert_every_second_is_the_counters(counts)
        self.assertEqual(temps, [])
        self.assertNotIn(b"<GETTEMP>>", self.dev.commands)
        self.assertEqual(self.temp_rows(), [])
        self.assertIn("temperature not asked", said)

    def test_a_firmware_the_document_does_not_name_is_not_asked(self):
        self.dev.version = b"GMC-320Re 2.10"
        counts, temps, _ = self.serve("--temp-every", "1")
        self.assert_every_second_is_the_counters(counts)
        self.assertEqual(temps, [])
        self.assertNotIn(b"<GETTEMP>>", self.dev.commands)

    def test_another_model_is_not_asked(self):
        self.dev.version = b"GMC-300Re 4.26"
        counts, temps, _ = self.serve("--temp-every", "1")
        self.assert_every_second_is_the_counters(counts)
        self.assertNotIn(b"<GETTEMP>>", self.dev.commands)


class TestACounterThatDoesNotAnswer(TempCase):
    # A second apart, as the real stream is: the wait for an answer is
    # shorter than that, and must be, or a silence would swallow counts.
    tick = 1.0
    seconds = 12.0

    def test_it_is_asked_three_times_and_then_left_alone(self):
        self.dev.temp_known = False
        counts, temps, said = self.serve("--temp-every", "1")
        sent = list(self.dev.sent)
        self.assertGreaterEqual(len(counts), 5)
        self.assertEqual(counts, sent[:len(counts)])
        self.assertEqual(temps, [])
        self.assertEqual(self.dev.commands.count(b"<GETTEMP>>"), 3)
        self.assertIn("not asking it again", said)


class TestNow(TempCase):
    seconds = 9.0

    def test_another_program_reads_cpm_and_temperature_from_the_service(self):
        self.dev.start()
        proc = subprocess.Popen(
            [BINARY, "-d", self.dev.path, "--logs", self.dir, "--no-backfill",
             "--no-frames", "--duration", str(self.seconds),
             "--temp-every", "1", "service"],
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        try:
            time.sleep(5.0)
            out = subprocess.run([BINARY, "--logs", self.dir, "now", "--json"],
                                 capture_output=True, text=True, timeout=20)
            self.assertEqual(out.returncode, 0, out.stderr)
            doc = json.loads(out.stdout)
            self.assertEqual(len(doc["counters"]), 1)
            c = doc["counters"][0]
            self.assertEqual(c["serial"], "123456789ABCDE")
            self.assertTrue(c["live"])
            self.assertEqual(c["temp_c"], 26.1)
            self.assertLess(c["temp_age"], 5.0)
            # The fake draws each beat at 600 CPM and `now` takes a beat for
            # a second, as the real stream's are: 600, give or take the draw.
            self.assertGreater(c["cpm"], 450.0)
            self.assertLess(c["cpm"], 750.0)
            table = subprocess.run([BINARY, "--logs", self.dir, "now"],
                                   capture_output=True, text=True, timeout=20)
            lines = table.stdout.splitlines()
            self.assertTrue(lines[0].startswith("#serial\tcpm\t"))
            self.assertEqual(lines[1].split("\t")[5], "26.1")
        finally:
            proc.communicate(timeout=20)

    def test_with_no_service_it_says_so_and_fails(self):
        out = subprocess.run([BINARY, "--logs", self.dir, "now"],
                             capture_output=True, text=True, timeout=20)
        self.assertEqual(out.returncode, 1)
        self.assertIn("nothing is serving", out.stderr)


if __name__ == "__main__":
    unittest.main()
