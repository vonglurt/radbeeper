#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Paul Richeson
"""The window's still photographs: both themes, and the squeezed layout.

WHY THESE THREE AND WHY THEY ARE A TOOL. `guicast.py` records the window
moving, which is what the top of the README wants. These are the ones a reader
compares: the same panel under the desktop's dark theme and under Antiquity's
light one -- which is the whole claim that the window follows the desktop -- and
the same panel squeezed into a quarter screen, which is the claim that the
charts are the last thing to go.

They used to be taken by hand, which meant they were taken when somebody
remembered, and a release could ship a picture of the version before it. A
release records its own pictures or it does not have any worth trusting.

EACH THEME IS A FRESH WINDOW, because the theme is read once at startup: the
panel asks the desktop what it is wearing when it opens and does not watch for
changes. So the shot for Antiquity is a window started with `--theme antiquity`,
not the dark one re-skinned.

Stdlib only, like everything else in tools/. grim takes the picture and hyprctl
says where the window is; both are named plainly if they are missing.
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import time

APP_ID = "radbeeper-gui"

# (theme, width, height, filename). The two comparison shots are the same size
# so the only difference between them is the one being demonstrated.
SHOTS = [
    ("dark", 900, 720, "gui-dark.png"),
    ("antiquity", 900, 720, "gui-antiquity.png"),
    ("dark", 560, 400, "gui-small.png"),
]


def need(program, why):
    if shutil.which(program) is None:
        sys.exit("guishots: %s is not installed -- %s" % (program, why))


def window(app_id):
    """The window's address, geometry and workspace, or None."""
    try:
        out = subprocess.run(["hyprctl", "clients", "-j"],
                             capture_output=True, text=True, timeout=10)
        for c in json.loads(out.stdout):
            if c.get("class") == app_id or c.get("initialClass") == app_id:
                return (c["address"],
                        "%d,%d %dx%d" % (c["at"][0], c["at"][1],
                                         c["size"][0], c["size"][1]),
                        c.get("workspace", {}).get("id"))
    except (OSError, ValueError, KeyError, subprocess.SubprocessError):
        pass
    return None, None, None


def showing():
    """Which workspace the monitor is actually displaying."""
    try:
        out = subprocess.run(["hyprctl", "activeworkspace", "-j"],
                             capture_output=True, text=True, timeout=10)
        return json.loads(out.stdout).get("id")
    except (OSError, ValueError, subprocess.SubprocessError):
        return None


def shoot(binary, theme, w, h, out, logs, settle, workspace):
    subprocess.run(["pkill", "-f", binary], capture_output=True)
    time.sleep(2)
    cmd = [binary, "--theme", theme]
    if logs:
        cmd += ["--logs", logs]
    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL)
    # The panel has to attach to the stream and fill its windows before it is
    # worth a picture: a shot taken at once is a shot of "no counter".
    address = None
    for _ in range(40):
        time.sleep(1)
        address, _geom, _ws = window(APP_ID)
        if address:
            break
    if not address:
        proc.terminate()
        return "no window appeared for --theme %s" % theme

    if workspace:
        subprocess.run(["hyprctl", "dispatch", "movetoworkspacesilent",
                        "%s,address:%s" % (workspace, address)],
                       capture_output=True)
        subprocess.run(["hyprctl", "dispatch", "workspace", str(workspace)],
                       capture_output=True)
        time.sleep(1)
    for verb, arg in (("setfloating", ""),
                      ("resizewindowpixel", "exact %d %d," % (w, h)),
                      ("movewindowpixel", "exact 20 40,")):
        subprocess.run(["hyprctl", "dispatch", verb,
                        "%saddress:%s" % (arg, address)], capture_output=True)
        time.sleep(0.4)

    # LET THE PANEL FILL BEFORE THE SHUTTER. The 3-second and 30-second
    # windows are what the dials read, and a picture taken before they have
    # anything in them is a picture of two needles on the stop.
    time.sleep(settle)
    _address, geom, ws = window(APP_ID)
    if not geom:
        proc.terminate()
        return "the window went away before the shot"
    # A SCREEN GRAB TAKES WHAT IS BEING COMPOSITED, NOT WHAT IS THERE. The
    # window's geometry is in global coordinates and grim will happily hand
    # back whatever is displayed at them -- so a window sitting on a workspace
    # nobody is looking at photographs as the terminal in front of it. That is
    # not a hypothetical: it is how this tool's first run produced three
    # pictures of a code editor.
    on = showing()
    if ws is not None and on is not None and ws != on:
        subprocess.run(["hyprctl", "dispatch", "workspace", str(ws)],
                       capture_output=True)
        time.sleep(1.5)
        on = showing()
        if ws != on:
            proc.terminate()
            return "the window is on workspace %s and %s is displayed" % (ws, on)
    subprocess.run(["grim", "-g", geom, out], check=True, capture_output=True)
    proc.terminate()
    time.sleep(1)
    return None


def main():
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("-o", "--into", default="docs/screenshots",
                   help="where the pictures go")
    p.add_argument("--binary", default="gui/target/release/radbeeper-gui")
    p.add_argument("--logs", help="a log directory, if not the default")
    p.add_argument("--settle", type=float, default=45.0,
                   help="seconds to let the windows fill before each shot")
    p.add_argument("--workspace", help="borrow this workspace for the shots")
    args = p.parse_args()

    need("grim", "it is what takes the screen grabs")
    need("hyprctl", "it is what says where the window is")
    if not os.environ.get("WAYLAND_DISPLAY"):
        sys.exit("guishots: no WAYLAND_DISPLAY -- this photographs a Wayland window")
    if not os.path.exists(args.binary):
        sys.exit("guishots: no %s -- run `make gui-build`" % args.binary)

    os.makedirs(args.into, exist_ok=True)
    here = None
    if args.workspace:
        try:
            out = subprocess.run(["hyprctl", "activeworkspace", "-j"],
                                 capture_output=True, text=True, timeout=10)
            here = json.loads(out.stdout).get("id")
        except (OSError, ValueError, subprocess.SubprocessError):
            here = None

    bad = 0
    for theme, w, h, name in SHOTS:
        out = os.path.join(args.into, name)
        print("guishots: %s -- %s at %dx%d" % (name, theme, w, h))
        why = shoot(args.binary, theme, w, h, out, args.logs, args.settle,
                    args.workspace)
        if why:
            print("  SKIPPED %s -- %s" % (name, why))
            bad += 1
        else:
            print("  %s -- %.1f KB" % (out, os.path.getsize(out) / 1024.0))
    if here is not None:
        subprocess.run(["hyprctl", "dispatch", "workspace", str(here)],
                       capture_output=True)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
