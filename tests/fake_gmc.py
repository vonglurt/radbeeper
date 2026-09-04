# SPDX-License-Identifier: MIT
"""A fake GQ GMC-320 on a pseudo-terminal.

WHY THIS EXISTS. --source sim proves the averaging and the display, and proves
nothing at all about the serial code: it replaces the device *and* the wire.
Everything between os.open and struct.unpack -- the termios setup, the
exact-length reads, the command framing, the heartbeat stream, the chunked
SPIR download -- was untested until this, and that is the half that talks to
the hardware nobody has on the desk today.

A pty is a real serial file descriptor. termios configures it, select polls
it, short reads happen on it. Pointing radbeeper at one exercises the whole
path with only the glass and the tube missing.

Standalone, it prints the device path and serves until interrupted, so a
monitor can be pointed at a counter that is not there:

    python3 tests/fake_gmc.py --cpm 400
    radbeeper -d /dev/pts/N watch
"""
import os
import pty
import random
import select
import struct
import threading
import time

VERSION = b"GMC-320Re 4.26"
SERIAL = bytes([0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE])


def build_history(seconds=120, cpm=30.0, seed=5, size=4096):
    """A flash image in the shape the real one has.

    A timestamp marker, then one byte per saved sample, with a two-byte
    marker wherever the count does not fit in a byte -- which is the case
    the decoder is most likely to get wrong, so the fixture makes sure it
    happens.
    """
    rng = random.Random(seed)
    out = bytearray()
    out += bytes([0x55, 0xAA, 0x00, 26, 9, 4, 11, 0, 0, 2])   # mode 2, per minute
    for _ in range(seconds):
        n = max(0, int(rng.gauss(cpm, cpm / 3)))
        if n > 255:
            out += bytes([0x55, 0xAA, 0x01]) + struct.pack(">H", n)
        else:
            out.append(n)
    out += bytes([0x55, 0xAA, 0x02, 5]) + b"note!"
    out += b"\xff" * max(0, size - len(out))
    return bytes(out[:size])


class FakeGMC(threading.Thread):
    daemon = True

    def __init__(self, cpm=30.0, seed=3, history=None, tick=1.0):
        super().__init__()
        self.master, self.slave = pty.openpty()
        self.path = os.ttyname(self.slave)
        self.rate = cpm / 60.0
        self.random = random.Random(seed)
        self.history = history if history is not None else build_history()
        self.tick = tick
        self.heartbeat = False
        self.running = True
        self.commands = []

    def _draw(self):
        L = pow(2.718281828459045, -self.rate)
        k, p = 0, 1.0
        while True:
            p *= self.random.random()
            if p <= L:
                return k
            k += 1

    def stop(self):
        self.running = False
        if self.is_alive():
            self.join(timeout=3)
        for fd in (self.master, self.slave):
            try:
                os.close(fd)
            except OSError:
                pass

    def run(self):
        buf = b""
        next_beat = time.monotonic() + self.tick
        while self.running:
            timeout = max(0.02, next_beat - time.monotonic()) if self.heartbeat else 0.1
            r, _, _ = select.select([self.master], [], [], timeout)
            if r:
                try:
                    buf += os.read(self.master, 256)
                except OSError:
                    break
                buf = self._consume(buf)
            if self.heartbeat and time.monotonic() >= next_beat:
                # The real device sets flag bits in the top two; masking them
                # off is the receiver's job and this makes sure it does it.
                value = self._draw() | 0x8000
                self._send(struct.pack(">H", value & 0xFFFF))
                next_beat += self.tick

    def _send(self, data):
        try:
            os.write(self.master, data)
        except OSError:
            pass

    def _consume(self, buf):
        while True:
            start = buf.find(b"<")
            if start < 0:
                return b""
            end = buf.find(b">>", start)
            if end < 0:
                return buf[start:]
            cmd = buf[start:end + 2]
            buf = buf[end + 2:]
            self._handle(cmd)

    def _handle(self, cmd):
        self.commands.append(cmd)
        body = cmd[1:-2]
        if body == b"GETVER":
            self._send(VERSION)
        elif body == b"GETSERIAL":
            self._send(SERIAL)
        elif body == b"GETCPM":
            self._send(struct.pack(">H", int(self.rate * 60)))
        elif body == b"GETCPS":
            self._send(struct.pack(">H", self._draw() | 0x8000))
        elif body == b"GETVOLT":
            self._send(bytes([39]))
        elif body == b"GETDATETIME":
            t = time.localtime()
            self._send(bytes([t.tm_year - 2000, t.tm_mon, t.tm_mday,
                              t.tm_hour, t.tm_min, t.tm_sec, 0xAA]))
        elif body == b"HEARTBEAT1":
            self.heartbeat = True
        elif body == b"HEARTBEAT0":
            self.heartbeat = False
        elif body.startswith(b"SPIR"):
            args = body[4:]
            if len(args) == 5:
                addr = (args[0] << 16) | (args[1] << 8) | args[2]
                length = (args[3] << 8) | args[4]
                self._send(self.history[addr:addr + length])


def main():
    import argparse
    p = argparse.ArgumentParser(description="a fake GMC-320 on a pty")
    p.add_argument("--cpm", type=float, default=30.0)
    p.add_argument("--seed", type=int, default=3)
    args = p.parse_args()
    dev = FakeGMC(cpm=args.cpm, seed=args.seed)
    dev.start()
    print("fake GMC-320 on %s  (%.0f CPM)" % (dev.path, args.cpm))
    print("point radbeeper at it:  radbeeper -d %s watch" % dev.path)
    try:
        while True:
            time.sleep(1)
    except KeyboardInterrupt:
        dev.stop()


if __name__ == "__main__":
    main()
