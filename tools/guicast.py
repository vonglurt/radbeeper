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


def clients_of(app_id, pid=None):
    """The compositor's records of this program's windows, newest last.

    BY PROCESS ID WHEN ONE IS GIVEN. `radbeeper hotplug` opens a window of
    its own whenever a counter appears, in whatever theme the desktop wears,
    and a recording that takes the first radbeeper-gui it finds has recorded
    that one: the theme shots came out light with `--theme dark` on the
    command line for exactly this reason (2026-09-30). The window started
    for the recording is the one whose pid the caller knows.
    """
    if shutil.which("hyprctl") is None:
        return []
    try:
        out = subprocess.run(["hyprctl", "clients", "-j"],
                             capture_output=True, text=True, timeout=10)
        clients = json.loads(out.stdout)
    except (OSError, ValueError, subprocess.SubprocessError):
        return []
    mine = [c for c in clients
            if c.get("class") == app_id or c.get("initialClass") == app_id]
    if pid:
        mine = [c for c in mine if c.get("pid") == pid]
    return mine


def window_geometry(app_id, pid=None):
    """Ask the compositor where the window is, as grim wants it: 'X,Y WxH'.

    HYPRLAND FIRST because that is what this is developed against; anything
    else falls through to --geometry, which is why that flag exists.
    """
    for c in clients_of(app_id, pid):
        # The title carries the live reading and changes every second, so the
        # id is the only stable handle on this window.
        (x, y), (w, h) = c["at"], c["size"]
        return "%d,%d %dx%d" % (x, y, w, h)
    return None


def window_address(app_id, pid=None):
    """The compositor's handle on the window, for asking it to do something."""
    for c in clients_of(app_id, pid):
        return c.get("address")
    return None


# Every headless output this tool makes is named with this prefix, so that
# several recordings at once -- one per theme -- each get their own and the
# real display is always the one without it.
HEADLESS_PREFIX = "cast-"


def primary_monitor():
    """The real display: its name and size, for a headless twin of it."""
    try:
        out = subprocess.run(["hyprctl", "monitors", "-j"],
                             capture_output=True, text=True, timeout=10)
        for m in json.loads(out.stdout):
            if not m.get("name", "").startswith(HEADLESS_PREFIX):
                return m["name"], m["width"], m["height"]
    except (OSError, ValueError, KeyError, subprocess.SubprocessError):
        pass
    return None, 0, 0


def headless_start(address, workspace, name, slot):
    """A screen of the display's size that nobody is looking at, with the
    window on it, full screen. Returns the display to give focus back to.

    A SCREEN GRAB TAKES WHAT IS ON THE SCREEN. The first drum clip caught
    eleven minutes of the terminal this was typed in, because the person
    typing it switched workspaces to type, and a window on a workspace
    nobody is showing is not composited at all. A headless output IS
    composited -- the compositor renders it for the copy request -- and it
    is not the screen anybody is working on. So the window goes there, on a
    workspace of its own, and the person keeps their desktop for the
    twenty-two minutes the paper takes to fill.
    """
    primary, w, h = primary_monitor()
    if not primary or not w:
        return None
    showing = current_workspace()
    # SIDE BY SIDE TO THE LEFT OF THE DISPLAY, one slot each, so several
    # recordings at once do not overlap. To the left, at negative x: the
    # display is placed automatically, after everything else, and outputs
    # to its right once moved it to x = 12800 (2026-09-30).
    subprocess.run(["hyprctl", "output", "create", "headless", name],
                   capture_output=True)
    time.sleep(1.0)
    subprocess.run(["hyprctl", "keyword", "monitor",
                    "%s,%dx%d@60,%dx0,1" % (name, w, h, -w * slot)],
                   capture_output=True)
    time.sleep(1.0)
    # A NEW OUTPUT HELPS ITSELF TO A WORKSPACE -- the next free one, which
    # is one of the person's. Note which, to hand it back.
    stolen = None
    try:
        out = subprocess.run(["hyprctl", "monitors", "-j"],
                             capture_output=True, text=True, timeout=10)
        for m in json.loads(out.stdout):
            if m.get("name") == name:
                stolen = m.get("activeWorkspace", {}).get("id")
    except (OSError, ValueError, subprocess.SubprocessError):
        pass
    # THE WORKSPACE EXISTS ONLY ONCE SOMETHING IS ON IT, so the window goes
    # to it first and the workspace is moved to the output second. The
    # other order moved nothing, and the first parallel run recorded
    # twenty-two minutes of wallpaper three times over (2026-09-30).
    ws = workspace or str(90 + slot)
    subprocess.run(["hyprctl", "dispatch", "movetoworkspacesilent",
                    "%s,address:%s" % (ws, address)], capture_output=True)
    time.sleep(0.5)
    subprocess.run(["hyprctl", "dispatch", "moveworkspacetomonitor",
                    "%s %s" % (ws, name)], capture_output=True)
    time.sleep(0.5)
    if stolen is not None and str(stolen) != ws:
        subprocess.run(["hyprctl", "dispatch", "moveworkspacetomonitor",
                        "%s %s" % (stolen, primary)], capture_output=True)
        time.sleep(0.5)
    subprocess.run(["hyprctl", "dispatch", "fullscreenstate",
                    "1 -1,address:%s" % address], capture_output=True)
    time.sleep(1.5)
    # THE POINTER'S PICTURE STAYS WHERE THE POINTER LAST WAS ON THIS
    # OUTPUT. A new output warps the pointer to its centre, and on a
    # software-cursor machine that image is composited into every grab of
    # the output for as long as the pointer is elsewhere -- an arrow in the
    # middle of every frame (2026-09-30). So the pointer is walked to the
    # output's bar, just right of the menu where it spoils nothing, and only
    # then brought home to the real display.
    subprocess.run(["hyprctl", "dispatch", "focusmonitor", name],
                   capture_output=True)
    subprocess.run(["hyprctl", "dispatch", "movecursor",
                    str(-w * slot + 40), "12"], capture_output=True)
    time.sleep(0.3)
    # The window took focus with it; give the person their screen back,
    # on the workspace they were looking at.
    subprocess.run(["hyprctl", "dispatch", "focusmonitor", primary],
                   capture_output=True)
    if showing is not None:
        subprocess.run(["hyprctl", "dispatch", "workspace", str(showing)],
                       capture_output=True)
    return primary


def on_output(address, name):
    """Whether the window is being composited on that output, by id."""
    try:
        out = subprocess.run(["hyprctl", "monitors", "-j"],
                             capture_output=True, text=True, timeout=10)
        ids = {m["name"]: m["id"] for m in json.loads(out.stdout)}
        out = subprocess.run(["hyprctl", "clients", "-j"],
                             capture_output=True, text=True, timeout=10)
        for c in json.loads(out.stdout):
            if c.get("address") == address:
                return c.get("monitor") == ids.get(name)
    except (OSError, ValueError, KeyError, subprocess.SubprocessError):
        pass
    return False


def headless_stop(primary, name, park=None):
    """Take the headless output down without taking the pointer with it.

    THE POINTER FIRST, THEN THE OUTPUT. Removing an output the pointer was
    on left it stranded at an x beyond every screen, where no `movecursor`
    could reach it until the compositor's config was reloaded (2026-09-30).
    So the pointer is brought home to the real display and parked before
    the output goes, and if it is still off the screen afterwards the
    reload is done here rather than by the person, blind.
    """
    if primary:
        subprocess.run(["hyprctl", "dispatch", "focusmonitor", primary],
                       capture_output=True)
        if park:
            park_cursor(park[0], park[1], primary)
    subprocess.run(["hyprctl", "output", "remove", name],
                   capture_output=True)
    time.sleep(1.0)
    if primary:
        subprocess.run(["hyprctl", "dispatch", "focusmonitor", primary],
                       capture_output=True)
        if park:
            park_cursor(park[0], park[1], primary)
            _, w, _h = primary_monitor()
            try:
                out = subprocess.run(["hyprctl", "cursorpos"],
                                     capture_output=True, text=True, timeout=10)
                x = int(out.stdout.split(",")[0])
            except (OSError, ValueError, subprocess.SubprocessError):
                x = 0
            if w and x >= w:
                subprocess.run(["hyprctl", "reload"], capture_output=True)
                time.sleep(2.0)
                park_cursor(park[0], park[1], primary)


def park_cursor(x, y, primary=None):
    """Put the pointer somewhere it does not spoil the picture.

    JUST RIGHT OF THE BAR'S MENU. The pointer is drawn into the grab, so
    it has to sit outside the window; on the bar is outside. Not the
    top-right corner: that is the clock, and hovering it spawns its
    overlay, which the antiquity shot of 2026-09-30 duly recorded.
    """
    if primary:
        subprocess.run(["hyprctl", "dispatch", "focusmonitor", primary],
                       capture_output=True)
    subprocess.run(["hyprctl", "dispatch", "movecursor", str(x), str(y)],
                   capture_output=True)


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


def capture(geometry, seconds, fps, into, every=None, output=None):
    """One PNG per frame, on the clock rather than as fast as grim goes.

    THE INSTRUMENT UPDATES ONCE A SECOND, so frames are paced to wall time and
    not to how long a screen grab happens to take: a capture that drifted would
    show the clock skipping, which is the one thing a recording of a clock must
    not do.

    `every` is the other pace: one frame per `every` seconds, for a thing
    that moves slower than the clock. The drum spectrogram lays a row every
    eight seconds and holds ninety-six, so a frame per row is a frame every
    eight seconds for twenty-one minutes -- the paper filling, once per
    line, in a clip that plays in under a minute.
    """
    period = every if every else 1.0 / fps
    frames = int(round(seconds / period))
    started = time.monotonic()
    for i in range(frames):
        path = os.path.join(into, "f%05d.png" % i)
        where = ["-o", output] if output else ["-g", geometry]
        subprocess.run(["grim"] + where + [path], check=True,
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
    p.add_argument("--every", type=float,
                   help="instead of --fps: seconds BETWEEN frames, for a thing "
                        "that moves slower than the clock (8 is one drum row)")
    p.add_argument("--still",
                   help="also keep the last frame, at full resolution, as "
                        "this PNG -- the panel as it stood when the clip ended")
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
    p.add_argument("--pid", type=int,
                   help="record the radbeeper-gui with this process id, not "
                        "the first one the compositor lists")
    p.add_argument("--headless", metavar="NAME",
                   help="record on a headless output of this name, the size "
                        "of the display, so the desktop stays free to use")
    p.add_argument("--slot", type=int, default=1,
                   help="which headless output this is, 1, 2, ..., so several "
                        "recordings at once sit side by side (default 1)")
    p.add_argument("--park", nargs=2, type=int, metavar=("X", "Y"),
                   help="move the pointer here first, out of the picture")
    args = p.parse_args()

    need("grim", "it is what takes the screen grabs")
    need("ffmpeg", "it is what turns them into a GIF")
    if not os.environ.get("WAYLAND_DISPLAY"):
        sys.exit("guicast: no WAYLAND_DISPLAY -- this records a Wayland window")

    address = window_address(APP_ID, args.pid)
    came_from = None
    primary = None
    headless = (HEADLESS_PREFIX + args.headless) if args.headless else None
    if headless and address:
        primary = headless_start(address, args.workspace, headless, args.slot)
        if not primary:
            sys.exit("guicast: could not create a headless output")
        if not on_output(address, headless):
            # Better no clip than twenty-two minutes of the wallpaper.
            headless_stop(primary, headless, args.park)
            sys.exit("guicast: the window did not land on %s -- not recording"
                     % headless)
    elif args.workspace and not args.geometry:
        came_from = to_workspace(address, args.workspace)
    if args.park:
        # On the real display the pointer is in the picture unless it is
        # parked; on a headless output it has just been walked to that
        # output's bar, and this is the walk home.
        park_cursor(args.park[0], args.park[1], primary)
    restore = False
    if args.fullscreen and not args.geometry and not primary:
        restore = fullscreen(address, True)
        if not restore:
            sys.stderr.write("guicast: could not maximise the window; "
                             "recording it where it is\n")

    geometry = args.geometry or window_geometry(APP_ID, args.pid)
    if primary:
        geometry = "the headless output"
    if not geometry:
        if restore:
            fullscreen(address, False)
        sys.exit("guicast: no %s window found. Start it, or pass --geometry "
                 "'X,Y WxH'." % APP_ID)

    into = tempfile.mkdtemp(prefix="guicast-")
    capture_fps = (1.0 / args.every) if args.every else float(args.fps)
    play_fps = args.play_fps or (args.fps if not args.every else 4)
    speed = play_fps / capture_fps
    print("guicast: %s at %s, %gs at %s%s"
          % (APP_ID, geometry, args.seconds,
             ("a frame every %gs" % args.every) if args.every
             else ("%d fps" % args.fps),
             "" if speed == 1 else ", played at %gx" % speed))
    try:
        frames = capture(geometry, args.seconds, args.fps, into, args.every,
                         headless if primary else None)
        if restore:
            fullscreen(address, False)
            restore = False
        if args.still and frames:
            last = os.path.join(into, "f%05d.png" % (frames - 1))
            os.makedirs(os.path.dirname(os.path.abspath(args.still)),
                        exist_ok=True)
            shutil.copyfile(last, args.still)
            print("guicast: %s -- the last frame, %d bytes"
                  % (args.still, os.path.getsize(args.still)))
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
            assemble(into, args.output, play_fps, w)
            size = os.path.getsize(args.output)
            print("guicast:   %d px wide -- %.1f MB" % (w, size / 1048576.0))
            if budget is None or size <= budget:
                break
        else:
            sys.stderr.write(
                "guicast: still %.1f MB at %d px -- record fewer seconds\n"
                % (size / 1048576.0, widths[-1]))
    finally:
        if primary:
            headless_stop(primary, headless, args.park)
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
