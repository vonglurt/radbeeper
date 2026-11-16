# The thermometer

**A Geiger counter that also knows how warm it is: reading the GMC-320's
temperature in the middle of its count stream, without losing a count.**

*A lab report in the IEEE style, on asking a GQ GMC-320 for its temperature
while it is streaming counts, on the one reading it gives that is not a
temperature, on where the readings are kept, and on the command another
program uses to ask for them. Written against the radbeeper 0.6.0 working
tree, 16 November 2026.*

---

## Abstract

The GQ GMC-320 has a thermometer, and its serial protocol has a command for
it. RadBeeper did not use it: the service holds the counter in heartbeat mode,
where the counter sends two bytes every second with no framing, and a question
written into that stream looked like a way to corrupt it. This report shows
that it is not. A GMC-320 Re 4.26 answers `<GETTEMP>>` mid-stream in about 55
ms and goes on sending its seconds a second apart; the service now asks once a
log interval, in the gap after a sample, and writes the answer to a file of
its own beside the count log. One answer is not a temperature: 85.0 °C, which
both counters on the bench gave the first time they were asked, and which is
discarded. A fake counter that records every count it sends is used to show
that the service publishes exactly those counts, in order, while being asked
for its temperature every second in the least convenient order the wire can
deliver. A new command, `radbeeper now`, gives another program the counts per
minute and the temperature from the running service without opening the port.
What the thermometer is, and how far the inside of the case is from the air of
the room, were not measured.

**Index terms** -- serial protocols, unframed byte streams, temperature
measurement, radiation monitoring, inter-process communication.

---

## 1. The question

The counter's display shows a temperature. GQ's protocol document, GQ-RFC1201,
gives the command:

| | |
|---|---|
| Command | `<GETTEMP>>` |
| Reply | four bytes: whole degrees Celsius, tenths, a sign (non-zero is below zero), and `0xAA` |
| Firmware | "GMC-320 Re.3.01 or later" |

The two counters on the bench are a `GMC-320Re 4.26` and a
`GMC-320+V4Re 4.83`. Both are named by that line; no other model is.

## 2. The reading that is not a temperature

On 15 November both counters were asked once each, with the stream stopped.
Both answered `55 00 00 AA`, which decodes as 85.0 °C. Neither was at 85 °C.

On 16 November the Re 4.26 was moved to another computer and asked in a loop:

| When | Answer |
|---|---|
| Five readings, two seconds apart | 26.2 °C each |
| First reading after being unplugged and plugged in again | 85.0 °C |
| The next ten, thirty seconds apart | 26.1 °C each |

Its own screen read 26.1 °C beside the first set. So the command works on this
firmware, and 85.0 is what the counter says before its thermometer has a
reading to give. 85 °C is the value a common kind of one-wire thermometer
holds until its first conversion; whether that is the part inside was not
established, and the handling does not depend on it. The exact answer 85.0 is
treated as "not yet": it is never logged, and the question is put again two
seconds later.

The +V4 Re 4.83 has been asked once, on 15 November, and answered 85.0. It has
not been asked a second time.

## 3. Asking in the middle of the stream

In heartbeat mode the counter sends a 16-bit count every second. There is no
framing: the reader stays in step only by taking two bytes at a time. A reply
to any command arrives in the same stream.

The test, on the Re 4.26: heartbeat on, and `<GETTEMP>>` written 0.2 s after
every fifth sample, for forty seconds.

| | |
|---|---|
| Questions asked | 9 |
| Answered whole | 9, each `1A 01 00 AA` (26.1 °C) |
| Time from question to answer | 51 to 55 ms |
| Seconds of counts in the forty | every one, a second apart |

The counter answers mid-stream and does not stop counting. Nothing needs to be
switched off to ask.

## 4. What the service does

The thread that reads a counter's samples is the only reader its port has. It
now also asks the temperature:

1. A sample is read, stamped and sent on exactly as before.
2. If a temperature is due, `<GETTEMP>>` is written. The input is **not**
   flushed first, as it is for every other question the program asks: a flush
   would throw away a second that was in flight.
3. Four bytes are waited for, up to 0.3 s. The next sample is a second away
   and the answer a twentieth of one.
4. If the four bytes are not an answer, two more are waited for. If the last
   four of the six are an answer, the first two were a second of counts that
   arrived ahead of it; that second is kept and sent on.
5. Two bytes and nothing more is a second of counts and no answer. Anything
   else is discarded and the port left to fall quiet before reading resumes.

A counter that gives nothing, or nothing sensible, to three questions in a row
is not asked again until it is next plugged in. A counter whose firmware the
document does not name is never asked. The service asks once a log interval
(thirty seconds) by default; `--temp-every SECONDS` changes that and
`--no-temp` asks nothing.

A temperature goes nowhere a count goes: not into an average, the spectrum,
the entropy pool or a row of the count log.

## 5. Where the readings are kept

`temp-<serial>-YYYY-MM.tsv`, beside the counter's `cpm-` file:

```
#time	temp_c
2026-11-16T10:06:33	26.1
2026-11-16T10:06:45	26.0
```

The count log's row is not changed. Its characters are compared byte for byte
with the Python program's by the differential tests, and every reader of it
matches columns by a header that a new column would change in the middle of a
month.

The service also sends each reading to every attached program as a line of
its own on the stream (`t`, the counter's index, the time, the degrees), and
gives the newest reading of each counter to a program that attaches later. A
monitor built before this ignores the line.

## 6. Asking the service: `radbeeper now`

Another program must not open the port. `radbeeper now` attaches to the
running service, reads what it replays, and prints one line for each counter:

```
#serial	cpm	cpm_300	usvh	age	temp_c	temp_age	live	port	version
F48824B8207F7E	60.0	60.0	0.3960	-0.0	26.0	6.0	yes	/dev/cu.usbserial-230	GMC-320Re 4.26
```

| Column | Meaning |
|---|---|
| `cpm`, `cpm_300` | counts a minute over the newest minute and the newest five, from the seconds themselves |
| `usvh` | the five-minute rate as a dose rate |
| `age` | seconds since that counter's newest second |
| `temp_c`, `temp_age` | the newest temperature and how old it is; empty if there is none |
| `live` | whether the counter is still answering |

`radbeeper now --json` gives the same as one JSON object. The command exits 1
when no service is serving the log directory.

## 7. The proof that no count is lost

The fake counter used by the test suite (`tests/fake_gmc.py`) now has a
thermometer and writes down every count it sends. `tests/test_temp.py` runs
the real service against it and compares what the service published with what
the fake sent, second for second:

| Case | Result |
|---|---|
| Temperature asked every second | counts identical; every answer 26.1 |
| A second of counts sent ahead of every answer | counts identical; every answer 26.1 |
| 85.0 for the first two answers | counts identical; 85.0 never logged |
| Below freezing | the sign is kept |
| A firmware that never answers | asked three times, then left alone; counts identical |
| Re 2.10, and a GMC-300 | never asked |
| `--no-temp` | never asked; no temperature file |

The whole suite, 272 Python tests and the Rust tests, passes with the change.

## 8. What was not measured

- **What the thermometer reads.** It is inside the counter's case. It agreed
  with the counter's own screen, which is the same sensor; it has not been
  compared with a thermometer in the room's air.
- **The +V4 Re 4.83.** Asked once, answered 85.0. That it then gives real
  readings is expected from the other unit and has not been seen.
- **Whether asking moves a count in time.** The seconds kept arriving a second
  apart and none was lost. Whether a count that would have been reported in
  one second is reported in the next because the counter was busy answering
  was not tested, and would matter only to the random numbers drawn from the
  timing.
- **Why 85.0.** See section 2.

## 9. Running it on a Mac

The program was Linux only. Two things stood in the way and both are now
conditional: the check of whether the system clock is steered (Linux's
`adjtimex`), and the name of the serial port, which on a Mac is
`/dev/cu.usbserial-*`. The service, `probe`, `now` and the window build and
run there. The service installed by Copal, the hotplug rule and the picture
tools remain Linux only.

---

## See also

- [The stream](the-stream.md) — the socket `now` reads, and why a second
  program never opens the port.
- [The log](the-log.md) — the count log this leaves untouched.
- [Reference](reference.md) — every command and option.
