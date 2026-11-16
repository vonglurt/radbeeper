<!-- SPDX-License-Identifier: MIT — Copyright (c) 2026 Paul Richeson -->

# Reading the window

**What every number on the RadBeeper window means, feature by feature.**

![the window, live for 24 seconds](screenshots/gui.gif)

The window is `radbeeper-gui`. It opens no serial port; it reads the same
stream the service publishes, so what it shows is what the log has. From the
top down, one page each:

1. **[The counters and the dials](reading-the-counters.md)** — which counters are connected, what each counted last second, and the bands on the dials
2. **[The big number and the five averages](reading-the-averages.md)** — the same reading over five windows, and how well each window knows its number
3. **[The status line](reading-the-status-line.md)** — this second's count, the run so far, and how two counters share the second
4. **[The drum spectrogram, read as a landscape](reading-the-drum-as-a-landscape.md)** — height, tint and brightness: the striped panel as a relief map, and the paper filling from empty
5. **[The counts](reading-the-counts.md)** — a bar a second, older bars packed coarser, the trend line and the pointer
6. **[The random line](reading-the-random-line.md)** — the newest bits drawn from decay timing, and which counter earned them
7. **[The table](reading-the-table.md)** — the trend line in numbers, and where the table goes in a small window

The window follows the desktop's light or dark theme, and fits whatever
space it is given: in a small window the table goes first and the charts
last.

**Added 16 November 2026: the thermometer, the tiles and the long papers.**

- **The thermometer**, top right, is the counter's own, on a scale from 50 to
  100 °F with the reading in °F and °C. It is there only when a counter has
  given a temperature. Click it, or press `t`, for the temperature drum.
- **The striped chart has three papers.** The live one at the foot is as it
  was. Above it is a line for each day, and above that a line for each month.
  They are full when the window opens, because they are made from the log.
- **A tile** is a paper folded away: click a paper, or press `d`, `m` or `t`,
  and it becomes a small picture of itself under the big number; click the
  tile to open it again.
- **The averages and the long bars of the counts are full at once.** When
  the window opens it reads the last hours of the log, so the 3000-second and
  30000-second averages do not start from nothing. Bars finer than 32
  seconds stay empty for the stretch that came from the log. With two
  counters this is not done.

[Reading the drum spectrogram](reading-the-drum-spectrogram.md) has the
pictures.

[← back to the overview](../README.md)
