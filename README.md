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
| `cpm.tsv` | one row every 30s: the rate, the windows, and their peaks |
| `history-*.bin` | a raw flash image from `log pull` |
| `history-*.csv` | the decoded version of the same image |

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
#time	cps	counts	seconds	cpm_3	cpm_30	cpm_300	peak_3	peak_30	peak_300
2026-09-04T11:54:37	0.633	19	30	40.0	38.0	36.8	140.0	52.0	37.1
```

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
