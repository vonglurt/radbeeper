# The stream

**One reader, many watchers: sharing an exclusively-locked serial port among
a logger, a terminal monitor and a window, and the instrument panel built on
top of it.**

*A lab report in the IEEE style, on the process architecture RadBeeper uses to
fan one Geiger counter out to every program that wants it, on the arrangement
that lets two counters be read at once, and on the layout of the window that
draws the result. Written against radbeeper 0.3.1 and radbeeper-gui 0.3.1, 18
September 2026.*

---

## Abstract

A serial port is an exclusive resource: two processes reading one tty do not
each receive the byte stream, they receive a share of it each, and neither is
told. RadBeeper therefore takes an advisory `flock` on the device. For the
whole of the program's life that lock was read as a stronger claim than it
makes -- *only one program may hold the port* was understood as *only one
program may have the counter* -- and the consequence was a handover protocol:
stop the logger, watch, start it again, and lose an hour of record for every
evening spent looking at the instrument. This report shows the two claims are
not the same, because a GQ GMC-320 delivers two bytes per second and every
number the program draws is arithmetic over that stream. It describes the
resulting architecture: the process holding the lock publishes each sample on
a unix socket beside the log, and every other program attaches instead of
competing. It gives the protocol, the invariants that make concurrent readers
safe, the replay that makes a freshly-opened window complete rather than
empty, the generalisation to two counters and what a second tube does and does
not buy, the layout and data flow of the graphical panel, three renderer
constraints discovered the expensive way, and the measurements taken against
real and synthetic hardware.

**Index terms** -- process architecture, inter-process communication, unix
domain sockets, publish-subscribe, sensor fusion, Poisson counting statistics,
instrument display, radiation monitoring, graphical user interfaces.

---

## I. Introduction

### A. The problem, as it presented

The command that prompted this work:

```
$ radbeeper watch
port busy: the port is already open by another radbeeper
    /dev/ttyUSB0 is locked by another process.
```

The machine was behaving exactly as designed. An OpenRC service starts at boot,
takes `/dev/ttyUSB0`, and logs a row every thirty seconds. A person who then
wants to *see* their own counter must stop that service, and the log carries a
hole for as long as they watch.

Two further faults were found while investigating, both in the seam between the
halves of the design:

1. **`hotplug` could not open a window while the service ran.** The function
   that decides whether a window is warranted asked the *port* whether a
   counter was present, received `busy`, and returned. On every machine where
   the logger worked -- which is every machine it was designed for -- the
   window could therefore never open at all.
2. **Nothing ran `hotplug` at all.** The session-side autostart line had only
   ever been written into the i3 configuration. The desktop in use is Hyprland,
   whose configuration had no such line.

### B. What the instrument actually provides

The counter, in heartbeat mode, sends **two bytes once per second**: a 16-bit
big-endian value whose top two bits are status flags. That is the entire live
data feed. Everything else RadBeeper shows --

- five running averages (3 s, 30 s, 300 s, 3000 s, 30000 s),
- the cascade strip,
- the arrival spectrum and its significance threshold,
- the entropy pool and its emissions,
- the log rows, and the web page built from them

-- is pure computation over a sequence of *(when, counts)* pairs. Nothing below
the read touches the device.

### C. The claim the lock actually makes

The lock exists for one narrow and correct reason, stated in `src/serial.rs`:
two readers of one tty split the bytes between them silently, so a logger and a
monitor running together would each report roughly half the true count -- a
wrong answer that looks entirely plausible. That is an argument for **one
reader**. It is not an argument for **one consumer**, and the difference is the
whole of this report.

---

## II. The fan-out

### A. Topology

```
            before                            after
     ┌──────────────┐                  ┌──────────────┐
     │ /dev/ttyUSB0 │                  │ /dev/ttyUSB0 │
     └──────┬───────┘                  └──────┬───────┘
         flock (exclusive)                 flock (exclusive)
            │                                 │
     ┌──────┴──────┐                   ┌──────┴───────┐
     │   service   │                   │   service    │ owns port, logs,
     └─────────────┘                   └──────┬───────┘ owns the pools
            ↑  ✗ port busy                    │
      watch ─┘                       sock (0660 root:dialout)
      probe ─┘                    ┌────────────┼────────────┐
      gui   ─┘                  watch        probe         gui
```

### B. Where the socket lives, and why

`<logdir>/sock`, which on a Copal machine is `/var/lib/radbeeper/sock`.

Not `/run`. The log directory is the one location both halves of the system
already agree on; Copal creates it `2775 root:dialout`; and a socket inheriting
that group is exactly the access-control statement wanted -- **the same
`dialout` membership that entitles a person to read the port entitles them to
read the stream**. A path under `/run` would need its own directory, its own
tmpfiles rule, and its own justification.

### C. Protocol

Newline-delimited, tab-separated text, in keeping with the TSV log format the
project already owns. `PROTOCOL = 2`.

```
hello   2   <spans,csv>   <n>
c   0   /dev/ttyUSB0   115200   GMC-320Re 4.26   F48824B8207F7E
c   1   /dev/ttyUSB1   115200   GMC-320Re 4.26   AA1122BB3344CC
s   <who>  <unix-time>  <counts>      ← replayed history, then live
row <who>  <tab-separated log row>
r   <who>  <hex>  <at>  <suspect>
live                                   ← end of history
```

Tabs and not spaces because a firmware string is `GMC-320Re 4.26` and contains
one. Unknown verbs are skipped rather than fatal, so a newer server may say more
to an older client without either having to care; `PROTOCOL` is bumped only when
an existing line changes meaning.

Times are **wall clock, not monotonic**. A sample that will be replayed to a
process which was not running when it was taken has to mean something to that
process, and "412.7 seconds after some other program started" does not. It is
also the only clock two counters can be placed on together.

### D. Replay, which is the point of the design

On connect the server writes the greeting, then its ring of the last
`RING = 30_000` samples, then the last `ROWS = 64` log rows, then `live`.

A monitor attaching to a service that has been up since breakfast therefore
opens with its 3-second window full, its 30-second window full, and its
300-second window full -- and its working-day window full if the service has
been up that long. The previous `watch` began from nothing on **every** launch
and spent its first eight hours showing a countdown where the day average
belongs. The fan-out is not merely as good as the old handover; on this axis it
is strictly better than anything the program has ever done.

The greeting and replay are written **blocking with a five-second deadline**;
everything after `live` is non-blocking. Half a megabyte down a unix socket to a
client that is reading takes microseconds. A client that is *not* reading is
dropped -- and once live, a stalled reader must cost the counter nothing, or one
wedged window would stop the logger for every other process on the machine.

### E. Invariants

These five rules are what make a service and two windows safe to run together.

| | |
|---|---|
| **The port-holder serves** | `service` normally; `watch` when nothing else is serving. Holding the flock obliges you to fan out. |
| **A client writes nothing** | No log, no emission, no `index.html`, and it never opens a port. One writer, always. The rule has no exceptions; that is what makes it usable as an argument. |
| **Ask the socket before `/dev`** | Order matters. If a service is logging there is nothing to negotiate. |
| **A named device is not overruled** | `-d /dev/pts/7` means that device; the socket is used only when it serves what was asked for. |
| **The pools belong to the port-holder** | Emissions are drawn once, by the server, and published. Two clients deriving their own from the same counts would print two different "random" lines and only one would be in the audit log. |

The fourth invariant was not designed; it was extracted from a failure. The
differential test suite began reporting a layout difference that turned out to
be a `-d <pty>` monitor silently drawing the **real** counter, because a service
was running and the socket was consulted first. Every synthetic-counter test in
the suite had quietly stopped testing what it named.

### F. Failure modes

- **Stale socket.** A `SIGKILL` or a power cut leaves the node in the filesystem
  with nothing behind it, and `bind` then fails `EADDRINUSE` forever.
  `Server::start` connects to an existing node first: a refusal means the node
  is a corpse and is unlinked.
- **A live socket** is never stolen: if the connect succeeds, the second server
  declines to start.
- **A window that was closed** and a window that has stopped reading are the
  same thing from the server's side and get the same treatment -- dropped on the
  first failed write, silently, because people close windows.
- **A false positive from the node's mere existence** is harmless. `hotplug`
  tests for the file rather than connecting, because greeting a client costs the
  server its whole history and that is not a price to pay fifteen times a minute
  to answer a yes/no. If the node is stale, the monitor it opens finds nothing
  behind it and takes the port itself, which is what should happen anyway.

---

## III. Two tubes

Nothing in the fan-out cared how many counters were behind it, which made the
extension to a second tube a matter of tagging rather than redesign.

### A. Accuracy and precision are different questions

Two GMC-320s in one room are two measurements of **one** number. They do not
double the dose. Summing their counts and reporting the result would be simply
wrong -- the room is not twice as radioactive for having been watched twice.

What doubles is the **evidence**. Arrivals are Poisson, so *N* of them determine
a rate to within a relative error of *1/√N*. Two tubes over the same interval
give *2N*, and

    σ_combined / σ_single = 1/√2 ≈ 0.707

Identical accuracy, better precision, by exactly root two. This is the entire
bargain, and the window prints the uncertainty beside the number because it is
the only place the benefit is visible:

```
453  CPM ±21 (4.7%)
```

The combined figure is the counts-weighted mean -- counts summed, seconds
summed -- and not the mean of the two rates, which would give a tube that
recorded for an hour the same say as one that recorded for a week.

### B. Do they agree?

The one question a second instrument exists to answer. For tubes *a* and *b*
with rates *r* and uncertainties *σ*,

    z = |r_a − r_b| / √(σ_a² + σ_b²)

Identical tubes in one room should sit within about two sigma. The page reports
the widest gap between any pair, in sigmas, with the reading it deserves: *they
agree* under 2, *they differ -- watch it* to 4, *they disagree -- not the same
measurement* beyond. A gap that keeps growing is the tubes differing, not the
room being interesting.

### C. The interleave, measured rather than assumed

Two counters on their own clocks do not agree about when a second starts, and
neither clock can be steered. If the offset happens to be near half a second the
pair samples the room twice a second and the display gains real time resolution;
if the offset is near zero the tubes fire together, which still doubles the
counts and still buys the precision but adds no resolution in time whatever.

Claiming "0.5 s per bar" without knowing which case obtains would be a lie the
display tells itself. So the offset is measured -- the mean gap between
consecutive samples from *different* tubes -- and reported:

```
2 tubes · interleave 0.50s (100%)
```

**An honest limit.** Even at a perfect interleave, each bar remains a
*one-second integration* placed where it arrived. What improves is the sampling
rate, not the integration window. That is still worth having: it is the
difference between seeing a one-second modulation and aliasing it.

### D. The fifth tier

`analysis::tiers` was generalised so a bar may cover a fractional second. With
one counter the cascade is unchanged at `8s · 4s · 2s · 1s`; with any number
above one it grows **exactly one** more tier and runs `8s · 4s · 2s · 1s · 1/n`
— `1/2` for two tubes, `1/9` for nine.

**One tier, not one per doubling**, and the first attempt got this wrong. Adding
a tier per doubling of the tube count keeps the ratio between neighbours at two,
which looks like the invariant worth protecting; it is not. Every tier at or
above a second aggregates *time* — a 4-second bar is four seconds of the room,
and halving it is a finer view of the room. The fine tier aggregates nothing: a
bar in it is one tube's whole one-second reading, placed where it arrived, so
1/n is its **spacing and not its integration window**. Tiers between the two —
2/9, 4/9, 8/9 of a second — are therefore neither. They average measurements
that each already span a second, which makes them smoothed views of the same
second rather than sharper views of time, and at nine tubes they consumed half
the width of the strip to display fifty seconds in units nobody thinks in.

So the ratio is two between every aggregating tier and *n* at the single
boundary below them. That boundary is worth marking rather than smoothing over:
it is exactly where the strip stops measuring time and starts measuring
arrival — and it now *is* marked, with a rule drawn down it, because the
alternating tier wash can only say "another tier" and this is not another tier.

**And it is the narrowest tier, not an equal share.** Five tiers of a fifth
each gave the interleave forty-eight bars, which at two tubes is twenty-four
seconds of arrival order: a quarter of the strip's width spent on the one tier
that is not measuring time, and spent on a stretch of it long enough that
nobody reads the far end. What the interleave is *for* is the last few seconds
— whether the tubes are taking turns, and whether they agree about the second
happening now — so it is capped at `INTERLEAVE_SECONDS * n` bars, four seconds
of the rota at any tube count: eight bars with a pair, thirty-six with nine,
and the same stretch of wall clock in both cases. The width it gives back goes
to the aggregating tiers, which reach further for having it.

The cap is in **seconds and not in bars** deliberately. A fixed bar count would
show four seconds with a pair and half a second with nine, so the tier would
mean something different on every rig.

The finest tier is drawn **in each tube's own colour**, because every bar in it
is one tube's reading. Every tier to its left is a mean over both and takes the
ordinary level colours: by then the two have merged into one number. The
hand-over is therefore visible rather than asserted.

### E. A counter plugged into a running service

Once a log cycle, the service looks for a port it is not already reading. The
**flock does the filtering**: a port this process already holds fails to open
exactly as another process's would, so every candidate can be tried and the ones
already held fall out on their own, with no list to keep and no list to go
stale.

A counter found that way is adopted live — backfilled from its own flash,
given its own log, its own windows and its own pool, and announced on the wire
so every attached window learns about it. `Event::Counter` carries the new
index; the client updates the identity it reports, so a caller that asks after
the fact gets the tube rather than indexing past the end of a list.

**A slot belongs to a serial.** A tube that stops answering has its `Counter`
dropped — which closes the descriptor and releases the flock, so the port can be
taken again — while its slot, its index and its log are kept. Plugging it back
in returns it to the same place. A serial only reclaims a slot that is *empty*:
matching on the serial alone let a second counter reporting the same one take
over a live slot and close the port of the counter already in it, which no two
real tubes would do but every synthetic counter does.

**And the service no longer exits when a counter goes quiet.** It reports the
departure, keeps logging whatever else is present, and picks the tube back up on
a later sweep. Exiting was defensible when a service read one counter and a
counter going away meant there was nothing to do; with several, it would mean
one unplugged tube stopping the record of all the others.

### F. Records stay apart; displays may average

Each tube keeps its own log file, its own backfill from its own flash, and its
own entropy pool. The service records; it does not average. A row blending two
instruments would be a reading neither of them took, and no later analysis could
unpick it. Averaging is a question about a *display*, and every display can ask
it from the stream.

**Except for one thing a display cannot reconstruct afterwards.** The
interleave — whether the tubes took turns or fired together — is a fact about
arrival times that no per-counter row records, and it is the difference between
*n* tubes buying time resolution and *n* tubes buying only precision. It has to
be measured as it happens, by the process holding the ports.

So the merge is a file of its own, `cpm-merged-YYYY-MM.tsv`, written *beside*
the per-counter logs and never into them. It carries both halves of every
interval — the raw arrivals as integers with a `per_tube` breakdown by serial,
and the merged rate of the room, counts over *tube*-seconds — at a precision
the other format deliberately does not keep: `log::exact` writes the shortest
string that parses back to identical bits, where `log::g` rounds to six
significant figures because its characters are the Python's to the byte.

Both rules therefore hold at once. The record of an instrument stays the record
of that instrument, and the record of the room exists as well, taken apart again
by anyone who wants the tubes back.

**One counter writes no merged file.** There is nothing to merge and nothing to
interleave, and a second file that only restates the first to more decimal
places is a second file to explain, to back up and to get out of step. The
differential suite states the same rule from the outside: a single-counter
`watch` leaves exactly one `cpm-*.tsv` behind.

---

## IV. The window: flow and layout

### A. Where the arithmetic happens, and why not in the interface

Attaching to a service that has been running for hours delivers tens of
thousands of samples in a few milliseconds. Turning those into as many UI
messages would wedge the event loop for as long as it took to drain them, and
every frame drawn on the way would be discarded unseen.

So the **feed thread owns the model**. It holds the windows, the ladder of
spectra, the pool and the strip; it folds the replayed history in as fast as it
arrives without emitting anything; and it hands the interface **one immutable
snapshot** when the history is exhausted and one per second thereafter. The
interface is a pure function of that snapshot.

```
socket ──► feed thread ──────────────────────────► Iced runtime ──► view
           Windows ×tubes, Windows (all)           Message::Update
           Ladder [512, 4096, 32768]               (≈1 Hz, bounded)
           Entropy pool, strip ring
           → Snapshot
                     └──────────────────────────► Message::Moved
           Recent (30 s of arrivals)               (≤12 Hz, and only
           Ballistic needle, Drift ×2               when something moved)
           → Meters
```

**Two messages, on two beats.** A `Snapshot` is the cascade, three spectra and
the table: it changes once a second and costs a clone of all of it. A `Meters`
is a handful of floats and wants to *move*, so it travels separately and twelve
times as often — but only when the needle has actually shifted, because a
settled reading is the ordinary case and every message is a full software
re-render on the machine this usually runs on.

The channel is bounded at 32 and the thread uses a non-blocking send: if the
interface is a second behind, the snapshot is dropped, because the next one
supersedes it. There is nothing to retry and nothing to queue.

Waking twelve times a second needs `Client::poll` rather than `Client::next`.
`next` collapses a read timeout and a closed socket into the same `None`, which
is the right answer at ten seconds — a silence that long *is* the server going
away — and the wrong one at eighty milliseconds, where every single wake-up
would read as a dead counter. `poll` returns `Event`, `Idle` or `Closed`, and
keeps the half-line a short timeout leaves behind: `read_until` appends what it
got and *then* reports the error, so a timeout landing between the `s` and the
newline has already taken those bytes out of the socket. Dropped, the sample
goes with them.

### B. Reading order

The panel is laid out in descending order of how often a person looks at it.

```
┌──────────────────────────────────────────────────────────────┐
│ A /dev/ttyUSB0 · GMC-320Re 4.26 · F48824B8207F7E             │ identity
│ B /dev/pts/3   · GMC-320Re 4.26 · 123456789ABCDE             │ one line each
├───────────────┬──────────────────────────────────────────────┤
│   ╭─────╮     │  453  CPM ±21 (4.7%)                         │ the reading
│   │ ◜ ◝ │ ×n  │       2.945 uSv/h                            │ and how well
│   ╰─────╯     │     3s  610.0   3.966  12.8%                 │ it is known
│   dials       │    30s  453.0   2.945   4.7%                 │
│               │   300s  473.7   3.080   1.5%                 │ five windows
│               │  3000s  filling 2016s                        │ + precision
│               │ 30000s  filling 29016s                       │
│               │ now 15  run 15804 in 984s  2 tubes · 0.50s   │
├───────────────┴──────────────────────────────────────────────┤
│ 8s/bar · 6m   F 4s/bar · 3m   F 2s/bar · 96s   F 1s · 48s ...│ captions
│ ▁▃▂▅▁▂▃▁▄▂▃▁▂▅▃▁▂▃▄▁▂▃▁▅▂▃▁▂▄▃▁▂▃▁▂▅▃▁▂▃▄▁▂▃▁▂▃▅▂▃▁▂▃▄▁▂▃▁▂ │ cascade
│ ─────────────────────────── luck line ────────────────────── │ spectrum
│ ▁▂▁▃▁▂▁▁▂▁▃▁▂▁▁▂▃▁▂▁▁▃▁▂▁▂▁▁▃▁▂▁▁▂▁▃▁▂▁▂▁▁▃▁▂▁▁▂▁▃▁▂▁▁▂▁▃▁▂ │ overlay
│ 9h 6m                    period · log                    7s  │ axis
│    7s–8m flat · 9 windows                                    │ per layer
│   58s–1h 8m filling, 25m to go                               │ verdicts
│    7m–9h 6m filling, 8h 23m to go                            │
│ random · next in 828s                                        │
│ 2026-09-18 12:40:28                                          │
│ time      cps   counts  seconds  cpm_3  cpm_30  ...          │ the log, as
│ 13:03:54  0.767  23      30      20.0   46.0    ...          │ the server
└──────────────────────────────────────────────────────────────┘   wrote it
```

Vertical space is deliberately tight: the dials and the numbers sit *side by
side* rather than stacked, because the readout alone consumed a third of the
height and left the column beside it empty. The charts take whatever the window
has left, split 62/38 in the cascade's favour, so a tall window grows the
instruments rather than the margins.

### C. The dials

One per counter. A 270° sweep from down-left to down-right -- the arc a needle
can cross without the eye losing it, and what every speedometer and tachometer
does. The level bands are painted onto the face at the same thresholds the rest
of the program uses, so the colour under the needle and the colour of the number
above it agree by construction rather than by being kept in step by hand. Fifty
ticks, every fifth major. A counterweight behind the hub, which is what stops a
needle looking like a clock hand. The reading in digits on the black face
beneath, as a marine gauge does it.

**The full scale is chosen from a fixed list** (60, 120, 300, 600, 1200, 3000,
6000 CPM) with 15% headroom. A gauge whose range moves continuously is not a
gauge: the needle has to mean the same thing minute to minute.

With two counters the two needles side by side answer *do they agree?* before
any number has been read.

#### Three time constants on the collected face

A dial can show more than one answer at a time and a number cannot, which is
most of the argument for drawing one. The collected face carries the **three
seconds** its needle points at, the **half minute** the chrome pointer sits at
— the same window the headline number is quoted over — and the half minute and
minute the needle has been bouncing between, as the two drifting arcs outside
the bands. The gap between needle and pointer *is* the trend.

Three seconds rather than one because one second of one tube is a handful of
arrivals, and a needle drawn from it draws the counting statistics rather than
the room; at three times the counts it is √3 steadier and still quick enough
to show a source passing under the tube. The one-second figure is printed on
the plate under the bezel, which is where a number that jumps several times a
second belongs.

**The needle has mass.** `Ballistic` leans it out with a 0.25 s constant and
settles it back with 0.9 s — fast enough not to smooth away a real excursion,
slow enough that one Poisson lump does not read as a spike. It is the same
idea as the range bugs with a different asymmetry: a bug must not *miss* an
excursion, so it snaps out instantly; a needle must be *readable*, so it
accelerates instead. Without it the needle teleports once a second and the eye
cannot follow which way it went.

**Only the reading goes on the face.** A face is round and a line of text is
not: everything drawn on it has to fit the chord at its own height, and the
band arcs sit at 0.82 of the radius, so a line forty pixels below centre has
65 pixels before its *ends* cross them. Both supporting lines therefore sit on
a nameplate under the bezel, where the width available is the dial's whole
share of the row and does not depend on how far down the line is.

#### The reading does not wait for the second after it

The collected meter used to close a second only when the **first sample of the
next one** arrived — so the needle always showed a second that had already
finished, and with several tubes it was whichever tube ticked over first that
ended the wait. `Recent` rolls a window over the arrivals themselves and has
the second the moment it has gone by, at any tube count.

Its divisor is the **samples in the window, not the span times the tubes**.
Each sample covers one second of one tube, so `sum * 60 / samples` is counts
per minute per tube whatever happens to a tube mid-window; dividing by
`span * tubes` reports a tube that has stopped answering as the room having
gone quiet.

The meters travel apart from the snapshot and twelve times as often — a
`Meters` is a handful of floats where a `Snapshot` is the cascade, three
spectra and the table — and **only when something has moved**. A settled
reading sends nothing between snapshots, which matters because this program's
usual home is a VM with no GPU, where every message is a full software
re-render of the panel. `Client::poll` is what makes the short wake-up safe:
`next` collapses a timeout and a closed socket into the same `None`, which is
right at ten seconds and would read every twelfth of a second as a dead
counter.

### D. The cascade

`analysis::tiers_with`, at a fixed `STRIP_COLS = 240` columns regardless of
window width. Fixed, because the captions above it are text **widgets** given
the tiers' widths as fill portions, and a canvas cannot tell a widget how wide
it turned out to be. The canvas stretches those columns to whatever width it is
handed.

### E. The spectrum overlay

Decay is Poisson and the power spectrum of a Poisson process is flat, so a
featureless panel is the good answer. Something arriving on a schedule is not
decay.

A spectrum resolves periods only up to its own window, so one window is always
the wrong question for an unknown period. Three run at once -- 512 s, 4096 s,
32768 s -- drawn over one another **on a shared logarithmic period axis**, long
periods to the left, each in its own primary at 55% alpha. A real line stands at
the same *x* in every layer that can reach it, which is most of what separates
it from a fluke; layers that cannot reach it stop short, and a period only the
longest window sees is exactly the one worth doubting.

**The floor is solved, not chosen.** A window of *W* seconds has a bin at every
*W/n*, so bins crowd towards the short-period end: at two seconds a nine-hour
window has sixteen thousand of them and the axis has a few hundred columns.
Folding twenty-seven bins into one column and taking the loudest does not draw a
line -- it draws the largest of twenty-seven draws, which is high by
construction and high *everywhere*, and the resulting wall of noise conceals
precisely what the panel exists to find.

A layer therefore stops where its bins are closer together than a bar is wide.
Bins are spaced *P/W* of an e-fold at period *P*, so over an axis of *span*
e-folds and *C* columns that is one column when

    P_floor(W) = W · span / C

and the axis floor is the shortest window's own floor. This is implicit -- the
floor sets the span and the span sets the floor -- so it is iterated to a fixed
point, converging in three or four passes. For the ladder above at *C* = 600 it
lands at **7.2 s**, and the bands become `7s–8m`, `58s–1h 8m`, `7m–9h 6m`. Two
seconds remains a hard floor regardless: it is the Nyquist limit of a
one-second sample.

An earlier hand-picked floor of 30 s was wrong in the other direction, discarding
an octave the 512-second window could honestly have shown.

### F. Colour is the only legend

Tube A and tube B have colours, used for their identity lines, their per-tube
readings and their bars in the finest cascade tier. The three spectrum layers
have colours, used for their bars and their verdict lines. Nothing anywhere is
labelled "key"; the colour *is* the key, and it is consistent across every
component that mentions the thing.

---

## V. Three constraints of the renderer

Discovered in the order below, each costing more than it should have, and each
now recorded at the point in the source where it would otherwise be
reintroduced.

### A. A static musl binary cannot `dlopen`

Rust on a musl host defaults to `+crt-static`. Every Wayland toolkit reaches
`libwayland-client` through `dlopen` at runtime. A fully static binary has no
dynamic loader in the process, so the call cannot succeed, and the error
surfaces as

```
Create event loop: Os(... WaylandError(Connection(NoWaylandLib)))
```

which reads as *Wayland is not installed* and is nothing of the kind -- the
libraries were present and loaded perfectly from every other program on the
machine. `gui/.cargo/config.toml` disables the static link for this crate alone;
the core binary stays static, which is what the releases page ships.

### B. Only the last canvas in a view is drawn

With the cascade above and the spectrum below as two `canvas` widgets, the
spectrum rendered and the cascade was an empty box. A solid fill over the whole
cascade area vanished identically, which is what finally identified it after an
afternoon spent blaming the bar arithmetic. Deleting the spectrum widget brought
the cascade back.

Consequently **the entire instrument panel is one canvas** -- dials, cascade and
spectrum, splitting its height between them.

### C. A `fill_text` takes its frame's geometry with it

Text drawn into a canvas program caused every rectangle in that frame to be
dropped. Splitting the two into separate `Frame`s did not help. So the canvas
draws shapes only, and every caption around it is an ordinary text widget laid
out by the same engine, at the same sizes, in the same font -- which is better
typography besides.

These two facts together dictate the layout mechanics: one canvas, and a
`stack` placing the readouts and captions over it.

---

## VI. Application structure

### A. Two crates, and why

```
radbeeper/                  one dependency, and it is libc
├── src/
│   ├── main.rs             CLI, Feed, Bank, watch, service, hotplug
│   ├── lib.rs              the library face, so the format is testable
│   ├── broker.rs           the fan-out: Server, Client, protocol
│   ├── serial.rs           termios, flock, poll
│   ├── counter.rs          find, find_all, the GMC conversation
│   ├── analysis.rs         Windows, tiers_with, Spectrum, Ladder
│   ├── entropy.rs          the pool, emissions, audit
│   ├── log.rs              the TSV format, Writer, sites
│   ├── history.rs          the counter's flash, backfill
│   ├── export.rs           index.html, random.html, together()
│   └── clock.rs, sha256.rs
├── examples/format_oracle.rs   what this crate would write, for the differ
├── tests/                      the Python differential suite
└── gui/                    a separate crate, NOT a workspace member
    ├── Cargo.toml          [workspace] — detaches it from the parent
    ├── .cargo/config.toml  -crt-static, for §V.A
    └── src/main.rs         App, Snapshot, Chart, the feed thread
```

The core crate's manifest carries an essay about having exactly one dependency.
Iced brings several hundred, which is the correct price for a GPU-accelerated
window and the wrong price to put on `cargo install radbeeper`. An empty
`[workspace]` table in `gui/Cargo.toml` detaches it from the parent, so the root
lock file does not grow by two orders of magnitude and `cargo package` never
sees it.

The GUI has **no serial code at all**. It cannot open a port; it can only attach
to one being served. That is not a discipline, it is a property of the binary.

### B. The abstraction that makes one monitor serve both roles

```rust
enum Feed {
    Own { bank: Bank, srv: Option<Server> },   // holds the flock, must serve
    Attached(Client),                          // holds nothing, writes nothing
}
```

`Feed::open` asks the socket, then `/dev`. The drawing code cannot tell which
variant it has, and that is the test of whether the abstraction sits in the
right place: everything below `next_sample` is arithmetic, and the port is not
part of it.

`Bank` is the multi-counter reader: a thread per counter feeding one
`mpsc::Receiver`, which restores the samples in arrival order -- the order the
display wants. A thread each costs a few kilobytes of stack and makes the quiet
case free; reading two descriptors in turn on one thread would cost the live
counter a 2.5-second timeout every second that the other was unplugged.

### C. A hazard the structure created

`Bank::open` originally started its reader threads immediately. `watch` then
held its backfill conversation -- request and reply -- over the same file
descriptor. The reader thread consumed the flash as though it were counts and
handed the backfill counts as though they were flash. The monitor displayed

```
now  16383 counts this second
run  226110 counts in 1s
```

16383 is `COUNT_MASK`: every bit set. Nothing about the symptom suggested a
race; it suggested a broken counter. Reading is now a separate `Bank::start`,
called only after every port conversation is finished, and the comment at that
function says why at length.

### D. Process lifecycle

| Process | Started by | Holds | Writes |
|---|---|---|---|
| `radbeeper service` | OpenRC at boot; udev on plug-in | the flock(s) | logs, emissions, the socket |
| `radbeeper hotplug` | the desktop autostart, in the session | nothing | nothing |
| `radbeeper-gui` | hotplug, or by hand | nothing | nothing |
| `radbeeper watch` | hotplug `--tui`, or by hand | the flock **only if nothing is serving** | logs **only if it holds the flock** |

The split between the first two is deliberate and predates this work: udev fires
as root with no display, no session bus and no way to know which of several
logged-in people a window would belong to. Starting a daemon asks none of those
questions; opening a window asks all of them. So the rule starts the logger and
the session opens the window.

---

## VII. Measurements

Taken against a GQ GMC-320 Plus on `/dev/ttyUSB0` and a synthetic Poisson
counter on a pseudo-terminal, 18 September 2026.

| Observation | Result |
|---|---|
| `probe` with the service holding the port | identifies both counters; previously `port busy` |
| Two monitors attached simultaneously | identical readings (`98 counts in 168s`, 36.0 CPM at 30 s) -- the stream is shared, not halved |
| Service logging throughout | uninterrupted; no gap in the record |
| Window opened against a 43-minute-old service | `1750 counts in 2627s` immediately; 300 s window already full |
| Two counters, separate logs | real tube 0.42 cps, synthetic 14.7 cps, kept apart |
| Combined of two matched tubes | 38.8 CPM ±0.1 (0.17%), 361,454 arrivals over 155.1 tube-hours |
| Agreement, matched tubes | 0.7 σ -- *they agree* |
| Agreement, 25 CPM against 880 CPM | 118.6 σ -- *they disagree; not the same measurement* |
| Measured interleave, two tubes | 0.50 s (100%) |
| Spectrum floor, solved | 7.2 s; bands `7s–8m`, `58s–1h 8m`, `7m–9h 6m` |
| Rust test suite | 112 tests pass (107 core, 5 GUI); clippy clean on both crates |

---

## VIII. Limitations

1. **The 0.5-second tier is a sampling rate, not an integration window.** Each
   bar remains one tube's one-second reading. §III.C.
2. **The interleave cannot be controlled**, only measured. Two counters whose
   clocks happen to align give precision but no extra time resolution.
3. **The nine-hour spectrum takes nine hours.** The replay ring holds 30,000
   samples -- about eight hours of one counter, four of two -- so the longest
   layer fills in real time rather than on connect. It says how long it has to
   go.
4. **Commands that need the port still need the port.** `clock --set`,
   `log pull` and `backfill` are conversations *with* the counter, and only the
   holder can have one. Only reading is fanned out.
5. **`port busy` has not been abolished, and should not be.** It now means the
   holder is not sharing -- a `random`, a `backfill`, a one-shot -- which is a
   different and rarer situation, and the message says so.
6. **The terminal monitor is frozen, not finished.** `watch` remains correct for
   a single counter and is what `hotplug --tui` opens where no GUI is installed;
   it was deliberately left byte-identical in the single-counter case so the
   differential suite still guards it. It does not display a second tube.
7. **One differential test fails**, `test_the_table_fits_a_thirty_row_screen_under_the_clock`.
   Verified identical against a clean build of the previous revision: it is
   pre-existing and timing-sensitive, depending on whether a 128-sample spectrum
   has filled by the fourteen-second mark, which slips when the machine is busy.
8. **The exporter's byte-for-byte agreement with the Python program is over.**
   The page now carries a combined section, an overlay and an audit page per
   counter, none of which the Python writes. This was a deliberate decision, not
   a regression.

---

## IX. Summary

The exclusive lock on a serial port protects against two readers splitting a
byte stream. It says nothing about how many programs may *consume* what one
reader produces. Recognising that distinction converted a handover protocol --
with its hole in the record every time somebody looked at the instrument -- into
a publisher with any number of subscribers, at a cost of one unix socket, one
text protocol and a 700 KB ring buffer; made a freshly-opened window more
complete than the old one ever was at any age; and generalised without further
work to two counters, where the same stream carries both and the display is free
to average what the record keeps apart.
