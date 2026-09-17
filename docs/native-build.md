<!-- SPDX-License-Identifier: MIT — Copyright (c) 2026 Paul Richeson -->

# The native build

**This is where new work goes now.** The one-file Python is the original, and
it is now the archive: it is kept as the differential oracle, as the owner of
the parts that have no Rust counterpart yet, and because it runs on a fresh Pi
with python3 and nothing else. Every top-level function and class in it carries
a `# PORT:` line saying which Rust file and which Rust name replaced it, or
saying plainly that nothing has:

```
# PORT: replaced by src/analysis.rs :: Ladder (renamed) -- new/add/best, …
# PORT: NOT PORTED. `recompute` is still Python.
```

The renames all run one way, towards shorter names inside a module that already
names the subject — `SpectrumLadder` → `analysis::Ladder`, `LogWriter` →
`log::Writer`, `log_header` → `log::header`, `write_entropy` →
`entropy::write_record`, `spans_arg` → `parse_spans`. Everything else kept its
name; classes became structs with the same methods. The one shape change worth
knowing is `Counter.samples()`, a generator, becoming
`Counter::next_sample(timeout)`, which is pulled rather than yielded.

```sh
cargo install radbeeper
```

That is the whole install: crates.io builds it and drops the binary in
`~/.cargo/bin/radbeeper`, which cargo already put on your PATH. Nothing is
installed system-wide and nothing needs root.

No toolchain on the machine? Take a static binary from the
[releases](https://github.com/vonglurt/radbeeper/releases) — musl, so one file
runs on Alpine, on Debian and on a Pi with no libc to match:

```sh
v=0.2.0; t=aarch64-unknown-linux-musl        # or x86_64-…, armv7-…, arm-… for a Zero
curl -LO https://github.com/vonglurt/radbeeper/releases/download/v$v/radbeeper-$v-$t.tar.gz
curl -LO https://github.com/vonglurt/radbeeper/releases/download/v$v/SHA256SUMS
sha256sum --check --ignore-missing SHA256SUMS
tar xzf radbeeper-$v-$t.tar.gz
install -m 0755 radbeeper-$v-$t/radbeeper ~/.local/bin/radbeeper
```

Every release is cut from a tag by `.github/workflows/release.yml` — the tag
has to match the version in the manifest or nothing is published, and crates.io
is reached over GitHub's OIDC identity rather than an API key stored here.
[RELEASING.md](RELEASING.md) is the procedure.

**This repository is that crate.** `Cargo.toml`, `Cargo.lock` and `src/` sit
at the root, which is where `copal-build` and every other tree in this account
put them; the crate carries the **read side** natively: `probe`, `cpm`
and the full monitor — the same five time constants, coloured counts chart,
accumulating spectrum ladder and twelve-row digits. One dependency, `libc`,
because a serial port is termios and termios is libc; the FFT, the digits and
the drawing are arithmetic and escape codes.

```sh
make build      # build it, into target/release/radbeeper
make install    # cargo install --path .
make package    # exactly what a publish would upload
make check      # the warning-free build, the tests, clippy and the oracle
```

The one-file Python program is still at the root beside it, still runnable,
and its own targets are prefixed: `make py-test`, `make py-check`,
`make py-install`.

A full probe against the counter takes **82 ms** and the binary is 400 KB.

## Going native, one piece at a time

`service`, `backfill`, `site`, `random`, `recompute` and `hotplug`
are still Python, and the binary says so if you ask it for one. They are being
ported. **Two implementations of a file format is how a file format acquires
two dialects**, so the port is arranged around one rule: nothing counts as
ported until both programs produce the *same characters* on the same input.

`tests/test_differential.py` is that rule. It drives
`examples/format_oracle.rs` and the Python's own functions with identical
directives and compares the output byte for byte — not equivalent, identical.
It skips rather than fails where there is no Rust toolchain, because the
Python suite has to run on a machine with nothing installed.

There is a second half to it, and it is the one that matters more: the Rust
`service` is run against the counter and **the Python's own reader parses what
it wrote** — same header, same columns, same meaning for an empty field, one
row per slot across a restart, and the file still chronological under plain
`sort`. Comparing strings tests one end of a format. Reading the file with the
other implementation tests both.

`radbeeper service --logs DIR` is what makes that testable at all; without it
the only way to exercise the logger is against the machine's real log.

**The entropy pool was the one place a difference would have been silent.** A
log row that disagrees between the two is at least visible in the file; a
digest that disagrees is sixty-four characters of hex that look exactly as
random either way, and the only thing that would ever notice is somebody
running `--check` a year later and being told their audit trail is a lie. So
the Rust recomputes **every line this counter has ever emitted** — real
emissions recorded by the Python before any of the Rust existed — and gets the
same digests. If its SHA-256, its NUL framing, its integer formatting of the
opened second or its nibble packing were off by one byte, not one of them
would match.

```sh
diff <(radbeeper random --check logs/random-*.tsv) \
     <(target/release/radbeeper random --check logs/random-*.tsv)
```

is empty, character for character. SHA-256 is written out by hand — sixty
lines, no dependency — and checked against the FIPS 180-4 vectors, the
million-`a` vector and the 55/56-byte cases that catch a padding path which
has only ever seen one block.

The Rust monitor also has the random line and its countdown now, which it
never had at all.

**The history decoder is where GQ's document is wrong twice**, so it gets the
most testing of anything here: thirteen constructed images covering truncation
mid-record, corrupt timestamps, notes, erased flash, marker bytes appearing as
ordinary counts and the measured-interval median — and then
`tests/fixtures/flash-gmc320re.bin`, **16 KiB cut out of this counter's own
flash at a timestamp marker**. 82,084 records off a 96 KiB read decode
identically in both. Then the whole chain end to end: `backfill --image`
through both command lines, into a fresh log, and the two files compared byte
for byte — 394 rows, decoder to merge, identical.

It earned its place on the first run. Python's `%g` — which formats the
`seconds` column and every span in the header, and which Rust has no formatter
for — picks scientific notation from the exponent the value has **after**
rounding to six significant figures. Taking it from the unrounded value prints
`999999.5` as `1000000` where C and Python print `1e+06`. One character, in a
column nobody reads, in a file whose one promise is that `sort` on it is
chronological.

**`export` writes the same two pages.** `index.html` and `random.html` are what
a fork publishes and what the workflow commits back on every push, so a page
that changed by a character depending on which binary built it would be a diff
in every one of those commits. Both exports are run on this repository's own
`logs/` and on a constructed directory — two counters, a two-hour hole, empty
window columns, a site that moves, a night the clocks go back, a 1969 pool —
and the files are compared byte for byte. Every coordinate in the charts is
printed to a tenth, so the Rust does the arithmetic in the Python's order,
calls libm's `pow` where Python's `**` does, and sums the variance with
CPython's compensated `sum()`. Both honour `SOURCE_DATE_EPOCH` for the
"generated" line, which is the one thing that would otherwise differ.

| | |
|---|---|
| **native now** | `probe`, `clock` and `clock --set`, `cpm` (its own 30 s window, not the device's 60 s one), `watch`, `service`, `random`, `random --check`, **`backfill`**, **`log info`/`log pull`** and **`export`** (`index.html` and `random.html`, byte for byte the Python's); the log format; the entropy pool, the SP 800-90B estimator and SHA-256; the history decoder with both corrections to GQ's published format, the wrapped-ring search and the measured sample intervals; local time, which Rust's standard library does not have at all |
| **still Python** | `site` (the write side), `recompute`, `--plain` and `--source sim` |
| **the rule** | one dependency, still `libc`. It has `strftime`, `strptime` and `mktime`, so the port does not need a date crate; the one primitive that must be written out is SHA-256 |

Nothing about this is a reason to hurry the Python out: it runs on a machine
with no toolchain and no network, which is the whole reason it has no
dependencies.

**Where the speed actually was.** The monitor's budget is one sample a second
and Python used a fraction of a percent of it, so this is about start-up and
footprint rather than throughput. The one genuinely slow path was `backfill`,
and that was an algorithm — fixed in the Python for a 7× win before any of this
was written.

---

[← back to the README](../README.md)
