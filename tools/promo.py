#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Paul Richeson
"""Regenerate every screenshot in docs/screenshots from the real program.

Nothing here is drawn by hand or edited afterwards. Each shot is a real
session, recorded through a pty by tools/record.py and rendered from the
resulting cast, so a screenshot cannot claim something the program does not
do -- and when the layout changes, `make promo` is the whole of the fix.

    make promo                # all of it: needs the counter plugged in
    make promo SHOTS=probe    # one of them

The monitor shots come from a single long recording, because that is what
they are: one session, sampled at four moments and animated between two of
them. `--keep` reuses it instead of spending another six minutes.
"""
import argparse
import os
import subprocess
import sys
import textwrap

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
SHOTS = os.path.join(ROOT, "docs", "screenshots")
RECORD = os.path.join(HERE, "record.py")

# The monitor needs a real run behind it: 128 s before the spectrum has its
# first window, 300 s before the 5-minute average is full, and -- since the
# pool started measuring its min-entropy instead of modelling it -- about
# 450 s before it has earned a line. That last one is why this is not 380 any
# more: the shot at 340 s used to catch a random line and now would not, and a
# screenshot that quietly stops showing a feature is worse than no screenshot.
# 560 s clears all three with room to animate.
MONITOR_SECONDS = 560
MONITOR_COLS, MONITOR_ROWS = 160, 30

ARROW = "\u21e5"          # U+21E5, what a tab is drawn as in the log shot


def run(*argv):
    print("  " + " ".join(str(a) for a in argv))
    subprocess.run([str(a) for a in argv], check=True, cwd=ROOT)


def cast(name):
    return os.path.join(CASTS, name + ".cast")


def png(name):
    return os.path.join(SHOTS, name + ".png")


def session(name, script, cols=90, rows=30, seconds=0):
    """Record a shell transcript: each command echoed, then run.

    A real shell prompt would drag in whatever PS1 the machine happens to
    have, so the prompt is printed by the script itself and is the same in
    every shot.
    """
    # `radbeeper`, not `./radbeeper`: a bin/ of one symlink goes on PATH so
    # the shot shows the command somebody actually types after `make
    # install`, and still runs this working tree rather than whatever is
    # already installed on the machine.
    binv = os.path.join(CASTS, "bin")
    os.makedirs(binv, exist_ok=True)
    link = os.path.join(binv, "radbeeper")
    if not os.path.islink(link):
        os.symlink(os.path.join(ROOT, "radbeeper"), link)
    sh = ["#!/bin/sh", "cd " + ROOT, "PATH=%s:$PATH" % binv, "export PATH"]
    for line in script:
        if not line:
            sh.append("echo")                       # a blank separator row
        elif line.startswith("#"):
            sh.append("printf '$ %s\\n' " + shquote(line))
        else:
            sh.append("printf '$ %s\\n' " + shquote(line))
            sh.append(line)
    sh.append("printf '$ '")
    path = os.path.join(CASTS, name + ".sh")
    with open(path, "w") as f:
        f.write("\n".join(sh) + "\n")
    os.chmod(path, 0o755)
    run(sys.executable, RECORD, "capture", cast(name),
        "--cols", cols, "--rows", rows, "--seconds", seconds or 30,
        "--", "/bin/sh", path)


def shquote(s):
    return "'" + s.replace("'", "'\\''") + "'"


def still(name, at, size=17, rows=0, cursor=False, source=None, top=0):
    argv = [sys.executable, RECORD, "still", cast(source or name),
            "-o", png(name), "--at", at, "--size", size]
    if rows:
        argv += ["--rows", rows]
    if top:
        argv += ["--top", top]
    if cursor:
        argv += ["--cursor"]
    run(*argv)


def main():
    global CASTS
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--casts", default=os.path.join(ROOT, ".casts"))
    ap.add_argument("--keep", action="store_true",
                    help="reuse an existing monitor recording")
    ap.add_argument("--only", default="",
                    help="comma-separated shot names")
    a = ap.parse_args()
    CASTS = a.casts
    os.makedirs(CASTS, exist_ok=True)
    os.makedirs(SHOTS, exist_ok=True)
    only = set(x for x in a.only.split(",") if x)

    def want(name):
        return not only or name in only

    # ---------------------------------------------------------- monitor ---
    if any(want(n) for n in ("watch", "watch-filling", "watch-spectrum",
                             "watch-300-320")):
        if not (a.keep and os.path.exists(cast("watch-long"))):
            print("recording %d s of the monitor -- this takes that long"
                  % MONITOR_SECONDS)
            run(sys.executable, RECORD, "capture", cast("watch-long"),
                "--cols", MONITOR_COLS, "--rows", MONITOR_ROWS,
                "--seconds", MONITOR_SECONDS, "--", "./radbeeper", "watch")

        if want("watch"):
            # Late enough that the 3 s, 30 s and 300 s windows are full, the
            # spectrum has accumulated and the random pool has earned a line.
            # The 3000 s and 30000 s windows are still counting down, and say
            # so, which is half the point of the panel.
            still("watch", 520, size=17, rows=MONITOR_ROWS,
                  source="watch-long")
        if want("watch-filling"):
            # Early, and saying so: three of the five still counting down.
            # Far enough in that two windows have a number and three are
            # still counting: the point of the shot is the pair side by side.
            # 14 rows, not 13 -- the fifth window pushed everything down one.
            still("watch-filling", 200, size=17, rows=14, source="watch-long")
        if want("watch-spectrum"):
            # Just the spectrum band and its axis, cut out of the same frame
            # as the hero shot: five rows of power against period. Row 20,
            # not 19, for the same reason watch-filling grew a row.
            still("watch-spectrum", 520, size=15, rows=6, top=20,
                  source="watch-long")
        if want("watch-300-320"):
            run(sys.executable, RECORD, "gif", cast("watch-long"),
                "-o", os.path.join(SHOTS, "watch-300-320.gif"),
                "--from", 300, "--to", 320, "--step", 1, "--speed", 10,
                "--rows", MONITOR_ROWS, "--size", 13)

    # ------------------------------------------------------------ probe ---
    if want("probe"):
        session("probe", ["radbeeper probe"], cols=80, rows=20, seconds=25)
        still("probe", 24, cursor=True)

    # --------------------------------------------------------- --plain ---
    if want("watch-plain"):
        # 112 columns, not 96: a fifth window is thirteen more characters of
        # line and at 96 every row wrapped, folding `total` onto the next one.
        # A screenshot of the output mangled by its own terminal is worse than
        # no screenshot -- the width is part of what is being shown.
        session("watch-plain", ["radbeeper --plain --duration 14 watch"],
                cols=112, rows=22, seconds=30)
        still("watch-plain", 29, cursor=True)

    # --------------------------------------------------------- commands ---
    if want("commands"):
        session("commands", [
            "radbeeper log info",
            "",
            "radbeeper site --serial F48824B8207F7E",
            "",
            "radbeeper export --logs logs -o /tmp/i.html",
        ], cols=88, rows=22, seconds=90)
        still("commands", 89, cursor=True)

    # ------------------------------------------------------------- logs ---
    if want("log-output"):
        session("log-output", [
            "ls /var/log/radbeeper",
            "",
            "{ head -1; tail -3; } < /var/log/radbeeper/"
            "cpm-F48824B8207F7E-2026-09.tsv | cut -f1-8 | column -t",
            "",
            "wc -l /var/log/radbeeper/cpm-*.tsv",
        ], cols=104, rows=24, seconds=20)
        still("log-output", 19, cursor=True)

    if want("log-tabs"):
        # The arrow is a literal U+21E5 in the command, because that is what
        # somebody would type and because busybox sed does not read \x
        # escapes -- which is exactly the sort of thing a screenshot that is
        # really a recording catches and a hand-made one does not.
        session("log-tabs", [
            "head -4 logs/cpm-F48824B8207F7E-2026-09.tsv"
            + r' | sed "s/\t/ ' + ARROW + r' /g"',
            "",
            "# every " + ARROW + " is one tab. An empty field between two",
            "# of them is a window that was not full yet -- not a zero.",
        ], cols=152, rows=16, seconds=20)
        still("log-tabs", 19, size=12, cursor=True)

    print("\ndone -- docs/screenshots is regenerated")


if __name__ == "__main__":
    main()
