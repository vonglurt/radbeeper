<!-- SPDX-License-Identifier: MIT — Copyright (c) 2026 Paul Richeson -->

# The big number and the five averages

[← The counters and the dials](reading-the-counters.md) · [↑ Reading the window](reading-the-window.md) · [The status line →](reading-the-status-line.md)

**The heart of the instrument: one measurement, taken over five windows
that each get ten times longer, and how well each one knows its number.**

## The big number

The counts per minute **over the last 30 seconds**, averaged across the
counters. Three seconds is too jumpy to read as a headline and five minutes
too slow to react to anything you are doing with your hands. Beside it:

```
CPM ±6 (17.4%)
0.211 uSv/h
```

`±6` is one standard deviation, in CPM, and `17.4%` is the same thing as a
fraction of the reading. `uSv/h` is the reading divided by the tube factor
(151.5 for the M4011 in a GMC-320; `--cpm-per-usvh` sets another).

Under it, with more than one counter, **each counter's own 30-second
figure** in its own colour: `A 18  B 48`. The big number is the *mean* of
these, not the sum. It is the number one counter would report, measured
from twice the arrivals.

## The five averages

```
    3s    34.3   0.223   50.0%
   30s    32.5   0.211   17.4%
  300s    27.1   0.176    6.1%
 3000s    filling  2044s
30000s    filling 29044s
```

| Window | What it is for |
|---|---|
| **3 s** | To see a source come and go under your hand. |
| **30 s** | To read the room. The big number. |
| **300 s** | Five minutes: a figure worth writing down. |
| **3000 s** | Fifty minutes: what the background here actually is. |
| **30000 s** | Eight hours and twenty minutes: a working day. |

Each row has three numbers.

**The first is CPM**, the mean rate over that window, coloured by the band
it falls in. The **second is the same rate in µSv/h**, by the tube factor.

**The third is how well the window knows its number.** Radioactive decay is
Poisson: the whole of the uncertainty in a rate is the number of counts
behind it. *N* counts give a relative error of 1/√*N*. So the figure is

```
precision  =  100% / √(counts the window holds)
```

and the counts a window holds is roughly CPM × seconds × counters ÷ 60. In
the block above, the 30-second row rests on about 32 counts, so it is known
to 17%; the 300-second row rests on about 270, so to 6%. The 3-second row
had four counts in it and is known to 50%. **Every step down the block is
ten times the counts and three times the precision.** Without this column
the only visible difference between the 3-second figure and the working-day
figure would be that one of them jumps about.

This is also what a second counter buys. Two tubes put twice the counts
behind the same mean, so every window is known a factor of √2 better. The
column is the only place that benefit is visible.

## A window that is still filling

It shows `filling` and the seconds it still needs, and no number at all,
rather than a number made from part of a window. The 3000-second row fills
fifty minutes after the service starts; the 30000-second row fills eight
hours and twenty minutes after. Restart the service and they start again
from empty. A window opened later inherits the service's history, so it
shows the same rows as one that has been open all along.

---

[← The counters and the dials](reading-the-counters.md) · [↑ Reading the window](reading-the-window.md) · [The status line →](reading-the-status-line.md)
