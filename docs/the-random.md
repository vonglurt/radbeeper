# Random numbers out of decay

*A lab report on RadBeeper's entropy source: where the bits come from, three
ways this is normally done wrong, why the model that used to be assumed was
measurably wrong about this tube, and what the serial link costs.*

> Treat this as a good physical entropy source, not a certified one. It has
> not been through a statistical test battery, and 256 bits of accounted
> min-entropy is a claim about the model of the source, not a proof about the
> output.

---

The moment a nucleus decays is not determined by anything, which makes a counter
the textbook hardware entropy source.

```sh
radbeeper random
```

```
b17c9c60 d5deeb7b 4b102dfc bec14efc b523818b 18d3ada0 582bd912 e61ff856

  256 bits, min-entropy 258 measured, from 448 seconds at 0.68 counts/s
  440 bits is what a Poisson model would have claimed for the same 448 seconds
  spectrum flat -- the source looks like decay
  recorded in /var/lib/radbeeper/random-F48824B8207F7E.tsv
```

That second line is the point: on this run the model would have handed over the
same sixty-four characters after 260 seconds and called them 256 bits.

---

## Three ways this normally goes wrong

And what is done instead.

*Not from the FFT.* A transform is linear and invertible — it moves information
about, it does not make any. Worse, these coefficients come from a
mean-subtracted, Hann-tapered window, so neighbouring bins are correlated by
construction. Bits pulled from them would look beautiful and carry far less than
they appear to. The entropy is in the counts; the spectrum is a *view* of them.

*Not by XOR and rotation.* Shifting and XOR-ing a block rearranges what is in it.
A block holding twenty bits of entropy holds twenty bits after any amount of
barrel-shifting, while looking more and more convincing. The tool for condensing
a lot of weakly-random data into a little strongly-random data is a cryptographic
hash — SHA-256 is in the standard library and is faster than the shifting.

*Not proven by a flat spectrum.* Flatness is necessary and nowhere near
sufficient: a counter, an LFSR and a square wave at the Nyquist rate all pass it.
It is used as a **health check** — a peak means something periodic is
contaminating the arrivals — and an emission is marked suspect when it fails.
Nor does the spectrum *add* anything: the FFT is a deterministic function of the
same samples, and H∞(f(X)) ≤ H∞(X) for any deterministic f.

---

## The bits are measured, not modelled

**And the model was wrong.** This used to
compute min-entropy as −log₂ max_k P(k) for *Poisson* at the observed rate,
which is the textbook thing to do and is not what the data supports. Across
1,128 recorded samples from a GMC-320Re the variance is **2.54×** the mean;
Poisson requires 1.00. Empty seconds are 36% more common than the model allows
and the tail runs far past it — five counts in a second happens fourteen times
too often. Every pool is over-dispersed on its own, so it is not a mixture of
quiet and busy periods, and high seconds fall next to each other about 1.6× as
often as independence permits. Ground-level coincidences will do that.

Over-dispersion piles the distribution onto its mode, and the mode is exactly
what min-entropy is about, so the model was **claiming 1.15 bits a second where
the samples support 0.62**. What is used now is NIST SP 800-90B's most-common-
value estimator: the observed frequency of the commonest value, pushed to the
far end of its 99% confidence interval so the entropy is a lower bound rather
than a point estimate.

| | Poisson model | measured |
|---|---|---|
| bits per second | 1.150 | **0.618** |
| seconds for 256 bits | 223 | **≈ 415** |

A line therefore arrives about half as often as it used to, and the number on
it is one the data will support. A dead counter never becomes ready, however
long it sits there — and neither does a counter *stuck at exactly one count a
second*, which the Poisson model would have credited with half a bit a second
for having an ordinary-looking mean.

---

## There is a page for this

`radbeeper export` writes
`random.html` beside the index: the counts drawn against the model, the bits
accumulating second by second with the target line across them, what the serial
link's one-second resolution costs, and every emission with the counts behind
it.

---

## What the interface costs
 `<HEARTBEAT1>>` gives two bytes once per second
and nothing finer, so one sample is one integer and that is the entire raw
material. If the link reported the *time* of each arrival instead, the gap
between two would be exponential, and quantised to a millisecond it would carry
about **10 bits per arrival** — roughly 8 bits a second at this background,
against the 0.62 actually available. GQ's protocol has no such message:
`<GETCPS>>` and the heartbeat both answer with a count, never a timestamp. The
limit is the interface, not the tube.

---

## From a beep to a frame

Every emission now writes **three** files, not two. The third is the raw
material, and this is the whole path from a decay in the tube to a row of bytes
on disk.

```
  a decay in the M4011 tube
        |  the counter's own discriminator fires
        v
  one pulse, on the counter's internal counter
        |  <HEARTBEAT1>> -- TWO BYTES, ONCE A SECOND
        v
  one integer: how many pulses were in that second
        |  Entropy::add_at(when, counts)
        v
  the pool: counts[] and times[], growing until it has earned 256 bits
        |  Entropy::frame(seq, suspect)
        v
  a Frame:  started, and (gap, count) for every second
        |  Frame::encode()  -- one byte an ordinary second
        v
  random-<serial>.bin, appended
```

**The beep is never timed, and that is the interface's fault rather than the
tube's.** It is worth being exact about, because the obvious frame format is a
list of intervals between pulses — that is where the entropy physically is, and
quantised to a millisecond it would carry about ten bits per arrival against
the 0.62 bits a second this counter yields. `<HEARTBEAT1>>` answers with a
**count**; `<GETCPS>>` answers with a **count**. There is no message in GQ's
vocabulary that reports *when* a pulse landed. One second holding one integer
is the entire raw material, and a file that implied otherwise would be
inventing precision the wire never carried.

### What a frame holds

| | |
|---|---|
| `started` | whole seconds since the epoch, of the **first** sample — the same number that went into the digest, so a frame can recompute its own key |
| `suspect` | the spectrum was not flat when this was drawn |
| `seq` | which emission this is, for the same counter |
| samples | `(gap, count)` per second: how long since the previous sample, and exactly how many pulses were in this one |
| `key` | the 256 bits this frame produced |

### What it costs

A naive record is a timestamp and a count per second: **twelve bytes**, eleven
of which say *"and then it was a second later"*. The cadence is the default, so
only departures from it are worth encoding:

| tag byte | means |
|---|---|
| `0x00`–`0xFD` | one second after the last, and **the byte is the count** |
| `0xFE` | escape: a varint gap and a varint count follow |
| `0xFF` | unused, so a run of padding can never parse as data |

An ordinary second is therefore **one byte**. A four-hundred-second frame is
about 450 bytes including its header and key — against 4.8 kB stored the naive
way, and against a `.tsv` line that would have clamped every second to fifteen.

**There is no gzip in this**, and not only because the crate has one dependency
and it is `libc`. A DEFLATE header alone is most of what this encoding costs in
total, and a stream of bytes valued 0–5 is the shape a generic compressor does
worst on. The structure *is* the compression.

### The magic is per frame, not per file

`RBF1` heads every frame rather than the file. The file is appended to forever,
may be concatenated with another, and will one day be truncated by a full disk
or a crash — so a reader that loses its place scans forward to the next magic
and carries on, and a damaged frame costs one frame instead of the rest of the
record. A frame is only accepted when the byte after it is another magic or the
end of the file: corrupting a length field does not make a frame fail to parse,
it makes it parse as a *different* frame that swallows the next one, and that is
the failure the boundary check catches.

### Looking at one

```sh
radbeeper random --frames logs/random-F48824B8207F7E.bin
```

```
  seq 0    2026-09-19T12:21:28     20s     21 samples  peak    63  1.920 bits/s  recomputes
  seq 1    2026-09-19T12:21:49     23s     24 samples  peak    60  1.724 bits/s  recomputes
radbeeper: 2 of 2 frames recompute from their own seconds
```

**"recomputes" is the claim being checked.** Each frame re-derives the key
written inside it from the counts written inside it. Change one second and it
stops recomputing. It says nothing about the *next* key, which comes from decays
that have not happened yet.

`--no-frames` turns the file off; `--entropy-bits N` changes what a line is
worth. Both the service and the monitor write frames whenever they are the
process holding the port.

---

## Reproducible is not the same as predictable
 The counts behind each line are
written beside it in `random-<serial>.tsv`, so anyone can recompute it and check
it was not invented:

```sh
radbeeper random --check logs/random-F48824B8207F7E.tsv
```

That is an audit trail. It says nothing about the *next* line, which comes from
decays that have not happened yet.

**For the numbers alone**, every line is also appended to
`random-<serial>.hex` — the time it was drawn, two spaces, sixty-four hex
digits — by `random` and by `watch` alike:

```sh
tail -f /var/lib/radbeeper/random-F48824B8207F7E.hex | cut -c22-
```

---

## Further reading

- [The spectrum](the-spectrum.md) — the flatness check this uses as a health
  test, and why flat is necessary and nowhere near sufficient.
- [The log](the-log.md) — where the emission record and its counts are kept.
- [Reference](reference.md) — the counter's protocol, and what it will and
  will not tell you.

---

MIT License — Copyright (c) 2026 Paul Richeson

---

[← back to the README](../README.md)
