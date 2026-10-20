#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Paul Richeson
"""Take the pictures for docs/FEATURE_GUIDE.md, with no counter on the desk.

WHY A FAKE COUNTER AND NOT `--source sim`. The simulator replaces the device
and the wire, and is only in the Python. tests/fake_gmc.py is a GMC-320 on a
pseudo-terminal: the native build opens it, configures it with termios, reads
its heartbeat and downloads its flash exactly as it would a real one. So every
terminal picture here is the real `radbeeper` binary doing real work; only the
counts come from a seeded Poisson source instead of a tube.

WHAT IT WRITES. Everything the program writes goes into a fresh temporary
directory (`--logs logs`, relative to it), never into /var/lib/radbeeper, so a
service logging a real counter on the same machine is not touched. The only
output kept is docs/features/NN-name.png. The web pages are built from the
repository's own logs/ (the published record) and photographed with headless
Chromium.

Stdlib only, like everything in tools/. It drives tools/record.py, which needs
Pillow to render a still, exactly as `make promo` does.

    python3 tools/featureguide.py                  # about eleven minutes
    python3 tools/featureguide.py --monitor-seconds 120   # a quick look
"""
import argparse
import datetime
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
OUT = os.path.join(ROOT, "docs", "features")
RECORD = os.path.join(HERE, "record.py")
FAKE = os.path.join(ROOT, "tests", "fake_gmc.py")
NATIVE = os.path.join(ROOT, "target", "release", "radbeeper")

# Two counters for the monitor (the "two tubes" line needs two), and a third,
# hotter one for `random`, `log info` and `backfill`, which want the port to
# themselves. Serials are made up and say so: 0A1B..., 1B2C..., 2C3D...
FAKES = {
    "a": ("0A1B2C3D4E5F60", 40, 3),
    "b": ("1B2C3D4E5F6071", 40, 4),
    "c": ("2C3D4E5F607182", 400, 5),
}
# 620 s, as in tools/promo.py: past the spectrum's first window (128 s), the
# 5-minute average (300 s), the random pool's first line and the counts
# strip's leftmost tier.
MONITOR_SECONDS = 620
MONITOR_COLS, MONITOR_ROWS = 160, 40


def run(*argv, **kw):
    print("  " + " ".join(str(a) for a in argv))
    return subprocess.run([str(a) for a in argv], check=True, **kw)


def shquote(s):
    return "'" + s.replace("'", "'\\''") + "'"


class Guide:
    def __init__(self, work):
        self.work = work
        self.casts = os.path.join(work, "casts")
        os.makedirs(self.casts)
        os.makedirs(os.path.join(work, "logs"))
        # `radbeeper` on PATH, pointing at this working tree's build, so the
        # pictures show the command a person types.
        self.bin = os.path.join(work, "bin")
        os.makedirs(self.bin)
        os.symlink(NATIVE, os.path.join(self.bin, "radbeeper"))
        self.procs = []
        self.dev = {}

    def start_fakes(self):
        for key, (serial, cpm, seed) in FAKES.items():
            p = subprocess.Popen(
                [sys.executable, "-u", FAKE, "--cpm", str(cpm),
                 "--seed", str(seed), "--serial", serial],
                stdout=subprocess.PIPE, text=True)
            line = p.stdout.readline()
            # "fake GMC-320 on /dev/pts/N  (40 CPM, serial ...)"
            self.dev[key] = line.split(" on ", 1)[1].split()[0]
            self.procs.append(p)
            print("  fake %s on %s (%d CPM)" % (serial, self.dev[key], cpm))

    def stop(self):
        for p in self.procs:
            p.terminate()
        for p in self.procs:
            try:
                p.wait(5)
            except subprocess.TimeoutExpired:
                p.kill()

    def cast(self, name):
        return os.path.join(self.casts, name + ".cast")

    def session(self, name, script, cols=90, rows=24, seconds=60, cwd=None):
        """A shell transcript: each command echoed after a fixed prompt."""
        sh = ["#!/bin/sh", "cd " + shquote(cwd or self.work),
              "PATH=%s:$PATH" % self.bin, "export PATH"]
        for line in script:
            if not line:
                sh.append("echo")
            elif line.startswith("#"):
                sh.append("printf '$ %s\\n' " + shquote(line))
            else:
                sh.append("printf '$ %s\\n' " + shquote(line))
                sh.append(line)
        sh.append("printf '$ '")
        path = os.path.join(self.casts, name + ".sh")
        with open(path, "w") as f:
            f.write("\n".join(sh) + "\n")
        os.chmod(path, 0o755)
        run(sys.executable, RECORD, "capture", self.cast(name),
            "--cols", cols, "--rows", rows, "--seconds", seconds,
            "--", "/bin/sh", path)

    def text(self, name):
        out = subprocess.run([sys.executable, RECORD, "text", self.cast(name)],
                             capture_output=True, text=True)
        return out.stdout

    def when(self, name, match, after=0):
        out = subprocess.run(
            [sys.executable, RECORD, "when", self.cast(name), "--match", match,
             "--after", str(after)], capture_output=True, text=True)
        return float(out.stdout.strip()) if out.returncode == 0 else None

    def still(self, cast, png, at=1e9, size=17, rows=0, top=0, cursor=False):
        argv = [sys.executable, RECORD, "still", self.cast(cast),
                "-o", os.path.join(OUT, png), "--at", at, "--size", size]
        if rows:
            argv += ["--rows", rows]
        if top:
            argv += ["--top", top]
        if cursor:
            argv += ["--cursor"]
        run(*argv)


def chromium():
    for c in ("chromium", "chromium-browser", "google-chrome"):
        if shutil.which(c):
            return c
    return None


def shoot(page, png, width, height, profile):
    """One headless screenshot of a local file, the first `height` pixels."""
    exe = chromium()
    if not exe:
        print("  (no chromium -- %s skipped)" % png)
        return
    run(exe, "--headless=new", "--no-sandbox", "--hide-scrollbars",
        "--user-data-dir=" + profile, "--window-size=%d,%d" % (width, height),
        "--virtual-time-budget=4000",
        "--screenshot=" + os.path.join(OUT, png), "file://" + page,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--monitor-seconds", type=int, default=MONITOR_SECONDS)
    ap.add_argument("--work", default=None,
                    help="where the temporary directory goes (default $TMPDIR);"
                         " the monitor's recording is several MB, too much"
                         " for a small /tmp")
    ap.add_argument("--keep", action="store_true",
                    help="leave the temporary directory behind")
    a = ap.parse_args()

    if not os.path.exists(NATIVE):
        run("cargo", "build", "--release", "--locked", cwd=ROOT)
    os.makedirs(OUT, exist_ok=True)
    work = tempfile.mkdtemp(prefix="radbeeper-guide-", dir=a.work)
    g = Guide(work)
    month = datetime.date.today().strftime("%Y-%m")
    A, B, C = (FAKES[k][0] for k in "abc")
    try:
        g.start_fakes()
        da, db, dc = g.dev["a"], g.dev["b"], g.dev["c"]

        # 01: find the counter.
        g.session("probe", ["radbeeper -d %s probe" % da],
                  cols=80, rows=12, seconds=25)
        g.still("probe", "01-probe.png", cursor=True)

        # A key, on the third counter. Before the monitor, not beside it: a
        # fake that misses a beat for 2.5 s under load ends `random` early,
        # and a picture of "interrupted" is not the feature. Tried twice.
        for _ in range(2):
            g.session("random", ["radbeeper -d %s --logs logs random" % dc],
                      cols=96, rows=14, seconds=400)
            if "256 bits," in g.text("random"):
                break
        g.still("random", "10-random.png", cursor=True)

        # The monitor, two counters, for as long as it takes to fill; and
        # meanwhile, on the third counter, the flash.
        mon = threading.Thread(target=run, args=(
            sys.executable, RECORD, "capture", g.cast("watch"),
            "--cols", MONITOR_COLS, "--rows", MONITOR_ROWS,
            "--seconds", a.monitor_seconds, "--", "/bin/sh", "-c",
            "cd %s && exec %s/radbeeper -d %s -d %s --logs logs watch"
            % (shquote(work), g.bin, da, db)))
        mon.start()
        g.session("flash", ["radbeeper -d %s log info" % dc, "",
                            "radbeeper -d %s --logs logs backfill" % dc],
                  cols=96, rows=16, seconds=120)
        mon.join()

        late = a.monitor_seconds - 5
        g.still("watch", "02-watch.png", at=late, size=14, rows=MONITOR_ROWS)
        g.still("watch", "03-watch-filling.png", at=min(150, late // 2),
                size=14, rows=13)
        g.still("flash", "09-flash.png", cursor=True)

        # 08: the rows the monitor wrote, tabs shown as tabs are not.
        g.session("log", [
            "ls logs",
            "",
            "{ head -1; tail -3; } < logs/cpm-%s-%s.tsv | cut -f1-8 | column -t"
            % (A, month),
            "",
            "{ head -1; tail -3; } < logs/cpm-merged-%s.tsv | cut -f1-6 | column -t"
            % month,
        ], cols=120, rows=24, seconds=20)
        g.still("log", "08-log.png", cursor=True)

        # 11: the frames behind the keys the monitor earned.
        g.session("frames", [
            "radbeeper --logs logs frames list", "",
            "radbeeper --logs logs frames verify", "",
            "radbeeper --logs logs frames show --serial %s" % A,
        ], cols=100, rows=50, seconds=30)
        g.still("frames", "11-frames.png", cursor=True)

        # 12-14: the published pages, from the repository's own logs.
        pub = os.path.join(work, "pub")
        os.makedirs(pub)
        os.symlink(os.path.join(ROOT, "logs"), os.path.join(pub, "logs"))
        g.session("export", ["radbeeper export --logs logs -o monitor.html"
                            " --random-output random.html"],
                  cols=96, rows=10, seconds=120, cwd=pub)
        g.still("export", "12-export.png", cursor=True)
        profile = os.path.join(work, "chromium")
        shoot(os.path.join(pub, "monitor.html"), "13-monitor-page.png",
              1280, 1500, profile)
        shoot(os.path.join(pub, "random.html"), "14-random-page.png",
              1280, 1100, profile)
    finally:
        g.stop()
        if a.keep:
            print("kept " + work)
        else:
            shutil.rmtree(work, ignore_errors=True)
    print("\ndone -- docs/features is regenerated")


if __name__ == "__main__":
    main()
