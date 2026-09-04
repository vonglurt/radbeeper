# RadBeeper

**A GQ GMC-320 Plus Geiger–Müller counter on the desk, read from Alpine Linux.**

MIT · `0.1.0` · one file, `python3` and nothing else

The counter is a USB-serial device. Plug it into a machine running Alpine —
including a VM the counter is shared into over USB pass-through — and RadBeeper
finds it, shows what it is counting, pulls the history it recorded while nobody
was watching, and builds a web page out of the result. Fork this repository,
drop your own logs into `logs/`, and a GitHub Action regenerates that page on
every push.

```sh
radbeeper probe            # find the counter and say what it is
radbeeper watch            # the monitor: 3s / 30s / 300s at once
radbeeper service          # log to disk, a row every 30 seconds
radbeeper backfill         # fill the log's gaps from the counter's own flash
radbeeper site             # where this counter is, and where it has been
radbeeper export           # build index.html from the logs
radbeeper log pull         # download the raw history to .bin and .csv
```

---

## 1. Install

No packages, no build. The program is one stdlib Python file and the install is
a copy:

```sh
git clone https://github.com/vonglurt/radbeeper.git
cd radbeeper
make install          # copies to ~/.local/bin/radbeeper
```

`make install` takes `PREFIX=` if you want it somewhere else. On Alpine you
need `python3` and nothing more.

**Add yourself to `dialout`**, or the serial node will not open:

```sh
doas adduser $USER dialout    # then log out and back in
```

## 2. Find the counter

```sh
radbeeper probe
```

![radbeeper probe](docs/screenshots/probe.png)

If it does not find one, the message says which of three things went wrong,
because they have three different fixes — see [Troubleshooting](#troubleshooting).

## 3. Watch it

```sh
radbeeper watch
```

![radbeeper watch](docs/screenshots/watch.png)

Three averages at once, because one is not enough. The counter's own reading is
a rolling 60-second count: one number, one time constant, one question
answered. RadBeeper counts the blips itself and keeps three windows:

| Window | What it is for |
|---|---|
| **3 s** | Watching a source come and go as you move it. Jumpy, and honestly so |
| **30 s** | Reading the room. Settled enough to compare two places |
| **300 s** | A number worth writing down |

**A window shows nothing until it is full.** Start the monitor and the long
window tells you how long it still needs:

![radbeeper watch, still filling](docs/screenshots/watch-filling.png)

A three-second CPM built from one sample is twenty times noisier than it looks,
and drawing it as though it were settled is how a 25 CPM background reads as 60
and somebody goes hunting for a leak.

The colour bands are calm / raised / high. `q` quits. The row of blocks along
the bottom is the last few minutes, one block per second.

### The spectrum, and why flat is the good answer

The monitor also accumulates a power spectrum of the per-second counts and
draws it along the bottom, coloured, one column per column of your terminal.

![the accumulating spectrum](docs/screenshots/watch-spectrum.png)

**Radioactive decay is a Poisson process, and the power spectrum of a Poisson
process is flat** — white noise, every frequency carrying the same expected
power. So a healthy counter watching background produces no shape at all, and
that featureless strip is the useful result: a statement that nothing periodic
is happening.

The feature earns its place on the other case. A peak means something is
arriving on a schedule, and decay does not have a schedule — mains hum on the
tube's high-voltage supply, a fan or a pump carrying a source past, a loose
connector chattering, firmware that batches its reporting. In the time domain
all of those look exactly like more counts, and no amount of staring at a CPM
number separates them from noise.

It **accumulates**, which is what makes it readable: a single periodogram of a
Poisson process is flat in expectation and violently noisy in fact — every bin
an exponential variable whose standard deviation equals its own mean. Averaging
*N* of them divides that scatter by √*N*, so a real line climbs out of the grass
while the grass settles.

That also means the significance needs **no extra state kept alongside**. The
scatter of white noise averaged *N* times is exactly 1/√*N*, so how many sigma a
bin stands above flat is arithmetic on the number of windows:

```
spectrum   flat -- arrivals look random, as decay should (28 windows)
spectrum   peak at 8s, 9.4x the mean, 7.2 sigma (28 windows)
```

Windows are **half-overlapped** (Welch rather than Bartlett), which gets two
averages out of each window's worth of data instead of one — the noise settles
about 1.4× faster for the same waiting. The segments share half their samples,
so the scatter falls as though there were 9/11 as many, and that is what the
sigma is judged on.

### Resolution grows with time, because it has to

Frequency resolution is 1/*T* for an observation of length *T*. You cannot
resolve a 512-second period in 128 seconds of listening — that is physics, not a
limitation of the program — so a long window is strictly better in the end and
strictly slower to say anything at all.

Rather than pick one, RadBeeper runs a **ladder**: 128, 256 and 512 seconds side
by side, all fed the same samples. The 128 answers after two minutes; the 512
takes eight and a half but resolves four times as finely, and by the time it has
anything to say it is the better answer. The display shows the finest rung with
enough averages behind it and falls back as far as it must. It costs about ten
kilobytes, fixed for the life of the process — a running sum of periodograms
*is* the average, given the count of them, so nothing accumulates per hour.

The axis runs from long periods on the left (the window) to short on the right
(2 s, the Nyquist limit). Both charts are **coloured by what they mean**: the
counts by the same calm / raised / high bands as the numbers above them, and the
spectrum by significance — because a bin at twice the mean is exciting after
fifty windows and meaningless after two, and the colour is the only place that
difference can show.

### Line output, for a pipe or a log

`--plain` gives one line per second instead of the full-screen monitor. It is
also what you get automatically when stdout is not a terminal, so
`radbeeper watch > file` does the sensible thing:

```sh
radbeeper --plain --duration 14 watch
```

![radbeeper --plain watch](docs/screenshots/watch-plain.png)

### No counter on the desk?

Every command runs against a built-in source, and it is a real one — radioactive
decay is a Poisson process, so the simulator draws Poisson samples. Variance
equals the mean, which is exactly the property that makes the 3-second average
jump and the 300-second one sit still.

```sh
radbeeper --source sim --sim-cpm 400 watch
```

## 4. Log it to disk

```sh
radbeeper service         # what the boot service runs, in the foreground
```

One row every 30 seconds into a dated file per counter:

![the log on disk](docs/screenshots/log-output.png)

| Column | |
|---|---|
| `time` | first and big-endian, so `sort cpm-*.tsv` is chronological with no flags |
| `cps` | counts per **one** second, whatever the row spacing |
| `counts`, `seconds` | so the division can be checked, and a short row is obviously short |
| `cpm_3` … `cpm_300` | the three windows, empty until full |
| `peak_3` … `peak_300` | the highest each window reached **since the last row** |
| `src` | `live` if this machine measured it, `flash` if it was reconstructed |
| `site` | where the counter was at that row's own time |

**The peaks are the point.** A row carrying only the averages as they stood at
the instant it was written would miss a source that came and went between two
rows — which is the one event actually worth having a log for.

Tabs are invisible and that matters here, because an **empty field is not a
zero** — it is a window that was not full yet. Made visible:

![the log with its tabs shown](docs/screenshots/log-tabs.png)

State lives in `/var/log/radbeeper` when that is writable and
`~/.local/share/radbeeper` when it is not. Files rotate by month and by counter
(`cpm-<serial>-YYYY-MM.tsv`), which needs no cron entry and nothing that renames
a file while a service is appending to it.

### As a boot service

Copal's stage 10 installs an OpenRC service and a udev rule. **Dormant is the
normal state**: with no counter plugged in the service writes down why and
exits 0 — a stopped service, not a crash loop. A USB device that is not plugged
in will not become plugged in because a daemon asked again four seconds later.

```sh
rc-service radbeeper start        # or just plug the counter in
cat /var/log/radbeeper/status     # what it is doing, and why
```

`radbeeper hotplug` is the other half: it sits in your desktop session and opens
the monitor in a terminal when a counter appears — at login if one is already
there, and on plug-in at any point after.

## 5. Backfill from the counter's own memory

The counter records to its flash whether or not anything is listening. When the
service starts it reads the tail of that flash and fills the log's gaps, so one
file answers "what was it doing" for the whole period and not only the parts
somebody was watching.

```sh
radbeeper backfill                          # from the counter
radbeeper backfill --image hist.bin --serial ABC123
radbeeper --clock-offset 2180 backfill      # if you have since set its clock
```

Three things it gets right:

- **One row per slot.** A row is identified by the log interval it falls in, so
  a backfill can never write over a live measurement, and a slot with no
  evidence stays absent. Nothing is interpolated across a gap.
- **The counter's second is measured, not assumed.** It is 1.011 of ours on the
  unit this was written against — 39 seconds of drift an hour — so the spacing
  is taken from each pair of timestamps in the recording.
- **The flash is a ring.** On a counter that has been running a while there is
  no unwritten byte in it: the newest sample sits just *before* the write
  pointer. Reading the physical tail would hand back the oldest hours while
  claiming they were the newest.

## 6. Say where it is

```sh
radbeeper site                        # where is it, and where has it been
radbeeper site --name "The garage"    # it moved, from now
```

![site, log info and export](docs/screenshots/commands.png)

A reading without a place is half a measurement, and these get carried about —
so the place is a property of a **serial number over time**, appended to
`sites.tsv`, never overwritten. A reading from last Tuesday resolves to where
the counter was last Tuesday.

It records a **name and nothing finer, on purpose**. These logs get published; a
place name is what a reader needs, while a decimal fix is a street address for
whoever is holding the counter. Nothing is assumed either: until you record a
place, the column is empty.

## 7. Publish it

```sh
radbeeper export                      # index.html from the state directory
radbeeper export --logs logs -o index.html
```

One self-contained page: summary cards, a log-scale plot of counts per minute by
the hour, a by-day table and the latest rows. **No JavaScript, no web fonts, no
CDN** — the chart is SVG the program draws itself, and the full record is one
link away as the file it already lives in.

To put it on the web: **fork this repository, copy your `cpm-*.tsv` and
`sites.tsv` into `logs/`, and push.** `.github/workflows/pages.yml` rebuilds
`index.html` and commits it back, so GitHub Pages serves it with no build step.
There is nothing to install in the workflow — the generator is this same file,
which is also why the page cannot drift from the log format.

## Troubleshooting

`radbeeper probe` distinguishes the ways a counter fails to appear, because
they have different fixes:

**1. No serial node.** Either nothing is plugged in, or the running kernel has
no USB-serial driver. Alpine's `linux-virt` ships none — no `ch341`, no
`usbserial` — so a counter plugged into a VM running it can never appear as
`/dev/ttyUSB0`, and `dmesg` is silent because nothing ever claims the device.
`linux-lts` and `linux-rpi` carry the drivers.

```
no counter: no USB-serial driver in this kernel
```

**2. Permission denied.** The node is `root:dialout`. `doas adduser $USER
dialout`, then log in again.

**3. Something is there but is not a GMC.** A CH340 is a generic USB-serial
cable and plenty of things that are not Geiger counters use one.

**4. The port is busy.** Only one program can read a serial device sensibly —
two readers share the bytes between them and neither is told — so RadBeeper
locks the port. If the logger has it:

```
port busy: the port is already open by another radbeeper
    doas rc-service radbeeper stop  hands it over.
```

The service waits on the lock rather than giving up, so the log picks up again
by itself when you close the monitor.

## Reference

### Commands

| | |
|---|---|
| `probe` | find the counter and say what it is |
| `watch` | the monitor; `--plain` for line output |
| `cpm` | the counter's own CPM once, for a script |
| `service` | monitor and log; dormant when there is nothing to read |
| `hotplug` | sit in the session, open the monitor on plug-in |
| `backfill` | fill the log's gaps from the counter's flash |
| `site` | where a counter is, and where it has been |
| `export` | build `index.html` from the logs |
| `log info` / `log pull` | how much history flash, and download it |

### Speed

The monitor's budget is one sample a second and it uses a fraction of a percent
of it — a 512-point FFT is 0.5 ms, once every few minutes. The one place that
ever mattered was `backfill`, and it turned out to be an algorithm rather than a
language: each window re-summed its own tail on every sample, which is
O(samples × window) and cost 16.5 s to turn 850,000 samples into 28,000 rows —
765 million additions. Keeping a running sum per window and subtracting what
falls out the back is O(1) per sample. **Same output, byte for byte, in 2.3 s
instead of 16.5.**

Useful options: `--source sim`, `--sim-cpm`, `--seed`, `--spans 3,30,300`,
`--cpm-per-usvh`, `--log-every`, `--duration`, `--clock-offset`,
`--backfill-bytes`, `--device`, `--baud`.

### The tube factor

µSv/h is CPM divided by a number that belongs to the **tube**, not the counter.
The default, 151.5, is the M4011 in a GMC-320. A 500 with a different tube needs
a different number, which is why it is `--cpm-per-usvh` and not a constant
buried in the arithmetic.

### Protocol

GQ's RFC for the GMC series. Commands are ASCII `<NAME>>`; replies are raw bytes
with no framing, so every read asks for an exact count and times out rather than
blocking.

| Command | Reply |
|---|---|
| `<GETVER>>` | 14 bytes, e.g. `GMC-320Re 4.26` |
| `<GETSERIAL>>` | 7 bytes |
| `<GETCPM>>` | 2 bytes, big-endian |
| `<GETCPS>>` | 2 bytes, mask `0x3FFF` |
| `<HEARTBEAT1>>` | then 2 bytes every second until `<HEARTBEAT0>>` |
| `<GETVOLT>>` | 1 byte, tenths of a volt |
| `<GETDATETIME>>` | 7 bytes |
| `<SPIR[addr][len]>>` | `len` bytes of history flash |

Baud is 115200 on the 320 and 57600 on the 300; RadBeeper tries both.

There is no `pyserial` here. Alpine packages it, but this runs on a Pi Zero with
512 MB and on a fresh install with no network, and a serial port is thirty lines
of `termios`.

**The history format is best-effort and the raw bytes are always kept.** `log
pull` writes the flash image to disk first and decodes second: a decoder that
mis-reads a marker costs a bad CSV, never the data. Two corrections to GQ's
published document, both measured against a full 1 MiB image from a GMC-320Re
4.26 — the datetime record is **nine** bytes and not ten, and `55 AA 01` is a
**three-byte marker that carries no payload**, not a two-byte count. Reading it
as a count invented 1,701 readings between 256 and 21,930 counts per second on a
tube that saturates three orders of magnitude below that.

### Tests

```sh
make check        # syntax, then 104 tests: no hardware, no network
```

`tests/fake_gmc.py` serves a fake GMC-320 on a pseudo-terminal, so the serial
path — termios, exact-length reads, command framing, the heartbeat stream, the
chunked history download — is tested without a counter on the desk. It also runs
standalone:

```sh
python3 tests/fake_gmc.py --cpm 400
radbeeper -d /dev/pts/N watch
```

---

MIT — Copyright (c) 2026 Paul Richeson
