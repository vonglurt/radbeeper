# RadBeeper

**A GQ GMC-320 Plus Geiger–Müller counter on the desk, read from Alpine Linux —
and from [Copal](https://github.com/vonglurt/copal), its distillation.**

MIT · `0.3.1` · `cargo install radbeeper` · one dependency, and it is `libc`

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

**Ten minutes of it, three ways.** One session against the counter these logs
came from, recorded through a pty at 160 × 40 and cut into three clips: the
first five minutes at a hundred times speed, the hundred seconds after them
at ten, and the second the random pool delivered its first line.

![the monitor: five minutes at 100x, then a hundred seconds at 10x, then the random line](https://raw.githubusercontent.com/vonglurt/radbeeper/main/docs/screenshots/watch-hero.gif)

**The first clip is the counts strip filling**, which is the thing to watch.
A second a bar at the right, and each panel to its left takes a bar half as
often: the 1-second panel is full after 42 seconds, the 2-second after 78,
the 4-second after 156, the 8-second after 312 — so the left of the strip is
still filling when the right has scrolled through seven times over. Ten
minutes of history end up in one row of a terminal, at one second of
resolution where it matters. **[The cascade strip](docs/cascade.md)** is what
that arrangement is, how it works, and who else has built one. The five
averaging windows arrive in the same order, 3 s and then 30 s and then 300 s,
while the 50-minute and working-day ones count themselves down.

**The second clip is a hundred seconds slow enough to read.** Individual
counts landing as bars, the 30-second window walking while the 3-second one
jumps around it, a log row closing every thirty seconds under the `¦` ticks,
and the 5-minute average finally a number instead of a countdown.

**The third is the random line.** 256 bits of hex out of the timing of decay,
printed the moment the pool has measured enough min-entropy to justify them
— eight minutes and forty-nine seconds into this run, and a different second
in every other one.

## Fast track

With the counter plugged in and switched on:

```sh
radbeeper probe      # find it, confirm it's talking
radbeeper watch      # the monitor — q to quit
```

That is the whole of it. The `dialout` line in the install above is not
optional: the serial node is `root:dialout` and RadBeeper does not want root.
**Only one program can hold the port — but only one needs to.** The process
that has it serves what it reads on a socket beside the log, and `watch` asks
that socket before it asks `/dev`: with the boot service logging, the monitor
attaches to it and the port is never touched. Nothing has to be stopped, and
the log does not skip a second while you watch. With no service running, the
monitor takes the port itself and serves it in turn, so the second window
attaches to the first. [The stream](#the-stream) is what that is.
If `probe` finds nothing it says which of four things went wrong, and they have
four different fixes — [§1](#1-what-you-need) is the list of what has to be
true. **No counter yet?** `tests/fake_gmc.py` puts a synthetic counter on a
pseudo-terminal — a real Poisson background, because decay is a Poisson
process — and exercises the serial path as well as the display:
`python3 tests/fake_gmc.py --cpm 400`, then `radbeeper -d /dev/pts/N watch`.

<details>
<summary>Everything else it does</summary>

```sh
radbeeper service          # log every counter it finds, and serve them all
radbeeper-gui              # the same counter in a window (separate crate, gui/)
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
plugged in there is nothing to read — though `tests/fake_gmc.py` will put a
synthetic counter on a pseudo-terminal if you want to see the monitor working
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

`probe`, `clock`, `cpm`, `watch`, `service`, `random`, `backfill`, `log`,
`hotplug` and `export` are the native build. Four corners of the program —
`site`'s write side, `recompute`, `--plain` and `--source sim` — are not
ported yet and are marked where they appear below.
[§10](docs/native-build.md) says where the port stands.

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

- **Setting the clock needs the port itself**, which is the one thing the
  stream cannot pass on: stop the service (`doas rc-service radbeeper stop`),
  set it, start it again. Watching needs nothing stopped.
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

**Four tiers, finer to the right, each one a doubling.** The rightmost quarter
is one second a bar, newest at the edge. Each quarter to its left holds *k*
times as long in a bar, with *k* the smallest factor that makes the whole
strip reach back as far as the spectrum's window — at 160 columns and the
usual *k* = 2 that is 42 bars of 1 s, then 39 of 2 s, 39 of 4 s and 39 of 8 s:
**588 seconds of history in one row.** A second that scrolls off the fine tier
lands in the newest bar of the next, which fills as its seconds arrive; that
bar in turn lands in the next, and that one in the last.

```
8s/bar · 5m                 F 4s/bar · 3m                F 2s/bar · 78s        F 1s/bar · 42s    ¦
```

The `F` above each tier marks the hand-over, with how long a bar is and how far
back the tier reaches. Over the fine tier, a `¦` marks where each log row
closes: the frames the log is cut into, scrolling left with the counts.

Each tier left takes a bar half as often and twice as long to fill, so the
reach grows geometrically while the cost stays a quarter of a row: a fifth
tier would reach 1212 s, a sixth 2460 s. **[The cascade strip](docs/cascade.md)**
writes the technique up properly — the invariants, the arithmetic, the prior
art from RRDtool's round-robin archives to exponential histograms, and what
to call it.

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
what you get automatically when stdout is not a terminal. *Not in the native
build yet.*

```sh
radbeeper --plain --duration 14 watch
```

![radbeeper --plain watch](https://raw.githubusercontent.com/vonglurt/radbeeper/main/docs/screenshots/watch-plain.png)

### No counter on the desk?

`tests/fake_gmc.py` puts a counter on a pseudo-terminal and answers the real
protocol, so the monitor is driven the way the hardware drives it — down to
the serial reads. The counts are drawn from a Poisson process, because that
is what decay is: variance equals the mean, which is exactly the property
that makes the 3-second average jump while the 300-second one sits still.

```sh
python3 tests/fake_gmc.py --cpm 400      # prints the /dev/pts/N it made
radbeeper -d /dev/pts/N watch
```

There is also a built-in synthetic source, `--source sim --sim-cpm 400`,
which needs no pseudo-terminal. *Not in the native build yet.*

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

### The stream

**The counter sends two bytes a second, and that is the whole of the live
data.** Five averaging windows, the cascade strip, the spectrum, the entropy
pool, the log rows, `index.html` — every one of them is arithmetic over a
stream of *(when, counts)*. Nothing below the read touches the port.

So the lock is on the wrong thing to be a queue for. Two processes reading one
tty do not each get the stream; they get a share of it each and neither is
told, which is why the port is locked at all — but that is an argument for one
*reader*, not one *consumer*. Whichever RadBeeper holds the port publishes what
it reads on a unix socket beside the log:

```
/var/lib/radbeeper/sock      root:dialout, 0660 — the same membership that
                             lets you read the counter lets you read this
```

`watch` asks that socket before it asks `/dev`. The rules are short:

| | |
|---|---|
| **The port-holder serves** | `service` normally; `watch` when no service is running. Whoever has the flock owes everyone else the stream. |
| **A client writes nothing** | No log, no emission, no `index.html`, and it never opens the port. One writer, always — a rule with no exceptions is what makes two windows and a service safe together. |
| **Attaching gives you the history first** | Up to eight hours of it, replayed in milliseconds, so a window you just opened has its 30-second average *filled* rather than filling. The working-day window is full if the service has been up that long. |
| **A named device is not overruled** | `-d /dev/pts/7` means that device. The socket is only used when it is serving the counter you asked for. |
| **Closing a window costs the log nothing** | It was never what was writing it. |
| **One socket carries every counter** | Each sample says which tube it came from, so a machine with two serves both down one stream. See [Two counters](#two-counters). |

What this replaces is the handover: `doas rc-service radbeeper stop`, watch,
start it again, and an hour of log missing for every evening spent watching.
That is no longer how you look at your own counter. It is still how you set its
clock or read its flash, because those are questions *for* the counter and only
the port-holder can ask them.

```sh
radbeeper probe                 # works while the service holds the port
radbeeper watch                 # attaches; nothing is stopped
radbeeper-gui                   # so does the window
```

If `port busy` still appears, the holder is something that wants the port to
itself — a `random`, a `backfill`, a `log pull` — or a build older than this
one. `--wait` waits for it.

### Two counters

**Plug in a second GMC and `radbeeper service` reads both.** No flag is needed;
with no `-d` it takes every counter that answers, and `-d A -d B` names them.

Two tubes watching one room are two measurements of one number. They do **not**
double the dose — what doubles is the evidence:

| | |
|---|---|
| **Each keeps its own log** | Separate files, keyed by serial, as before. They are separate instruments and the record has to say which said what; a row that averaged two of them would be a reading neither took. |
| **The display averages them** | Every combined figure is the *mean* across the tubes, which is the same number one tube would report, measured from twice the arrivals. |
| **The precision is what improves** | Poisson error is 1/sqrt(N), so twice the counts is a factor of root two better. The window prints it: `452 CPM ±21 (4.7%)`. |
| **The cascade grows a fifth tier** | Two counters on their own clocks interleave, so the strip runs `8s/bar · 4s · 2s · 1s · 0.5s` instead of stopping at one second. |
| **The finest tier is coloured by tube** | Each bar in it is one tube's reading, drawn in that tube's colour, so the interleave is visible. Every tier left of it is a mean over both and takes the level colours — the two have merged into one number by then. |

**The interleave is measured, not assumed.** Two counters only sharpen *time* if
they disagree about when a second starts, and neither clock can be steered, so
the window reports the offset it actually sees: `2 tubes · interleave 0.50s
(100%)`. Half a second is a perfect interleave. Near zero is two tubes firing
together — still twice the counts and still the better precision, but no extra
resolution in time, and the display says so rather than claiming a half-second
bar it has not earned.

### Looking for a period

Decay has none: the power spectrum of a Poisson process is flat, and a
featureless strip is the useful answer. Something arriving on a schedule is
not decay, and that is worth being able to see.

A spectrum resolves periods up to its own window and no further — you cannot
find an hourly rhythm in ten minutes of listening — so one window is always the
wrong question for somebody hunting an unknown period. The window runs **three
at once** (8m, 68m, 9h) and draws them over one another on a shared
**logarithmic period axis**, long periods on the left, each in its own primary
at partial alpha:

- a real line stands at the same place in every layer that can reach it, which
  is most of what separates it from a fluke;
- the layers that cannot reach it simply stop short, and a period only the
  longest window sees is exactly the one worth doubting;
- the luck line is drawn across the panel — the height a peak has to clear
  before it means anything, which is not a fixed multiple and gets *harder* to
  clear the longer you watch.

**Each layer is drawn only over the band it can resolve**, and says so:
`7s–8m`, `58s–1h 8m`, `7m–9h 6m`. A window of *W* seconds has a bin at every
*W/n*, so the bins crowd towards the short-period end — by two seconds a
nine-hour window has sixteen thousand of them and the axis has a few hundred
columns to put them in. Folding twenty-seven bins into one column and taking
the loudest does not draw a line; it draws the largest of twenty-seven draws,
which is high by construction and high *everywhere*, and the resulting wall of
noise hides exactly what the panel is for. So a layer stops where its bins get
closer together than a bar is wide. A long window covers the long end, a short
one the short end, and they overlap in the middle where both are honest.

**The floor is solved, not chosen.** It is wherever the *shortest* window's
bins are still a bar apart — which depends on that window, on the longest one
(the two set the span of the axis) and on how many columns there are. That
makes it implicit, since the floor sets the span and the span sets the floor,
so it is iterated to a fixed point and lands near seven seconds for this
ladder. Change the windows and it follows. Nothing is drawn below two seconds
whatever happens: that is the Nyquist limit of a one-second sample.

With two counters both spectra are fed the whole-second sum: a period is a
property of the room and both tubes are looking at the same room, so adding
them is more signal on one time base.

### The window

`gui/` is a second crate: the same counter, in an Iced window, on Wayland.

```sh
cd gui && cargo build --release      # then ./target/release/radbeeper-gui
```

**It is a separate crate on purpose.** RadBeeper has one dependency and it is
`libc`; Iced brings several hundred, which is the right price for a
GPU-accelerated window and the wrong price to put on `cargo install radbeeper`.
The GUI has no serial code at all — it cannot open a port, only attach to one
that is being served — which is exactly what makes it safe to open and close
all day.

Two things about building it on Alpine, both of which cost an afternoon
somewhere:

- **It must be linked dynamically.** Rust on musl defaults to `+crt-static`,
  and a static binary cannot `dlopen` anything — including `libwayland-client`,
  which every Wayland toolkit loads at runtime. The failure reads
  `WaylandError(Connection(NoWaylandLib))`, which sounds like a missing package
  and is not one. `gui/.cargo/config.toml` turns the static link off, and says
  so at length.
- **Software rendering is expected in a VM.** With no virgl driver, `wgpu`
  cannot start and prints `virtio_gpu: driver missing`. Both renderers are
  compiled in, so Iced falls back to `tiny-skia` by itself and the window comes
  up regardless.
- **One canvas, and no text inside it.** On that software renderer only the
  *last* canvas widget in a view is drawn, and a `fill_text` anywhere in a
  canvas takes the rest of that frame's shapes with it. Both charts therefore
  share a single canvas and every caption around them is a text widget. The
  symptom is an empty box where a chart should be, which looks exactly like a
  bug in the chart's arithmetic and is not one — `gui/src/main.rs` says so at
  the point where it would otherwise be reintroduced.

An instrument cluster: a round dial per counter with a 270-degree sweep, the
bands painted on the face at the same thresholds everything else uses, and the
reading in digits on the black face under the needle. Beside it the five
averaging windows, each with the **precision** of its own figure — the long
windows are the precise ones, and without that printed the only visible
difference between the 3-second number and the working-day number is that one
of them jumps about. Then the cascade, the spectrum overlay, the emission and
its countdown, and the log rows as the server wrote them.

The charts come out of the same code the terminal monitor uses —
`analysis::tiers_with` and the same `Spectrum` — so a counter cannot look one
way in a terminal and another way in a window.

`radbeeper hotplug` opens the GUI when it is installed and a terminal running
`watch` when it is not; `--tui` asks for the terminal either way.

### The page, with two counters

`radbeeper export` reads every counter's log in the directory, and with two of
them the page opens on **Both counters** before either of them individually:

| | |
|---|---|
| **Mean, together** | Counts-weighted across the tubes — adding the counts and adding the seconds. Averaging the two *rates* instead would give a tube that recorded for an hour the same say as one that recorded for a week. |
| **Precision** | `±0.1 CPM (0.17% of the mean)`. All a second counter buys, and the only place the benefit is visible. |
| **Arrivals behind it** | The count the precision comes from, and the tube-hours it took. |
| **Do they agree?** | The widest gap between any two tubes, in sigmas. Identical tubes in one room should sit within about two; a gap that keeps growing is the tubes differing, not the room being interesting. |

Under it, every counter's hourly mean on one pair of axes, a colour each. Two
tubes watching one room draw the same shape, and where they stop being parallel
is where something is wrong with a tube, a cable or a pass-through — which no
table of averages shows as quickly.

**Every counter gets its own audit page.** This used to write the first
counter's emissions and silently drop every other tube's, so with two plugged in
half the record had no page and nothing said so. The first keeps the name
`random.html` — or whatever `--random-output` asked for — and the rest go beside
it as `random-<serial>.html`, each linked from its own counter's section. An
emission is an audit trail of *one* source, recomputed from that source's own
counts, so they cannot be merged either.

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
| [The cascade strip](docs/cascade.md) | the counts strip written up as a lab report: chained dyadic time compression, its invariants and arithmetic, the prior art from RRDtool to exponential histograms, and what to call it |
| [The spectrum](docs/the-spectrum.md) | why flat is the good answer, how windows are accumulated, and why a peak is not called on sigma alone |
| [The native build](docs/native-build.md) | the Rust crate, what is ported and what is not, and how it is held to the reference implementation |
| [Reference](docs/reference.md) | the tube factor, the counter's protocol, what it costs to run, the tests, and how the screenshots are made |
| [Troubleshooting](docs/troubleshooting.md) | the four things it can be when `probe` finds nothing, and the four different fixes |
| [Prior art](docs/prior-art.md) | what else reads these counters, and what this does differently |
