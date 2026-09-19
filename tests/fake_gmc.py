# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Paul Richeson
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


def build_history(seconds=120, cpm=30.0, seed=5, size=4096,
                  interval=1.011, per_mark=20, start=(26, 9, 4, 11, 0, 0)):
    """A flash image in the shape the real one has.

    Timestamp markers every `per_mark` samples, one byte per sample in
    between, and a two-byte marker wherever the count does not fit in a byte
    -- the case the decoder is most likely to get wrong, so the fixture makes
    sure it happens.

    THE DATETIME RECORD IS NINE BYTES, no save-mode byte, and it is followed
    by a bare `55 AA 01` -- that is what a GMC-320Re 4.26 writes, measured over
    a full 1 MiB image where all 4,709 of them sat exactly nine bytes after a
    timestamp and nowhere else. The fixture used to write a ten-byte datetime
    and to use `55 AA 01` as a two-byte count, and so agreed with the decoder's
    bugs instead of with the hardware.

    `interval` is the counter's true seconds per sample and defaults to 1.011,
    which is what the unit on the bench measures. A fixture that ticked exactly
    once a second would let an assumed interval pass every test and then place
    real samples a minute out.

    `per_mark` is 20 rather than the device's 180 so that a short fixture still
    contains two marks: one mark measures no interval, and a decoder given one
    mark can place nothing -- correct, but useless to test against.
    """
    rng = random.Random(seed)
    out = bytearray()
    t = time.mktime((2000 + start[0],) + start[1:] + (0, 1, -1))
    written = 0
    while written < seconds:
        stamp = time.localtime(t)
        out += bytes([0x55, 0xAA, 0x00, stamp.tm_year - 2000, stamp.tm_mon,
                      stamp.tm_mday, stamp.tm_hour, stamp.tm_min, stamp.tm_sec])
        out += bytes([0x55, 0xAA, 0x01])   # follows every timestamp
        n_here = min(per_mark, seconds - written)
        for _ in range(n_here):
            # 254, not 255: 0xFF is unwritten flash and the decoder skips
            # it, so a single byte cannot express a count of 255 at all.
            n = max(0, min(254, int(rng.gauss(cpm, cpm / 3))))
            out.append(n)
        written += n_here
        # The RTC has one-second resolution, so the mark it writes is the
        # rounded truth -- which is exactly why the interval has to be
        # measured over many samples rather than read off one gap.
        t = round(t + n_here * interval)
    out += bytes([0x55, 0xAA, 0x02, 5]) + b"note!"
    out += b"\xff" * max(0, size - len(out))
    return bytes(out[:size])


FLASH_SIZE = 0x100000     # a GMC-320's history flash


class FakeGMC(threading.Thread):
    daemon = True

    def __init__(self, cpm=30.0, seed=3, history=None, tick=1.0, serial=None):
        super().__init__()
        # A SERIAL OF ITS OWN, so two of these are two INSTRUMENTS. radbeeper
        # keys a counter's log, its colour and its slot in the bank off the
        # serial, and treats a serial it already knows as that instrument
        # coming back -- which is right for a tube replugged and wrong for two
        # fakes, who without this both claim to be 123456789ABCDE and collapse
        # into one counter the moment a multi-counter test looks at them.
        self.serial = serial if serial is not None else SERIAL
        self.master, self.slave = pty.openpty()
        self.path = os.ttyname(self.slave)
        self.rate = cpm / 60.0
        self.random = random.Random(seed)
        self.history = history if history is not None else build_history()
        self.tick = tick
        self.heartbeat = False
        # A beat queued just ahead of every reply while the stream is on: the
        # worst case of a race the real device loses often enough to matter.
        self.interleave = False
        # Seconds its real-time clock runs ahead of this machine's; negative
        # is behind. <SETDATETIME>> sets it, as setting the real one does.
        self.clock_ahead = 0.0
        # How many <SPIR>> replies to cut short before answering whole: a
        # link that drops bytes now and then, which the reader must survive.
        self.short_spir = 0
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
        if self.heartbeat and self.interleave:
            self._send(struct.pack(">H", self._draw() | 0x8000))
        if body == b"GETVER":
            self._send(VERSION)
        elif body == b"GETSERIAL":
            self._send(self.serial)
        elif body == b"GETCPM":
            self._send(struct.pack(">H", int(self.rate * 60)))
        elif body == b"GETCPS":
            self._send(struct.pack(">H", self._draw() | 0x8000))
        elif body == b"GETVOLT":
            self._send(bytes([39]))
        elif body == b"GETDATETIME":
            t = time.localtime(time.time() + self.clock_ahead)
            self._send(bytes([t.tm_year - 2000, t.tm_mon, t.tm_mday,
                              t.tm_hour, t.tm_min, t.tm_sec, 0xAA]))
        elif body.startswith(b"SETDATETIME") and len(body) == 17:
            y, mo, d, h, mi, s = body[11:17]
            when = time.mktime((2000 + y, mo, d, h, mi, s, 0, 0, -1))
            self.clock_ahead = when - time.time()
            self._send(b"\xaa")
        elif body == b"HEARTBEAT1":
            self.heartbeat = True
        elif body == b"HEARTBEAT0":
            self.heartbeat = False
        elif body.startswith(b"SPIR"):
            args = body[4:]
            if len(args) == 5:
                addr = (args[0] << 16) | (args[1] << 8) | args[2]
                length = (args[3] << 8) | args[4]
                # A real 320 answers every address in its megabyte of flash,
                # and unwritten flash reads as 0xFF: a reply is only short
                # when the link loses bytes, never because the history ends.
                reply = self.history[addr:addr + length]
                if addr + length <= FLASH_SIZE:
                    reply = reply + b"\xff" * (length - len(reply))
                if self.short_spir > 0:
                    self.short_spir -= 1
                    reply = reply[:len(reply) // 3]
                self._send(reply)


def main():
    import argparse
    p = argparse.ArgumentParser(description="a fake GMC-320 on a pty")
    p.add_argument("--cpm", type=float, default=30.0)
    p.add_argument("--seed", type=int, default=3)
    p.add_argument("--serial", help="14 hex digits; two fakes need two of these")
    args = p.parse_args()
    serial = bytes.fromhex(args.serial) if args.serial else None
    dev = FakeGMC(cpm=args.cpm, seed=args.seed, serial=serial)
    dev.start()
    print("fake GMC-320 on %s  (%.0f CPM, serial %s)"
          % (dev.path, args.cpm, dev.serial.hex().upper()))
    print("point radbeeper at it:  radbeeper -d %s watch" % dev.path)
    try:
        while True:
            time.sleep(1)
    except KeyboardInterrupt:
        dev.stop()


if __name__ == "__main__":
    main()
