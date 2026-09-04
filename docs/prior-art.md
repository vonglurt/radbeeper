# Prior art: what GeigerLog and gq-gmc-control already know

Read on 4 September 2026, against radbeeper 0.1.0.

Two programs already talk to a GQ GMC counter over USB, and both are worth
reading before writing a third. This is what they do, where radbeeper differs
on purpose, and the two places it differs by mistake.

| | lines | shape | what it is for |
|---|---|---|---|
| **GeigerLog** (ullix, v2.2pre01) | ~78,000 Python | Qt desktop app, 20+ device backends, SQLite, plotting, a web server | a lab instrument's whole workbench |
| **gq-gmc-control** (chaim-zax) | 1,200 Python | one library + one argparse CLI, `pyserial` | a scriptable remote control for the device |
| **radbeeper** | 1,026 Python | one stdlib file, no dependencies | what it is counting *now*, on three time constants |

Neither flashes firmware. gq-gmc-control has the option in its CLI --
`-F/--firmware-update` -- and the function behind it is two lines:

```python
def firmware_update():
    print('ERROR: option not yet available')
```

GeigerLog mentions firmware forty times and every one is a *workaround for*
a firmware revision, never a write to one. So the gap in the table is real:
nothing in the open-source world flashes these counters, and both projects
that got close stopped at the same place.

## Two bugs this reading found in radbeeper

### 1. The 0x3FFF mask is for CPS, and it is on CPM too

radbeeper masks both:

```python
def cpm(self):
    b = self._ask(CMD_CPM, 2)
    return struct.unpack(">H", b)[0] & COUNT_MASK   # COUNT_MASK = 0x3FFF
```

Both references say that mask belongs to CPS alone. GeigerLog is explicit --

```python
if maskHighBit: value = value & 0x3fff   # ONLY for CPS* calls on 300 series counters
```

-- and gq-gmc-control masks in `set_heartbeat()` (which is CPS) and does *not*
mask in `get_cpm()`. radbeeper's own README agrees with them and disagrees with
its code:

> | `<GETCPM>>` | 2 bytes, big-endian |
> | `<GETCPS>>` | 2 bytes, mask `0x3FFF` |

The consequence is not cosmetic. The mask silently wraps any CPM at or above
16384: a counter genuinely reading 20,000 CPM reports 3,616. On the M4011 at
151.5 CPM per uSv/h that is 132 uSv/h being displayed as 24 -- an order of
magnitude, in the direction of "this is fine", in the one regime where the
number matters. It is invisible in ordinary use because background is 25 CPM,
and it is exactly wrong the first time somebody puts the tube against a real
source.

`samples()` masks too, and there it is correct: the heartbeat stream is CPS.

### 2. The 500 and 600 series answer in four bytes, not two

radbeeper hardcodes a 2-byte reply for every count command, and its README
claims the 300/500/600. GeigerLog carries a per-model, per-firmware table and
the 500/600 rows say otherwise:

```python
GMC_Bytes = 4    # Yields 4 bytes on all CPx calls!    (500, 500+, 600 series)
GMC_Bytes = 2    #                                     (280, 300, 320)
```

with at least one 500 firmware back at 2, which is why the table is per
firmware and not per model. On a 4-byte counter, reading the first two bytes
of a big-endian 32-bit count gives the *high* half: a real reading of 25 comes
back as 0. The two leftover bytes then sit in the buffer -- `_ask()` calls
`flush_input()` first so the simple queries resynchronise, but `samples()`
does not flush per read, so the monitor's stream would stay one sample out of
phase permanently.

gq-gmc-control has the same bug and documents its testing as "done on a GQ
GMC-500", which is probably one of the 2-byte firmwares.

Nothing here is testable without hardware of that series. The honest fix is
either to detect the width (ask `<GETCPM>>` and see how many bytes arrive
before the timeout) or to narrow the README's claim to the 300 series until a
500 is on the desk.

## Three things worth taking

**The tube factor is in the counter.** radbeeper's 151.5 is a flag with a
sensible default. It does not have to be: the device stores three calibration
points, and gq-gmc-control reads them out of `<GETCFG>>` and averages them.

```python
cal1_sv = m_config['cal1_sv'] * 1000 / m_config['cal1_cpm']
...
cal_sv = (cal1_sv + cal2_sv + cal3_sv) / 3
```

`--cpm-per-usvh auto` would read the factor from the tube it is actually
attached to, which is better than a default that is right for one model. The
GMC-500+ has two tubes with sensitivities of 153.8 and 21.4, so on that
hardware a single default cannot be right at all.

**`<GETCFG>>` is a plain byte array with known offsets.** Reading it is safe
and cheap and would let `probe` print the counter's own calibration, its
alarm threshold, its save mode and (on wifi models) the server it is
uploading to. Writing it is not cheap: gq-gmc-control's `write_config()` has
to `<ECFG>>` (erase all of flash config), then `<WCFG>>` every byte back one
at a time, then `<CFGUPDATE>>`. A failure halfway leaves the counter with a
partially written configuration. That is a good reason for radbeeper to read
config and not write it.

**GeigerLog's model table is the actual documentation.** GQ's RFC1201 is the
published protocol and both projects note where shipped units disagree with
it. GeigerLog resolves that by branching on firmware version with forum links
in the comments -- the GMC-500+ 1.18 that returns nothing on the *first*
`<GETVER>>` and needs the connection cycled, the 2.42 voltage reply with no
null terminator, dead-time settings that only exist from 2.41. If radbeeper
ever meets a counter that misbehaves, `gdev_gmc.py` around line 4550 is where
to look first.

## What radbeeper does that neither does

Both references report the counter's own number. `<GETCPM>>` is a 60-second
rolling average computed on the device, so both are showing one time constant
and calling it "the reading". gq-gmc-control's `--heartbeat` prints raw CPS
once a second with no averaging at all, which is the opposite extreme: pure
noise, 25 background counts a minute arriving as a column of 0s and 1s.

Counting the blips locally and running three windows over them is radbeeper's
one real idea, and neither program does it. Because neither builds a local
averaging window at all, neither has to answer the question the `--` answers
-- what to show while a window is still filling -- so there is no prior art
here to borrow from or argue with.

The other difference is the failure path. GeigerLog's answer to "no counter"
is a dialog; gq-gmc-control's is `ERROR: no device connected`. Neither
distinguishes *no device* from *no driver*, which is the failure that cost an
afternoon here and the reason `probe` names all three.
