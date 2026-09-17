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
# The native build, for the verbs it owns. `probe` and `watch` are shown as
# the Rust program draws them, because that is what `make install` puts on
# PATH; `--plain`, `site` and `export` are not ported, so those shots still
# run the one-file Python beside it.
NATIVE = os.path.join(ROOT, "target", "release", "radbeeper")
PYTHON_PROGRAM = os.path.join(ROOT, "radbeeper")

# The monitor needs a real run behind it: 128 s before the spectrum has its
# first window, 300 s before the 5-minute average is full, and -- since the
# pool started measuring its min-entropy instead of modelling it -- about
# 450 s before it has earned a line. That last one is why this is not 380 any
# more: the shot at 340 s used to catch a random line and now would not, and a
# screenshot that quietly stops showing a feature is worse than no screenshot.
# 560 s clears all three with room to animate.
#
# 620 s, because the counts strip is four tiers deep now and the leftmost of
# them takes 588 s to fill at 160 columns -- a shot taken before that shows a
# display that is still arriving, which is a different picture from the one
# the program settles into. It is also past the 450 s the random pool needs.
#
# `--monitor-seconds` runs it longer. 3300 s is the one worth knowing: the
# 50-minute window fills at 3000 s, so the hero shot has four of the five
# windows with a number in them instead of two still counting down.
MONITOR_SECONDS = 620
# Forty rows, not thirty: the monitor's log table takes the rows after 30, and
# under 36 it squeezes the charts to make room -- the shot should show both
# at full size.
MONITOR_COLS, MONITOR_ROWS = 160, 40

ARROW = "\u21e5"          # U+21E5, what a tab is drawn as in the log shot


def run(*argv):
    print("  " + " ".join(str(a) for a in argv))
    subprocess.run([str(a) for a in argv], check=True, cwd=ROOT)


def cast(name):
    return os.path.join(CASTS, name + ".cast")


def png(name):
    return os.path.join(SHOTS, name + ".png")


def session(name, script, cols=90, rows=30, seconds=0, native=False):
    """Record a shell transcript: each command echoed, then run.

    A real shell prompt would drag in whatever PS1 the machine happens to
    have, so the prompt is printed by the script itself and is the same in
    every shot.
    """
    # `radbeeper`, not `./radbeeper`: a bin/ of one symlink goes on PATH so
    # the shot shows the command somebody actually types after `make
    # install`, and still runs this working tree rather than whatever is
    # already installed on the machine.
    # One bin/ per implementation, and the link is checked, not just found:
    # a single bin/ made on a run before the port kept pointing at the Python.
    binv = os.path.join(CASTS, "bin-native" if native else "bin-python")
    os.makedirs(binv, exist_ok=True)
    link = os.path.join(binv, "radbeeper")
    target = NATIVE if native else PYTHON_PROGRAM
    if os.path.islink(link) and os.readlink(link) != target:
        os.unlink(link)
    if not os.path.islink(link):
        os.symlink(target, link)
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


# THE SHOTS CARRY THE VERSION THAT DREW THEM, which is the whole point of the
# nameplate in the monitor's corner -- not the version the README happens to
# be published as. A patch release does not re-record eleven minutes of a
# counter to advance a string in the corner of a picture; a release that
# changes what the screen looks like does, and `make promo` is how.
def when(name, match, after=0):
    """The second a recording first shows something, or None.

    A moment worth animating is a property of the run, not of the clock: the
    random pool delivers its first line when it has measured enough
    min-entropy to justify one, which is a different second in every session.
    Found, therefore, rather than written down.
    """
    out = subprocess.run(
        [sys.executable, RECORD, "when", cast(name), "--match", match,
         "--after", str(after)],
        cwd=ROOT, capture_output=True, text=True)
    if out.returncode != 0:
        print("  (no frame matches %s -- clip skipped)" % match)
        return None
    return float(out.stdout.strip())


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
    ap.add_argument("--monitor-seconds", type=int, default=MONITOR_SECONDS,
                    help="how long to record the monitor (default %(default)s)")
    ap.add_argument("--only", default="",
                    help="comma-separated shot names")
    a = ap.parse_args()
    CASTS = a.casts
    seconds = a.monitor_seconds
    late = seconds - 5          # the hero and the spectrum: as late as it gets
    os.makedirs(CASTS, exist_ok=True)
    os.makedirs(SHOTS, exist_ok=True)
    only = set(x for x in a.only.split(",") if x)
    run("cargo", "build", "--release", "--locked")

    def want(name):
        return not only or name in only

    # ---------------------------------------------------------- monitor ---
    if any(want(n) for n in ("watch", "watch-filling", "watch-spectrum",
                             "watch-hero")):
        if not (a.keep and os.path.exists(cast("watch-long"))):
            print("recording %d s of the monitor -- this takes that long"
                  % seconds)
            run(sys.executable, RECORD, "capture", cast("watch-long"),
                "--cols", MONITOR_COLS, "--rows", MONITOR_ROWS,
                "--seconds", seconds, "--", NATIVE, "watch")

        if want("watch"):
            # Late enough that the 3 s, 30 s and 300 s windows are full, the
            # spectrum has accumulated and the random pool has earned a line.
            # The 3000 s and 30000 s windows are still counting down, and say
            # so, which is half the point of the panel.
            still("watch", late, size=17, rows=MONITOR_ROWS,
                  source="watch-long")
        if want("watch-filling"):
            # Early, and saying so: three of the five still counting down.
            # Far enough in that two windows have a number and three are
            # still counting: the point of the shot is the pair side by side.
            # 14 rows, not 13 -- the fifth window pushed everything down one.
            # Twelve rows now: the tier labels sit on the thirteenth.
            still("watch-filling", min(200, seconds // 2), size=17, rows=12,
                  source="watch-long")
        if want("watch-spectrum"):
            # Just the spectrum band and its axis, cut out of the same frame
            # as the hero shot: five rows of power against period. Row 19:
            # the counts moved up one when the clock took a row of air.
            still("watch-spectrum", late, size=15, rows=6, top=19,
                  source="watch-long")
        if want("watch-hero"):
            # The README's moving picture, and the only one: two clips in a
            # single file, because the display has two things to show and
            # they want different speeds.
            #
            # First the whole five minutes at 100x, which is the one way to
            # watch the counts strip fill -- a second a bar on the right, and
            # then each tier to its left taking a bar half as often, so the
            # fourth is still filling when the first has scrolled twice.
            # Then the hundred seconds after it, at 10x: slow enough to read
            # a window changing and a log row closing, which at 100x is a
            # flicker.
            #
            # It replaced watch-fast.gif (the session at 40x) and
            # watch-20s.gif (twenty seconds at 10x), which were these two
            # clips as two files and two paragraphs.
            #
            # And then, third, the second the random pool has measured
            # enough min-entropy and prints its first 256 bits -- found in
            # the recording rather than timed, because it lands wherever the
            # source puts it.
            # Anchored on the monitor's first frame, not on the recording's:
            # the opening backfill is a line of text and a pause of anywhere
            # between forty seconds and two and a half minutes, depending on
            # how much of the counter's flash is new. Timed from zero, a slow
            # one eats a third of the fast clip and the file opens on a blank
            # screen -- which is also the still GitHub shows before it plays.
            start = when("watch-long", r"s/bar") or 0.0
            clips = ["%g:%g:4:100" % (start, start + 300),
                     "%g:%g:1:10" % (start + 300, start + 400)]
            got = when("watch-long", r"^random   [0-9a-f]{8} ")
            if got is not None:
                clips.append("%g:%g:1:10" % (max(0.0, got - 12.0), got + 8.0))
            argv = [sys.executable, RECORD, "gif", cast("watch-long"),
                    "-o", os.path.join(SHOTS, "watch-hero.gif")]
            for c in clips:
                argv += ["--clip", c]
            run(*argv, "--rows", MONITOR_ROWS, "--size", 11)

    # ------------------------------------------------------------ probe ---
    if want("probe"):
        session("probe", ["radbeeper probe"], cols=80, rows=20, seconds=25,
                native=True)
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
            "ls /var/lib/radbeeper",
            "",
            "{ head -1; tail -3; } < /var/lib/radbeeper/"
            "cpm-F48824B8207F7E-2026-09.tsv | cut -f1-8 | column -t",
            "",
            "wc -l /var/lib/radbeeper/cpm-*.tsv",
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
