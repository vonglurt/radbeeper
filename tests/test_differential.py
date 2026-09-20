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
import contextlib
import os
import subprocess
import sys
import time
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

ORACLE = os.path.join(ROOT, "target", "release", "examples",
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
            cwd=ROOT, check=True,
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


@unittest.skipIf(ORACLE_PATH is None, "no Rust toolchain; nothing to diff")
class TestTheSameBitsComeOut(unittest.TestCase):
    """The entropy pool, which is the one place a difference would be silent.

    A log row that disagrees between the two is at least visible in the file.
    A digest that disagrees is 64 characters of hex that look exactly as
    random either way, and the only thing that would ever notice is somebody
    running `--check` a year later and being told their audit trail is a lie.
    """

    def rust(self, directives):
        out = subprocess.run([ORACLE_PATH], input="\n".join(directives) + "\n",
                             capture_output=True, text=True, check=True)
        return out.stdout.splitlines()

    def test_sha256_is_sha256(self):
        import hashlib
        msgs = ["", "abc", "radbeeper/entropy/1", "a" * 55, "a" * 56,
                "a" * 63, "a" * 64, "a" * 65, "x" * 1000]
        got = self.rust(["sha256\t" + m for m in msgs])
        for m, g in zip(msgs, got):
            self.assertEqual(g, hashlib.sha256(m.encode()).hexdigest(),
                             "length %d" % len(m))

    def test_counts_pack_to_the_same_nibbles(self):
        cases = [[0], [0, 1, 2, 3], [15], [16], [99], list(range(0, 20)),
                 [0] * 40 + [7, 15, 3]]
        got = self.rust(["pack\t" + ",".join(str(c) for c in cs)
                         for cs in cases])
        for cs, g in zip(cases, got):
            self.assertEqual(g, radbeeper.pack_counts(cs), "%r" % cs)

    def test_the_measured_entropy_is_the_same_number(self):
        cases = [
            [0, 1, 2, 3] * 100,
            ([0] * 300) + ([6] * 100),
            [1] * 50,
            [0, 1] * 7,
            list(range(0, 16)) * 20,
        ]
        got = self.rust(["mcv\t" + ",".join(str(c) for c in cs)
                         for cs in cases])
        for cs, g in zip(cases, got):
            self.assertAlmostEqual(float(g), radbeeper.mcv_min_entropy(cs),
                                   places=10, msg="%d samples" % len(cs))

    def test_the_model_it_replaced_is_also_the_same_number(self):
        rates = [0.0, 0.1, 0.68, 0.797, 1.0, 2.0, 10.0, 100.0]
        got = self.rust(["poisson\t%r" % r for r in rates])
        for r, g in zip(rates, got):
            self.assertAlmostEqual(float(g), radbeeper.poisson_min_entropy(r),
                                   places=10, msg="rate %r" % r)

    def test_the_rust_recomputes_every_line_this_counter_ever_emitted(self):
        """The proof for the whole port of this module.

        These are real emissions from the GMC-320Re on this desk, recorded by
        the Python months of session-time before any of this existed. If the
        Rust's SHA-256, its NUL framing, its integer formatting of the opened
        second, or its nibble packing were off by one byte in any of them,
        not one of these would match.
        """
        path = os.path.join(ROOT, "logs", "random-F48824B8207F7E.tsv")
        if not os.path.exists(path):
            self.skipTest("no recorded emissions in the repository")
        with open(path) as f:
            rows = [l.rstrip("\n").split("\t") for l in f
                    if not l.startswith("#")]
        self.assertTrue(rows, "the emission log is empty")
        directives, want = [], []
        with fixture_timezone():
            for r in rows:
                started = int(time.mktime(
                    time.strptime(r[1], "%Y-%m-%dT%H:%M:%S")))
                directives.append(
                    "digest\t%s\t%s\t%s" % (r[0], started, r[7]))
                want.append(r[6])
        got = self.rust(directives)
        self.assertEqual(got, want)
        # And the Python still agrees with itself, so this is a three-way
        # equality rather than two programs sharing one mistake.
        for r in rows:
            with fixture_timezone():
                started = time.mktime(
                    time.strptime(r[1], "%Y-%m-%dT%H:%M:%S"))
            record = {"seq": int(r[0]), "started": started, "hex": r[6],
                      "counts": r[7]}
            self.assertTrue(radbeeper.check_entropy_record(record))


# The recorded emissions in logs/ carry local time with no offset, so the
# epoch a digest was computed over can only be reconstructed by knowing the
# zone they were written in: this counter's desk. mktime in whatever zone the
# test happens to run in reproduced them there and nowhere else -- in UTC,
# which is what CI runs in, every digest came out different and the failure
# said only that two 64-character strings were not equal. The rows span
# December and September, so this is the zone and not a fixed offset: the
# difference between them is an hour of daylight saving.
FIXTURE_TZ = "America/Los_Angeles"


@contextlib.contextmanager
def fixture_timezone():
    """Read the recorded timestamps in the zone that wrote them."""
    was = os.environ.get("TZ")
    os.environ["TZ"] = FIXTURE_TZ
    time.tzset()
    try:
        yield
    finally:
        if was is None:
            os.environ.pop("TZ", None)
        else:
            os.environ["TZ"] = was
        time.tzset()


def flash(records):
    """Build a history image out of a little script.

    ("mark", y, m, d, hh, mm, ss)  a timestamp marker
    ("tick",)                      the 55 AA 01 that follows every timestamp
    ("note", text)                 a note typed on the device
    ("counts", [n, ...])           ordinary samples
    ("raw", bytes)                 whatever is wanted, verbatim
    """
    out = bytearray()
    for r in records:
        if r[0] == "mark":
            out += bytes([0x55, 0xAA, 0x00]) + bytes(r[1:])
        elif r[0] == "tick":
            out += bytes([0x55, 0xAA, 0x01])
        elif r[0] == "note":
            body = r[1].encode("ascii")
            out += bytes([0x55, 0xAA, 0x02, len(body)]) + body
        elif r[0] == "counts":
            out += bytes(r[1])
        elif r[0] == "raw":
            out += bytes(r[1])
    return bytes(out)


@unittest.skipIf(ORACLE_PATH is None, "no Rust toolchain; nothing to diff")
class TestTheHistoryDecodesTheSame(unittest.TestCase):
    """GQ's history format, decoded by both, record for record.

    This is where the two corrections to GQ's published document live -- the
    nine-byte datetime record and the three-byte marker carrying no payload --
    and both of them were found by noticing that the decoded numbers were
    impossible. A second decoder that got either one subtly different would
    produce a plausible CSV and a wrong one, which is the failure this format
    is most prone to and the reason the raw image is always kept.
    """

    def rust(self, blob):
        out = subprocess.run(
            [ORACLE_PATH], input="history\t" + blob.hex() + "\n",
            capture_output=True, text=True, check=True)
        return [l for l in out.stdout.splitlines() if l != "--"]

    def python(self, blob):
        rows = []
        for off, when, dt, count, note in radbeeper.history_records(blob):
            rows.append("\t".join([
                str(off),
                "-" if when is None else "%.6f" % when,
                "-" if dt is None else "%.9f" % dt,
                "-" if count is None else str(count),
                note,
            ]))
        return rows

    def same(self, blob, why):
        self.assertEqual(self.rust(blob), self.python(blob), why)

    def test_an_ordinary_recording(self):
        img = flash([
            ("mark", 26, 9, 4, 12, 0, 0), ("tick",),
            ("counts", [0, 1, 0, 2, 0, 0, 3, 1]),
            ("mark", 26, 9, 4, 12, 0, 9), ("tick",),
            ("counts", [1, 0, 0, 4, 0, 1, 0, 0, 2]),
            ("mark", 26, 9, 4, 12, 0, 18), ("tick",),
            ("counts", [0, 0, 1]),
        ])
        self.same(img, "the shape every real image has")

    def test_the_datetime_record_is_nine_bytes(self):
        # If either side read a tenth byte it would swallow the 0x55 of the
        # next marker and decode 0xAA as a count of 170 -- which is exactly
        # the bug the Python's comment describes.
        img = flash([
            ("mark", 26, 9, 4, 12, 0, 0), ("tick",), ("counts", [1, 2]),
            ("mark", 26, 9, 4, 12, 0, 3), ("tick",), ("counts", [3]),
        ])
        self.same(img, "nine bytes, no save-mode byte")
        self.assertNotIn("\t170\t", "\n".join(self.rust(img)))

    def test_the_marker_after_a_timestamp_carries_no_payload(self):
        # Read as a two-byte count it would invent a reading in the tens of
        # thousands per second on a tube that saturates far below that.
        img = flash([
            ("mark", 26, 9, 4, 12, 0, 0), ("tick",),
            ("counts", [0, 0]),
            ("mark", 26, 9, 4, 12, 0, 2), ("tick",), ("counts", [0]),
        ])
        self.same(img, "three bytes, and the two after it are samples")

    def test_a_note_typed_on_the_device(self):
        img = flash([
            ("mark", 26, 9, 4, 12, 0, 0), ("tick",), ("counts", [1]),
            ("note", "bench"),
            ("counts", [2, 3]),
            ("mark", 26, 9, 4, 12, 0, 4), ("tick",), ("counts", [0]),
        ])
        self.same(img, "notes come back as text and do not count as samples")

    def test_unwritten_flash_is_skipped_rather_than_counted(self):
        img = flash([
            ("mark", 26, 9, 4, 12, 0, 0), ("tick",), ("counts", [1, 2]),
            ("raw", [0xFF] * 32),
            ("mark", 26, 9, 4, 12, 0, 3), ("tick",), ("counts", [4]),
        ])
        self.same(img, "0xFF is absence, not a count of 255")

    def test_a_corrupt_timestamp_is_refused_by_both(self):
        # mktime normalises rather than refuses, so month 99 day 99 is 2034 to
        # it. Half-erased flash throws these up and a mark accepted from one
        # would place every sample after it in the wrong decade.
        for bad in ((26, 99, 99, 0, 0, 0), (26, 0, 1, 0, 0, 0),
                    (26, 1, 0, 0, 0, 0), (26, 1, 1, 99, 0, 0),
                    (26, 1, 1, 0, 99, 0), (26, 1, 1, 0, 0, 99)):
            img = flash([
                ("mark", 26, 9, 4, 12, 0, 0), ("tick",), ("counts", [1]),
                ("mark",) + bad, ("counts", [2, 3]),
                ("mark", 26, 9, 4, 12, 0, 4), ("tick",), ("counts", [0]),
            ])
            self.same(img, "refused %r" % (bad,))

    def test_samples_before_the_first_timestamp_cannot_be_placed(self):
        img = flash([
            ("counts", [5, 6, 7]),
            ("mark", 26, 9, 4, 12, 0, 0), ("tick",), ("counts", [1, 2]),
            ("mark", 26, 9, 4, 12, 0, 3), ("tick",), ("counts", [0]),
        ])
        self.same(img, "a partial read of the middle of the flash")

    def test_a_truncated_record_at_the_end_of_the_image(self):
        # A tail read cuts wherever it cuts, and every one of these is a
        # marker chopped mid-record.
        base = flash([("mark", 26, 9, 4, 12, 0, 0), ("tick",),
                      ("counts", [1, 2, 3])])
        for tail in ([0x55], [0x55, 0xAA], [0x55, 0xAA, 0x00],
                     [0x55, 0xAA, 0x00, 26, 9], [0x55, 0xAA, 0x02],
                     [0x55, 0xAA, 0x02, 40, 0x61]):
            self.same(base + bytes(tail), "truncated %r" % (tail,))

    def test_the_interval_is_measured_and_an_outlier_takes_the_median(self):
        # Nine samples over ten seconds is 1.111 s each; the counter's second
        # is not ours and assuming 1.000 files an hour-old sample most of a
        # minute from where it belongs. And a four-hour hole is a hole, not a
        # stretch that recorded one sample every eighty seconds.
        img = flash([
            ("mark", 26, 9, 4, 12, 0, 0), ("tick",), ("counts", [1] * 9),
            ("mark", 26, 9, 4, 12, 0, 10), ("tick",), ("counts", [1] * 9),
            ("mark", 26, 9, 4, 12, 0, 20), ("tick",), ("counts", [1] * 9),
            ("mark", 26, 9, 4, 16, 0, 0), ("tick",), ("counts", [1] * 9),
            ("mark", 26, 9, 4, 16, 0, 10), ("tick",), ("counts", [1] * 3),
        ])
        self.same(img, "measured intervals, and the median for an outlier")

    def test_an_image_with_one_timestamp_measures_nothing_and_says_so(self):
        img = flash([("mark", 26, 9, 4, 12, 0, 0), ("tick",),
                     ("counts", [1, 2, 3])])
        self.same(img, "None rather than assuming a second")

    def test_an_empty_image_and_an_all_erased_one(self):
        self.same(b"", "nothing at all")
        self.same(bytes([0xFF] * 64), "erased flash")

    def test_a_real_image_off_a_real_counter(self):
        """The one that is not a construction.

        16 KiB out of the flash of the GMC-320Re these logs come from, cut at
        a timestamp marker so it is a recording rather than a slice through
        the middle of one. Synthetic images test the cases somebody thought
        of; this tests the case the firmware actually produces, including
        whatever it does that nobody has noticed yet.
        """
        path = os.path.join(ROOT, "tests", "fixtures", "flash-gmc320re.bin")
        if not os.path.exists(path):
            self.skipTest("no flash fixture in the repository")
        with open(path, "rb") as f:
            blob = f.read()
        mine = self.python(blob)
        self.assertGreater(len(mine), 10000, "the fixture decoded to nothing")
        self.assertEqual(self.rust(blob), mine)
        # And it is a recording, not a field of 0xFF that trivially agrees.
        marks = [l for l in mine if l.split("\t")[3] == "-"
                 and l.split("\t")[4] == ""]
        self.assertGreater(len(marks), 40, "too few timestamps to be a test")

    def test_the_rows_a_backfill_would_write_are_the_same_rows(self):
        """The whole chain, on real bytes.

        Decode the image, measure the sample intervals, replay the samples
        through four averaging windows, break the averages at every hole, and
        format one row per slot. Every step of that is arithmetic on floats
        with a chance to differ in the last place, and the output is what
        would be merged into a month of somebody's log.
        """
        path = os.path.join(ROOT, "tests", "fixtures", "flash-gmc320re.bin")
        if not os.path.exists(path):
            self.skipTest("no flash fixture in the repository")
        with open(path, "rb") as f:
            blob = f.read()
        spans = (3.0, 30.0, 300.0, 3000.0)
        samples = sorted(radbeeper.history_samples(blob, 0.0))
        want = [line for _when, line in radbeeper.rows_from_samples(
            samples, spans, 30.0, 300.0)]
        self.assertGreater(len(want), 100, "the fixture produced too few rows")
        got = subprocess.run(
            [ORACLE_PATH],
            input="rows\t%s\t%s\t30.0\t300.0\t0.0\n"
                  % (blob.hex(), ",".join(repr(s) for s in spans)),
            capture_output=True, text=True, check=True)
        self.assertEqual(
            [l for l in got.stdout.splitlines() if l != "--"], want)

    def test_a_marker_byte_appearing_as_an_ordinary_count(self):
        # 0x55 not followed by 0xAA is a count of 85, and 0xAA on its own is a
        # count of 170. Both are perfectly ordinary readings.
        img = flash([
            ("mark", 26, 9, 4, 12, 0, 0), ("tick",),
            ("raw", [0x55, 0x01, 0xAA, 0x55, 0xAA]),
            ("mark", 26, 9, 4, 12, 0, 3), ("tick",), ("counts", [1]),
        ])
        self.same(img, "0x55 and 0xAA are counts unless they are a marker")


@unittest.skipIf(ORACLE_PATH is None, "no Rust toolchain; nothing to run")
class TestTheRustServiceWritesAReadableLog(unittest.TestCase):
    """A file one implementation wrote, read by the other.

    The oracle above proves the two agree on how to format a value. This
    proves the whole round trip: the Rust service writes a real log against
    real hardware or none, and the PYTHON's own reader parses it -- same
    header, same columns, same meaning for an empty field. A format is only
    one format if both ends of it agree, and only one of those ends is being
    tested by comparing strings.
    """

    BINARY = os.path.join(ROOT, "target", "release", "radbeeper")

    @classmethod
    def setUpClass(cls):
        if not os.path.exists(cls.BINARY):
            try:
                subprocess.run(["cargo", "build", "--release"],
                               cwd=ROOT, check=True,
                               stdout=subprocess.DEVNULL,
                               stderr=subprocess.DEVNULL, timeout=300)
            except (subprocess.SubprocessError, OSError):
                raise unittest.SkipTest("could not build the binary")

    def run_service(self, seconds=8, every=2):
        import tempfile
        d = tempfile.mkdtemp()
        out = subprocess.run(
            [self.BINARY, "service", "--logs", d, "--log-every", str(every),
             "--duration", str(seconds), "--no-backfill"],
            capture_output=True, text=True, timeout=seconds + 60)
        files = [os.path.join(d, n) for n in os.listdir(d)
                 if n.endswith(".tsv")]
        return d, files, out

    def test_the_python_reads_what_the_rust_wrote(self):
        d, files, out = self.run_service()
        if not files:
            # Two ways to write no rows, and neither is this test failing:
            # no counter on the machine, which is dormant and stops, or a
            # counter somebody already holds -- a monitor open in the next
            # terminal -- which is waiting. Silence is still not accepted on
            # trust: the service has to say which of the two it was.
            said = out.stdout + out.stderr
            self.assertTrue("dormant" in said or "waiting" in said, said)
            self.skipTest("no counter attached" if "dormant" in said
                          else "the port is held by another radbeeper")
        self.assertEqual(len(files), 1, "one file per counter per month")
        path = files[0]

        with open(path) as f:
            head = f.readline().rstrip("\n")
        self.assertEqual(head,
                         radbeeper.log_header((3, 30, 300, 3000, 30000)),
                         "the Rust wrote a different header")

        names = radbeeper.log_columns(head)
        rows = radbeeper.read_table(path, names)
        self.assertTrue(rows, "no rows in %s" % path)
        for when, cells in rows:
            got = dict(zip(names, cells))
            self.assertEqual(got["src"], radbeeper.SRC_LIVE)
            self.assertEqual(len(cells), len(names))
            # cps is per ONE second, whatever the row spacing.
            self.assertAlmostEqual(float(got["cps"]),
                                   int(got["counts"]) / float(got["seconds"]),
                                   places=2)
            # An empty window is empty, not zero -- and the ones that cannot
            # have filled in eight seconds must be empty.
            self.assertEqual(got["cpm_3000"], "",
                             "a 3000s window cannot be full in 8 seconds")
            self.assertEqual(got["cpm_30000"], "",
                             "nor can a 30000s one")

    def test_the_file_is_chronological_under_plain_sort(self):
        d, files, out = self.run_service()
        if not files:
            self.skipTest("no counter attached")
        with open(files[0]) as f:
            lines = [l.rstrip("\n") for l in f if not l.startswith("#")]
        self.assertEqual(lines, sorted(lines),
                         "the one promise the format makes")

    def test_a_backfill_of_the_same_image_writes_the_same_file(self):
        """End to end, through both command lines.

        Not a function against a function: the actual `backfill --image`
        command in each program, folding a real 16 KiB flash image into a
        fresh log, and the two files compared byte for byte. Everything is in
        this one -- the decoder, the measured sample intervals, four averaging
        windows replayed sample by sample, the peaks, the gap handling, the
        slot arithmetic, the row formatting and the merge.
        """
        import tempfile
        image = os.path.join(ROOT, "tests", "fixtures", "flash-gmc320re.bin")
        if not os.path.exists(image):
            self.skipTest("no flash fixture in the repository")
        d = tempfile.mkdtemp()
        py = os.path.join(d, "py.tsv")
        rs = os.path.join(d, "rs.tsv")
        args = ["backfill", "--image", image, "--serial", "F48824B8207F7E"]
        a = subprocess.run([sys.executable, os.path.join(ROOT, "radbeeper")]
                           + args + ["-o", py],
                           capture_output=True, text=True, timeout=120)
        b = subprocess.run([self.BINARY] + args + ["-o", rs],
                           capture_output=True, text=True, timeout=120)
        self.assertEqual(a.returncode, 0, a.stderr)
        self.assertEqual(b.returncode, 0, b.stderr)
        with open(py) as f:
            want = f.read()
        with open(rs) as f:
            got = f.read()
        self.assertGreater(len(want.splitlines()), 100, "too few rows to test")
        self.assertEqual(got, want)

    def test_a_backfill_will_not_guess_which_counter_an_image_came_from(self):
        # A dumped image carries no serial, and rows that cannot say which
        # counter they came from are half a measurement. Both refuse.
        import tempfile
        image = os.path.join(ROOT, "tests", "fixtures", "flash-gmc320re.bin")
        if not os.path.exists(image):
            self.skipTest("no flash fixture in the repository")
        d = tempfile.mkdtemp()
        for cmd in ([sys.executable, os.path.join(ROOT, "radbeeper")],
                    [self.BINARY]):
            out = subprocess.run(
                cmd + ["backfill", "--image", image, "-o",
                       os.path.join(d, "x.tsv")],
                capture_output=True, text=True, timeout=120)
            self.assertNotEqual(out.returncode, 0, "%r accepted it" % cmd[-1])
            self.assertIn("--serial", out.stdout + out.stderr)

    def test_a_second_run_does_not_write_a_row_for_a_finished_slot(self):
        # One row per slot is the rule the whole format rests on, and a
        # service coming back mid-interval is exactly how it gets broken.
        d, files, out = self.run_service(seconds=6, every=2)
        if not files:
            self.skipTest("no counter attached")
        with open(files[0]) as f:
            before = f.read().splitlines()
        subprocess.run(
            [self.BINARY, "service", "--logs", d, "--log-every", "2",
             "--duration", "4", "--no-backfill"],
            capture_output=True, text=True, timeout=90)
        with open(files[0]) as f:
            after = f.read().splitlines()
        slots = [radbeeper.slot_of(radbeeper.row_time(l + "\t"), 2.0)
                 for l in after if not l.startswith("#")]
        self.assertEqual(len(slots), len(set(slots)), "a slot was written twice")
        self.assertGreaterEqual(len(after), len(before))


class TestTheTwoMonitorsDrawTheSameScreen(unittest.TestCase):
    """The panels line up row for row, or one of them has drifted.

    THE BUG THIS EXISTS FOR. The Rust monitor drew the counts chart, the
    random line and the spectrum footer each one row higher than the Python
    did, for the whole of the port. `row += 2` after the now/run pair where
    the Python's arithmetic lands on `row += 3` -- one statement, invisible in
    review, invisible in both suites, and invisible on screen unless somebody
    puts the two terminals side by side, which nobody does.

    Values cannot be compared here: both monitors are reading a live stream a
    beat apart, so the 3-second window legitimately differs between two runs
    of the same fixture. WHERE each label lands cannot differ, and that is
    what is asserted.
    """

    BINARY = os.path.join(ROOT, "target", "release", "radbeeper")
    RECORD = os.path.join(ROOT, "tools", "record.py")
    ANCHORS = ("now", "run", "random", "clock", "spectrum")

    @classmethod
    def setUpClass(cls):
        if not os.path.exists(cls.BINARY):
            raise unittest.SkipTest("no release binary to compare against")
        if not os.path.exists(cls.RECORD):
            raise unittest.SkipTest("no recorder")

    def screen(self, argv, seconds=12):
        """One monitor, through a pty, as the text on screen at the end."""
        import tempfile
        sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
        from fake_gmc import FakeGMC
        cast = os.path.join(tempfile.mkdtemp(), "m.cast")
        dev = FakeGMC(cpm=1800.0, seed=5)
        dev.start()
        try:
            subprocess.run(
                [sys.executable, self.RECORD, "capture", cast,
                 "--cols", "160", "--rows", "30", "--seconds", str(seconds),
                 "--"] + argv + ["-d", dev.path, "watch"],
                cwd=ROOT, capture_output=True, timeout=seconds + 60)
        finally:
            dev.stop()
        out = subprocess.run(
            [sys.executable, self.RECORD, "text", cast,
             "--at", str(seconds - 2)],
            cwd=ROOT, capture_output=True, text=True, timeout=60)
        return out.stdout.splitlines()

    def rows_of(self, lines):
        """Which row each anchor label and each window landed on."""
        where = {}
        for i, line in enumerate(lines):
            head = line.strip().split(" ")[0] if line.strip() else ""
            if head in self.ANCHORS and head not in where:
                where[head] = i
            if head.endswith("s") and head[:-1].isdigit():
                where.setdefault("span " + head, i)
        return where

    def test_every_row_of_the_panel_is_in_the_same_place(self):
        mine = self.rows_of(self.screen([sys.executable,
                                         os.path.join(ROOT, "radbeeper")]))
        # --no-log: the table of log rows and the logging behind it are the
        # native monitor's alone, and without them it draws the Python's
        # screen exactly -- which is the thing this compares.
        theirs = self.rows_of(self.screen([self.BINARY, "--no-log"]))
        if not mine or not theirs:
            self.skipTest("neither monitor drew anything")
        # Both have to have drawn the whole panel, or the comparison passes
        # by drawing nothing and proves the opposite of what it claims.
        for label in self.ANCHORS:
            self.assertIn(label, mine, "the Python drew no %r row" % label)
            self.assertIn(label, theirs, "the Rust drew no %r row" % label)
        self.assertEqual(mine, theirs,
                         "the two monitors put the same rows in different "
                         "places:\n  python %r\n  rust   %r" % (mine, theirs))


class TestTheRustProbeAfterAKilledSession(unittest.TestCase):
    """A counter still streaming heartbeats is still answered in step.

    `watch` turns the stream off when it exits cleanly. Killed, it does not,
    and the counter goes on sending two bytes a second. The probe that ran
    next asked for the clock between two of them and printed 20128-00-26 --
    a year of 128 is the heartbeat's status bit, read as the answer.
    """

    BINARY = os.path.join(ROOT, "target", "release", "radbeeper")

    @classmethod
    def setUpClass(cls):
        if not os.path.exists(cls.BINARY):
            raise unittest.SkipTest("no release binary to run")

    def test_probe_reads_the_clock_through_a_running_stream(self):
        import time
        sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
        from fake_gmc import FakeGMC
        dev = FakeGMC(cpm=600.0, seed=7)
        dev.heartbeat = True
        dev.interleave = True
        dev.start()
        try:
            time.sleep(0.2)
            out = subprocess.run([self.BINARY, "-d", dev.path, "probe"],
                                 capture_output=True, text=True, timeout=30)
        finally:
            dev.stop()
        self.assertEqual(out.returncode, 0, out.stderr)
        clock = [l for l in out.stdout.splitlines()
                 if l.startswith("its clock")]
        self.assertEqual(len(clock), 1, out.stdout)
        self.assertIn(time.strftime(" %Y-%m-"), clock[0])
        self.assertIn("reading    600 CPM", out.stdout)


class TestTheRustWaitsForABusyPort(unittest.TestCase):
    """`--wait` is for the handover: stop the logger, and the monitor takes it.

    The lock is an flock, and an flock belongs to a process -- so no terminal
    multiplexer, no GUI session and no fresh login gets a second reader onto
    the port, and there is no handover request to send either. The holder has
    to let go. `--wait` is what stands there until it does, instead of making
    a person race `rc-service radbeeper stop` from a second window.
    """

    BINARY = os.path.join(ROOT, "target", "release", "radbeeper")

    @classmethod
    def setUpClass(cls):
        if not os.path.exists(cls.BINARY):
            raise unittest.SkipTest("no release binary to run")

    def held_counter(self):
        """A fake counter whose port is already locked, as the service holds it."""
        import fcntl
        sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
        from fake_gmc import FakeGMC
        dev = FakeGMC(cpm=600.0, seed=13, tick=0.05)
        dev.start()
        time.sleep(0.2)
        fd = os.open(dev.path, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        return dev, fd

    def test_it_takes_the_port_when_the_holder_lets_go(self):
        import threading
        dev, fd = self.held_counter()
        threading.Timer(2.0, lambda: os.close(fd)).start()
        started = time.time()
        try:
            out = subprocess.run(
                [self.BINARY, "-d", dev.path, "--wait", "30", "probe"],
                capture_output=True, text=True, timeout=90)
        finally:
            dev.stop()
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertIn("waiting", out.stdout)
        self.assertIn("GMC-320", out.stdout)
        # It waited rather than failing, and it did not somehow succeed
        # before the lock was let go.
        self.assertGreaterEqual(time.time() - started, 2.0)

    def test_a_wait_that_runs_out_still_names_the_command_that_frees_it(self):
        # The detail is the part that says `doas rc-service radbeeper stop`,
        # and a wait that just timed out is exactly when it is worth reading.
        dev, fd = self.held_counter()
        try:
            out = subprocess.run(
                [self.BINARY, "-d", dev.path, "--wait", "2", "probe"],
                capture_output=True, text=True, timeout=90)
        finally:
            os.close(fd)
            dev.stop()
        self.assertEqual(out.returncode, 1)
        self.assertIn("Waited 2s", out.stderr)
        self.assertIn("rc-service radbeeper stop", out.stderr)

    def test_a_missing_device_is_not_waited_on(self):
        # Waiting fixes a locked port and nothing else. A counter that is not
        # there will not turn up because a program asked a second time, and a
        # --wait that sat on a misspelt device would look exactly like a hang.
        started = time.time()
        out = subprocess.run(
            [self.BINARY, "-d", "/dev/definitely-not-here", "--wait", "60",
             "probe"], capture_output=True, text=True, timeout=30)
        self.assertEqual(out.returncode, 1)
        self.assertLess(time.time() - started, 10.0)
        self.assertNotIn("waiting", out.stdout)


class TestTheRustServiceBackfillsAtStart(unittest.TestCase):
    """The boot service reads the counter's flash before it logs anything.

    The Python service always did; the Rust one printed "backfill is not in
    this build yet" and started logging, so a machine switched to the native
    build lost every hour the counter recorded while the machine was off.
    """

    BINARY = os.path.join(ROOT, "target", "release", "radbeeper")

    @classmethod
    def setUpClass(cls):
        if not os.path.exists(cls.BINARY):
            raise unittest.SkipTest("no release binary to run")

    def run_service(self, *extra):
        import tempfile
        sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
        from fake_gmc import FakeGMC, build_history
        d = tempfile.mkdtemp()
        dev = FakeGMC(cpm=600.0, seed=9, tick=0.05,
                      history=build_history(seconds=600, cpm=200.0,
                                            size=64 * 1024))
        dev.start()
        try:
            out = subprocess.run(
                [self.BINARY, "-d", dev.path, "service", "--logs", d,
                 "--duration", "1"] + list(extra),
                capture_output=True, text=True, timeout=120)
        finally:
            dev.stop()
        rows = []
        for n in os.listdir(d):
            if n.endswith(".tsv") and n.startswith("cpm-"):
                with open(os.path.join(d, n)) as f:
                    rows += [l for l in f.read().splitlines()
                             if l and not l.startswith("#")]
        return out, rows

    def test_the_flash_becomes_rows_before_the_live_log_starts(self):
        out, rows = self.run_service()
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertIn("radbeeper: backfill --", out.stdout)
        self.assertTrue([r for r in rows if r.split("\t")[-2] == "flash"],
                        "no flash rows written:\n" + out.stdout)

    def test_no_backfill_skips_it(self):
        out, rows = self.run_service("--no-backfill")
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertNotIn("backfill", out.stdout)
        self.assertFalse([r for r in rows if r.split("\t")[-2] == "flash"])


class TestTheCountersClock(unittest.TestCase):
    """How far the counter's clock is out, to the hundredth, and setting it.

    `probe` used to print the counter's time beside this machine's as a unix
    number and leave the subtraction to the reader -- and a reading in whole
    seconds is only good to a second however carefully it is subtracted.
    """

    BINARY = os.path.join(ROOT, "target", "release", "radbeeper")

    @classmethod
    def setUpClass(cls):
        if not os.path.exists(cls.BINARY):
            raise unittest.SkipTest("no release binary to run")

    def device(self, ahead):
        sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
        from fake_gmc import FakeGMC
        dev = FakeGMC(cpm=600.0, seed=3)
        dev.clock_ahead = ahead
        dev.start()
        self.addCleanup(dev.stop)
        return dev

    def run_native(self, dev, *argv):
        out = subprocess.run([self.BINARY, "-d", dev.path] + list(argv),
                             capture_output=True, text=True, timeout=60)
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
        return out.stdout

    def test_probe_says_how_far_ahead_to_the_tenth(self):
        out = self.run_native(self.device(111.4), "probe")
        self.assertIn("111.4 s ahead of this machine", out)
        self.assertIn("radbeeper clock --set", out)

    def test_probe_says_behind_when_it_is_behind(self):
        out = self.run_native(self.device(-2180.25), "probe")
        self.assertRegex(out, r"2180\.[123] s behind this machine")

    def test_set_brings_it_within_a_tenth_and_says_so(self):
        dev = self.device(-500.0)
        out = self.run_native(dev, "clock", "--set")
        self.assertAlmostEqual(dev.clock_ahead, 0.0, delta=0.1)
        self.assertIn("matches this machine", out.split("set ", 1)[1])
        self.assertIn("carries the old clock, 500 s", out)

    def test_the_python_measures_the_same_offset(self):
        dev = self.device(37.25)
        c = radbeeper.identify(dev.path, baud=115200)
        try:
            self.assertAlmostEqual(radbeeper.measure_clock_offset(c), -37.25,
                                   delta=0.1)
        finally:
            c.close()


class TestTheMonitorLogsWhileItIsOpen(unittest.TestCase):
    """`watch` is the service with a screen: backfill, rows, random, a table.

    The monitor and the logger cannot both hold the port, so while the
    monitor was open nothing was written at all -- the log had a hole exactly
    where somebody was watching. Now the monitor backfills at connect, writes
    the same rows the service does, appends every random line, and shows the
    log's newest rows scrolling up at the bottom of the screen.
    """

    BINARY = os.path.join(ROOT, "target", "release", "radbeeper")
    RECORD = os.path.join(ROOT, "tools", "record.py")

    @classmethod
    def setUpClass(cls):
        if not os.path.exists(cls.BINARY):
            raise unittest.SkipTest("no release binary to run")

    def run_monitor(self, *extra, rows=46, seconds=45):
        import tempfile
        sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
        from fake_gmc import FakeGMC, build_history
        tmp = tempfile.mkdtemp()
        logs = os.path.join(tmp, "logs")
        cast = os.path.join(tmp, "m.cast")
        dev = FakeGMC(cpm=600.0, seed=5, tick=0.2,
                      history=build_history(seconds=600, cpm=200.0,
                                            size=64 * 1024))
        dev.start()
        try:
            subprocess.run(
                [sys.executable, self.RECORD, "capture", cast,
                 "--cols", "160", "--rows", str(rows), "--seconds", str(seconds),
                 "--", self.BINARY, "-d", dev.path, "--logs", logs,
                 "--log-every", "2", "--backfill-bytes", "2048"]
                + list(extra) + ["watch"],
                cwd=ROOT, capture_output=True, timeout=seconds + 120)
        finally:
            dev.stop()
        text = subprocess.run(
            [sys.executable, self.RECORD, "text", cast, "--at", str(seconds - 1)],
            cwd=ROOT, capture_output=True, text=True, timeout=60).stdout
        return logs, text.splitlines()

    def test_backfill_rows_and_the_table(self):
        logs, screen = self.run_monitor()
        names = sorted(os.listdir(logs))
        tsv = [n for n in names if n.startswith("cpm-")]
        self.assertEqual(len(tsv), 1, names)
        # Exported after the backfill and again on quit, beside the log.
        self.assertIn("index.html", names)
        with open(os.path.join(logs, tsv[0])) as f:
            body = [l.split("\t") for l in f.read().splitlines()
                    if l and not l.startswith("#")]
        self.assertTrue([r for r in body if r[-2] == "flash"],
                        "the backfill at connect wrote nothing")
        live = [r for r in body if r[-2] == "live"]
        self.assertTrue(live, "the monitor logged no rows")
        heads = [i for i, l in enumerate(screen) if l.startswith("#time")]
        self.assertEqual(len(heads), 1, "\n".join(screen))
        table = [l for l in screen[heads[0] + 1:]
                 if l[:4].isdigit()]
        # Every row on screen is a row on disk -- the table is the log, not
        # a second opinion of it -- and the live ones are among them. Not
        # "the newest row on disk is on screen": the frame and the write
        # are a beat apart, and which lands first is not the point.
        on_disk = {r[0] for r in body}
        self.assertTrue(table, "\n".join(screen))
        self.assertTrue(all(l.split()[0] in on_disk for l in table),
                        "\n".join(screen))
        self.assertTrue(any(l.split()[0] in {r[0] for r in live} for l in table),
                        "\n".join(screen))
        self.assertTrue(screen[-1].startswith("q to quit"))
        self.assertTrue(screen[-2].startswith("spectrum"))

    def test_the_table_fits_a_thirty_row_screen_under_the_clock(self):
        _logs, screen = self.run_monitor(rows=30)
        self.assertEqual(len(screen), 30, "\n".join(screen))
        clock = [i for i, l in enumerate(screen) if l.startswith("clock")]
        head = [i for i, l in enumerate(screen) if l.startswith("#time")]
        self.assertEqual((len(clock), len(head)), (1, 1), "\n".join(screen))
        self.assertEqual(head[0], clock[0] + 1)
        self.assertEqual(head[0], 30 - 2 - 6)

    def test_no_log_writes_nothing_and_draws_no_table(self):
        logs, screen = self.run_monitor("--no-log")
        self.assertFalse(os.path.exists(logs) and os.listdir(logs))
        self.assertFalse([l for l in screen if l.startswith("#time")])


def build_binary():
    """The release binary, rebuilt whenever there is a cargo to rebuild it.

    For the oracle's reason: a stale binary passes, silently, against a Rust
    that has since changed.
    """
    binary = os.path.join(ROOT, "target", "release", "radbeeper")
    if any(os.access(os.path.join(p, "cargo"), os.X_OK)
           for p in os.environ.get("PATH", "").split(os.pathsep)):
        try:
            subprocess.run(["cargo", "build", "--release"], cwd=ROOT,
                           check=True, stdout=subprocess.DEVNULL,
                           stderr=subprocess.DEVNULL, timeout=300)
        except (subprocess.SubprocessError, OSError):
            return None
    return binary if os.path.exists(binary) else None


class TestTheTwoSiteGeneratorsWriteTheSamePages(unittest.TestCase):
    """`radbeeper pages` and tools/landing.py agree, byte for byte.

    TWO GENERATORS FOR ONE SITE, AND THE SAME ARGUMENT AS THE LOG FORMAT. The
    Python one exists so the GitHub Action can rebuild the site with no
    toolchain at all -- stdlib only, nothing to install -- and the Rust one
    exists so `make release` is one binary and one command. Two dialects of a
    site is the failure this comparison prevents: a landing page that said one
    thing when a bot built it and another when a person did.

    The documents are the repository's own, so this also catches a markdown
    construct somebody uses for the first time that only one of the two
    renderers understands.
    """

    BINARY = os.path.join(ROOT, "target", "release", "radbeeper")

    def build_both(self):
        import shutil
        import tempfile
        out = []
        for i, how in enumerate(("python", "rust")):
            d = tempfile.mkdtemp()
            self.addCleanup(shutil.rmtree, d, ignore_errors=True)
            shutil.copy(os.path.join(ROOT, "README.md"), d)
            shutil.copy(os.path.join(ROOT, "Cargo.toml"), d)
            shutil.copytree(os.path.join(ROOT, "docs"), os.path.join(d, "docs"),
                            ignore=shutil.ignore_patterns("screenshots", "*.html"))
            if how == "python":
                r = subprocess.run(
                    [sys.executable, os.path.join(ROOT, "tools", "landing.py"),
                     "--quiet"],
                    cwd=d, capture_output=True, text=True, timeout=120)
            else:
                r = subprocess.run([self.BINARY, "pages", "-o", d],
                                   cwd=d, capture_output=True, text=True,
                                   timeout=120)
            self.assertEqual(r.returncode, 0, "%s: %s" % (how, r.stderr))
            out.append(d)
        return out

    def test_every_page_is_the_same_bytes(self):
        if not os.path.exists(self.BINARY):
            self.skipTest("no Rust build")
        py, rs = self.build_both()
        names = set()
        for base in (py, rs):
            for root, _dirs, files in os.walk(base):
                for f in files:
                    if f.endswith((".html", ".xml", ".txt")) or f == ".nojekyll":
                        names.add(os.path.relpath(os.path.join(root, f), base))
        self.assertTrue(names, "neither generator wrote anything")
        # The landing page, every lab report, the docs index, the archive, the
        # sitemap and robots.txt -- all of it, or the comparison is worth less
        # than it looks.
        self.assertIn("index.html", names)
        self.assertIn(os.path.join("docs", "the-log.html"), names)
        self.assertIn("sitemap.xml", names)
        for name in sorted(names):
            a = os.path.join(py, name)
            b = os.path.join(rs, name)
            self.assertTrue(os.path.exists(a), "only the Rust wrote %s" % name)
            self.assertTrue(os.path.exists(b), "only the Python wrote %s" % name)
            with open(a, "rb") as f:
                want = f.read()
            with open(b, "rb") as f:
                got = f.read()
            if want == got:
                continue
            at = 0
            while at < min(len(want), len(got)) and want[at] == got[at]:
                at += 1
            self.fail("%s differs at byte %d of %d:\n  python %r\n  rust   %r"
                      % (name, at, len(want),
                         want[max(0, at - 80):at + 80],
                         got[max(0, at - 80):at + 80]))


class TestTheTwoExportsWriteTheSamePages(unittest.TestCase):
    """index.html and random.html, from both, compared byte for byte.

    The pages are what a fork publishes and what the workflow commits back on
    every push, so a page that depends on which binary built it is a diff in
    every one of those commits. Both are run on the same logs, each into its
    own directory, and the files -- and the lines printed about them -- must
    be the same characters.

    THE ONE HOOK. Both pages print when they were generated, so both
    implementations honour SOURCE_DATE_EPOCH, and this sets it. TZ is set too,
    to a zone with summer time, so the day boundaries and the midnight ticks
    are worked out in local time across a clock change rather than in UTC,
    where a mistake in the conversion would hide.
    """

    EPOCH = "1789000000"
    TZ = "EST5EDT,M3.2.0,M11.1.0"

    @classmethod
    def setUpClass(cls):
        cls.BINARY = build_binary()
        if cls.BINARY is None:
            raise unittest.SkipTest("no release binary to compare against")

    def tempdir(self):
        import shutil
        import tempfile
        d = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, d, True)
        return d

    def run_both(self, logs, before=(), after=(), output="index.html",
                 shots=()):
        """[(directory, stdout)] for the Python, then the Rust.

        `before` goes ahead of the verb and `after` behind it, which is where
        the Python's argparse wants its global and its export options.
        """
        env = dict(os.environ, SOURCE_DATE_EPOCH=self.EPOCH, TZ=self.TZ)
        results = []
        for argv in ([sys.executable, os.path.join(ROOT, "radbeeper")],
                     [self.BINARY]):
            d = self.tempdir()
            page_dir = os.path.join(d, os.path.dirname(output))
            os.makedirs(page_dir, exist_ok=True)
            # Screenshots are linked only when they exist beside the page.
            for name in shots:
                path = os.path.join(page_dir, "docs", "screenshots", name)
                os.makedirs(os.path.dirname(path), exist_ok=True)
                open(path, "w").close()
            out = subprocess.run(
                list(argv) + list(before)
                + ["export", "--logs", logs, "-o", output] + list(after),
                cwd=d, env=env, capture_output=True, text=True, timeout=120)
            self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
            # The audit page is printed as an absolute path, and each run has
            # its own directory; that directory is the only difference allowed.
            printed = out.stdout.replace(os.path.realpath(d), "<dir>")
            results.append((d, printed.replace(d, "<dir>")))
        return results

    def same_files(self, results, names):
        (py, py_out), (rs, rs_out) = results
        self.assertEqual(rs_out, py_out)
        for name in names:
            with open(os.path.join(py, name), "rb") as f:
                want = f.read()
            with open(os.path.join(rs, name), "rb") as f:
                got = f.read()
            self.assertTrue(want, "%s is empty" % name)
            if got != want:
                at = next((i for i, (a, b) in enumerate(zip(want, got))
                           if a != b), min(len(want), len(got)))
                self.fail("%s differs at byte %d of %d:\n  python %r\n"
                          "  rust   %r" % (name, at, len(want),
                                           want[max(0, at - 80):at + 80],
                                           got[max(0, at - 80):at + 80]))

    def synthetic_logs(self):
        """Two counters, gaps, empty windows, a move, a stale emission."""
        import time
        d = self.tempdir()
        saved = os.environ.get("TZ")
        os.environ["TZ"] = self.TZ
        time.tzset()
        try:
            self.write_synthetic(d)
        finally:
            if saved is None:
                os.environ.pop("TZ", None)
            else:
                os.environ["TZ"] = saved
            time.tzset()
        return d

    def write_synthetic(self, d):
        import time

        def stamp(t):
            return time.strftime("%Y-%m-%dT%H:%M:%S", time.localtime(t))

        def row(*cells):
            return "\t".join(cells) + "\n"

        head = radbeeper.log_header((3, 30, 300, 3000)) + "\n"
        # 2026-11-01 01:06 EDT: the night the clocks go back, so one local
        # day is 25 hours long. Rows every 30 s with a two-hour hole in the
        # middle, the long window empty for the first twenty minutes, a
        # stretch with no peak at all, short rows, and a move part way.
        t0 = 1793509560.0
        with open(os.path.join(d, "cpm-A1B2-2026-11.tsv"), "w") as f:
            f.write(head)
            for i in range(700):
                if 200 <= i < 440:
                    continue
                f.write(row(
                    stamp(t0 + i * 30), "%.3f" % (0.6 + (i % 5) * 0.05),
                    str(18 + i % 9), "27.15" if i % 11 == 0 else "30",
                    "%.1f" % (20 + i % 60), "%.1f" % (36 + i % 4),
                    "%.1f" % (38 + i % 3),
                    "" if i < 40 else "%.1f" % (35 + i % 7),
                    "" if 60 <= i < 120 else "%.1f" % (40 + (i * 37) % 900),
                    "", "%.1f" % (52 + i % 5), "39.2",
                    "flash" if i < 300 else "live",
                    "" if i < 500 else "The garage & <shed>"))
        # The same counter a month earlier: a second file to link, a line
        # that is not a row, a blank, a row whose counts are not a number,
        # and a short row with nothing past the first window.
        with open(os.path.join(d, "cpm-A1B2-2026-10.tsv"), "w") as f:
            f.write(head)
            f.write(row(stamp(1792000000), "0.5", "15", "30", "30.0", "", "",
                        "", "90.0", "", "", "", "live", ""))
            f.write("not a row at all\n\n")
            f.write(row(stamp(1792000030), "0.5", "many", "30"))
            f.write(row(stamp(1792000060), "1.0", "30", "30.5", "60.0"))
        # A second counter: a few rows two hours apart, so every point is
        # its own segment, and no site recorded for it.
        with open(os.path.join(d, "cpm-Z9-2026-11.tsv"), "w") as f:
            f.write(head)
            for i in range(5):
                f.write(row(stamp(t0 + 172800 + i * 7200), "0.7", str(21 + i),
                            "30", "", "", "", "", "", "", "", "", "live", ""))
        with open(os.path.join(d, "sites.tsv"), "w") as f:
            f.write("#serial\tfrom\tname\n")
            f.write("A1B2\t%s\tThe bench\n" % stamp(1780000000))
            f.write("A1B2\t%s\tThe garage & <shed>\n" % stamp(t0 + 15000))
        # Emissions: a stale 1969 pool, one with a tie for the most common
        # count, one with a tail and marked not flat; and an emission log for
        # the second counter with nothing in it, which is not a page.
        pools = [("1969-12-31T21:01:12", "0102" * 40),
                 (stamp(t0), "1100" * 60 + "53"),
                 (stamp(t0 + 900), "0" * 50 + "1" * 50 + "2" * 30 + "f7a3")]
        with open(os.path.join(d, "random-A1B2.tsv"), "w") as f:
            f.write("#seq\ttime\tseconds\trate\tbits\tflat\thex\tcounts\n")
            for seq, (when, counts) in enumerate(pools):
                rate = sum(int(c, 16) for c in counts) / float(len(counts))
                f.write("%d\t%s\t%d\t%.4f\t256.0\t%s\t%s\t%s\n"
                        % (seq, when, len(counts), rate,
                           "no" if seq == 2 else "yes", "ab" * 32, counts))
        with open(os.path.join(d, "random-Z9.tsv"), "w") as f:
            f.write("#seq\ttime\tseconds\trate\tbits\tflat\thex\tcounts\n")

    def test_the_repositorys_own_logs(self):
        logs = os.path.join(ROOT, "logs")
        if not os.path.exists(os.path.join(logs, "random-F48824B8207F7E.tsv")):
            self.skipTest("no logs in the repository")
        # With the screenshots beside the page, as at the repository root,
        # so the figures are compared too.
        shots = ["probe.png", "watch.png", "watch-filling.png",
                 "watch-spectrum.png", "log-output.png", "log-tabs.png",
                 "watch-20s.gif"]
        results = self.run_both(logs, shots=shots)
        # THE JOINED TABLE IS COMPARED TOO. It is a published artefact -- the
        # page links it, and it is what a reader who does not want to run
        # javascript reads instead -- so a column that depends on which binary
        # built it is the same defect as a page that does.
        self.same_files(results, ["index.html", "random.html",
                                  "frames-F48824B8207F7E.tsv"])
        with open(os.path.join(results[0][0], "index.html")) as f:
            self.assertIn("docs/screenshots/watch-20s.gif", f.read())

    def test_a_synthetic_log_directory(self):
        results = self.run_both(self.synthetic_logs())
        self.same_files(results, ["index.html", "random.html",
                                  "frames-A1B2.tsv"])
        with open(os.path.join(results[0][0], "index.html")) as f:
            page = f.read()
        # The comparison is only worth something if the cases are on it.
        self.assertIn("Counter Z9", page)
        self.assertIn("The garage &amp; &lt;shed&gt;", page)
        self.assertEqual(page.count('<path class="mean"'), 2)

    def test_a_page_in_a_subdirectory_with_its_own_options(self):
        results = self.run_both(
            self.synthetic_logs(), before=["--cpm-per-usvh", "108"],
            after=["--title", "Bench \"B\" & desk",
                   "--random-output", "audit.html"],
            output="site/page.html", shots=["probe.png"])
        self.same_files(results, ["site/page.html", "audit.html",
                                  "site/frames-A1B2.tsv"])

    def test_no_random_page(self):
        results = self.run_both(self.synthetic_logs(),
                                after=["--no-random-page"])
        self.same_files(results, ["index.html"])
        for d, _out in results:
            self.assertFalse(os.path.exists(os.path.join(d, "random.html")))

    def test_no_logs_at_all(self):
        results = self.run_both(self.tempdir())
        self.same_files(results, ["index.html"])


class TestABackfillWritesNothingFromTheFuture(unittest.TestCase):
    """The newest end of a wrapped ring is only known to a timestamp.

    The tail search stops at the first old mark after the newest data, so the
    old samples between the write pointer and that mark were read as new and
    placed after the newest mark -- minutes past the present, in the slots
    the live logger was about to write. Both implementations now drop every
    sample from the current slot on.
    """

    BINARY = os.path.join(ROOT, "target", "release", "radbeeper")

    def image(self):
        import time
        sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
        from fake_gmc import build_history
        # A mark two minutes ago with five minutes of samples after it.
        t = time.localtime(time.time() - 120)
        return build_history(seconds=420, cpm=120.0, per_mark=400,
                             size=4096, start=(t.tm_year - 2000, t.tm_mon,
                                               t.tm_mday, t.tm_hour,
                                               t.tm_min, t.tm_sec))

    def rows_after(self, d, cut):
        out = []
        for n in os.listdir(d):
            if n.startswith("cpm-"):
                with open(os.path.join(d, n)) as f:
                    for line in f:
                        if line[:1].isdigit():
                            w = radbeeper.row_time(line)
                            if w is not None and w >= cut:
                                out.append(line)
        return out

    def test_the_python_writes_no_row_for_now_or_later(self):
        import argparse
        import tempfile
        import time
        cut = (int(time.time()) // 30) * 30
        d = tempfile.mkdtemp()
        path = os.path.join(d, "cpm-FUTURE1-now.tsv")
        args = argparse.Namespace(spans=[3.0, 30.0, 300.0, 3000.0, 30000.0],
                                  log_every=30.0, max_gap=10.0)
        report = radbeeper.backfill(self.image(), args, 0.0, path=path)
        self.assertGreater(report["rows"], 0)
        self.assertEqual(self.rows_after(d, cut), [])

    def test_the_rust_writes_no_row_for_now_or_later(self):
        import tempfile
        import time
        if not os.path.exists(self.BINARY):
            self.skipTest("no release binary to run")
        img = os.path.join(tempfile.mkdtemp(), "h.bin")
        with open(img, "wb") as f:
            f.write(self.image())
        cut = (int(time.time()) // 30) * 30
        d = tempfile.mkdtemp()
        out = subprocess.run(
            [self.BINARY, "backfill", "--logs", d, "--image", img,
             "--serial", "FUTURE1"],
            capture_output=True, text=True, timeout=120)
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
        self.assertTrue(os.listdir(d), "nothing was backfilled:\n" + out.stdout)
        self.assertEqual(self.rows_after(d, cut), [])


class TestThePageReadsColumnsByName(unittest.TestCase):
    """The latest-rows table, the peak card and the chart, from a 5-window log.

    The page read cells by position -- counts at 2, peak at 7, source at 10,
    site at 11 -- which is the three-window layout. A five-window row put
    cpm_3000 under "Peak 3s", peak_300 under "Source" and peak_3000 under
    "Site", in both implementations, which agreed with each other perfectly.
    """

    BINARY = os.path.join(ROOT, "target", "release", "radbeeper")
    HEADER = ("#time\tcps\tcounts\tseconds\tcpm_3\tcpm_30\tcpm_300\tcpm_3000"
              "\tcpm_30000\tpeak_3\tpeak_30\tpeak_300\tpeak_3000\tpeak_30000"
              "\tsrc\tsite")
    ROW = ("2026-09-16T18:01:46\t2.367\t71\t30\t180.0\t142.0\t101.1\t116.8"
           "\t60.2\t420.0\t148.0\t123.6\t120.1\t60.3\tlive\tThe bench")
    WANT = ("<tr><td>2026-09-16T18:01:46</td><td>2.367</td><td>180.0</td>"
            "<td>142.0</td><td>101.1</td><td>420.0</td><td>live</td>"
            "<td>The bench</td></tr>")

    def page(self, argv):
        import tempfile
        d = tempfile.mkdtemp()
        with open(os.path.join(d, "cpm-NAMES1-2026-09.tsv"), "w") as f:
            f.write(self.HEADER + "\n" + self.ROW + "\n")
        out = os.path.join(d, "index.html")
        run = subprocess.run(argv + ["export", "--logs", d, "-o", out,
                                     "--no-random-page"],
                             capture_output=True, text=True, timeout=120)
        self.assertEqual(run.returncode, 0, run.stdout + run.stderr)
        with open(out) as f:
            return f.read()

    def test_the_python_page(self):
        self.assertIn(self.WANT, self.page([sys.executable,
                                            os.path.join(ROOT, "radbeeper")]))

    def test_the_rust_page(self):
        if not os.path.exists(self.BINARY):
            self.skipTest("no release binary to run")
        self.assertIn(self.WANT, self.page([self.BINARY]))


class TestAShortReplyIsNotTheEndOfTheFlash(unittest.TestCase):
    """A <SPIR>> reply that loses bytes is asked again, not taken as the end.

    One short reply ended the whole read: a monitor's backfill got the oldest
    7 KiB of the 64 it asked for, added nothing, and left a thirteen-minute
    hole the counter had recorded.
    """

    BINARY = os.path.join(ROOT, "target", "release", "radbeeper")

    def test_log_pull_gets_every_byte_through_two_short_replies(self):
        import tempfile
        if not os.path.exists(self.BINARY):
            self.skipTest("no release binary to run")
        sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
        from fake_gmc import FakeGMC, build_history
        size = 16 * 1024
        dev = FakeGMC(cpm=60.0, seed=4,
                      history=build_history(seconds=3000, size=size))
        dev.start()
        try:
            stem = os.path.join(tempfile.mkdtemp(), "h")
            dev.short_spir = 2
            out = subprocess.run(
                [self.BINARY, "-d", dev.path, "log", "pull", "-o", stem,
                 "--bytes", str(size)],
                capture_output=True, text=True, timeout=180)
        finally:
            dev.stop()
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
        with open(stem + ".bin", "rb") as f:
            self.assertEqual(f.read(), dev.history[:size])

    def test_the_python_reader_gets_every_byte_through_two_short_replies(self):
        sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
        from fake_gmc import FakeGMC, build_history
        size = 16 * 1024
        dev = FakeGMC(cpm=60.0, seed=4,
                      history=build_history(seconds=3000, size=size))
        dev.start()
        try:
            c = radbeeper.identify(dev.path, baud=115200)
            dev.short_spir = 2
            got = c.read_history(0, size)
            c.close()
        finally:
            dev.stop()
        self.assertEqual(got, dev.history[:size])
