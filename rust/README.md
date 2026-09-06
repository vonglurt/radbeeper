# radbeeper (Rust)

The live monitor, native.

```sh
cargo install radbeeper
radbeeper probe
radbeeper watch
```

This crate is [RadBeeper](https://github.com/vonglurt/radbeeper): finding a GQ
GMC counter on USB, asking it what it is, and the full-screen monitor — **five
time constants at once** (3 s, 30 s, 5 minutes, 50 minutes and a working day),
a coloured counts chart, an accumulating power spectrum and the reading in
twelve-row block digits. `service`, `random`, `backfill` and `log` are native
too, and write the same log format byte for byte.

`export`, `recompute`, `hotplug`, `--plain` and `--source sim` are not ported
and still live in the one-file Python program in the same repository. The
binary says so if you ask it for one of those.

Why both: the Python is the original, and is now the archive — kept as the
oracle the port is checked against byte for byte, as the owner of the commands
above, and because it runs on a machine with no toolchain and no network. New
behaviour goes here.

**Linux, for now.** The port scan reads `/dev/ttyUSB*`, `/dev/ttyACM*` and
`/sys/bus/usb-serial`. The crate compiles anywhere POSIX and `cargo install`
will succeed on a Mac — it just will not find a counter until the scan learns
`/dev/cu.*`.

No toolchain on the target machine? Every tagged release carries static musl
binaries for x86\_64, aarch64, armv7 and armv6 (Pi Zero), with a `SHA256SUMS`
alongside: <https://github.com/vonglurt/radbeeper/releases>.

MIT License — Copyright (c) 2026 Paul Richeson
