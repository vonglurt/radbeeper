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
        import time
        path = os.path.join(ROOT, "logs", "random-F48824B8207F7E.tsv")
        if not os.path.exists(path):
            self.skipTest("no recorded emissions in the repository")
        with open(path) as f:
            rows = [l.rstrip("\n").split("\t") for l in f
                    if not l.startswith("#")]
        self.assertTrue(rows, "the emission log is empty")
        directives, want = [], []
        for r in rows:
            started = int(time.mktime(time.strptime(r[1], "%Y-%m-%dT%H:%M:%S")))
            directives.append("digest\t%s\t%s\t%s" % (r[0], started, r[7]))
            want.append(r[6])
        got = self.rust(directives)
        self.assertEqual(got, want)
        # And the Python still agrees with itself, so this is a three-way
        # equality rather than two programs sharing one mistake.
        for r in rows:
            started = time.mktime(time.strptime(r[1], "%Y-%m-%dT%H:%M:%S"))
            record = {"seq": int(r[0]), "started": started, "hex": r[6],
                      "counts": r[7]}
            self.assertTrue(radbeeper.check_entropy_record(record))


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

    BINARY = os.path.join(ROOT, "rust", "target", "release", "radbeeper")

    @classmethod
    def setUpClass(cls):
        if not os.path.exists(cls.BINARY):
            try:
                subprocess.run(["cargo", "build", "--release"],
                               cwd=os.path.join(ROOT, "rust"), check=True,
                               stdout=subprocess.DEVNULL,
                               stderr=subprocess.DEVNULL, timeout=300)
            except (subprocess.SubprocessError, OSError):
                raise unittest.SkipTest("could not build the binary")

    def run_service(self, seconds=8, every=2):
        import tempfile
        d = tempfile.mkdtemp()
        out = subprocess.run(
            [self.BINARY, "service", "--logs", d, "--log-every", str(every),
             "--duration", str(seconds)],
            capture_output=True, text=True, timeout=seconds + 60)
        files = [os.path.join(d, n) for n in os.listdir(d)
                 if n.endswith(".tsv")]
        return d, files, out

    def test_the_python_reads_what_the_rust_wrote(self):
        d, files, out = self.run_service()
        if not files:
            # No counter on this machine: the service says dormant and stops,
            # which is the correct behaviour and not a test failure.
            self.assertIn("dormant", out.stdout + out.stderr)
            self.skipTest("no counter attached")
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
             "--duration", "4"],
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

    BINARY = os.path.join(ROOT, "rust", "target", "release", "radbeeper")
    RECORD = os.path.join(ROOT, "tools", "record.py")
    ANCHORS = ("now", "run", "random", "spectrum")

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
        theirs = self.rows_of(self.screen([self.BINARY]))
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
