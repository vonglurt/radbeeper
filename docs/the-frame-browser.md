<!-- SPDX-License-Identifier: MIT — Copyright (c) 2026 Paul Richeson -->

# Reading the Raw Record: A Frame Browser for a Continuously Sampled Decay Log

**Paul Richeson** · RadBeeper · Copal Linux

---

## Abstract

*RadBeeper has kept the raw seconds behind every random number it publishes
since version 0.5, in an append-only binary format that costs one byte for an
ordinary second. Until now those bytes could be summarised, audited and
embedded in a page, but not browsed: there was no way to move through a
month, pick a frame out of a day and look at what the tube actually did. This
report describes the frame browser added alongside 0.5 — a generated page
that navigates months, days and frames, and shows each frame both as a strip
of seconds and as a spectrum — and the three programming surfaces underneath
it: a library, a set of command-line verbs, and the live socket the recording
process already serves. We give the measured cost of the format (81 fixed
bytes a frame, 64 of them the key and the chain link, and 1.00 bytes a second
thereafter), state the import procedure and its three rules, and report what
pins each implementation to the others. We are explicit about two
limitations: the page is written by the Rust build alone and is therefore not
refreshed by the workflow that rebuilds the rest of the site, and only the
process holding the serial port writes frames at all.*

**Index Terms** — data browsing, binary log formats, append-only storage,
spectral analysis, hash chain, differential testing, Geiger–Müller counter,
entropy audit.

---

## I. Introduction

A counter left running writes two records. The first is the count log: a row
every thirty seconds, five averaging windows, the shape of a month. The
second is the emission log: a line for every 256 bits of hex the decay timing
has earned, with the counts that produced it. Both are text, both are
published, and both have had a page to read them in since 0.2.

The third record is newer and is not text. Since 0.5 every emission also
writes a **frame** — the raw seconds behind that one key, unclamped, with the
gaps where the counter was away kept as gaps. [The
random](the-random.md) describes why the frame exists and what is in it;
[the monthly record](the-monthly-record.md) describes the rotation and the
chain that runs through it. Neither describes how to *read* one, because
until now the answer was a summary table, a command that printed every frame
in a file at once, or a base64 blob embedded in the audit page for a viewer
that showed the rows and not the seconds.

That is the gap this closes. A month of a busy counter is a few thousand
frames and a few megabytes; the question a reader actually has — *what did
the tube do at nine o'clock on the fourteenth* — was not answerable without
writing a program.

## II. What a Frame Holds, and What It Costs

A frame is the stretch of decay one key came out of:

| | |
|---|---|
| `started` | whole seconds since the epoch, of the **first** sample — the same number that went into the digest, so a frame can recompute its own key |
| `suspect` | the spectrum was not flat when this was drawn |
| `seq` | which emission this is, for this counter; it restarts, so it is not an order |
| samples | `(gap, count)` a second: how long since the previous sample, and exactly how many pulses were in this one |
| `key` | the 256 bits this frame produced |
| `link` | `H("radbeeper/chain/1" ‖ previous link ‖ this key)`, hashed beside the key and never into it |

The encoding is described in full in [the random](the-random.md#what-it-costs):
a tag byte of `0x00`–`0xFD` *is* the count and means one second later, `0xFE`
escapes to a varint gap and a varint count, and `0xFF` never appears so that
padding cannot parse as data.

**The measured cost, over 707 seconds of recorded frames:**

```
frames 8   samples 707   bytes 1355
1.92 bytes a second overall
81.0 bytes fixed per frame, and 1.00 bytes for every ordinary second after it
```

The interesting half of that is the fixed cost. Eighty-one bytes a frame, of
which **sixty-four are the key and the chain link** — two thirty-two-byte
digests that have nothing to do with how long the frame is. A frame covering
500 seconds of background therefore costs about 1.16 bytes a second; one
covering 90 seconds costs 1.90. The compression is in the seconds, and the
overhead is in the cryptography, and the ratio between them is a function of
how long the pool takes to earn its bits — which is to say, of how quiet the
room is.

**There is no general-purpose compressor in this, and not only because the
crate has one dependency and it is `libc`.** A stream of bytes valued 0–5 is
the shape DEFLATE does worst on, and a DEFLATE header alone is a fair
fraction of what a short frame costs in total. The structure is the
compression.

## III. Appended by the Day, Filed by the Month

Frames go into `random-<serial>-YYYY-MM.bin`, appended and never rewritten.
The month in the name is the month the frame **opened** in, so a frame that
starts at 23:58 on the last of the month is in that month's file whatever
time it finishes — a reader that went looking for it by its end time would
otherwise find nothing.

Days are not a storage unit. They are derived from `started` when the record
is read, in local time, which is the only way a day can mean what a person
means by it: the files are UTC epochs and the reader is in a timezone. The
browser groups by day; nothing on disk does. That is deliberate — a second
index is a second copy of the truth, and this format's whole argument is that
there is only ever one.

**The magic is per frame, not per file.** A reader that loses its place scans
forward to the next magic, so a file cut short by a full disk still yields
every complete frame before the cut, and a damaged frame costs one frame
rather than the rest of the month. A frame is only accepted when the byte
after it is another magic or the end of the file, because corrupting a length
field does not make a frame fail to parse — it makes it parse as a *different*
frame that swallows its neighbour.

## IV. The Browser

`radbeeper export --frames-page` writes `frames.html` beside the other pages.
It opens on the newest month and narrows downward:

**Months.** Every `.bin` the counter has, with its frames and its size.
Recent months are carried in the page as base64 against a byte budget, so a
page saved to a stick and opened off `file://` still browses; older months are
fetched when they are asked for, and say plainly when there is nothing to
fetch from.

**Days.** A strip of bars, one a day, height by frame count, orange where a
frame that day was recorded suspect. This is the shape of the month before
any of it is read — a gap in the strip is a day the counter was off.

**Frames.** A sortable table: when it started, how long it covered, how many
samples, the counts, the peak second, the rate, how many gaps, and whether the
spectrum was flat.

**One frame.** Selecting a row opens the inspector, in two views:

*Second by second* — one bar a second on a real time axis, so a gap is drawn
as distance rather than as a zero, with the gaps marked. Wheel to zoom, drag
to pan.

*Spectrum* — the same arithmetic `radbeeper watch` draws live: a Hann taper,
half-overlapped windows, power averaged, every bin against the average bin,
on the widest of the 128/256/512-second rungs that has two windows in it.
**Flat — 1.0 — is the good answer.** The line marked *luck* is how high the
tallest bin gets by chance alone given how many windows were averaged.

**The frame's spectrum and the frame's `suspect` flag are not the same
measurement, and the browser says so when they differ.** The flag is written
out of the monitor's *running* ladder — every second that process has seen,
summed across both tubes when two are plugged in — while the page recomputes
from the frame's own samples and nothing else. A 363-second frame gives four
128-second windows; the recorder may have had a hundred, and a period an hour
long is invisible to the frame and obvious to the run. The two answer
different questions — *was the room periodic while this was drawn* against
*is this frame periodic* — so a disagreement is information rather than a
fault, and nothing in the suite requires them to match. This was found by
recording against a deliberately periodic synthetic source, where the running
ladder called every frame suspect and no individual frame was.

Below both views the frame's seconds are written out — one character each for
an ordinary counter, and the numbers themselves once a second can hold more
than 35, because a grid that renders 150 and 180 identically is a blank
reading rather than a compressed one.

**A frame has an address.** The page's fragment is `#<month>/<seq>` —
`frames.html#2026-09/124` opens that month and that frame. An audit trail
people quote at each other needs a link per frame, not one per page.

## V. Three Surfaces

The browser is a reader, not the reader. Everything it shows is available
three other ways, and all four go through the same code.

### A. The library

`radbeeper::frames` is the documented surface, and it holds no format of its
own — `entropy` owns the encoding and the chain, `analysis` owns the
spectrum, and this composes them:

```rust
use radbeeper::frames;

let series = frames::Series::open(dir, "F48824B8207F7E");
for month in series.months() {
    println!("{} {} frames {} bytes", month.label, month.frames, month.bytes);
}
let all = series.read_all();               // file order, which is chain order
let days = frames::by_day(&all);           // local days, derived not stored
let spec = frames::spectrum(&all[0].counts());
let verdict = frames::verify(&all, entropy::GENESIS_LINK);
```

`Series::open` reads the files. A frame is about 450 bytes and a busy month a
couple of megabytes, so a lazy handle that has to be asked twice for the same
answer would cost more in surface than it saves in reads.

### B. The command line

```sh
radbeeper frames list                      # months, frames, bytes, span, chain head
radbeeper frames show --seq 124            # one frame: seconds, spectrum, key, link
radbeeper frames export --month 2026-09 --json
radbeeper frames import september.bin
radbeeper frames verify                    # every key, every link, from genesis
```

`show` is the inspector in a terminal, spectrum included, because a terminal
is the reader that is always there — over ssh, on a machine with no display —
and a page that could show something the shell could not would be a reason to
open a browser to read a file.

### C. The live socket

The process holding the port already serves what it reads on a socket beside
the log, which is how a monitor attaches without a handover. That stream
carries emissions as they are drawn. It is the only one of the three surfaces
that answers about decay that has not happened yet; the other two answer
about the record, which is finished.

## VI. Getting Data Out, and Back In

Out is wider than in, on purpose.

**Out** has three forms. The **bytes** are canonical and lossless — the file,
or a slice of it. The **TSV** is a row a frame, for a spreadsheet or an eye.
The **JSON** is the frame as a structure, field for field, which is the form
something else's program wants. `tests/fixtures/frames.json` is that shape,
and the suite asserts it parses back to exactly what the bytes beside it
decode to.

**In** takes bytes or JSON, and applies three rules:

1. **A frame must recompute.** Its counts are hashed and the result has to be
   the key it carries. That is the only claim a frame makes and the only one
   checkable without trusting whoever sent it. A frame that fails is refused
   and named; the frames around it still land.
2. **A key already on disk is not written twice.** Importing the same file
   twice leaves the record as it was, so a re-run after a half-finished
   transfer is safe.
3. **The link is recomputed here, never carried in.** A chain is a property
   of the order frames landed in *this* directory. Keeping a sender's links
   would either fork the chain or silently claim their history happened here,
   so the head is read off the disk and the import joins it.

```sh
# on the machine that recorded it
radbeeper frames export --month 2026-09 --bin -o september.bin

# on the machine that is going to keep it
radbeeper frames import september.bin --serial F48824B8207F7E
radbeeper frames verify
```

## VII. What Pins What

The frame format now has **four** implementations, and none of them is
allowed to drift alone:

| implementation | what it does | what holds it |
|---|---|---|
| `src/entropy.rs` | writes and reads the bytes | `tests/frames_fixture.rs` pins the committed bytes |
| `src/frames.js` | reads them in the audit page | `tests/test_frames_js.py` decodes the fixture |
| `src/browser.js` | reads them in the browser page | `tests/test_browser_js.py` decodes the same fixture **and compares frame for frame against `src/frames.js`** |
| `radbeeper` (Python) | writes the same log format | `tests/test_differential.py`, byte for byte |

The second javascript file is a deliberate duplication. `src/frames.js` is
byte-locked to a copy pasted into the Python program, because the two have to
produce `random.html` character for character; the browser page has no Python
half and never will, so sharing that file would drag the whole parity
apparatus across for no benefit. The condition of keeping the duplication is
that a test decodes one fixture with both files and fails if they ever
disagree.

The browser's **spectrum** is pinned the same way, to the Python reference the
Rust was ported from: the suite runs a deterministic 600-second sample —
once quiet, once with a 64-second square wave in it — through both, and
requires the same window, the same number of averages, the same loudest bin,
the same verdict, and every relative bin equal to eight decimal places.

## VIII. Limitations

**Only the port-holder writes frames.** A monitor attached to a running
service shows the countdown but draws nothing: its pool exists for the
display, and the emission and the frame are written by the one process that
holds the port. A machine whose logging service is running an older build
therefore records emissions and no frames at all, and nothing about the
emission log looks wrong while that is true. `radbeeper frames list` on the
log directory is the check.

**The page is Rust-only, so the workflow does not refresh it.** The site is
rebuilt on every push by the Python program, which has no frame decoder.
`frames.html` is written by `make site` from a machine with a toolchain and
left alone by everything else. The page says so in its own footer rather than
going quietly stale, but a reader who arrives at a month-old page has no way
to know the record moved on unless they read it.

**The browser does not verify.** It decodes, groups and draws; it does not
recompute keys or walk the chain, which is `random.html`'s job and
`frames verify`'s. Adding a second verifier would mean a second SHA-256 in
the page, and two verifiers that could disagree is worse than one that
cannot.

**The interactions are still checked by eye.** The zoom and the pan are
driven by pure functions the suite can call — `clampView` cannot produce a
view outside its data — but nothing in the suite drives a pointer. The
screenshot in `docs/screenshots/` is a person looking at it, which is not a
regression test.

## IX. Conclusion

The raw record was already being kept, already chained, and already published
against a byte budget. What it lacked was a way in at human scale. Months,
days and frames is the nesting the files are already in; a strip of seconds
and a spectrum are the two questions anybody has about a stretch of decay;
and a library, five verbs and a socket are the same answers for a program.
The format did not change to make any of this possible, which is the outcome
worth reporting: a record that has to be migrated to be read was not designed
as a record.

---

## References

- [The random](the-random.md) — how a frame comes to exist, and what a
  beep costs.
- [The monthly record](the-monthly-record.md) — rotation, the chain, and
  bounding the page.
- [The spectrum](the-spectrum.md) — why flat is the good answer.
- [The log](the-log.md) — the count record the frames sit beside.

[← back to the overview](../README.md)
