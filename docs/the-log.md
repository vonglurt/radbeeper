# The log

*A lab report on the files RadBeeper writes: what is in a row, why an empty
field is not a zero, where the bytes live, and the second file that records
the room rather than any one instrument.*

**The format is owned by the reference implementation** — the one-file Python
`radbeeper` at the root of this repository — and the Rust build produces the
same characters. Not "equivalent": identical, checked byte for byte by
`tests/test_differential.py` on the same inputs. Two dialects of one file
format is the failure this arrangement exists to prevent, and it is only worth
anything because somebody checks.

---

## The per-counter log

One row every 30 seconds into a dated file per counter, `cpm-<serial>-YYYY-MM.tsv`.
`watch` writes the same rows while it is open, so it does not matter which of
the two has the port.

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

---

## And a second file, for the room

**Nothing is ever merged into a counter's own file.** A row that blended two
instruments could not be taken apart again, and whether the two agree is the
one question a second tube exists to answer. So the merge is written *beside*
them, as `cpm-merged-YYYY-MM.tsv`, by whichever process holds the ports —
**and only when there is more than one counter**, because with one there is
nothing to merge and nothing to interleave. Plug a second tube into a running
service and the merged record starts then, which is exactly when it becomes
worth keeping.

| Column | |
|---|---|
| `time`, `unix` | the stamp a person reads, and the epoch that survives it — a sample is not on a whole second |
| `tubes`, `interleave` | how many were reporting, and the mean gap between one tube's sample and **the next tube's** |
| `counts` | **raw**: every arrival off every tube, as an integer |
| `seconds`, `tube_seconds` | the wall clock the row covers, and the instrument-time behind it — with *n* tubes the second is *n* times the first, which is the whole arithmetic of a merge in two numbers |
| `cps` | **merged**: counts over *tube*-seconds, the rate of the room |
| `cps_raw` | counts over wall seconds, the arrival rate at the machine |
| `cpm_3` … , `sigma_30`, `peak_*` | the combined windows, and what a second tube actually buys |
| `per_tube` | `SERIAL=counts` for each, so the merge can be taken apart again |

**Two tubes do not double the dose — what doubles is the evidence.** `cps` is
therefore counts over tube-seconds and not over the wall clock; dividing by
the wall clock would report a pair as twice the background, which is the one
arithmetic mistake a second instrument makes easy.

**The interleave is the fact no per-counter file can hold.** Each tube has its
own clock and its own phase and none can be steered, so the gap is whatever it
is — and the ideal is 1/*n* of a second, not half of one. Near it, the tubes
are taking turns and the pair resolves time *n* times as finely as one of them
could; near zero they fire together, which still buys the precision above and
no time resolution at all.

**Its numbers read back as the numbers that were written.** The per-counter
file rounds to a tenth of a CPM, because its characters are the Python's to
the byte; this one writes the shortest string that parses back to identical
bits — which is simultaneously the most precise form there is and, for
ordinary values, the shortest. `radbeeper export` reads both and puts them on
the page side by side, under **The merged record**.

---

## Where the bytes live

State lives in `/var/lib/radbeeper` when that is writable, `/var/log/radbeeper`
when only that is, and `~/.local/share/radbeeper` otherwise.

**`/var/lib`, not `/var/log`, because `/var/log` is often in RAM.** On Alpine
desktops it is commonly a tmpfs — it was on the machine these logs came from —
and every reboot emptied the log, leaving only what the counter's flash still
held to backfill from. A measurement record is state, not a log to rotate away.

---

## Rotation, by construction

Files rotate by month and by counter, `cpm-<serial>-YYYY-MM.tsv`, and there is
no rotation code because there is no rotation event. A row is written
to the file for **its own month**, so a month ending is not something that
happens to a file: that one stops growing and the next one starts. Nothing is
scheduled, and nothing renames a file while a service is appending to it.

Two things the writer watches for, both once per row and so once every thirty
seconds, against the two syscalls the row itself costs:

- **The month turning over**, which is the whole of rotation.
- **The file being replaced underneath it.** A backfill merges by writing a new
  file and renaming it over the old one, which leaves an appender holding a
  descriptor onto an orphaned inode: it goes on writing, to nothing anybody
  will ever read. Comparing the inode catches that and reopens.

---

## Reading a log back

Columns are matched **by name, never by position.** Adding a span inserts
`cpm_3000` after `cpm_300` and `peak_3000` after `peak_300` — in the middle of
the row, twice. Padding such a row on the right slides every value after the
insertion point one column left and writes a counter's `src` into a `peak`
column. The row still parses; it is just wrong.

A file may therefore carry **more than one header**. A four-window logger
replaced mid-month by a five-window one writes a second `#` line, and both
readers take the last header above a row as that row's. A file with no header
at all is read as the three-window layout the positions were written for.

---

## Backfill from the counter's own memory

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

---

## Where a counter is, and where it has been

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

---

## Further reading

- [The stream](the-stream.md) — how several counters become one record, and
  why the interleave is the fact no per-counter file can hold.
- [The cascade strip](cascade.md) — the counts strip the log rows tick under.
- [Reference](reference.md) — the tube factor, the protocol, the tests.

---

MIT License — Copyright (c) 2026 Paul Richeson

---

[← back to the README](../README.md)
