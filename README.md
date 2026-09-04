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
`cpm.csv`. If there is not, **it writes down why and exits 0** — a stopped
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
| `cpm.csv` | one row per second: CPS and each window's CPM |
| `history-*.bin` | a raw flash image from `log pull` |
| `history-*.csv` | the decoded version of the same image |

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
