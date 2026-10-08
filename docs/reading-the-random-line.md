<!-- SPDX-License-Identifier: MIT — Copyright (c) 2026 Paul Richeson -->

# The random line

[← The counts](reading-the-counts.md) · [↑ Reading the window](reading-the-window.md) · [The table →](reading-the-table.md)

**One line under the counts: the newest random number, and who earned it.**

```
random · next in 1012s
```

The counter's timing is a source of randomness: the interval between one
count and the next is decided by nothing. The service gathers those
intervals into a pool and, when the pool has measured enough entropy to
justify it, draws 256 bits as hex. This line shows the newest draw and, with
more than one counter, the letter of the counter whose counts it came from:
each earns its own, from its own arrivals. `next in` is how long the pool
needs at the current rate before it can draw again.

**[The random](the-random.md)** is the lab report: how the entropy is
measured and why the draw waits for it.

---

[← The counts](reading-the-counts.md) · [↑ Reading the window](reading-the-window.md) · [The table →](reading-the-table.md)
