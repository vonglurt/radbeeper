<!-- SPDX-License-Identifier: MIT — Copyright (c) 2026 Paul Richeson -->

# The status line

[← The big number and the five averages](reading-the-averages.md) · [↑ Reading the window](reading-the-window.md) · [The drum spectrogram, read as a landscape →](reading-the-drum-as-a-landscape.md)

**One line under the averages: what this second held, the run so far, and
how the counters share the second.**

```
now 0    run 940 in 956s   2 tubes · interleave 0.50s (99%)
```

**`now`** is what was counted in the last second, across the counters.

**`run`** is every count since the service started, and the seconds it
took. Divide one by the other for the session's rate so far.

**The note at the end** appears with more than one counter. Each counter
answers once a second; two of them can answer at the same moment or half
a second apart. `interleave 0.50s` is the gap between their answers and the
percentage is how close that is to ideal. Interleaved, the pair samples the
room twice a second. `averaged` means they are answering together, and
`1 of 2 tubes averaged` means one has stopped and the figures are from the
other alone.

---

[← The big number and the five averages](reading-the-averages.md) · [↑ Reading the window](reading-the-window.md) · [The drum spectrogram, read as a landscape →](reading-the-drum-as-a-landscape.md)
