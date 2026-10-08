<!-- SPDX-License-Identifier: MIT — Copyright (c) 2026 Paul Richeson -->

# The counters and the dials

[↑ Reading the window](reading-the-window.md) · [The big number and the five averages →](reading-the-averages.md)

**The top of the window: one line per counter, and one dial each.**

![the window](screenshots/gui-drum-window.png)

## The counters

One line per counter: its port, its model and firmware, its serial, and what
it counted **in the last second**. An empty space where the count should be
means that counter has stopped answering. A counter that is not answering is
left out of every average, so a figure is never made from a stale reading.
It is the instrument saying it is not measuring, rather than claiming to.

## The dials

Two dials, needles on a fixed logarithmic scale with a 270-degree sweep,
the reading in digits on the face. The coloured bands on the faces are the
same thresholds everything else on the window uses, in counts per minute:

| Band | From |
|---|---|
| nominal | 30 CPM |
| advisory | 120 CPM |
| warning | 240 CPM |
| deadly | 600 CPM |

Below 30 CPM the reading is *attenuated*: less than an ordinary room, which
usually means the tube is shielded or has stopped.

**The left dial is raw.** One needle per counter, in that counter's colour,
at its own 30-second rate; the letters under the face say which counters
are on it, and the digits read their mean. Captioned `raw · 30s`.

**The right dial is the live one.** Its needle sits at the last **three
seconds**, the shortest window the panel keeps: one second of one tube is
two or three arrivals and mostly noise, and three seconds is enough to
read a value off while still showing a source passing under the tube. The
chrome pointer behind it marks the 30-second figure, and the two arcs
outside the bands show the lowest and highest the needle has been in that
half minute. Its caption says all three: `3s held · 8–78` is the needle's
window and the range, and the line above it, `1s 0 · 30s 33`, is this
second's count and the half-minute rate the pointer is at.

---

[↑ Reading the window](reading-the-window.md) · [The big number and the five averages →](reading-the-averages.md)
