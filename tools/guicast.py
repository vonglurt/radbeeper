#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Paul Richeson
"""Record the radbeeper-gui window to an animated GIF.

WHY NOT tools/record.py. That one records a TERMINAL: it captures the bytes a
program writes to a pty and replays them, which is exact, tiny, and completely
useless for a window. A Wayland surface has no byte stream to keep -- the only
honest record of it is pixels, so this grabs frames with `grim` and assembles
them with `ffmpeg`.

    tools/guicast.py -o docs/screenshots/gui.gif
    tools/guicast.py -o /tmp/x.gif --seconds 30 --fps 2 --width 540

THE WINDOW IS FOUND, NOT GUESSED. radbeeper-gui sets an application id, so the
compositor can be asked where it is; `--geometry` overrides that for a
compositor this does not know how to ask.

Stdlib only, as everything in tools/ is. grim and ffmpeg are the two outside
programs, and both say so plainly if they are missing.
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

APP_ID = "radbeeper-gui"


def need(program, why):
    """Refuse early and in words, rather than failing inside a pipe."""
    if shutil.which(program) is None:
        sys.exit("guicast: %s is not installed -- %s" % (program, why))


def window_geometry(app_id):
    """Ask the compositor where the window is, as grim wants it: 'X,Y WxH'.

    HYPRLAND FIRST because that is what this is developed against; anything
    else falls through to --geometry, which is why that flag exists.
    """
    if shutil.which("hyprctl") is None:
        return None
    try:
        out = subprocess.run(["hyprctl", "clients", "-j"],
                             capture_output=True, text=True, timeout=10)
        clients = json.loads(out.stdout)
    except (OSError, ValueError, subprocess.SubprocessError):
        return None
    for c in clients:
        # The title carries the live reading and changes every second, so the
        # id is the only stable handle on this window.
        if c.get("class") == app_id or c.get("initialClass") == app_id:
            (x, y), (w, h) = c["at"], c["size"]
            return "%d,%d %dx%d" % (x, y, w, h)
    return None


def capture(geometry, seconds, fps, into):
    """One PNG per frame, on the clock rather than as fast as grim goes.

    THE INSTRUMENT UPDATES ONCE A SECOND, so frames are paced to wall time and
    not to how long a screen grab happens to take: a capture that drifted would
    show the clock skipping, which is the one thing a recording of a clock must
    not do.
    """
    period = 1.0 / fps
    frames = int(round(seconds * fps))
    started = time.monotonic()
    for i in range(frames):
        path = os.path.join(into, "f%05d.png" % i)
        subprocess.run(["grim", "-g", geometry, path], check=True,
                       capture_output=True)
        due = started + (i + 1) * period
        slack = due - time.monotonic()
        if slack > 0:
            time.sleep(slack)
        sys.stderr.write("\r  %d/%d frames" % (i + 1, frames))
        sys.stderr.flush()
    sys.stderr.write("\n")
    return frames


def assemble(into, out, fps, width):
    """Frames to a GIF, at a palette that suits a mostly-still panel.

    `stats_mode=diff` builds the palette from what CHANGES between frames
    rather than from the whole picture, and `diff_mode=rectangle` rewrites only
    the rectangle that moved. On this panel -- a dark instrument face where a
    needle, a strip of bars and a few numbers move and nothing else does --
    the pair is the difference between a file that fits in a README and one
    that does not.
    """
    chain = ("[0:v] scale=%d:-1:flags=lanczos,split [a][b];"
             "[a] palettegen=stats_mode=diff [p];"
             "[b][p] paletteuse=diff_mode=rectangle:dither=bayer:bayer_scale=3"
             % width)
    subprocess.run(
        ["ffmpeg", "-y", "-loglevel", "error",
         "-framerate", str(fps), "-i", os.path.join(into, "f%05d.png"),
         "-filter_complex", chain, "-loop", "0", out],
        check=True)


def main():
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("-o", "--output", required=True, help="the .gif to write")
    p.add_argument("--seconds", type=float, default=20.0,
                   help="how long to record (default 20)")
    p.add_argument("--fps", type=int, default=2,
                   help="frames a second, captured and played (default 2)")
    p.add_argument("--width", type=int, default=540,
                   help="scale the frames to this width (default 540)")
    p.add_argument("--geometry",
                   help="'X,Y WxH' instead of asking the compositor")
    p.add_argument("--keep", action="store_true",
                   help="leave the frames behind, for a look at them")
    args = p.parse_args()

    need("grim", "it is what takes the screen grabs")
    need("ffmpeg", "it is what turns them into a GIF")
    if not os.environ.get("WAYLAND_DISPLAY"):
        sys.exit("guicast: no WAYLAND_DISPLAY -- this records a Wayland window")

    geometry = args.geometry or window_geometry(APP_ID)
    if not geometry:
        sys.exit("guicast: no %s window found. Start it, or pass --geometry "
                 "'X,Y WxH'." % APP_ID)

    into = tempfile.mkdtemp(prefix="guicast-")
    print("guicast: %s at %s, %gs at %d fps"
          % (APP_ID, geometry, args.seconds, args.fps))
    try:
        frames = capture(geometry, args.seconds, args.fps, into)
        os.makedirs(os.path.dirname(os.path.abspath(args.output)), exist_ok=True)
        assemble(into, args.output, args.fps, args.width)
    finally:
        if args.keep:
            print("guicast: frames in %s" % into)
        else:
            shutil.rmtree(into, ignore_errors=True)
    size = os.path.getsize(args.output)
    print("guicast: %s -- %d frames, %.1f KB" % (args.output, frames, size / 1024.0))


if __name__ == "__main__":
    main()
