<!-- SPDX-License-Identifier: MIT — Copyright (c) 2026 Paul Richeson -->

# Reading the drum spectrogram

**The striped panel in the middle of the RadBeeper window, and how to read
it. It answers one question: is anything reaching the counter on a
schedule?**

![the drum spectrogram](screenshots/gui-drum-spectrogram.png)

---

## What it is for

Radioactive decay has no rhythm. Counts arrive at random, and a healthy
counter in an ordinary room shows that: no pattern at all.

If something *does* arrive on a schedule, it is not decay. It is a fan
carrying something past the tube every few seconds, electrical noise on the
counter's supply, or the counter itself batching its reports. In the big
number on the screen all of those look exactly like more radiation. Here
they look like a stripe.

---

## What you are looking at

**It is a paper chart, like the one a seismograph draws.** Every eight
seconds a new line is drawn at the bottom, just above the counts, and all
the older lines move up to make room. Each line is made from the last eight
and a half minutes of counts, and the line at the top is about twelve
minutes older than the one at the bottom.

**The very bottom line** is the same measurement averaged over hours. It is
drawn in the same colours as the lines above it, in the same places, so a
bump in it sits directly under the same bump in the lines above. It is
steadier than they are, and slower to change.

**Left to right is how often.** Slow rhythms are on the left and fast ones on
the right. The labels under the panel say where you are: in the picture,
the left edge is once an hour, `8m` is once every eight and a half minutes,
and the right edge is once every six seconds.

**Up is more.** Where a line rises, more of the counts arrived at that
rhythm.

**The colour and the thickness say how much.** The pen gets hotter and wider
as the signal gets stronger:

| You see | It means |
|---|---|
| Thin green lines | The ordinary background. Nothing there. |
| Lime to yellow | Stronger, but still within what chance does on its own. |
| Orange and red, thick and glowing | More than chance usually manages. |
| White | Much more. |
| A dot on a line | The top of a peak. |
| A white ring | The single strongest peak on the whole chart. |

**How bright a whole line is says how busy the counter was.** A line drawn
while the counter was counting twice its usual rate is at full brightness.
One drawn while it was counting half is faint.

---

## How to read it

### Mostly green: all is well

![the whole window](screenshots/gui-drum-window.png)

This is a counter in an ordinary room. The chart is green from edge to edge,
with a few warm patches that come and go. **This is the good answer.**

### A warm patch that comes and goes: chance

![a warm patch, twice the size](screenshots/gui-drum-detail.png)

Random counts make short-lived patterns by luck, and the chart shows them.
A patch like this one, ten lines tall, is what luck looks like. It will
drift up the chart and off the top, and another will turn up somewhere
else.

**The rule of thumb: a patch shorter than about a third of the chart is
chance.** Measured on simulated background, chance patches are usually five
lines tall, almost never more than thirteen, and were never seen to reach
half the chart. A patch that stays hot for longer than that is worth a
look.

### A stripe from top to bottom: something real

A hot stripe that runs **straight up the chart, in the same place, and
stays there** as new lines arrive is a rhythm that is really present. Read
where it is against the labels under the panel to see how often it repeats.

Then look for the cause near the counter: something that turns, switches or
pulses at that interval.

### A whole line lit up, side to side: the counts came in a rush

If an entire line is hot from the left edge to the right, nothing is
arriving on a schedule. For those minutes the counts came less evenly than
usual: in a rush, or in clumps. That is worth a look at the counter itself
as much as at the room.

---

## Three things to keep in mind

**The left side moves slowly.** Slow rhythms need a long time to measure, so
the lines on the left hardly change from one to the next and run down the
chart as parallel streaks. A streak there is not proof of anything. For slow
rhythms, trust the bottom line, which averages over hours.

**It only sees strong rhythms.** A faint one is invisible here. The bottom
line is what finds those, by averaging for a long time.

**It never goes off the scale.** Whatever is strongest on the chart sets the
top, and everything else is drawn against it. The colours do not rescale, so
yellow always means the same thing.

---

## Days and months: the long papers

*Added 16 November 2026.*

![the day and month papers over the live drum](screenshots/gui-long-drums.png)

The drum at the foot holds twelve minutes. A rhythm that comes round once an
hour is not in twelve minutes of anything. So there are two more papers above
it, drawn with the same pen on the same ground:

| Paper | A line is | Left to right |
|---|---|---|
| **Days** | one whole day, midnight to midnight; today's is at the foot | once every 8 hours, to once every 2 minutes |
| **Months** | one calendar month | once every 10 days, to once every 20 minutes |

They are read the same way as the drum below them. **A stripe that runs up
through line after line, in the same place, is something arriving on that
schedule day after day.** In the picture the stripe at `20m` is on every day,
and the month line above stands up at `1d`: something that rises and falls
once a day.

**They are full the moment the window opens.** The lines are worked out from
the log the counter has been keeping all along, a row every thirty seconds,
so every day and month there is a log for has its line, including the ones
from before this was added.

**Today's line is drawn before today is over.** It starts short, reaching only
the quick rhythms, and grows to the left as the day goes on. A line is not
asked about anything that has not had time to come round three times.

**A break is closed up.** A day when the counter was away has no line, and
the next day that has one sits directly above. The small dates down the left
edge say which day a line is.

**The live drum gives way to them.** As the papers gain lines they take room
from the top of the live drum's history, a line at a time. The counts under
the drum keep all their room.

### What the same days look like as they arrive

![a few hours](screenshots/gui-build-1.png)

*A few hours of record: one short line for today.*

![three days](screenshots/gui-build-2.png)

*Three days.*

![twenty days](screenshots/gui-build-3.png)

*Twenty days. An hourly rhythm that lasted one week shows on those seven
lines and no others.*

These three are made-up logs, written by `cargo run --release --example
sample_logs`, with a twenty-minute rhythm, a daily rise and fall and one week
of an hourly rhythm put in on purpose. Decay alone leaves every line green.

### Tucking a paper away

![the papers as tiles beside the dials](screenshots/gui-drum-tiles.png)

Click a paper, or press `d` for days and `m` for months, and it folds into a
small tile under the big number, beside the dials. Click the tile to open it
again. The live drum then has the room back.

---

## The temperature drum

![the temperature drum](screenshots/gui-temperature-drum.png)

A GMC-320 has a thermometer, and the service now reads it. The small
thermometer at the top right runs from 50 to 100 °F and gives the reading in
°F and °C. Click it, or press `t`, and the last ten days are laid down the
way a drum recorder would: **a line a day, today's at the foot, yesterday's
one track above.**

Every line is drawn to the same scale about the same temperature, so today
can be read against yesterday directly above it. A track is worth an ordinary
day's rise and fall, so a day like most days stays inside its own track. A
day that went out of the ordinary crosses into its neighbour's, and its pen
turns hot where it does. A room held steady to a twentieth of a degree is a
flat line.

The temperature is the inside of the counter's case, not the air of the room.
[The thermometer](the-thermometer.md) is the report on how it is read.

---

## In a terminal

![the terminal monitor with the three papers](screenshots/watch-drum.png)

`radbeeper watch` draws the same three papers in a terminal, in Braille dots,
with the same colours and the same period marks. `d` and `m` turn the day and
month papers through shown, folded to one row, and off; `t` swaps the charts
for the temperature drum.

---

## Where it is

Run `radbeeper-gui` with a counter attached. The chart is drawn when the
window is at least 640 pixels tall; in a smaller window the counts and the
bars keep the space.

---

## The details

**[The drum spectrogram](the-drum-spectrogram.md)** is the lab report: how a
line is computed, how much time the chart holds, the measurements behind the
rule of thumb above, and why it is drawn the way it is.

[← back to the overview](../README.md)
