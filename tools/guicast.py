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
    tools/guicast.py -o hero.gif --fullscreen --max-mb 9

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

# Widths tried in turn when a clip comes out over budget, largest first.
#
# A GIF IS A README'S DOWNLOAD, AND SOMEBODY PAYS FOR IT. The panel's hero
# clip is the first thing on the page and the first thing a phone on a train
# fetches, so the size is a promise the tool keeps rather than a number
# somebody checks afterwards and forgets to. Scaling down is the right lever:
# dropping frames makes a clock skip, and a clock that skips is the one thing
# a recording of an instrument must not do.
LADDER = [1100, 960, 840, 720, 620, 540, 460]


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


def window_address(app_id):
    """The compositor's handle on the window, for asking it to do something."""
    if shutil.which("hyprctl") is None:
        return None
    try:
        out = subprocess.run(["hyprctl", "clients", "-j"],
                             capture_output=True, text=True, timeout=10)
        clients = json.loads(out.stdout)
    except (OSError, ValueError, subprocess.SubprocessError):
        return None
    for c in clients:
        if c.get("class") == app_id or c.get("initialClass") == app_id:
            return c.get("address")
    return None


def to_workspace(address, workspace):
    """Move the window to a workspace of its own, and go and look at it.

    A SCREEN GRAB CANNOT GRAB WHAT IS NOT BEING SHOWN. The compositor hands
    over the pixels it is compositing, so a window on a workspace nobody is
    looking at records as whatever was last drawn -- or as nothing. Moving it
    somewhere empty and switching there is also the only way to be sure the
    clip is of the instrument and not of the instrument with a terminal over
    one corner of it.
    """
    if not address or not workspace:
        return None
    here = current_workspace()
    subprocess.run(["hyprctl", "dispatch", "movetoworkspacesilent",
                    "%s,address:%s" % (workspace, address)],
                   capture_output=True)
    subprocess.run(["hyprctl", "dispatch", "workspace", str(workspace)],
                   capture_output=True)
    time.sleep(1.0)
    return here


def current_workspace():
    """Which workspace is being looked at now, so it can be gone back to."""
    if shutil.which("hyprctl") is None:
        return None
    try:
        out = subprocess.run(["hyprctl", "activeworkspace", "-j"],
                             capture_output=True, text=True, timeout=10)
        return json.loads(out.stdout).get("id")
    except (OSError, ValueError, subprocess.SubprocessError):
        return None


def fullscreen(address, on):
    """Put the window full screen, or take it back out again.

    THE WHOLE SCREEN IS THE HONEST SIZE FOR A HERO CLIP. The panel lays itself
    out for whatever the compositor gives it -- the log table goes first, then
    the per-layer verdicts, then the axis -- so a clip recorded in a quarter
    of a screen is a recording of the panel with its best parts dropped. At
    full screen everything it can draw is on screen at once, which is what the
    top of a README is for.

    `fullscreen 1` is the MAXIMISED kind: the window keeps the compositor's
    gaps and bar rather than covering them. That is what somebody actually
    sees when they put this on a workspace, and a clip of the other kind
    would be a screenshot of a screen rather than of a program.
    """
    if not address:
        return False
    verb = "fullscreenstate" if on else "fullscreenstate"
    state = "1 -1" if on else "0 -1"
    r = subprocess.run(["hyprctl", "dispatch", verb,
                        "%s,address:%s" % (state, address)],
                       capture_output=True, text=True)
    if r.returncode != 0 or "ok" not in (r.stdout or "").lower():
        # Older Hyprland spells it `fullscreen <0|1>`.
        r = subprocess.run(["hyprctl", "dispatch", "fullscreen",
                            "1" if on else "0"],
                           capture_output=True, text=True)
    # The surface has to be reconfigured and repainted before it is grabbed,
    # and on the software renderer this takes a beat.
    time.sleep(1.5)
    return r.returncode == 0


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

    PLAYING FASTER THAN IT WAS CAPTURED is how a ten-minute session becomes a
    ten-second clip: the frames are a second apart on the wall clock and are
    written a twelfth of a second apart in the file. Nothing is dropped and
    nothing is interpolated -- every frame in the recording is a second that
    really happened, in order.

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
                   help="frames a second CAPTURED (default 2)")
    p.add_argument("--play-fps", type=int,
                   help="frames a second PLAYED; higher than --fps speeds the "
                        "recording up (default: the same, so real time)")
    p.add_argument("--width", type=int, default=540,
                   help="scale the frames to this width (default 540)")
    p.add_argument("--geometry",
                   help="'X,Y WxH' instead of asking the compositor")
    p.add_argument("--fullscreen", action="store_true",
                   help="maximise the window for the recording, then put it "
                        "back")
    p.add_argument("--workspace",
                   help="move the window to this workspace first, and switch "
                        "there, so nothing is sitting on top of it")
    p.add_argument("--max-mb", type=float,
                   help="re-encode narrower until the GIF fits this many MB")
    p.add_argument("--keep", action="store_true",
                   help="leave the frames behind, for a look at them")
    args = p.parse_args()

    need("grim", "it is what takes the screen grabs")
    need("ffmpeg", "it is what turns them into a GIF")
    if not os.environ.get("WAYLAND_DISPLAY"):
        sys.exit("guicast: no WAYLAND_DISPLAY -- this records a Wayland window")

    address = window_address(APP_ID)
    came_from = None
    if args.workspace and not args.geometry:
        came_from = to_workspace(address, args.workspace)
    restore = False
    if args.fullscreen and not args.geometry:
        restore = fullscreen(address, True)
        if not restore:
            sys.stderr.write("guicast: could not maximise the window; "
                             "recording it where it is\n")

    geometry = args.geometry or window_geometry(APP_ID)
    if not geometry:
        if restore:
            fullscreen(address, False)
        sys.exit("guicast: no %s window found. Start it, or pass --geometry "
                 "'X,Y WxH'." % APP_ID)

    into = tempfile.mkdtemp(prefix="guicast-")
    speed = (args.play_fps or args.fps) / float(args.fps)
    print("guicast: %s at %s, %gs at %d fps%s"
          % (APP_ID, geometry, args.seconds, args.fps,
             "" if speed == 1 else ", played at %gx" % speed))
    try:
        frames = capture(geometry, args.seconds, args.fps, into)
        if restore:
            fullscreen(address, False)
            restore = False
        os.makedirs(os.path.dirname(os.path.abspath(args.output)), exist_ok=True)
        widths = [args.width]
        if args.max_mb:
            # Start at the width asked for, then down the ladder. Encoding a
            # GIF of a mostly-still panel is a second or two, so trying a few
            # is cheaper than guessing and cheaper than shipping an oversized
            # one.
            widths += [w for w in LADDER if w < args.width]
        budget = args.max_mb * 1024 * 1024 if args.max_mb else None
        size = 0
        for w in widths:
            assemble(into, args.output, args.play_fps or args.fps, w)
            size = os.path.getsize(args.output)
            print("guicast:   %d px wide -- %.1f MB" % (w, size / 1048576.0))
            if budget is None or size <= budget:
                break
        else:
            sys.stderr.write(
                "guicast: still %.1f MB at %d px -- record fewer seconds\n"
                % (size / 1048576.0, widths[-1]))
    finally:
        if restore:
            fullscreen(address, False)
        if came_from is not None:
            subprocess.run(["hyprctl", "dispatch", "workspace", str(came_from)],
                           capture_output=True)
        if args.keep:
            print("guicast: frames in %s" % into)
        else:
            shutil.rmtree(into, ignore_errors=True)
    size = os.path.getsize(args.output)
    print("guicast: %s -- %d frames, %.1f MB" % (args.output, frames, size / 1048576.0))


if __name__ == "__main__":
    main()
