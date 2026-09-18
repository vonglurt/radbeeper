<!-- SPDX-License-Identifier: MIT — Copyright (c) 2026 Paul Richeson -->

# Reference

The parts of RadBeeper worth looking up rather than reading: the tube
factor, the counter's protocol, what it costs to run, how it is tested, and
how the screenshots are made.

## The tube factor

µSv/h is CPM divided by a number that belongs to the **tube**, not the counter.
The default, 151.5, is the M4011 in a GMC-320. A 500 with a different tube needs a
different number, which is why it is `--cpm-per-usvh` and not a constant buried in
the arithmetic.

## Protocol

Commands are ASCII `<NAME>>`; replies are raw bytes with no framing, so every read
asks for an exact count and times out rather than blocking.

| Command | Reply |
|---|---|
| `<GETVER>>` | 14 bytes, e.g. `GMC-320Re 4.26` |
| `<GETSERIAL>>` | 7 bytes |
| `<GETCPM>>` | 2 bytes, big-endian |
| `<GETCPS>>` | 2 bytes, mask `0x3FFF` |
| `<HEARTBEAT1>>` | then 2 bytes every second until `<HEARTBEAT0>>` |
| `<GETVOLT>>` | 1 byte, tenths of a volt |
| `<GETDATETIME>>` | 7 bytes |
| `<SPIR[addr][len]>>` | `len` bytes of history flash |

Baud is 115200 on the 320 and 57600 on the 300; RadBeeper tries both.

There is no `pyserial`. Alpine packages it, but this runs on a Pi Zero with 512 MB
and on a fresh install with no network, and a serial port is thirty lines of
`termios`.

**Two corrections to GQ's published history format**, both measured against a full
1 MiB image from a GMC-320Re 4.26. The datetime record is **nine** bytes, not ten.
And `55 AA 01` is a **three-byte marker carrying no payload**, not a two-byte
count: reading it as one invented 1,701 readings between 256 and 21,930 counts per
second on a tube that saturates three orders of magnitude below that. `log pull`
writes the raw flash image before decoding it, which is why both were fixable
against data already on disk.

## Speed

The monitor's budget is one sample a second and it uses a fraction of a percent of
it — a 512-point FFT is 0.5 ms, once every few minutes. The one place that ever
mattered was `backfill`, and it turned out to be an algorithm rather than a
language: each window re-summed its own tail on every sample, O(samples × window),
which cost 16.5 s to turn 850,000 samples into 28,000 rows — 765 million
additions. Keeping a running sum and subtracting what falls out the back is O(1)
per sample. **Same output, byte for byte, in 2.3 s instead of 16.5.**

## Tests

```sh
make check        # syntax, then 208 tests: no hardware, no network
```

`tests/fake_gmc.py` serves a fake GMC-320 on a pseudo-terminal, so the serial path
— termios, exact-length reads, command framing, the heartbeat stream, the chunked
history download — is tested without a counter on the desk. It also runs
standalone:

```sh
python3 tests/fake_gmc.py --cpm 400
radbeeper -d /dev/pts/N watch
```

The full-screen monitor is tested too, on a 132×46 pty, because it only runs when
stdout is a terminal and two bugs shipped in the part nothing was executing.

## The screenshots

Every image in `docs/screenshots` is a recording, not a picture. `tools/record.py`
runs the program under a pseudo-terminal, logs every byte it writes with a
timestamp, and replays that through a small VT emulator into a grid of cells,
which is then drawn with one rectangle per block glyph and one glyph per
character. `tools/promo.py` drives it.

```sh
make promo                 # all of them: needs the counter plugged in, ~7 min
make promo-fast            # the same, reusing the last monitor recording
make promo SHOTS=probe     # just one
```

### The window is pixels, because it has to be

That whole apparatus records a **terminal**: it keeps the bytes a program
writes to a pty, which is exact, tiny, and no use at all for a window. A
Wayland surface has no byte stream to keep. So `tools/guicast.py` grabs frames
with `grim` and assembles them with `ffmpeg`, and those are the only two
outside programs in `tools/`.

```sh
make gui                                    # the window has to be open
make gui-gif                                # 24 s of it, into docs/screenshots/gui.gif
make gui-gif GIFOUT=docs/screenshots/gui-pair.gif GIFSECS=30
```

Two frames a second, played back at two frames a second: the instrument
updates once a second, so the recording is real time and the clock in it can
be read. The window is *found* rather than guessed -- `radbeeper-gui` sets an
application id, so the compositor can be asked where it is -- and `--geometry
'X,Y WxH'` covers a compositor `guicast` does not know how to ask. The palette
is built from what changes between frames and only the moved rectangle is
rewritten, which is what keeps a 48-frame recording of a 626x756 window under
half a megabyte.

Two consequences worth having. **A screenshot cannot claim something the
program does not do** — the log-format shot found that busybox `sed` ignores
`\x` escapes, because the recording showed the escape instead of the arrow.
And **the monitor shots are one session**: the hero, the filling shot, the
spectrum strip and both animations are moments of the same 240-second run
of the native build against the counter, so the numbers in them agree with each
other because they are the same numbers.

`tools/record.py inventory <cast>` prints the escape sequences a recording
actually contains. The emulator implements that list and nothing else, so a
future ncurses emitting something new shows up there rather than as a quietly
wrong pixel.

**And it is tested**, in `tests/test_record.py`, because a bug in a terminal
emulator does not raise an exception — it publishes a picture of something the
program never drew. One did: on a screen busy enough for ncurses to decide
scrolling was cheaper than redrawing, it emits `CSI T` inside a scroll region,
and an emulator that ignores `DECSTBM` puts every row below the scroll point in
the wrong place. It drew the random line five rows up, on top of the spectrum.
The program was correct; the picture of it was not.

---

MIT License — Copyright (c) 2026 Paul Richeson

---

[← back to the README](../README.md)
