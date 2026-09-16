<!-- SPDX-License-Identifier: MIT — Copyright (c) 2026 Paul Richeson -->

# Troubleshooting

**1. No serial node.** `/dev/ttyUSB*` does not exist. Four causes, in the order
they are worth checking — the first three are the counter's end and cost nothing
to rule out ([§1](#1-what-you-need)):

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
port. `doas rc-service radbeeper stop` hands it over. The service waits on the
lock rather than giving up, so the log picks up again by itself when you close
the monitor.

---

[← back to the README](../README.md)
