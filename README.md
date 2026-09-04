# RadBeeper

**A GQ GMC Geiger-Muller counter on the desk.**

MIT · `0.1.0` · one file, `python3` and nothing else

Finds a GMC-320 (or 300/500/600) on USB, shows what it is counting now against
three time constants at once, and pulls the history it recorded while you were
not looking.

```sh
radbeeper probe            # find the counter and say what it is
radbeeper watch            # the monitor: 3s / 30s / 300s averages
radbeeper cpm              # the counter's own CPM, once, for a script
radbeeper log pull         # download the stored history to .bin and .csv
radbeeper service          # what the boot service runs
radbeeper hotplug          # sit in the session; open the monitor on plug-in
radbeeper backfill         # fill the log's gaps from the counter's history
radbeeper site             # where this counter is, and where it has been
radbeeper export           # build index.html from the logs
```

No counter on the desk? Every command works against a built-in source:

```sh
radbeeper --source sim --sim-cpm 400 watch
```

## Why three averages

`<GETCPM>>` returns the device's own rolling 60-second count. That is one
number with one time constant, and it answers one question. Three windows
answer three:

| Window | What it is for |
|---|---|
| **3 s** | Watching a source come and go as you move it. Jumpy, and honestly so. |
| **30 s** | Reading the room. Settled enough to compare two places. |
| **300 s** | A number worth writing down. |

Those cannot be derived from a single CPM reading, so radbeeper counts the blips
itself: one counts-per-second sample every second from the counter's own
`<HEARTBEAT1>>` stream, and each window is a sum over the samples it kept.

**A window reads `--` until it is full.** A "3-second CPM" built from one
sample is twenty times noisier than it looks, and drawing it as though it were
settled is how a 25 CPM background reads as 60 and somebody goes hunting for a
leak.

## The counter has to be visible first

`radbeeper probe` distinguishes the three ways a counter fails to appear,
because they have three different fixes:

```
no counter: no USB-serial driver in this kernel
    Running 6.18.48-0-virt. Alpine's linux-virt has no ch341/usbserial at all,
    so a counter plugged in here can never appear as /dev/ttyUSB0.
    linux-lts and linux-rpi carry the drivers.
```

1. **No serial node.** Either nothing is plugged in, or — the case that costs
   an afternoon — the running kernel has no USB-serial driver. Alpine's
   `linux-virt` ships none: no `ch341`, no `usbserial`, nothing. `dmesg` is
   silent, because the device is never claimed by anything.
2. **Permission denied.** The node is `root:dialout`. `doas adduser $USER
   dialout`, then log in again.
3. **Something is there but is not a GMC.** A CH340 is a generic USB-serial
   cable and plenty of things that are not Geiger counters use one.

## The service, and what "dormant" means

`radbeeper service` probes once. If a counter is there it monitors and logs to
`cpm.tsv`. If there is not, **it writes down why and exits 0** — a stopped
service, not a crash loop. A USB device that is not plugged in will not become
plugged in because a daemon asked again four seconds later, and a service that
respawns forever on a Pi Zero costs more than it measures.

The retry is the next boot, or `rc-service radbeeper start` the moment you plug
it in.

State lives in `/var/log/radbeeper` when that is writable and
`~/.local/share/radbeeper` when it is not:

| File | What it holds |
|---|---|
| `status` | one line: monitoring what, or dormant and why |
| `cpm-<serial>-YYYY-MM.tsv` | one counter, one month: a row every 30s |
| `sites.tsv` | which counter was where, and from when |
| `history-*.bin` | a raw flash image from `log pull` |
| `history-*.csv` | the decoded version of the same image |

## One file per counter per month

Rotation, by construction, so there is no rotation code and nothing to
schedule. A row is written to the file for its own counter and its own month,
so a month ending is not an event: that file stops growing and the next one
starts. Nothing renames a log while a service is appending to it, and there is
no window in which one is half-moved.

**The serial is in the name** because two counters on one machine are two
measurements, not one, and a file that mixed them could not be unmixed
afterwards. The names sort chronologically within a counter, so
`sort cpm-A1-*.tsv` is still one stream.

An older undated `cpm.tsv` is split across the dated files the first time it
is seen, its rows widened to the current columns and marked `live`, and the
original kept as `cpm.tsv.pre-rotation` rather than deleted.

## Where the counter is

A reading without a place is half a measurement -- 40 CPM means one thing in a
basement and another on a hillside -- and these things get carried about. So
the place is not a property of the machine or of the file: it is a property of
a **serial number over time**, which is what `sites.tsv` records.

```sh
radbeeper site                        # where is it, and where has it been
radbeeper site --name "The garage"    # it moved, from now
radbeeper site --serial A1            # without it plugged in
```

Append-only, one row per move, so a reading from last Tuesday resolves to
where the counter was last Tuesday and not to where it is now. A counter seen
for the first time is recorded at the default site, dated from the epoch --
its flash goes back further than the day somebody first wrote down where it
was, and those readings were still somewhere.

**A name, and nothing finer, on purpose.** These logs are meant to be
published. A place name is what a reader needs; a decimal fix to six places is
a street address for whoever is holding the counter. There is nowhere in the
file to put one.

## Opening on plug-in, in two halves

Plugging a counter into a running machine should do two things, and they want
different privileges, so they are two mechanisms rather than one:

| Half | What starts it | What it does |
|---|---|---|
| **the log** | a udev rule, as root | `rc-service radbeeper start` -- the counting begins whether or not anyone is logged in |
| **the window** | `exec-once`/`exec` in the desktop session | `radbeeper hotplug` opens the monitor in a terminal |

**A window cannot come from udev.** The rule runs as root, in whatever
environment udev has: no `WAYLAND_DISPLAY`, no session bus, and no way to know
which of several logged-in people the window belongs to. Guessing at those is
how you get a monitor on the wrong screen, or none at all with nothing in any
log to say why. Starting a *daemon* from udev has none of those questions, so
that is the half udev does.

**`hotplug` polls `/dev`, never the port.** Once every four seconds it lists
the device nodes. It does not open anything to ask `<GETVER>>`, because doing
that on a schedule would fight the running monitor for the device and would
rattle every other serial cable on the machine besides. A node *appearing* is
the event; whether it is a GMC is settled once, by the window that opens, which
is silent when it is not.

One window per plug event, not one per poll. A window that stays up marks the
event dealt with; one that exits at once is retried `--tries` times (3) and the
event is then written off -- which is the difference between tolerating a node
whose group udev has not set yet, and reopening some stranger's serial port
every four seconds until logout.

It replaces the older `radbeeper window` autostart line and subsumes it: a
counter already plugged in at login is treated as the same event, so the
already-there case and the plugged-in-later case run the same code.

```sh
radbeeper hotplug --poll 4 --settle 2 --tries 3      # the defaults
```

The udev rule matches the three USB-serial bridges GQ has shipped behind
(CH340, CP210x, PL2303). It is deliberately a little wide: a rule that matches
some other CH340 cable costs one probe, which finds no counter, writes its
reason and stops -- the dormant path, working as designed.

## One reader at a time

Two processes reading one tty do not each get the stream. They get a **share**
of it each, and neither is told: a logger and a monitor running together halve
both their counts and look entirely plausible doing it, which is the worst way
for a measurement to be wrong. So every port radbeeper opens is `flock`ed, and
a second open fails with a reason of its own rather than quietly succeeding:

```
port busy: the port is already open by another radbeeper
    /dev/ttyUSB0 is locked by another process. Nothing is wrong with the
    counter -- something else is reading it. Usually that is the logger
    service, which takes the port at boot and whenever one is plugged in:
    doas rc-service radbeeper stop  hands it over.
```

The two halves then settle it between them without either knowing about the
other:

- **The window gets it when somebody is logged in.** `hotplug` opens about two
  seconds after the node appears; the udev rule waits six before starting the
  service.
- **The service waits rather than giving up.** A locked port is the one case
  where it loops -- ten seconds at a time, status `waiting` -- because unlike
  an absent counter, a busy one *does* become free: the moment the person
  watching closes their monitor, the log picks up by itself.
- **`window` and `hotplug` stay silent on a busy port.** Already being read
  means already covered; a second window would only fail.

## The service log

One row every 30 seconds, tab-separated:

```
#time	cps	counts	seconds	cpm_3	cpm_30	cpm_300	peak_3	peak_30	peak_300	src	site
2026-09-04T11:54:37	0.633	19	30	40.0	38.0	36.8	140.0	52.0	37.1	live	Bellevue High School
```

`src` is `live` for a row this machine measured and `flash` for one
reconstructed from the counter's own history. `site` is where the counter was
at that row's own time, so a counter that moved mid-month leaves a file whose
rows are not all from one place and can still say which is which.

**The peaks are the point.** A row carrying only the averages as they stood at
the instant it was written would miss a source that came and went between two
rows -- which is the one event actually worth having a log for. `peak_3` and
`peak_30` are the highest each window reached since the previous row, so a
spike that lasted four seconds is still in the file an hour later.

**`cps` is per one second, always.** The row spacing does not change the unit:
19 counts over 30 seconds is 0.633 CPS, never 19. `counts` and `seconds` are
there so the division can be checked, and so a short final row is obviously
short rather than quietly wrong.

**The timestamp is first and big-endian**, so the file sorts chronologically
as plain text -- `sort cpm.tsv` with no flags, no field numbers, no awk. The
header starts with `#` so it sorts above the data instead of into the middle
of it, and `grep -v '^#'` drops it when something wants only rows.

An unfilled window writes an empty field, not a zero -- the same distinction
the monitor's `--` makes, for the same reason.

**Nothing accumulates in memory.** Between rows the program holds four numbers
and one float per window; `--log-every` changes the spacing without changing
that. At the default it is two syscalls a minute rather than a hundred and
twenty, which on a Pi Zero writing to an SD card is the difference that
matters. `--log-every 1` gets a row a second back if you want one.

## Backfilling from the counter's own log

The counter records to its flash whether or not anything is listening. When
`radbeeper service` starts -- at boot, or the moment a counter is plugged in --
it reads the tail of that flash first and fills in the log's gaps, so one file
answers "what was it doing" for the whole period rather than only the parts
somebody was watching.

```sh
radbeeper backfill                     # the counter, into the service log
radbeeper backfill --image hist.bin    # a raw dump from `log pull`
radbeeper backfill -o /tmp/try.tsv     # somewhere else, to look first
radbeeper service --no-backfill        # don't
```

Rows are built by replaying the flash samples through the **same** `Windows`
and `Interval` the live logger uses -- not a second implementation of the
averaging. A backfilled row and a live row of the same stretch agree, which is
the only way to be sure the reconstruction means what the live rows mean.

### One row per slot, and gaps stay gaps

A row is not identified by its timestamp, which is whatever instant the writer
happened to reach, but by its **slot**: the log interval it falls in, floor of
the time over the row spacing. Two rows clash if and only if they share a slot,
and **a backfill never writes into a slot that already has a row** -- the live
measurement is the better evidence and it stays. Slots with no evidence stay
absent from the file. Nothing is interpolated across them, because a row
claiming 0.000 CPS for a half hour nobody measured is worse than no row: it is
the same mistake as drawing an unfilled window as zero.

Every row says where it came from, in a `src` column: `live` or `flash`.

### The counter's second is not a second

```
55 AA 00 YY MM DD HH MM SS      a timestamp -- NINE bytes, no save-mode
```

The device writes a mark every few minutes and one sample per "second" in
between, and **its second is not one of ours**. On the unit this was written
against, 179 samples land between marks 181 seconds apart: 1.011 s each, which
is 39 seconds of drift an hour. So the spacing is measured per stretch --
the gap between two marks divided by the samples they hold -- and never
assumed. Assuming 1.000 would file an hour-old sample most of a minute from
where it belongs.

A stretch whose mark is followed *four hours* later did not record one sample
every eighty seconds; the counter was off in between. Measurements more than a
factor of two from their median are replaced by the median, so those samples
land in the three minutes after their own mark and the four-hour hole survives
as a hole.

**The RTC is a separate error with a separate cause.** `backfill` measures the
counter's clock against this machine's and shifts every timestamp by the
difference, printing what it applied. It assumes that offset held for the whole
recording — so if you *set* the counter's clock, data recorded before that
needs the old offset, passed by hand:

```sh
radbeeper --clock-offset 2180 backfill --image hist.bin
```

### The flash is a ring, and it had already wrapped

Reading a megabyte over 115200 baud takes ten minutes, and the rows a backfill
wants are the newest, so `backfill` reads only the tail -- 64 KiB by default,
about seventeen hours. Finding where the tail *is* has two cases:

- **Not yet full.** Writing runs forward from zero and the rest is `0xFF`.
  Eleven probes bisect for where the `0xFF` starts.
- **Already wrapped**, which is what the counter on the bench turned out to be:
  1 MiB of ring with no unwritten byte in it. The newest sample sits
  immediately before the write pointer and the oldest immediately after, so the
  flash reads as one long climb in time with exactly one step backwards in it.
  That step is the pointer, and it bisects too.

Getting this wrong is not a small error: reading the physical tail of a wrapped
ring hands back the *oldest* hours while claiming they are the newest, and the
backfill would file last week under this afternoon.

Against the counter here -- a full, wrapped megabyte -- that is 844,809 samples
over eleven days, 28,544 rows, and 42 holes left as holes.

## Publishing: fork it

The export turns the logs into one self-contained page -- summary cards per
counter, a by-day table and the latest rows, sortable and searchable.

```sh
radbeeper export                      # index.html from the state directory
radbeeper export --logs logs -o index.html
```

To put it on the web: **fork this repository, copy your `cpm-*.tsv` and
`sites.tsv` into `logs/`, and push.** `.github/workflows/pages.yml` rebuilds
`index.html` and commits it back, so GitHub Pages serves it with no build step
and no artifact to expire. There is nothing to install in the workflow: the
generator is the same one-file stdlib program the counter is read with, which
is also why the page cannot drift from the row format -- a column added to the
log is a column the export knows about in the same commit.

The page pulls DataTables from a CDN. That is a different question from what
this program depends on: a browser fetching a table widget is not a Pi Zero
fetching a package.

## Pulling the log

```sh
radbeeper log info          # how much history flash this model has
radbeeper log pull          # into the state directory
radbeeper log pull -o ~/attic/2026-09-04
```

**The raw image is always written first, and the decode second.** GQ's history
format is a byte stream with `0x55 0xAA` escape markers, it has changed between
firmware revisions, and the published document disagrees with some shipped
units. A decoder that misreads a marker costs a bad CSV, never the data, and a
better decoder can be run over the same `.bin` afterwards.

## The simulator is a real Poisson source

`--source sim` is not a smooth fake. Radioactive decay **is** a Poisson
process, so the simulator draws Poisson samples: variance equal to the mean,
which is exactly the property that makes a 3-second average jump around and a
300-second one sit still. Testing the display against a smooth signal would
prove nothing about either. `--seed` makes a run repeat exactly.

The suite checks that the drawn mean and variance match the requested rate:

```sh
make test
```

## The tube factor

µSv/h is CPM divided by a number that belongs to the **tube**, not the counter.
The default, 151.5, is the M4011 in a GMC-320. A 500 with a different tube
needs a different number, which is why it is `--cpm-per-usvh` and not a
constant buried in the arithmetic.

## Not built yet: listening for the blips

The counter also has a speaker, and a USB sound card could count the clicks
directly — useful for a counter with no serial port at all, and a cross-check
on the ones that have. It is not written. The shape is there for it: sources
already hand `(time, counts)` to the averaging, so an audio source is a new
producer and not a new program.

## Protocol

GQ's RFC for the GMC series. Commands are ASCII `<NAME>>`; replies are raw
bytes with no framing, so every read asks for an exact count and times out
rather than blocking.

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

Baud is 115200 on the 320 and 57600 on the 300; radbeeper tries both.

There is no `pyserial` here. Alpine packages it, but this runs on a Pi Zero
with 512 MB and on a fresh install with no network, and a serial port is
thirty lines of `termios`.
