<!-- SPDX-License-Identifier: MIT — Copyright (c) 2026 Paul Richeson -->

# The Monthly Record: Publishing a Continuous Measurement Without Publishing a Street Address

**Paul Richeson** · RadBeeper · Copal Linux

---

## Abstract

*A radiation counter left running produces an unbounded record, and an
unbounded record cannot be published: it outgrows a page, it outgrows a
repository, and its provenance decays faster than its content. This report
describes the release agenda RadBeeper adopted at version 0.5 — one release
per month, carrying that month's counts — and the four design decisions the
agenda forced. First, the record is dated by construction rather than
rotated, so a month ending is not an event. Second, emissions are chained by
`H(prev ‖ key)`, which is computed in both implementations from the emission
log and never from the raw frames, keeping tamper-evidence inside the
differential contract. Third, a record is identified by a serial number by
default, by a named place when one is given, and by a coordinate only at a
precision chosen when it is written — rounding on the way in rather than on
the way out, which turns a display convention into a property of the file.
Fourth, the published page embeds raw frames against a byte budget rather
than a time window, because the thing being bounded is page weight. We report
the measured costs, the three-implementation verification the frame format
now carries, and what a month of record does not establish.*

**Index Terms** — environmental monitoring, hash chain, tamper-evidence,
data publication, location privacy, differential testing, Geiger–Müller
counter, static site.

---

## I. Introduction

A measurement that nobody else can check is an anecdote. RadBeeper has, since
its first version, published the counts behind every claim it makes: the
monitor page links the tab-separated log it was drawn from, and the audit page
carries the per-second counts behind every random line so that any reader can
recompute the digest. That worked while the record was small.

It does not scale. A counter sampling once a second produces 31.5 million
samples a year. The emission log grows without bound; the audit page grows
with it; and the repository that carries both grows by the whole of it on
every rebuild. At some size each of these stops being publishable, and the
sizes are not large: a single year of continuous counting is about 30 MB of
frames, which is past what a browser will happily parse inline and well past
what anybody will download to check an arithmetic claim.

The obvious answer — publish a summary — gives up the property that made the
record worth publishing. This report describes the answer actually taken,
which is to make the record *periodic* rather than *cumulative*, and to make
each period independently checkable and cryptographically linked to the one
before it.

---

## II. The Agenda

The release cadence is one version per month, or thereabouts. It is not
automated and deliberately is not: `make release` is run by a person who has
read what changed.

| step | what happens | who |
| --- | --- | --- |
| 1 | The month's `cpm-<serial>-YYYY-MM.tsv` stops growing; the next month's begins. | nothing — see §III |
| 2 | `radbeeper site` is confirmed or corrected, if the counter moved. | a person |
| 3 | `make release` bumps the version, runs both suites, rebuilds every page, and tags. | a person |
| 4 | The tag builds binaries, publishes the crate, and deploys the pages. | CI |

Step 2 is the only one that is a judgement rather than a command, and it is
the reason the cadence is monthly rather than nightly. A fixed monitoring
station does not move often enough to justify recording its position on every
row, and does move often enough that recording it once, forever, would be a
lie. A month is the interval at which "where has this been?" is a question
with a short answer.

---

## III. Rotation by Construction

There is no rotation code in RadBeeper, and there is no scheduled job that
closes a file. A row is written to the file for *its own month*:

```rust
pub fn path(when: f64, directory: &Path, serial: Option<&str>) -> PathBuf {
    directory.join(format!("cpm-{}-{}.tsv",
        serial.unwrap_or("unknown"), clock::format(when, "%Y-%m")))
}
```

A month ending is therefore not an event. That file stops being appended to
and the next one starts, with nothing renamed while a service holds it open
and nothing to go wrong at midnight on the first. Version 0.5 extends the same
rule to the emission record, which had been accumulating in undated files
since the beginning:

```
random-<serial>-YYYY-MM.tsv     the audit trail: emissions with their counts
random-<serial>-YYYY-MM.hex     the digits alone, one line per emission
random-<serial>-YYYY-MM.bin     the raw seconds, one byte each
```

**The undated files are not migrated.** `random_series()` reads
`random-<serial>.<ext>` as though it were the oldest month, because it is.
Rewriting a file whose hash somebody may have published is a worse failure
than carrying a special case for one filename, and the special case is four
lines.

One subtlety is checked rather than assumed. A serial number may itself
contain a dash, so `random-AB-CD.bin` is ambiguous between "counter `AB-CD`,
undated" and "counter `AB`, some month `CD`". The suffix is therefore matched
against the shape `-YYYY-MM` digit by digit, and `is_dash_month` has a test of
its own for exactly this case.

---

## IV. The Chain

Dating the record creates a new problem: the record is now a sequence of
files, and a sequence of files can be reordered, truncated, or have a member
quietly removed. Each file remains internally valid throughout.

Version 0.5 adds a hash chain over emissions:

```
link₀ = H(LABEL ‖ 0…0 ‖ key₀)
linkₙ = H(LABEL ‖ linkₙ₋₁ ‖ keyₙ)
```

with `LABEL = "radbeeper/chain/1"` and `H` = SHA-256. Three properties of this
construction were chosen deliberately and each one costs something.

### A. The chain is beside the key, never inside it

The obvious design folds the previous link into the emission digest, so that
the chain *is* the key. It was rejected. The digest is

```
key = SHA256("radbeeper/entropy/1" ‖ seq ‖ started ‖ packed_counts)
```

and it is computed identically by the Rust and by the Python reference
implementation, which is what lets `radbeeper random --check` recompute any
line ever published. Folding the previous link in would change every hex line
the program has ever emitted, retroactively invalidate every recorded
emission, and make each one uncheckable by the reference implementation. The
chain is worth a great deal less than that.

So independence is spent on *ordering* and not on *bits*, and the two claims
stay separable: each emission is recomputable from its own counts alone, and
the chain additionally attests the order.

### B. It is tamper-evidence, not tamper-proofing

Anybody who can rewrite the file can recompute the whole chain. This is stated
plainly on the page itself rather than left for a reader to work out. What the
chain catches is the realistic accident — a truncated copy, a partial
download, two months stitched back together in the wrong order, a frame lost
to a full disk — and the realistic single-point edit, where one record is
changed and the rest are left standing.

The distinction is worth a test, because it is the failure mode per-record
integrity *cannot* see:

> `a_missing_frame_breaks_the_chain_though_every_frame_still_verifies`

Four frames are built, linked, and one is removed from the middle. Every
remaining frame still verifies against its own counts — that assertion is in
the test — and the chain does not.

### C. The chain head is read from the disk, not held in memory

`write_frame` reads the last link out of the newest frame file on every
append, by scanning backwards from the end for a magic that decodes and
finishes exactly at EOF. Holding a chain head in memory is cheaper and is
wrong: a service restart, a month boundary, or a second `radbeeper` started by
hand against the same directory each leave an in-memory head stale or absent,
and the chain forks silently. The file already knows what it ends with, so the
file is asked. The cost is one backward scan per emission, which is once every
few minutes.

---

## V. Identity of a Record

A count rate without a place is half a measurement. 40 CPM means one thing in
a cellar and another on granite at altitude, and a reader who cannot tell
which has been handed a number they cannot use.

RadBeeper identifies a record by the first of these that exists:

1. **A coordinate**, at the precision it was recorded with.
2. **A named place** — "the north window" — which is what a human reader
   actually wants.
3. **A serial number**, which is always available and identifies the
   instrument rather than the site.
4. **A braid of *n* counters**, where more than one tube is running; see §VII.

All of these are properties of a *serial number over time*, not of a machine
or of a file, and they live in append-only `sites.tsv`. A reading from last
month resolves to where the counter was last month.

---

## VI. Precision as a Privacy Parameter

Earlier versions refused to record coordinates at all, with this reasoning in
the source:

> *A decimal fix to six places is a street address for whoever is holding the
> counter.*

That reasoning is correct about the risk and wrong about the remedy. The
hazard was never the coordinate; it was the *precision*. So precision became
the control:

| places | ≈ error | identifies |
| --- | --- | --- |
| 1 | 11 km | a town and its surroundings |
| 2 | 1.1 km | a district |
| **3** | **110 m** | **a street — the default** |
| 4 | 11 m | a building |
| 6 | 11 cm | a doorstep |

```sh
radbeeper site --name "the north window" --at 51.3172461,0.8914772
# writes: 51.317   0.891
```

**The rounding happens on the way in.** The file never holds the discarded
digits, so it cannot be made to give them up — by a future version of this
program, by a reader of the raw `.tsv`, or by anybody who obtains the
repository. This is a stronger guarantee than a page that declines to print
what it stores, and it is strictly less convenient: the precision cannot be
recovered later if it turns out to have been wanted. That is the trade, and it
is the right way round for a file that is published by default.

Requesting more than four places prints a warning naming the ground distance
it corresponds to. It is not refused. The counter may legitimately be
somewhere that nobody lives.

---

## VII. What a Second Tube Buys — "A Braid, Not a Blend"

Running two counters invites an obvious claim: twice the entropy, pooled. The
claim is false here, and the page says so explicitly rather than quietly
benefiting from the ambiguity.

Each tube keeps **its own pool**. An emission is an audit trail of one source
and is recomputed from that source's own counts; a key mixed from two tubes
could not be recomputed from either, so no downstream reader could check it.
What two tubes actually provide is:

- **Throughput.** *n* pools reach 256 bits *n* times as often as one does.
  This is a linear gain and nothing cleverer.
- **A witness.** Two tubes in one room should agree to within counting
  statistics. When they stop agreeing, one is wrong — drifting, dying,
  shielded, or being pointed at something — and *a single tube cannot tell you
  that about itself at any price.* The monitor reports the agreement as a
  z-score.

The seconds themselves are interleaved rather than summed, one tier per tube,
so that any second can be traced back to the tube that counted it. The
shorthand for the arrangement is **braided decay**, and the distinction the
phrase is carrying is exact: a blend would average the tubes and discard which
was which, while a braid keeps every strand whole and legible along its entire
length and takes its strength from the strands having been spun
independently.

---

## VIII. Bounding the Page

The audit page embeds raw frames so that a copy saved to a disk still audits
with nothing to fetch and no server to fetch it from. That property is the
whole point of embedding — an audit trail that works only while a web server
is up is not much of one — and it has to be paid for in bytes.

The budget is **2 MiB of raw frame data by default**, `--frame-budget`
otherwise, and it is a budget in bytes rather than a window in time because
the thing that hurts is page weight. A quiet counter fits a year inside it; a
busy one fits a fortnight; both produce a page that loads.

Two measured costs justify the encoding:

- **One byte per ordinary second.** A second that arrived one second after the
  last one, carrying a count under 0xFD, is stored as a single byte that *is*
  the count. The test asserts the marginal cost directly: 540 more seconds
  cost exactly 540 more bytes.
- **No general-purpose compression.** Beyond the project's one-dependency
  rule, a DEFLATE header alone is most of what the encoding costs, and a
  stream of bytes valued 0–5 is the shape a generic compressor does worst on.
  The structure is the compression.

Files are embedded **whole**, newest first, stopping before the budget — never
part of one. This is not tidiness. The reference implementation cannot see
where a frame ends, because it is copying bytes it does not parse (§IX); a
month is a unit both implementations can agree on without either of them
understanding the contents.

---

## IX. Verification Across Three Implementations

RadBeeper's central discipline is that the Rust and the Python reference
implementation produce byte-identical output, enforced by
`tests/test_differential.py`. The frame viewer threatened it directly: the
audit page is compared byte for byte, and the Python has never heard of a
frame.

The resolution is a division of labour.

| artefact | built from | by whom |
| --- | --- | --- |
| joined table | emission `.tsv` + count `.tsv` | both, identically |
| chain links | emission keys | both, identically |
| embedded frames | `.bin`, base64, **unparsed** | both, identically |
| frame decoding | the embedded bytes | the browser, once |

Neither generator parses a frame. The viewer — `src/frames.js`, zero
dependencies, no network, no bundler — does the decoding in the browser, where
there is exactly one implementation of it. The Rust obtains it with
`include_str!`; the Python is installed as a lone file and must carry a copy,
which `tools/embedjs.py` generates and `tests/test_frames_js.py` verifies is
byte-identical to the source.

That leaves a third implementation of the frame format with nothing checking
it, so a fourth artefact was added: `tests/fixtures/frames.bin`, four frames
covering every branch the encoder has — an ordinary second, a count above
0xFD, a gap, and a suspect spectrum — committed, with the Rust's reading of
them in `frames.json`. The Rust asserts it still writes those exact bytes; the
JavaScript asserts it reads them to the same values, chain links included.
Round-tripping proves nothing here: a format that drifts drifts in both
directions at once and round-trips happily the whole way.

The chain check is the part worth having. It requires two independent SHA-256
implementations and two hex conventions to agree, which is precisely the
property that makes the chain *evidence*: a reader can recompute it without
the writer's code.

---

## X. Limitations

**A month is not attested.** The chain says these emissions were produced in
this order by this counter. It says nothing about when, beyond what the
timestamps claim, and the timestamps are the recording machine's. There is no
trusted timestamp anywhere in this record.

**The chain does not survive a rewrite.** See §IV-B. It detects accident and
single-point edits, not an adversary with write access to the repository.

**A fix is where somebody said the counter was.** Nothing verifies it. The
precision guarantee is about what the file *contains*, not about whether it is
true.

**Two tubes in one room are not two independent sites.** They share a
building, a mains supply, and whatever is in the air. They cross-check the
instruments, not the environment.

**`counts` in the emission log is clamped at fifteen a second.** That clamp is
part of the digest, so the key still recomputes from it exactly — but it is
lossy as a record of what the tube did. The unclamped record is the `.bin`
frame, which is why the frames exist at all, and the table's header says so on
every export.

**The entropy is not certified.** Measured min-entropy is a claim about the
samples that were seen. It has not been through a statistical test battery and
this report does not make it one.

---

## XI. Conclusion

Making a continuous record publishable turned out not to be a compression
problem. It was four separate decisions about *boundaries*: where a file ends
(by construction, at a month), how one period is bound to the last (a chain
beside the keys, never inside them), how much of a place to write down (as
much as was asked for, decided before writing), and how much of the record a
page carries (a byte budget, whole files only).

Each of those has a failure mode that is silent if it is got wrong, and each
is now covered by a test that names the failure rather than the behaviour. The
one that took the longest to see was the smallest: a raw-string continuation
put a single backslash at the head of the Python's copy of the viewer, and one
byte of difference in a 70 KB page is indistinguishable, in a diff, from
having built the wrong thing entirely.

---

## References

[1] RadBeeper, "Random numbers out of decay," `docs/the-random.md`, 2026.

[2] RadBeeper, "A signed chain of custody for a single-maintainer Rust crate,"
`docs/the-chain-of-custody.md`, 2026.

[3] RadBeeper, "The cascade," `docs/cascade.md`, 2026.

[4] NIST, "Recommendation for the Entropy Sources Used for Random Bit
Generation," SP 800-90B, 2018.

[5] NIST, "Secure Hash Standard," FIPS PUB 180-4, 2015.

[6] GQ Electronics, "GMC-320 Plus communication protocol," rev. 1.4.

[7] Copal Linux. [Online]. Available: https://github.com/vonglurt/copal

---

MIT License — Copyright (c) 2026 Paul Richeson

---

[← back to the README](../README.md)
