<!-- SPDX-License-Identifier: MIT — Copyright (c) 2026 Paul Richeson -->

# Troubleshooting

**1. No serial node.** `/dev/ttyUSB*` does not exist. Four causes, in the order
they are worth checking — the first three are the counter's end and cost nothing
to rule out ([what you need](../README.md#what-you-need)):

- **The counter is switched off.** Its USB-serial chip runs off the counter's
  own battery, not the bus, so a flat or powered-down 320 enumerates as nothing.
- **A charge-only USB cable.** Very common, and indistinguishable from a dead
  port until you try another cable. `dmesg` stays silent.
- **Nothing is actually plugged in.** Worth the glance.
- **The running kernel has no USB-serial driver.** Alpine's `linux-virt` ships
  none — no `ch341`, no `usbserial` — so a counter plugged into a VM running it
  can never appear as `/dev/ttyUSB0`, and `dmesg` is silent because nothing ever
  claims the device. `linux-lts` and `linux-rpi` carry the drivers, and Copal
  installs `linux-lts`, so this case does not arise there:
  `apk add linux-lts` and reboot is the fix on a plain Alpine that has it wrong.

`dmesg | tail` separates them: a working cable into a switched-on counter on a
kernel with `ch341` says `ch341-uart converter now attached to ttyUSB0`. Silence
means the device was never seen; a `ch341` line with no node means the driver is
missing.

**2. Permission denied.** The node is `root:dialout`. `doas adduser $USER
dialout`, then log in again.

**3. Something is there but is not a GMC.** A CH340 is a generic USB-serial cable
and plenty of things that are not Geiger counters use one.

**4. The port is busy.** Only one program can read a serial device sensibly — two
readers share the bytes between them and neither is told — so RadBeeper locks the
port. **That is no longer a reason to stop anything.** The process holding the
port serves what it reads on `/var/lib/radbeeper/sock`, and `watch`, `probe` and
`radbeeper-gui` all ask that socket before they ask `/dev`: with the service
logging, they attach to it, and the log does not skip a second.

So `port busy` now means the holder is *not* sharing — a `random`, a `backfill`
or a `log pull`, each of which wants the counter to itself, or a build older
than the socket. Those finish on their own; `--wait` waits for them. A command
that genuinely needs the port rather than the stream — `clock --set`, `log
pull`, `backfill` — still needs the service stopped:
`doas rc-service radbeeper stop`, and `start` gives it back.

The lock is an `flock`, which is the kernel's and belongs to a *process*: no
terminal multiplexer, no session and no desktop changes it, and the answer is
the same in `tmux`, in `screen` and on a bare console. There is no handover
request to send either — the holder has to let go. What the native build does
have is `--wait`, which waits for the port instead of giving up:

```sh
radbeeper --wait watch          # take the port the moment it is free
radbeeper --wait 30 watch       # or give up after thirty seconds
```

Run it, then stop the service in the other window; the monitor catches the port
within half a second of it coming free, and Ctrl-C stops the waiting. Only a
*busy* port is waited on — an unplugged counter or a misspelt `--device` still
fails at once, because no amount of waiting fixes either. And if the service is
`restart`ed rather than stopped, both are asking for the same port: whichever
gets there first wins, so stop it, start the monitor, and start the service
again after.

**5. The counter's date is wrong.** Its clock runs off the same battery as
everything else, so a battery pull sends it back to the year 2000, and it drifts
about half a minute a day between pulls. Nothing needs doing: every start of
`service` and `watch`, and every time a counter is plugged back in, reads the
counter's flash and *then* sets its clock from this machine when it is more than
a second out. After, not before, because the rows in the flash were stamped by
the old clock and are placed with it. `radbeeper clock` shows the figures and
`clock --set` does it by hand.

One thing the program cannot do is erase the counter's memory — GQ's protocol has
no command for it. That matters once, after a battery pull: the samples the
counter recorded between losing its clock and having it set carry year-2000
stamps, and the next start re-reads them under the corrected clock and files
them under a `cpm-<serial>-2000-01.tsv`. Erase saved data from the counter's own
menu once the clock is right, and delete the stray file if one appeared.

**6. `probe` shows one counter, or the status file names one.** `probe` lists
every counter: the ones the service holds, from what the service says, and the
ones on free ports, opened and asked their battery and clock. A counter that
arrives after the service started joins it on the next thirty-second sweep, and
until then it is on the free list. Under each counter `probe` prints what the
last start found, from `/var/lib/radbeeper/starts` — one line per counter per
start, appended, and readable without root, unlike the service's own log under
`/var/log/radbeeper`.

---

[← back to the README](../README.md)
