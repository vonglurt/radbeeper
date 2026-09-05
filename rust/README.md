# radbeeper (Rust)

The live monitor, native.

```sh
cargo install radbeeper
radbeeper probe
radbeeper watch
```

This crate is the **read side** of [RadBeeper](https://github.com/vonglurt/radbeeper):
finding a GQ GMC counter on USB, asking it what it is, and the full-screen
monitor — four time constants at once, a coloured counts chart, an
accumulating power spectrum and the reading in twelve-row block digits.

Everything that writes to the log format — `service`, `backfill`, `export`,
`site`, `random`, `hotplug` — lives in the one-file Python program in the same
repository, and stays there until the format stops moving. The binary says so
if you ask it for one of those.

Why both: the Python runs on a machine with no toolchain and no network, which
is the whole reason it has no dependencies. The Rust starts in a millisecond
and costs nothing to leave running. Neither replaces the other.

**Linux, for now.** The port scan reads `/dev/ttyUSB*`, `/dev/ttyACM*` and
`/sys/bus/usb-serial`. The crate compiles anywhere POSIX and `cargo install`
will succeed on a Mac — it just will not find a counter until the scan learns
`/dev/cu.*`.

No toolchain on the target machine? Every tagged release carries static musl
binaries for x86\_64, aarch64, armv7 and armv6 (Pi Zero), with a `SHA256SUMS`
alongside: <https://github.com/vonglurt/radbeeper/releases>.

MIT — Copyright (c) 2026 Paul Richeson
