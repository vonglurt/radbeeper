<!-- SPDX-License-Identifier: MIT — Copyright (c) 2026 Paul Richeson -->

# The spectrum, where flat is the good answer

![the accumulating spectrum](https://raw.githubusercontent.com/vonglurt/radbeeper/main/docs/screenshots/watch-spectrum.png)

Radioactive decay is a Poisson process, and **the power spectrum of a Poisson
process is flat** — white noise, every frequency carrying the same expected
power. A healthy counter watching background therefore produces no shape at all,
and that featureless strip is the useful result: a statement that nothing
periodic is happening.

It earns its place on the other case. A peak means something is arriving on a
schedule, and decay does not have a schedule — mains hum on the tube's supply, a
fan carrying a source past, a loose connector, firmware that batches its
reporting. In the time domain every one of those looks exactly like more counts.

**It accumulates.** One periodogram of a Poisson process is flat in expectation
and violently noisy in fact — every bin an exponential variable whose standard
deviation equals its own mean. Averaging *N* of them divides that scatter by
√*N*, so a real line climbs out of the grass while the grass settles. Windows are
half-overlapped (Welch rather than Bartlett), which gets two averages out of each
window's data instead of one.

**Sigma alone is not a reason to believe anything**, and this is the trap the
panel is most likely to fall into. Sigma is computed for one bin, but the eye
picks the *tallest of 127*, and the largest of many draws is far bigger than any
single draw. One bin of an *N*-window average is Gamma(*N*)/*N*; asking how high
*B* draws of it reach means solving *N*(r − 1 − ln r) = ln *B*, and expanding
that gives

> **chance max ≈ 1 + √(2·ln B / N) + ⅔·ln B / N**

At two windows that is **5.4×**. So a bin at 4.9×, reading as a confident five
sigma, is *below* what a perfectly healthy counter produces every time you look.
At twenty-eight windows the same arithmetic gives 1.86×, and 4.9× is then
overwhelming. The headline compares against *that*:

```
spectrum   flat -- arrivals look random, as decay should (28 windows)
spectrum   peak at 8s, 9.4x the mean (chance gives 1.86x), 7.2 sigma
```

**The leading term is the square root, and leaving it out cost a year of false
alarms.** This used to compute the bar as 1 + ln(B)/*N*, which is the correct
answer for *one* periodogram — a single bin of white noise is exponential, and
the largest of *B* of those does land near ln(*B*) above the mean — and the
wrong one for an average of *N*, whose tail is not exponential at all. The two
agree only at *N* = 1. Everywhere else the old form was too low, and it got
worse the longer you watched: **the bar sinks like 1/_N_ while the real peak
only sinks like 1/√_N_**, so the two cross over.

Measured against a null built from this counter's own recorded counts,
resampled i.i.d. so the spectrum is flat *by construction* and every flag is
a false one:

| Watching for | Old bar | Real median peak | Called suspect |
|---|---|---|---|
| 30 minutes | 2.13× | 2.51× | **33%** |
| 1 hour | 1.52× | 1.97× | **64%** |
| 2 hours | 1.25× | 1.64× | **80%** |

The longer the session, the more reliably it cried wolf — and a health check
that fires four times in five on healthy hardware is not a health check, it is
a decoration. **"Called suspect" is the rule the panel actually applies**, which
wants a peak at 1.25× the bar rather than merely above it; that margin is the
only reason the first row is a third and not a half, given that the median peak
has already overtaken the old bar. With the square-root term restored the same
null stays under 1% at every length.

**Raising the bar did not cost the detection it is there for.** The same null
with a period-8 s source added on top — Poisson arrivals at a fraction of the
background rate, which for this counter is 0.73 counts a second, about 44 CPM:

| Source at | Peak after 30 min | Called at 30 min | at 1 h | at 2 h |
|---|---|---|---|---|
| 0.2 × background | 2.7× | 2% | 2% | 11% |
| 0.3 × background | 3.0× | 19% | 50% | 92% |
| 0.4 × background | 4.5× | 65% | **98%** | **100%** |
| 0.5 × background | 6.4× | **95%** | **100%** | **100%** |

**The detection floor is a real number, and it sits between a third and a half
of background.** Half the counts arriving on a schedule is unmissable inside
half an hour. A fifth of them is invisible at every length, and no threshold
would fix that: half an hour at 44 CPM is thirteen hundred arrivals, and the
line is not in them to be found. `radbeeper` and the Rust build were both wrong
in the same way and are both fixed; `chance_max()`'s two quoted values, 5.4×
and 1.86×, have a test pinning them in each.

**Resolution grows with time, because it has to.** Frequency resolution is 1/*T*
for an observation of length *T* — you cannot resolve a 512-second period in 128
seconds of listening. Rather than pick one, RadBeeper runs a **ladder**: 128, 256
and 512 seconds side by side, fed the same samples. The 128 answers after two
minutes; the 512 takes eight and a half but resolves four times as finely, and by
the time it has anything to say it is the better answer. It costs about ten
kilobytes, fixed for the life of the process.

The axis runs from long periods on the left to short on the right (2 s, the
Nyquist limit), and the bars are coloured by significance rather than height.

---

[← back to the README](../README.md)
