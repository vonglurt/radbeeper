#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Paul Richeson
"""Record a terminal session to a cast file, and render casts to PNG and GIF.

Stdlib only, like everything else here. A cast is one JSON header line
followed by one ["t", "<base64 bytes>"] line per read, which is enough to
replay the session exactly and to stop the clock wherever a still is wanted.

    tools/record.py capture out.cast -- ./radbeeper watch
    tools/record.py frames  out.cast -o docs/screenshots/x.png --at 12.0
    tools/record.py gif     out.cast -o docs/screenshots/x.gif --from 300 --to 320
"""
import argparse
import base64
import json
import os
import pty
import select
import signal
import sys
import termios
import time
import tty


def capture(cmd, path, cols, rows, seconds, env_extra=None):
    """Run cmd under a pty of exactly cols x rows and log every byte read."""
    env = dict(os.environ)
    env.update({
        "TERM": "xterm-256color",
        "COLORTERM": "truecolor",
        "LINES": str(rows),
        "COLUMNS": str(cols),
    })
    env.update(env_extra or {})

    pid, fd = pty.fork()
    if pid == 0:
        os.execvpe(cmd[0], cmd, env)
        os._exit(127)

    # The size has to be set on the master, and before the child draws.
    import fcntl
    import struct
    fcntl.ioctl(fd, termios.TIOCSWINSZ,
                struct.pack("HHHH", rows, cols, 0, 0))

    started = time.time()
    out = open(path, "w")
    out.write(json.dumps({"version": 1, "width": cols, "height": rows,
                          "command": " ".join(cmd),
                          "recorded": int(started)}) + "\n")
    try:
        while True:
            now = time.time()
            if seconds and now - started >= seconds:
                break
            r, _, _ = select.select([fd], [], [], 0.05)
            if fd not in r:
                continue
            try:
                data = os.read(fd, 65536)
            except OSError:
                break
            if not data:
                break
            out.write(json.dumps([round(now - started, 4),
                                  base64.b64encode(data).decode()]) + "\n")
    finally:
        out.close()
        try:
            os.kill(pid, signal.SIGINT)
            time.sleep(0.3)
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        try:
            os.waitpid(pid, 0)
        except ChildProcessError:
            pass
    return path


def read_cast(path):
    with open(path) as f:
        head = json.loads(f.readline())
        events = []
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                t, b = json.loads(line)
            except ValueError:
                break        # a capture still being written; take what is there
            events.append((t, base64.b64decode(b)))
    return head, events




# ------------------------------------------------------------------ term ---
# Only what the inventory says is actually on the wire. `tools/record.py
# inventory <cast>` prints that list, and if a future ncurses starts emitting
# something new it will show up there rather than as a silently wrong pixel.
#
# Tokyo Night, because that is what every screenshot in docs/ is already in.
PALETTE = {
    "bg":      (0x1a, 0x1b, 0x26),
    "fg":      (0xc0, 0xca, 0xf5),
    0:         (0x41, 0x48, 0x68),   # black
    1:         (0xf7, 0x76, 0x8e),   # red
    2:         (0x9e, 0xce, 0x6a),   # green
    3:         (0xe0, 0xaf, 0x68),   # yellow
    4:         (0x7a, 0xa2, 0xf7),   # blue
    5:         (0xbb, 0x9a, 0xf7),   # magenta
    6:         (0x7d, 0xcf, 0xff),   # cyan
    7:         (0xc0, 0xca, 0xf5),   # white
}


def _blend(c, bg, a):
    return tuple(int(round(x * a + y * (1 - a))) for x, y in zip(c, bg))


class Cell:
    __slots__ = ("ch", "fg", "bold", "dim")

    def __init__(self):
        self.ch = " "
        self.fg = None          # None means the default foreground
        self.bold = False
        self.dim = False


class Screen:
    """Enough of a VT to replay what curses and plain prints put on the wire."""

    def __init__(self, cols, rows):
        self.cols, self.rows = cols, rows
        self.grid = [[Cell() for _ in range(cols)] for _ in range(rows)]
        self.x = self.y = 0
        # DECSTBM. ncurses sets a region at start-up and then scrolls inside
        # it when that is cheaper than redrawing, which is what CSI S and
        # CSI T in a cast are. Ignoring them puts every row below the scroll
        # point in the wrong place -- visibly, and only on busy screens.
        self.top = 0
        self.bot = self.rows - 1
        self.fg = None
        self.bold = self.dim = False
        self.buf = b""

    # -- writing ------------------------------------------------------------
    def _cell(self):
        return self.grid[self.y][self.x]

    def _put(self, ch):
        if self.x >= self.cols:
            self.x = 0
            self._down()
        c = self._cell()
        c.ch, c.fg, c.bold, c.dim = ch, self.fg, self.bold, self.dim
        self.x += 1

    def _blank_row(self):
        return [Cell() for _ in range(self.cols)]

    def _scroll_up(self, n=1):
        for _ in range(n):
            del self.grid[self.top]
            self.grid.insert(self.bot, self._blank_row())

    def _scroll_down(self, n=1):
        for _ in range(n):
            del self.grid[self.bot]
            self.grid.insert(self.top, self._blank_row())

    def _down(self):
        if self.y == self.bot:
            self._scroll_up()
        elif self.y + 1 < self.rows:
            self.y += 1

    def _blank(self, y, x0, x1):
        for x in range(max(0, x0), min(self.cols, x1)):
            self.grid[y][x] = Cell()

    # -- SGR ----------------------------------------------------------------
    def _sgr(self, params):
        if not params:
            params = [0]
        i = 0
        while i < len(params):
            p = params[i]
            if p == 0:
                self.fg, self.bold, self.dim = None, False, False
            elif p == 1:
                self.bold = True
            elif p == 2:
                self.dim = True
            elif p == 22:
                self.bold = self.dim = False
            elif 30 <= p <= 37:
                self.fg = p - 30
            elif 90 <= p <= 97:
                self.fg = p - 90
                self.bold = True
            elif p == 39:
                self.fg = None
            elif p == 38 and i + 2 < len(params) and params[i + 1] == 5:
                self.fg = params[i + 2]
                i += 2
            # Backgrounds are never set by anything here; the page is one
            # colour behind everything and stays that way.
            i += 1

    # -- the stream ---------------------------------------------------------
    def feed(self, data):
        self.buf += data
        b = self.buf
        i = 0
        n = len(b)
        while i < n:
            ch = b[i]
            if ch == 0x1b:
                j = self._escape(b, i)
                if j is None:            # incomplete, wait for more bytes
                    break
                i = j
                continue
            i += 1
            if ch == 0x0d:
                self.x = 0
            elif ch == 0x0a:
                self.x = 0
                self._down()
            elif ch == 0x08:
                self.x = max(0, self.x - 1)
            elif ch == 0x09:
                self.x = min(self.cols - 1, (self.x // 8 + 1) * 8)
            elif ch >= 0x20:
                # UTF-8, decoded a character at a time so a split multi-byte
                # sequence at the end of a read waits rather than corrupting.
                need = (1 if ch < 0x80 else
                        2 if ch >= 0xf0 else 0)
                if ch < 0x80:
                    self._put(chr(ch))
                else:
                    length = (4 if ch >= 0xf0 else 3 if ch >= 0xe0 else 2)
                    if i - 1 + length > n:
                        i -= 1
                        break
                    try:
                        self._put(b[i - 1:i - 1 + length].decode("utf-8"))
                    except UnicodeDecodeError:
                        self._put("?")
                    i += length - 1
        self.buf = b[i:]

    def _escape(self, b, i):
        n = len(b)
        if i + 1 >= n:
            return None
        k = b[i + 1]
        if k == 0x5b:                                    # CSI
            j = i + 2
            while j < n and (0x30 <= b[j] <= 0x3f or 0x20 <= b[j] <= 0x2f):
                j += 1
            if j >= n:
                return None
            final = b[j]
            body = b[i + 2:j].decode("latin1")
            self._csi(body, chr(final))
            return j + 1
        if k in (0x28, 0x29):                            # ESC(B, charset
            return i + 3 if i + 2 < n else None
        if k in (0x3d, 0x3e, 0x37, 0x38, 0x63):          # keypad, save, RIS
            return i + 2
        if k == 0x4d:                                    # RI
            self.y = max(0, self.y - 1)
            return i + 2
        return i + 2

    def _csi(self, body, final):
        private = body.startswith("?")
        raw = body[1:] if private else body
        parts = [p for p in raw.split(";")]
        nums = [int(p) if p.isdigit() else 0 for p in parts]

        def arg(k, default=1):
            v = nums[k] if k < len(nums) else 0
            return v if v else default

        if private:
            return                       # ?1049h, ?25l and friends: no pixels
        if final == "H" or final == "f":
            self.y = min(self.rows - 1, arg(0) - 1)
            self.x = min(self.cols - 1, arg(1) - 1)
        elif final == "A":
            self.y = max(0, self.y - arg(0))
        elif final == "B":
            self.y = min(self.rows - 1, self.y + arg(0))
        elif final == "C":
            self.x = min(self.cols - 1, self.x + arg(0))
        elif final == "D":
            self.x = max(0, self.x - arg(0))
        elif final == "d":
            self.y = min(self.rows - 1, arg(0) - 1)
        elif final == "G":
            self.x = min(self.cols - 1, arg(0) - 1)
        elif final == "m":
            self._sgr(nums if raw else [0])
        elif final == "J":
            mode = nums[0] if nums else 0
            if mode == 0:
                self._blank(self.y, self.x, self.cols)
                for y in range(self.y + 1, self.rows):
                    self._blank(y, 0, self.cols)
            elif mode == 1:
                for y in range(0, self.y):
                    self._blank(y, 0, self.cols)
                self._blank(self.y, 0, self.x + 1)
            else:
                for y in range(self.rows):
                    self._blank(y, 0, self.cols)
        elif final == "K":
            mode = nums[0] if nums else 0
            if mode == 0:
                self._blank(self.y, self.x, self.cols)
            elif mode == 1:
                self._blank(self.y, 0, self.x + 1)
            else:
                self._blank(self.y, 0, self.cols)
        elif final == "X":
            self._blank(self.y, self.x, self.x + arg(0))
        elif final == "P":
            row = self.grid[self.y]
            k = arg(0)
            del row[self.x:self.x + k]
            row.extend(Cell() for _ in range(k))
        elif final == "@":
            row = self.grid[self.y]
            k = arg(0)
            for _ in range(k):
                row.insert(self.x, Cell())
            del row[self.cols:]
        elif final == "L" or final == "M":
            # Insert and delete line act on the scroll region, and do nothing
            # at all with the cursor outside it.
            if not (self.top <= self.y <= self.bot):
                return
            for _ in range(arg(0)):
                if final == "L":
                    del self.grid[self.bot]
                    self.grid.insert(self.y, self._blank_row())
                else:
                    del self.grid[self.y]
                    self.grid.insert(self.bot, self._blank_row())
        elif final == "S":
            self._scroll_up(arg(0))
        elif final == "T":
            self._scroll_down(arg(0))
        elif final == "r":
            top = arg(0) - 1
            bot = (nums[1] - 1) if len(nums) > 1 and nums[1] else self.rows - 1
            if 0 <= top < bot < self.rows:
                self.top, self.bot = top, bot
            else:
                self.top, self.bot = 0, self.rows - 1
            # DECSTBM homes the cursor, and ncurses relies on that.
            self.y, self.x = self.top, 0
        # t (window ops) and l/h (modes): nothing to draw.

    # -- what is on it ------------------------------------------------------
    def used_rows(self):
        last = 0
        for y in range(self.rows):
            if any(c.ch != " " for c in self.grid[y]):
                last = y + 1
        return last

    def text(self):
        return "\n".join("".join(c.ch for c in row).rstrip()
                         for row in self.grid[:self.used_rows()])


# ----------------------------------------------------------------- pixels ---
FONT_CANDIDATES = [
    "/usr/share/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf",
    "/usr/share/fonts/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/liberation/LiberationMono-Regular.ttf",
]
FONT_BOLD = [
    "/usr/share/fonts/jetbrains-mono/JetBrainsMono-Bold.ttf",
    "/usr/share/fonts/dejavu/DejaVuSansMono-Bold.ttf",
    "/usr/share/fonts/liberation/LiberationMono-Bold.ttf",
]


def _font(paths, size):
    from PIL import ImageFont
    for p in paths:
        if os.path.exists(p):
            return ImageFont.truetype(p, size)
    raise SystemExit("no monospace font found; looked in %s" % paths)


class Renderer:
    """One cell, one glyph, and a block character that actually fills it.

    The bar charts and the twelve-row digits are drawn out of U+2588 FULL
    BLOCK. Fonts leave a hairline between adjacent blocks at most sizes, and
    a chart drawn out of hairline-separated bars is a different picture from
    the one on the terminal -- so blocks are painted as rectangles and never
    asked of the font.
    """

    # SPARK = " \u2581..\u2588" in the program: eighths of a cell, filled from
    # the bottom. Each maps to the fraction of the cell it covers.
    BLOCKS = {chr(0x2580 + k): (1.0 - k / 8.0, 1.0) for k in range(1, 9)}
    BLOCKS["\u2580"] = (0.0, 0.5)          # upper half

    def __init__(self, size=20, pad=10, radius=10, line=1.075):
        from PIL import ImageDraw  # noqa: F401  (import-time check)
        self.font = _font(FONT_CANDIDATES, size)
        self.bold = _font(FONT_BOLD, size)
        self.cw = self.font.getlength("M")
        self.ch = int(round(size * line))
        self.pad = pad
        self.radius = radius
        self.size = size

    def colour(self, cell):
        base = PALETTE["fg"] if cell.fg is None else PALETTE.get(
            cell.fg, PALETTE["fg"])
        if cell.dim:
            return _blend(base, PALETTE["bg"], 0.5)
        return base

    def image(self, screen, rows=None, cursor=False, top=0):
        from PIL import Image, ImageDraw
        rows = rows or screen.rows
        w = int(round(self.cw * screen.cols)) + 2 * self.pad
        h = self.ch * rows + 2 * self.pad
        img = Image.new("RGB", (w, h), PALETTE["bg"])
        d = ImageDraw.Draw(img)
        for y0 in range(rows):
            y = y0 + top
            if y >= screen.rows:
                break
            for x, cell in enumerate(screen.grid[y]):
                if cell.ch == " ":
                    continue
                px = self.pad + x * self.cw
                py = self.pad + y0 * self.ch
                col = self.colour(cell)
                span = self.BLOCKS.get(cell.ch)
                if span is not None:
                    # Named apart from the `top` parameter on purpose: this
                    # one is a pixel edge, that one is a row number.
                    edge = py + span[0] * self.ch
                    foot = py + span[1] * self.ch
                    d.rectangle([px, edge, px + self.cw, foot], fill=col)
                    continue
                f = self.bold if cell.bold else self.font
                d.text((px, py + (self.ch - self.size) / 2 - self.size * 0.11),
                       cell.ch, font=f, fill=col)
        if cursor and top <= screen.y < top + rows:
            px = self.pad + screen.x * self.cw
            py = self.pad + (screen.y - top) * self.ch
            d.rectangle([px, py + 1, px + self.cw * 0.55, py + self.ch - 1],
                        fill=_blend(PALETTE["fg"], PALETTE["bg"], 0.75))
        if self.radius:
            self._round(img)
        return img

    def _round(self, img):
        from PIL import Image, ImageDraw
        mask = Image.new("L", img.size, 0)
        ImageDraw.Draw(mask).rounded_rectangle(
            [0, 0, img.size[0] - 1, img.size[1] - 1], self.radius, fill=255)
        bg = Image.new("RGB", img.size, PALETTE["bg"])
        # The corners become the page behind them, which for both GitHub
        # themes and the exported page is near enough this same colour.
        img.paste(bg, (0, 0), Image.eval(mask, lambda v: 255 - v))


def replay(head, events, until):
    s = Screen(head["width"], head["height"])
    for t, data in events:
        if t > until:
            break
        s.feed(data)
    return s


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    sub = ap.add_subparsers(dest="cmd", required=True)

    c = sub.add_parser("capture", help="record a command to a cast")
    c.add_argument("out")
    c.add_argument("--cols", type=int, default=100)
    c.add_argument("--rows", type=int, default=30)
    c.add_argument("--seconds", type=float, default=0)

    i = sub.add_parser("inventory", help="what escapes the cast contains")
    i.add_argument("cast")

    x = sub.add_parser("text", help="the screen as text, for eyeballing")
    x.add_argument("cast")
    x.add_argument("--at", type=float, default=1e9)

    p = sub.add_parser("still", help="one PNG at one moment")
    p.add_argument("cast")
    p.add_argument("-o", "--out", required=True)
    p.add_argument("--at", type=float, default=1e9)
    p.add_argument("--rows", type=int, default=0, help="0 = trim to content")
    p.add_argument("--size", type=int, default=20)
    p.add_argument("--cursor", action="store_true")
    p.add_argument("--top", type=int, default=0,
                   help="first row to draw, for a strip out of the middle")

    g = sub.add_parser("gif", help="an animation between two moments")
    g.add_argument("cast")
    g.add_argument("-o", "--out", required=True)
    g.add_argument("--from", dest="start", type=float, required=True)
    g.add_argument("--to", dest="stop", type=float, required=True)
    g.add_argument("--step", type=float, default=1.0,
                   help="seconds of session per frame")
    g.add_argument("--speed", type=float, default=10.0)
    g.add_argument("--rows", type=int, default=0)
    g.add_argument("--size", type=int, default=16)

    argv_all = sys.argv[1:]
    child = []
    if "--" in argv_all:
        cut = argv_all.index("--")
        child = argv_all[cut + 1:]
        argv_all = argv_all[:cut]
    a = ap.parse_args(argv_all)

    if a.cmd == "capture":
        if not child:
            ap.error("give a command after --")
        capture(child, a.out, a.cols, a.rows, a.seconds)
        head, ev = read_cast(a.out)
        print("%s: %d events, %d bytes, %.1f s"
              % (a.out, len(ev), sum(len(b) for _, b in ev),
                 ev[-1][0] if ev else 0))
        return

    head, ev = read_cast(a.cast)

    if a.cmd == "inventory":
        import collections
        import re
        blob = b"".join(b for _, b in ev)
        seqs = collections.Counter()
        for m in re.finditer(
                rb"\x1b(\[[0-9;?]*[@-~]|[()][A-Za-z0-9]|[=>78MDEHc])", blob):
            seqs[re.sub(rb"[0-9]+", b"N", m.group(0))] += 1
        print("%s  %dx%d  %.1f s  %d bytes"
              % (a.cast, head["width"], head["height"],
                 ev[-1][0] if ev else 0, len(blob)))
        for s, n in seqs.most_common():
            print("  %-16s %6d" % (s.decode("latin1").replace("\x1b", "ESC"), n))
        ctrl = collections.Counter(bytes([b]) for b in blob
                                   if b < 0x20 and b != 0x1b)
        for s, n in ctrl.most_common():
            print("  %-16s %6d" % (repr(s), n))
        return

    if a.cmd == "text":
        print(replay(head, ev, a.at).text())
        return

    if a.cmd == "still":
        s = replay(head, ev, a.at)
        r = Renderer(size=a.size)
        rows = a.rows or max(1, s.used_rows() - a.top)
        img = r.image(s, rows, cursor=a.cursor, top=a.top)
        img.save(a.out)
        print("%s  %s" % (a.out, "x".join(map(str, img.size))))
        return

    if a.cmd == "gif":
        r = Renderer(size=a.size)
        frames = []
        t = a.start
        rows = a.rows or head["height"]
        while t <= a.stop + 1e-9:
            frames.append(r.image(replay(head, ev, t), rows))
            t += a.step
        ms = int(round(a.step * 1000 / a.speed))
        frames[0].save(a.out, save_all=True, append_images=frames[1:],
                       duration=ms, loop=0, optimize=True)
        print("%s  %d frames, %d ms each, %s"
              % (a.out, len(frames), ms,
                 "x".join(map(str, frames[0].size))))
        return


if __name__ == "__main__":
    main()
