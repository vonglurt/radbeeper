# RadBeeper

**A GQ GMC-320 Plus Geiger–Müller counter on the desk, read from Alpine Linux —
and from [Copal](https://github.com/vonglurt/copal), its distillation.**

MIT · `0.2.0` · `cargo install radbeeper` · one dependency, and it is `libc`

**You need a GQ GMC-320 Plus plugged into USB.** There is no substitute for it
in software: RadBeeper reads a real tube over a real serial port, and every
number on the screen comes off the wire. Plug it into a machine running Alpine
— **including a VM with the counter passed through, which is what these logs
were taken on** — switch it on, and RadBeeper finds it, shows what it is
counting, pulls the history it recorded while nobody was watching, and builds a
web page out of the result.

```sh
cargo install radbeeper            # or a static binary from the releases page
doas adduser $USER dialout         # once, then log out and back in
radbeeper probe                    # it should name your counter
```

[§1](#1-what-you-need) is the full list of what has to be true — and if you are
running Alpine in a VM, **[the kernel is the thing that catches people](#1-what-you-need)**:
`linux-virt` ships no USB-serial driver at all, so a perfectly good
pass-through produces no `/dev/ttyUSB0`.

![the monitor](https://raw.githubusercontent.com/vonglurt/radbeeper/main/docs/screenshots/watch.png)

**The fourth minute, in four seconds.** Seconds 180 to 240 of the session
below, a frame a second played at fifteen times speed:

![the monitor, the fourth minute at 15x](https://raw.githubusercontent.com/vonglurt/radbeeper/main/docs/screenshots/watch-3to4.gif)

A minute is how long it takes to see that this is a monitor and not a
screenshot. The 3-second window swings between 0 and 160 CPM while the
30-second one walks 28 up to 46 and back down to 22 — the last of the high
readings draining out of it. Two log rows close, thirty seconds apart. The
5-minute window counts itself down from 140 s to go to 79, the spectrum picks
up its second window, and the counts strip slides left a bar at a time.

**Four minutes of it, at forty times speed.** A whole session of the native
build against the counter these logs came from, a frame every four seconds,
recorded while something outside, probably construction, was
pushing the count to three times its usual background and then stopped:

![the monitor, a whole 240-second session at 40x](https://raw.githubusercontent.com/vonglurt/radbeeper/main/docs/screenshots/watch-fast.gif)

It opens on the monitor reading the counter's history to fill the log's gaps,
about twenty seconds of flash over the serial line. Then the windows arrive in
order, 3 s and then 30 s, while the three long ones count down. Early on the
30-second window sits between 84 and 124 CPM with red spikes through the
counts; by the end it is at 22. The log table fills from the bottom: first the
rows the backfill rebuilt, dim, then a live row every thirty seconds. The
counts strip on the left compresses as it ages, two and then four seconds a
bar.

**Twenty seconds of it, at ten times speed.** Seconds 200 to 220 of the same
session, one frame a second:

![the monitor, twenty seconds at 10x](https://raw.githubusercontent.com/vonglurt/radbeeper/main/docs/screenshots/watch-20s.gif)

The 3-second window swings between 0 and 60 CPM while the 30-second one holds
between 38 and 46, which is the whole argument for keeping more than one. The
5-minute window is still filling, from 119 s to go down to 99; the 50-minute and
working-day windows are further off. The bars recolour as individual seconds
land, the `¦` over the fine tier marks where each log row closes, and there is
no random line yet: the pool is still measuring the source.

## Fast track

With the counter plugged in and switched on:

```sh
radbeeper probe      # find it, confirm it's talking
radbeeper watch      # the monitor — q to quit
```

That is the whole of it. The `dialout` line in the install above is not
optional: the serial node is `root:dialout` and RadBeeper does not want root.
**Only one program can hold the port**, so if the boot service is logging,
`probe` says `port busy` and names it — `doas rc-service radbeeper stop` hands
the counter over, and `start` gives it back.
If `probe` finds nothing it says which of four things went wrong, and they have
four different fixes — [§1](#1-what-you-need) is the list of what has to be
true. **No counter yet?**
`radbeeper --source sim --sim-cpm 400 watch` draws the entire monitor against a
synthetic Poisson background, which is a real one: decay is a Poisson process.
That verb is the Python program's — a `cargo install radbeeper` does not carry
it. What the Rust build has instead is `tests/fake_gmc.py`, which puts a
counter on a pseudo-terminal and exercises the serial path as well as the
display: `python3 tests/fake_gmc.py --cpm 400`, then `radbeeper -d /dev/pts/N
watch`.

<details>
<summary>Everything else it does</summary>

```sh
radbeeper service          # log to disk, a row every 30 seconds
radbeeper backfill         # fill the log's gaps from the counter's own flash
radbeeper random           # 256 bits of hex, out of decay timing
radbeeper site             # where this counter is, and where it has been
radbeeper export           # build index.html and random.html from the logs
radbeeper log pull         # download the raw history to .bin and .csv
radbeeper clock --set      # set the counter's clock from this machine's
```

</details>

---

## 1. What you need

**A GQ GMC-320 Plus, plugged into USB.** That is the hardware, and there is no
substitute for it in software: RadBeeper reads a real tube over a real serial
port, and every number on the screen comes off the wire. Without a counter
plugged in there is nothing to read — though `--source sim` will draw the whole
monitor against a synthetic Poisson background if you want to see it working
before yours arrives.

| | |
|---|---|
| **The counter** | A **GQ GMC-320 Plus**. The 320 is what this was written against and what every screenshot here is. A **GMC-300** works — RadBeeper tries its 57600 baud as well as the 320's 115200 — and a 500 or 600 will be found and read, but its tube is not an M4011, so give it `--cpm-per-usvh` (see [The tube factor](docs/reference.md#the-tube-factor)). |
| **The cable** | The **USB cable that came with it**. The socket on the counter is USB-C on a Plus and micro-B on older units, and it is easy to grab a charge-only cable by mistake: one that carries no data leaves you at [Troubleshooting](docs/troubleshooting.md) case 1 with nothing plugged in as far as Linux is concerned. |
| **The counter, switched on** | The USB-serial chip inside is powered by the counter, not by the bus. A 320 that is off, or flat, enumerates as nothing. |
| **A kernel with `ch341`** | The 320 Plus presents as a CH340 USB-serial device. `linux-lts` and `linux-rpi` carry the driver; Alpine's `linux-virt` **does not**, which is the single most common reason a counter that is plugged in cannot be found. **Copal's stage 10 installs `linux-lts` when the running kernel is a `-virt` one**, so a Copal VM has the driver after its next boot. |
| **Membership of `dialout`** | The serial node is `root:dialout` and RadBeeper does not want root. |

Plug it in, and Linux should say so:

```sh
dmesg | tail -5                       # ch341-uart converter now attached to ttyUSB0
ls -l /dev/ttyUSB*                    # crw-rw---- 1 root dialout ... /dev/ttyUSB0
```

If `/dev/ttyUSB0` is there, you are done — `radbeeper probe` in [§3](#3-find-the-counter)
will identify it. If it is not, [Troubleshooting](docs/troubleshooting.md) names the four
things it can be and they have four different fixes.

**USB pass-through counts, and the kernel is what catches people.** These logs
were taken from a 320 Plus shared into a VM, and the counter cannot tell the
difference. What the VM's kernel needs is the same `ch341` — pass the device
through to a guest running `linux-virt` and it will never appear, however
correct the pass-through is.

It looks like this when the pass-through is working and the driver is not:

```
$ lsusb
Bus 003 Device 002: ID 1a86:7523  USB2.0-Serial      # the counter is right there

$ ls /dev/ttyUSB*
ls: cannot access '/dev/ttyUSB*': No such file or directory

$ find /lib/modules/$(uname -r) -name 'ch341*'       # nothing: linux-virt has none
```

`doas apk add linux-lts`, reboot into it, and the same device enumerates as
`/dev/ttyUSB0`. This is the trap **Copal** avoids by installing the `linux-lts`
Alpine package rather than the virt kernel a VM image would otherwise default
to: the counter appears because the driver is there to claim it.

## 2. Install

**The Rust build is the implementation.**

```sh
cargo install radbeeper                  # or a static binary from the releases
```

A static binary off the releases page needs no toolchain at all, which is the
point on a Pi. From a clone, `make install` is `cargo install --path .`: it
builds the release binary and puts it in `~/.cargo/bin`, which has to be ahead
of anything older on your `PATH` — `which -a radbeeper` shows the order.

The one-file `python3` program is still in the repository beside it and still
installs the same way — `make py-install` — because it owns the verbs the port
has not reached yet. `probe`, `clock`, `cpm`, `watch`, `service`, `random`, `backfill`, `log`, `hotplug`
and `export` are native; `site`, `recompute`, `--plain` and `--source sim` are
still the Python and are the reason it is still here.
[§10](docs/native-build.md) is that story.

**Add yourself to `dialout`**, or the serial node will not open:

```sh
doas adduser $USER dialout    # then log out and back in
```

## 3. Find the counter

```sh
radbeeper probe
```

![radbeeper probe](https://raw.githubusercontent.com/vonglurt/radbeeper/main/docs/screenshots/probe.png)

If it finds nothing, the message says which of four things went wrong, because
they have four different fixes — see [Troubleshooting](docs/troubleshooting.md).

### Its clock, and setting it

The counter keeps its own time, and nothing sets it for you — the one on this
desk had drifted to nearly two minutes fast. Every row RadBeeper rebuilds from
the counter's memory is placed by that clock, so `probe` says how far out it is:

```
its clock  2026-09-16 16:04:18   111.4 s ahead of this machine (±0.02 s)
           radbeeper clock --set  corrects it from this machine
```

**How it measures to a hundredth.** The counter answers in whole seconds, so
one reading is only good to a second — 0.85 s out, on this unit. RadBeeper asks
again, back to back, until the second changes: the counter ticked after the last
old answer was asked for and before the first new one came back, which pins the
tick between two round trips. Backfill uses the same measurement.

**Setting it:**

```
$ radbeeper clock --set
its clock      2026-09-16 16:17:32
               111.5 s ahead of this machine (±0.02 s)
this machine   synchronised (the kernel says NTP is steering it)
set            2026-09-16 16:15:43
               matches this machine (±0.02 s)

  history the counter recorded before now carries the old clock, 111 s
  ahead. A backfill applies one offset to everything it reads, so rows
  it rebuilds from before this moment will be out by that much.
```

`radbeeper clock` on its own is the measurement without the set. The set is
`<SETDATETIME>>`, sent as this machine's clock reaches the whole second it
names; the result is measured the same way, and if it landed off the timing is
corrected and it is sent once more. Three things to know first:

- **Only one program can hold the port.** Stop the service
  (`doas rc-service radbeeper stop`), set it, start it again.
- **Set it from a clock that is right.** `clock` asks the kernel whether NTP is
  steering this machine. A counter set from a clock nothing steers is only as
  right as that clock, and it says so rather than refusing.
- **Backfill first.** The counter's memory is not rewritten: what it recorded
  before the set keeps the old time, and a backfill applies one offset to
  everything it reads. If the log has gaps, fill them before you set the clock,
  or the rows rebuilt from before it will be out by what you corrected.

Afterwards, `radbeeper clock` may say `matches this machine (±0.1 s)` rather
than ±0.02. On this unit, after the set, the first answer after the counter's
second rolled over took about 250 ms every time, against 20–50 ms otherwise,
and a slow answer at the tick widens the bracket. The clock is still right to
the bracket; it is the measurement that got coarser.

## 4. Watch it

```sh
radbeeper watch
```

**The monitor is also the logger.** Only one program can hold the counter's
port, so while the monitor is open the service cannot log — and the log used to
have a hole exactly where somebody was watching. Now `watch` does what the
service does, with a screen on top:

1. **On connect it reads the counter's history** — "reading the counter's
   history to fill the log's gaps" — for fifteen or twenty seconds, and fills
   whatever the log is missing, from the ring of flash the counter kept while
   nothing was listening (see [§7](#7-backfill-from-the-counters-own-memory)).
2. **It writes a row every 30 seconds**, through the same code the service
   uses, to the same dated file.
3. **It appends every random line**, to `random-<serial>.tsv` with its counts
   and to `random-<serial>.hex` as a time and sixty-four digits.
4. **It writes `index.html` and `random.html`** beside the log: after the
   backfill, every hour, and when you quit.

What it did sits beside the clock. `--no-log` turns all four off, and
`--no-backfill` and `--no-export` turn off one each.

Six panels, top to bottom.

### The number, big

The 30-second CPM in twelve-row block digits, to the right of the serial. 3 s is
too jumpy to read as a headline and 300 s too slow to react to anything you are
doing with your hands, so the middle window is the one that gets the size. The
three horizontal bars are drawn two rows thick — a one-row bar between three-row
uprights reads as a scratch at this scale. It needs no font: the digits are made
from the same block glyph the charts are.

### Five averages, because one is not enough

The counter's own reading is a rolling 60-second count: one number, one time
constant, one question answered. RadBeeper counts the blips itself and keeps
five windows at once, each a factor of ten apart. **The row labels are
seconds** — that is what the program is actually averaging over — so here is
what each of them is in units anybody thinks in:

| Window | In plain time | What it is for |
|---|---|---|
| **3 s** | three seconds | **Too short to be a measurement.** On a 40 CPM background it swings between 0 and 80, because at this rate three seconds *is two counts*. Watch it to see a source come and go under your hand; do not write it down |
| **30 s** | half a minute | **The shortest window that is a count rather than a flicker.** Settled enough to compare two places, quick enough to follow your hands. This is the number in the big digits, and the one `radbeeper cpm` reports |
| **300 s** | **five minutes** | A number worth writing down |
| **3000 s** | **fifty minutes** | What the background here actually is, once the day's traffic through the room has averaged out |
| **30000 s** | **8 h 20 m — a working day** | Not a slower answer to the same question: the only window that spans a shift. A day that was different is visible against it as a difference |

Each is ten times the one above, and ten times is roughly what it takes for the
next one to be telling you something the last one wasn't. Going further would
be free — the windows keep running sums, so a span costs the same eight
microseconds a sample whatever its length — but 300000 s is three and a half
days, and nothing on a desk stays still that long.

**`radbeeper cpm` no longer asks the counter for its own number.** `<GETCPM>>`
returns the device's internal 60-second count, and that is a different time
constant from everything else here: the big digits are the 30 s window, so the
two disagreed by more than the noise with neither of them wrong. `cpm` now
counts the blips itself and prints its own 30 s average, on the same terms as
every other window — nothing until it is full, a note on stderr while it waits,
and the answer alone on stdout so it still pipes.

```sh
$ radbeeper cpm
counting for 30s...
40.0 CPM   0.264 uSv/h
```

`--spans` takes the list, so `--spans 1,10,60` is a different set of three
questions. Every window is a column in the log, whatever you choose. The
[animation at the top](#radbeeper) is twenty seconds of exactly this: the
3-second window swinging 0 to 60 while the 30-second one holds between 38 and 46.

**A window shows nothing until it is full**, and says how long it still needs:

![radbeeper watch, still filling](https://raw.githubusercontent.com/vonglurt/radbeeper/main/docs/screenshots/watch-filling.png)

A three-second CPM built from one sample is twenty times noisier than it looks,
and drawing it as though it were settled is how a 25 CPM background reads as 60
and somebody goes hunting for a leak.

### The counts, compressing as they age

Five rows tall. One row of block glyphs has eight levels, which is enough to say
something happened and not enough to say how much; five rows have forty.
Coloured by the same calm / raised / high bands as the numbers above it, so a
spike that reads red up there reads red down here without anyone converting in
their head.

**Three tiers, finer to the right.** The right half is one second a bar, newest
at the edge. The left half holds two more tiers, at *k* and *k²* seconds a bar,
with *k* the smallest factor that makes the whole strip reach back as far as the
spectrum's window — at 160 columns that is 80 s of seconds, then 40 bars of 3 s,
then 40 bars of 9 s, 559 s in all against a 512-second spectrum. A second that
scrolls off the fine tier lands in the newest bar of the next, which fills as
its seconds arrive, and that bar in turn lands in the coarsest.

```
9s/bar · 6m                 F 3s/bar · 117s               F 1s/bar · 80s      ¦            ¦
```

The `F` above each tier marks the hand-over, with how long a bar is and how far
back the tier reaches. Over the fine tier, a `¦` marks where each log row
closes: the frames the log is cut into, scrolling left with the counts.

- **A bar is a mean, not a sum.** A nine-second bar holding nine seconds of counts
  would dwarf the fine tier, and the colours are rates. The same height is the
  same rate in every tier.
- **A bar does not change as it scrolls.** Its edges are fixed to the count of
  samples, not to the screen, so a three-second bar always holds the same three
  seconds; only the newest, still-filling bar moves.

### The spectrum, where flat is the good answer

![the accumulating spectrum](https://raw.githubusercontent.com/vonglurt/radbeeper/main/docs/screenshots/watch-spectrum.png)

Radioactive decay is a Poisson process, and **the power spectrum of a Poisson
process is flat**. A healthy counter watching background produces no shape at
all, and that featureless strip is the useful result: a statement that nothing
periodic is happening.

It earns its place on the other case. A peak means something is arriving on a
schedule, and decay does not have one — mains hum on the tube's supply, a fan
carrying a source past, a loose connector, firmware that batches its reporting.
In the time domain every one of those looks exactly like more counts.

The panel accumulates windows so a real line climbs out of the noise, and it
will not call a peak on sigma alone, because the eye picks the tallest of 127
bins and the largest of many draws is far bigger than any single one.
[**How it decides, and why it is cautious →**](docs/the-spectrum.md)
### The random line, and the clock

256 bits of hex, out of decay timing, refreshed whenever the pool has earned
them — see [§6](#6-random-numbers-out-of-decay). Under it, the time now, so a
screenshot says when it was taken; beside that, what the log last did.

### The log, scrolling

The bottom of the screen is the log itself: its own header, then its newest
rows, oldest scrolling off the top like a terminal. They are the cells written
to disk, aligned into columns and cut at the screen's edge, so what is on screen
is what is in the file. Rows filled in from the counter's history are dim; an
empty field is a dim `-`, because a window that was not full yet is not a zero.

On a screen under 36 rows the charts go compact — the counts in three rows, the
spectrum in one — to leave the table six. From 36 rows the charts are full size
again and the table takes every row after 30.

### Line output, for a pipe or a log

`--plain` gives one line per second instead of the full-screen monitor, and is
what you get automatically when stdout is not a terminal:

```sh
radbeeper --plain --duration 14 watch
```

![radbeeper --plain watch](https://raw.githubusercontent.com/vonglurt/radbeeper/main/docs/screenshots/watch-plain.png)

### No counter on the desk?

The Python program runs against a built-in source, and it is a real one — decay
is a Poisson process, so the simulator draws Poisson samples. (`--source sim`
is not ported: the native build reads a counter or a `tests/fake_gmc.py`.) Variance equals the
mean, which is exactly the property that makes the 3-second average jump and the
300-second one sit still.

```sh
radbeeper --source sim --sim-cpm 400 watch
```

## 5. Log it to disk

```sh
radbeeper service         # what the boot service runs, in the foreground
```

One row every 30 seconds into a dated file per counter. `watch` writes the same
rows while it is open, so it does not matter which of the two has the port:

![the log on disk](https://raw.githubusercontent.com/vonglurt/radbeeper/main/docs/screenshots/log-output.png)

| Column | |
|---|---|
| `time` | first and big-endian, so `sort cpm-*.tsv` is chronological with no flags |
| `cps` | counts per **one** second, whatever the row spacing |
| `counts`, `seconds` | so the division can be checked, and a short row is obviously short |
| `cpm_3` … `cpm_3000` | one column per window, empty until that window is full |
| `peak_3` … `peak_3000` | the highest each window reached **since the last row** |
| `src` | `live` if this machine measured it, `flash` if reconstructed |
| `site` | where the counter was at that row's own time |

**The peaks are the point.** A row carrying only the averages as they stood at
the instant it was written would miss a source that came and went between two
rows — which is the one event actually worth having a log for.

Tabs are invisible and that matters here, because an **empty field is not a
zero** — it is a window that was not full yet:

![the log with its tabs shown](https://raw.githubusercontent.com/vonglurt/radbeeper/main/docs/screenshots/log-tabs.png)

State lives in `/var/lib/radbeeper` when that is writable, `/var/log/radbeeper`
when only that is, and `~/.local/share/radbeeper` otherwise. Files rotate by
month and by counter (`cpm-<serial>-YYYY-MM.tsv`), which needs no cron entry and
nothing that renames a file while a service is appending to it.

**`/var/lib`, not `/var/log`, because `/var/log` is often in RAM.** On Alpine
desktops it is commonly a tmpfs — it was on the machine these logs came from —
and every reboot emptied the log, leaving only what the counter's flash still
held to backfill from. A measurement record is state, not a log to rotate away.

### One log for the service and the monitor

The boot service runs as root and `watch` runs as you, and they should write the
same files. Make the directory the `dialout` group's, which you are already in
for the serial port, and have the service create files the group can write:

```sh
doas rc-service radbeeper stop
doas mkdir -p /var/lib/radbeeper
doas sh -c 'cp -p /var/log/radbeeper/*.tsv /var/log/radbeeper/*.hex /var/lib/radbeeper/ 2>/dev/null; true'
doas chown -R root:dialout /var/lib/radbeeper
doas chmod 2775 /var/lib/radbeeper
doas sh -c 'chmod g+w /var/lib/radbeeper/* 2>/dev/null; true'
# the service script: make the directory at start, files group-writable, status in the new place
doas sed -i -e 's|^\tcheckpath -d -m 0755 /var/log/radbeeper$|&\n\tcheckpath -d -m 2775 -o root:dialout /var/lib/radbeeper|' \
            -e 's|^command_background=true$|&\numask=002|' \
            -e 's|/var/log/radbeeper/status|/var/lib/radbeeper/status|g' /etc/init.d/radbeeper
doas rc-service radbeeper start
```

The setgid bit (the `2` in `2775`) makes every file created in the directory
belong to `dialout`, whoever creates it; `umask=002` makes the service's files
group-writable. `service.log` stays in `/var/log` — it is the service's own
output, and losing it at a reboot is what a log is for.

### As a boot service

[Copal](https://github.com/vonglurt/copal) — the Alpine distillation this was
written on — carries RadBeeper as stage 10, which installs an OpenRC service and
a udev rule alongside the `linux-lts` kernel that makes the counter visible in
the first place. **Dormant is the normal state**: with no counter plugged in the service writes down why and exits
0 — a stopped service, not a crash loop.

```sh
rc-service radbeeper start        # or just plug the counter in
cat /var/lib/radbeeper/status     # what it is doing, and why
```

**The service runs `/usr/local/bin/radbeeper`, not the one `make install` put
in `~/.cargo/bin`.** Upgrading your own copy leaves the boot service on
whatever was there before, so give it the new build too:

```sh
make build
doas rc-service radbeeper stop
doas install -m 0755 target/release/radbeeper /usr/local/bin/radbeeper
doas rc-service radbeeper start   # reads the flash first, then logs
```

On start it backfills from the counter's flash before appending anything —
`--no-backfill` skips that. If the new build logs a different set of windows
from the old one, it writes a second header line into the month's file
rather than writing five columns under a four-column header; both readers take
the last header above a row as that row's.

`radbeeper hotplug` is the other half: it sits in your desktop session and opens
the monitor when a counter appears — at login if one is already there, and on
plug-in at any point after.

## 6. Random numbers out of decay

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

**Three ways this normally goes wrong, and what is done instead.**

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

**The bits are measured, not modelled — and the model was wrong.** This used to
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

**[There is a page for this.](#9-publish-it)** `radbeeper export` writes
`random.html` beside the index: the counts drawn against the model, the bits
accumulating second by second with the target line across them, what the serial
link's one-second resolution costs, and every emission with the counts behind
it.

**What the interface costs.** `<HEARTBEAT1>>` gives two bytes once per second
and nothing finer, so one sample is one integer and that is the entire raw
material. If the link reported the *time* of each arrival instead, the gap
between two would be exponential, and quantised to a millisecond it would carry
about **10 bits per arrival** — roughly 8 bits a second at this background,
against the 0.62 actually available. GQ's protocol has no such message:
`<GETCPS>>` and the heartbeat both answer with a count, never a timestamp. The
limit is the interface, not the tube.

**Reproducible is not the same as predictable.** The counts behind each line are
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

> Treat this as a good physical entropy source, not a certified one. It has not
> been through a statistical test battery, and 256 bits of accounted min-entropy
> is a claim about the model of the source, not a proof about the output.

## 7. Backfill from the counter's own memory

The counter records to its flash whether or not anything is listening. When the
service starts it reads the tail of that flash and fills the log's gaps.

```sh
radbeeper backfill                          # from the counter
radbeeper backfill --image hist.bin --serial ABC123
radbeeper --clock-offset 2180 backfill      # if you have since set its clock
```

- **One row per slot.** A row is identified by the log interval it falls in, so a
  backfill can never write over a live measurement, and a slot with no evidence
  stays absent. Nothing is interpolated across a gap.
- **The counter's second is measured, not assumed.** It is 1.011 of ours on the
  unit this was written against — 39 seconds of drift an hour — so the spacing is
  taken from each pair of timestamps in the recording.
- **The flash is a ring.** On a counter that has been running a while there is no
  unwritten byte in it: the newest sample sits just *before* the write pointer.
  Reading the physical tail would hand back the oldest hours while claiming they
  were the newest.

## 8. Say where it is

```sh
radbeeper site                        # where is it, and where has it been
radbeeper site --name "The garage"    # it moved, from now
```

![site, log info and export](https://raw.githubusercontent.com/vonglurt/radbeeper/main/docs/screenshots/commands.png)

A reading without a place is half a measurement, and these get carried about — so
the place is a property of a **serial number over time**, appended to `sites.tsv`
and never overwritten. A reading from last Tuesday resolves to where the counter
was last Tuesday.

It records a **name and nothing finer, on purpose**. These logs get published; a
place name is what a reader needs, while a decimal fix is a street address for
whoever is holding the counter. Nothing is assumed either: until you record a
place, the column is empty.

## 9. Publish it

```sh
radbeeper export --logs logs -o index.html
```

`watch` writes both pages into the log directory by itself — after its
backfill, every hour and on quit — so a monitor left open keeps them current.

One self-contained page: a how-to, summary cards, a log-scale plot of counts per
minute by the hour, a by-day table and the latest rows. **No JavaScript, no web
fonts, no CDN** — the chart is SVG the program draws itself, and the full record
is one link away as the file it already lives in.

**`random.html` is written beside it** whenever there are emissions to account
for, and the index links to it. It is the one claim on the front page a reader
cannot check by looking — 256 bits out of decay — so the audit gets its own
page: the counts drawn against the Poisson model that used to be assumed, the
bits accumulating second by second with the target across them, what the serial
link's one-second resolution costs against timestamped arrivals, and every line
emitted with the counts behind it. `--no-random-page` turns it off.

To put it on the web: **fork this repository, copy your `cpm-*.tsv`,
`random-*.tsv` and `sites.tsv` into `logs/`, and push.**
`.github/workflows/pages.yml` rebuilds both pages and commits them back, so
GitHub Pages serves them with no build step. There is nothing to install in the
workflow — the generator is this same file, which is also why the pages cannot
drift from the log format. The native build's `radbeeper export` writes the
same two pages, byte for byte, and `tests/test_differential.py` is what says so.

## Reference

### Commands

| | |
|---|---|
| `probe` | find the counter and say what it is |
| `watch` | the monitor, logging while it is open; `--no-log` to only watch |
| `clock` | how far the counter's clock is out; `--set` corrects it from this machine |
| `cpm` | one 30-second average, for a script. Takes 30 s, and says so |
| `service` | monitor and log; dormant when there is nothing to read |
| `hotplug` | sit in the session, open the monitor on plug-in |
| `backfill` | fill the log's gaps from the counter's flash |
| `random` | 256 bits from decay timing, with the accounting for it |
| `recompute` | fill long-window columns in existing logs from their own counts |
| `site` | where a counter is, and where it has been |
| `export` | build `index.html` and `random.html` from the logs; `watch` does it hourly |
| `log info` / `log pull` | how much history flash, and download it |

Options: `--source sim`, `--sim-cpm`, `--seed`, `--spans 3,30,300`,
`--cpm-per-usvh`, `--log-every`, `--duration`, `--clock-offset`,
`--backfill-bytes`, `--max-gap`, `--entropy-bits`, `--device`, `--baud`,
`--no-log`, `--no-backfill`, `--no-export`.

## More documentation

| | |
|---|---|
| [The spectrum](docs/the-spectrum.md) | why flat is the good answer, how windows are accumulated, and why a peak is not called on sigma alone |
| [The native build](docs/native-build.md) | the Rust crate, what is ported and what is not, and how the two implementations are held to each other |
| [Reference](docs/reference.md) | the tube factor, the counter's protocol, what it costs to run, the tests, and how the screenshots are made |
| [Troubleshooting](docs/troubleshooting.md) | the four things it can be when `probe` finds nothing, and the four different fixes |
| [Prior art](docs/prior-art.md) | what else reads these counters, and what this does differently |
