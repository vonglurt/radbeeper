# The architecture

**One process reads each counter and writes the record; every other program is handed the same seconds over a socket and draws from them. This is how that is built, function by function, and a review of where it is sound and where it is not.**

*A description of the program's structure and techniques with links to the code, followed by a software architecture review. Written against the working tree of 16 November 2026, commit cf8eb39.*

---

## How to read this

Every function, type and constant named here is a link to its definition. The links point at the `main` branch on GitHub, and the line numbers are those of commit cf8eb39. A later reader who follows a link after `main` has moved may land a few lines off; the name in the link is what to search for. Two files, `src/site.rs` and `tools/landing.py`, were changed in the commit that added this document, and the links into them use their lines after it.

The code excerpts are copied from the files as they stand at that commit. Each has the file and its lines over it.

The sizes, so the rest can be weighed:

| Part | File | Lines |
|---|---|---|
| The command-line program | `src/main.rs` | 7,586, of which the last 1,037 are its tests |
| The window | `gui/src/main.rs` | 5,992, of which the last 843 are its tests |
| The library, 17 modules | `src/*.rs` named in `src/lib.rs` | 16,449 |
| The port, two modules of the binary and not of the library | `src/counter.rs`, `src/serial.rs` | 891 |
| The reference program | `radbeeper` (Python, one file) | 6,851 |

The root crate has one dependency, `libc`. The window is a separate crate in `gui/` that depends on the root crate and on iced, and is deliberately not a workspace member, so that the root's lock file does not grow by several hundred crates.

---

# Part one: how it is built

## 1. The processes, and who owns what

**One process holds a counter's serial port. It is the only reader the port has, the only writer the log directory has, and it serves what it reads on a unix socket in that directory. Everything else attaches to the socket.**

The port is exclusive because two processes reading one tty each get a share of the bytes and neither is told. [`Serial::open`](https://github.com/vonglurt/radbeeper/blob/main/src/serial.rs#L50-L69) takes an advisory lock before it touches the line settings, and every radbeeper takes it:

[`src/serial.rs`, lines 60 to 65](https://github.com/vonglurt/radbeeper/blob/main/src/serial.rs#L60-L65)

```rust
        // Advisory, and every radbeeper takes it -- which are exactly the
        // programs that would otherwise collide here.
        if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            unsafe { libc::close(fd) };
            return Err(OpenError::Busy);
        }
```

The lock argues for one reader. It does not argue for one consumer, and [The stream](the-stream.md) is the report on the difference. The process that holds the lock starts a [`Server`](https://github.com/vonglurt/radbeeper/blob/main/src/broker.rs#L180-L191) on the socket [`socket_path`](https://github.com/vonglurt/radbeeper/blob/main/src/broker.rs#L62-L64) names, which is `sock` in the log directory, and publishes every second it reads.

```
                      /dev/ttyUSB0            /dev/ttyUSB1
                           |                       |
                    flock  |                flock  |
              +------------+-----------------------+-----------+
              |  radbeeper service   (or a `watch` that found  |
              |                       nobody serving)          |
              |                                                |
              |  reader thread per counter  -->  one channel   |
              |  main loop: windows, pools, spectrum, loggers  |
              +------+--------------------------------+--------+
                     | writes                         | serves
                     v                                v
        LOGDIR/cpm-*.tsv  temp-*.tsv  spectrum-*   LOGDIR/sock
        random-*  drum-day.tsv  drum-month.tsv        |
        status  starts                                |
                     ^                                |
                     | reads only                     | reads only
        +------------+-----+-----------+--------------+---------+
        |                  |           |              |         |
   radbeeper watch   radbeeper-gui   radbeeper now  radbeeper probe
   (terminal)        (window)        (one line a    (identity of what
                                      counter)       is being served)
```

| Program | Entry | Holds a port | Writes the log directory |
|---|---|---|---|
| `radbeeper service` | [`service`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L3445-L3954) | yes | yes: everything in section 4 |
| `radbeeper watch`, nobody serving | [`watch`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L2387-L3429) over the `Own` arm of [`Feed`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L2005-L2020) | yes | yes, unless `--no-log`; no temperature (section 3) |
| `radbeeper watch`, a service running | [`watch`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L2387-L3429) over its `Attached` arm | no | no |
| `radbeeper-gui` | [`feed`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L4379-L4906) | no; the crate has no code for one | no |
| `radbeeper now` | [`now_cmd`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L4098-L4202) | no | no |
| `radbeeper probe` | [`probe_cmd`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L4204-L4267) | only ports nobody else holds | no |
| `radbeeper hotplug` | [`hotplug`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L6501-L6546) | it watches `/dev` for a node appearing; [`worth_a_window`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L6390-L6402) opens the port for a moment only when there is no socket | the `status` line, when nothing answers |
| `cpm`, `clock`, `log`, `backfill`, `random` | dispatched in [`main`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L5793-L6231) | yes, for one conversation | `backfill` and `random` do |

Which of the two a monitor is, is decided in one place. [`Feed::open`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L2083-L2121) asks the socket first and opens a port only when nobody answers:

[`src/main.rs`, lines 2094 to 2098](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L2094-L2098)

```rust
        if let Some(c) = broker::Client::attach(dir) {
            if devices.iter().all(|d| c.identity().serves(d)) {
                return Ok(Feed::Attached(c));
            }
        }
```

A monitor that does take the port serves in turn (`Feed::Own` carries its own `broker::Server`), so the next window attaches to it. The drawing code in `watch` is handed a [`Tick`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L2023-L2043) either way and cannot tell which kind of feed it has.

The rule that a client writes nothing is kept by construction where it can be: the window's crate does not contain the serial code, because [`counter`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L10) and [`serial`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L11) are modules of the command-line binary and are not in `src/lib.rs`. In the terminal monitor it is kept by a line: `logging.filter(|_| feed.owns_the_port())`.

## 2. The path of a count

**A count is two bytes on a wire, then a number on a channel, then a line on a socket. Between the channel and the socket the process that owns the port does everything that is done once: averages for the log, the entropy pool, the spectrum row, the log row.**

```
 counter --2 bytes/s--> Serial::read_exact_or_timeout
                              |
                        Counter::next_raw            src/counter.rs
                              |  u16, flag bits and all
                        reader thread (Bank::spawn)  src/main.rs
                              |  Tube::Sample { who, when, counts }
                        mpsc channel, one for all counters
                              |
                        service loop                 src/main.rs
          +---------+---------+----------+-----------+-----------+
          |         |         |          |           |           |
     Windows    Windows   Entropy     Ladder      Trail       Server::
     each[who]    all     pools[who]  (suspect?)  (512 s)     publish_sample
          |         |         |                      |           |
     Logger::add  Merged   write_record         write_spectrum   |  "s who when counts"
     log::Writer  Logger   write_frame                           v
          |         |         |                               LOGDIR/sock
     Server::       |     Server::                               |
     publish_row    |     publish_random                   broker::Client
                    |                                       /           \
              cpm-merged-*.tsv                    Feed::poll -> Tick   feed thread -> Snapshot
                                                  watch: Windows,      App::update, Chart::draw
                                                  Trail, Paper
```

**The wire.** In heartbeat mode the counter sends a 16-bit count every second with no framing. The reader stays in step only by asking for exactly two bytes and giving up at a deadline. [`Serial::read_exact_or_timeout`](https://github.com/vonglurt/radbeeper/blob/main/src/serial.rs#L130-L158) is the one primitive: `count` bytes, or fewer if the time runs out. [`Counter::next_raw`](https://github.com/vonglurt/radbeeper/blob/main/src/counter.rs#L361-L364) is the stream on top of it:

[`src/counter.rs`, lines 361 to 364](https://github.com/vonglurt/radbeeper/blob/main/src/counter.rs#L361-L364)

```rust
    pub fn next_raw(&self, timeout: Duration) -> Option<u16> {
        let r = self.port.read_exact_or_timeout(2, timeout);
        (r.len() == 2).then(|| u16::from_be_bytes([r[0], r[1]]))
    }
```

The top two bits are status flags; [`COUNT_MASK`](https://github.com/vonglurt/radbeeper/blob/main/src/counter.rs#L13) takes them off.

**The reader thread and the channel.** [`Bank`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L1652-L1671) holds one slot per counter ever seen, and one `mpsc` channel that every reader sends on. [`Bank::spawn`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L1742-L1859) starts a thread per counter that reads a second, stamps it with the wall clock and sends it. What it sends is a [`Tube`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L1625-L1632):

[`src/main.rs`, lines 1625 to 1632](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L1625-L1632)

```rust
enum Tube {
    Sample { who: usize, when: f64, counts: u32 },
    /// This tube stopped answering. Its slot stays reserved for its serial.
    Gone { who: usize },
    /// This tube's own thermometer, in degrees Celsius. Only from a bank
    /// that was told to ask, and a counter that has one. See `Bank::spawn`.
    Temp { who: usize, when: f64, celsius: f64 },
}
```

`who` is the counter's index in the bank, and a counter that is unplugged and put back keeps the index its serial had ([`Bank::adopt`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L1688-L1723)). Reading does not begin when the bank is made. [`Bank::start`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L1894-L1902) is separate from [`Bank::open`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L1674-L1684) because the flash download at start is a conversation on the same port, and a reader running during it takes the flash for counts.

**The service loop.** [`service`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L3445-L3954) is one loop on the main thread. Each turn lets in new clients ([`Server::accept_pending`](https://github.com/vonglurt/radbeeper/blob/main/src/broker.rs#L259-L270)), looks every thirty seconds for a counter that was not there before ([`Prober`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L1937-L1941), on threads of its own so a dead port holds nobody up), and takes one `Tube` from the bank. A sample goes to that counter's windows, to the room's windows, and out on the socket, in that order:

[`src/main.rs`, lines 3856 to 3860](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L3856-L3860)

```rust
        each[who].add(when, counts);
        all.add(when, counts);
        if let Some(s) = srv.as_mut() {
            s.publish_sample(who, when, counts);
        }
```

Then, in the same turn:

| What | Where | Rule |
|---|---|---|
| Averages | [`Windows`](https://github.com/vonglurt/radbeeper/blob/main/src/analysis.rs#L12-L23), one per counter and one across all | a window says nothing until it is full ([`Windows::average`](https://github.com/vonglurt/radbeeper/blob/main/src/analysis.rs#L86-L92)) |
| The whole second, summed across counters | `sec_bin` and `sec_sum` in the loop | closes when the wall clock's second does |
| Is the spectrum flat | [`Ladder`](https://github.com/vonglurt/radbeeper/blob/main/src/analysis.rs#L1121-L1123), fed the summed second | an emission is marked `suspect` when the loudest bin stands 1.25 times over [`Spectrum::chance_max`](https://github.com/vonglurt/radbeeper/blob/main/src/analysis.rs#L1092-L1094) |
| The spectrum row | [`Trail`](https://github.com/vonglurt/radbeeper/blob/main/src/analysis.rs#L684-L696) with a hop of one window, then [`write_spectrum`](https://github.com/vonglurt/radbeeper/blob/main/src/log.rs#L405-L430) | one row every 512 seconds |
| The entropy pool | [`Entropy`](https://github.com/vonglurt/radbeeper/blob/main/src/entropy.rs#L107-L123), one per counter, never merged | when [`Entropy::ready`](https://github.com/vonglurt/radbeeper/blob/main/src/entropy.rs#L197-L199), [`write_record`](https://github.com/vonglurt/radbeeper/blob/main/src/entropy.rs#L439-L462), [`write_frame`](https://github.com/vonglurt/radbeeper/blob/main/src/entropy.rs#L765-L773), [`Server::publish_random`](https://github.com/vonglurt/radbeeper/blob/main/src/broker.rs#L378-L380) |
| The log row | [`Logger`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L4286-L4295) over [`Writer`](https://github.com/vonglurt/radbeeper/blob/main/src/log.rs#L1180-L1194) | one row an interval, then [`Server::publish_row`](https://github.com/vonglurt/radbeeper/blob/main/src/broker.rs#L368-L374) |
| The room's row | [`MergedLogger`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L4382-L4419) | only when more than one counter is plugged in |

`Windows` keeps a running sum for each span and subtracts what falls out of the back, so a sample costs the same whatever the span:

[`src/analysis.rs`, lines 45 to 60](https://github.com/vonglurt/radbeeper/blob/main/src/analysis.rs#L45-L60)

```rust
        for i in 0..self.spans.len() {
            self.sums[i] += counts as u64;
            self.held[i] += 1;
            let cutoff = when - self.spans[i];
            let mut head = self.heads[i];
            while head - self.base < self.samples.len() {
                let (t, c) = self.samples[head - self.base];
                if t > cutoff {
                    break;
                }
                self.sums[i] -= c as u64;
                self.held[i] -= 1;
                head += 1;
            }
            self.heads[i] = head;
        }
```

**The socket.** The protocol is lines of tab-separated text. [`Server::publish_sample`](https://github.com/vonglurt/radbeeper/blob/main/src/broker.rs#L345-L351) keeps the sample in a ring and writes `s`, the index, the time and the count to every client. Live writes are non-blocking, and a client that does not take a line is dropped rather than waited for:

[`src/broker.rs`, lines 397 to 407](https://github.com/vonglurt/radbeeper/blob/main/src/broker.rs#L397-L407)

```rust
    fn publish(&mut self, line: &str) {
        let bytes = line.as_bytes();
        self.clients.retain_mut(|c| match c.write_all(bytes) {
            Ok(()) => true,
            // A window that was closed, and a window that has stopped
            // reading, are the same thing from here and get the same
            // treatment. Neither is an error worth printing: people close
            // windows.
            Err(_) => false,
        });
    }
```

A client that attaches is given the greeting (`hello`, the protocol number, the server's spans, and a `c` line for each counter), then the ring, then the newest log rows, then the newest temperature of each counter, then `live`. That replay is why a window opened on a service that has been up all day has its eight-hour average at once:

[`src/broker.rs`, lines 288 to 302](https://github.com/vonglurt/radbeeper/blob/main/src/broker.rs#L288-L302)

```rust
        for (who, when, counts) in &self.ring {
            out.push_str(&tab(&["s", &who.to_string(), &t(*when), &counts.to_string()]));
        }
        for (who, r) in &self.rows {
            out.push_str(&tab(&["row", &who.to_string(), r]));
        }
        // The newest temperature of each counter, so a client that attaches,
        // reads to `live` and leaves -- `radbeeper now` -- has it without
        // waiting half a minute for the next.
        for (who, at) in self.temps.iter().enumerate() {
            if let Some((when, celsius)) = at {
                out.push_str(&tab(&["t", &who.to_string(), &t(*when), &format!("{:.1}", celsius)]));
            }
        }
        out.push_str("live\n");
```

The ring is [`RING`](https://github.com/vonglurt/radbeeper/blob/main/src/broker.rs#L46), 30,000 samples, and the rows are [`ROWS`](https://github.com/vonglurt/radbeeper/blob/main/src/broker.rs#L49), 64.

**The clients.** [`Client::connect`](https://github.com/vonglurt/radbeeper/blob/main/src/broker.rs#L472-L504) reads the greeting into an [`Identity`](https://github.com/vonglurt/radbeeper/blob/main/src/broker.rs#L82-L89). [`Client::poll`](https://github.com/vonglurt/radbeeper/blob/main/src/broker.rs#L540-L569) gives one [`Event`](https://github.com/vonglurt/radbeeper/blob/main/src/broker.rs#L113-L141) at a time and keeps three answers apart: an event, nothing yet, and the socket closed. The parsing is [`parse`](https://github.com/vonglurt/radbeeper/blob/main/src/broker.rs#L592-L629), and a line whose first word it does not know is skipped.

The terminal monitor turns an `Event` into a `Tick` ([`Tick::of`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L2056-L2071)) and, in `watch`, feeds the same types the service feeds: `Windows`, `Ladder`, `Trail`. The window does the same on a thread ([`feed`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L4379-L4906)), packs what the interface needs into a [`Snapshot`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L1428-L1485) about once a second, and sends it as the `Update` arm of [`Message`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L1494-L1515). [`App::update`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L1569-L1641) stores it, [`App::view`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L1700-L2651) lays the widgets out and builds a [`Chart`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L2833-L2879), and the canvas calls its `draw`.

Neither client computes a random line of its own. The hex on a client's screen is the one the server published, so there is one line and it is the one in the file.

## 3. The path of a temperature

**The reader thread asks the counter its temperature in the gap after a second of counts, without stopping the stream. An answer is told from a count by its top bit, not by when it arrives. A temperature goes to a file of its own and to a line of its own on the socket, and nowhere a count goes.**

[The thermometer](the-thermometer.md) is the report, with the incident that fixed the design: the first version decided by timing, a slower firmware's answers arrived as the wait ran out, and three rows of one counter's log say about 6,600 counts in half a minute.

Only a service asks. `service` sets `bank.temp_every`; a monitor that holds the port itself leaves it `None` (the field's own comment in [`Bank`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L1652-L1671) says so). In [`Bank::spawn`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L1742-L1859) a counter is asked only if [`temp_supported`](https://github.com/vonglurt/radbeeper/blob/main/src/counter.rs#L128-L139) names its firmware (a GMC-320 at Re 3.01 or later), only after two seconds of stream, and only while every second so far has carried [`STREAM_FLAG`](https://github.com/vonglurt/radbeeper/blob/main/src/counter.rs#L117).

[`Counter::temp_in_stream`](https://github.com/vonglurt/radbeeper/blob/main/src/counter.rs#L396-L423) writes `<GETTEMP>>` without flushing the input, then reads pairs until its wait ([`TEMP_WAIT_MS`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L1648), 700 ms) runs out. This is the whole of the discrimination:

[`src/counter.rs`, lines 416 to 421](https://github.com/vonglurt/radbeeper/blob/main/src/counter.rs#L416-L421)

```rust
            let raw = u16::from_be_bytes([head[0], head[1]]);
            if raw & STREAM_FLAG != 0 {
                samples.push(raw & COUNT_MASK);
                continue;
            }
            return StreamTemp { answer: self.late_answer(raw), samples };
```

A pair with the bit is a second of counts and is handed back in the `samples` of a [`StreamTemp`](https://github.com/vonglurt/radbeeper/blob/main/src/counter.rs#L107-L110), which the reader forwards as ordinary `Tube::Sample`s. The first pair without it is the head of the answer. If the wait runs out first, the reader remembers that an answer is owed, and the next pair without the bit that turns up where a second was expected is taken as that answer by [`Counter::late_answer`](https://github.com/vonglurt/radbeeper/blob/main/src/counter.rs#L431-L443). The head of that branch in the reader:

[`src/main.rs`, lines 1763 to 1769](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L1763-L1769)

```rust
                if raw & counter::STREAM_FLAG == 0 {
                    // A pair without the mark, from a stream that has always
                    // had it, while a question is unanswered: the answer,
                    // late. It is read as one and it is NOT a second of
                    // counts.
                    if marked && seen > 0 && owed.map(|at| clock::now() - at < 5.0).unwrap_or(false) {
                        owed = None;
```

The four bytes are decoded by [`decode_temp`](https://github.com/vonglurt/radbeeper/blob/main/src/counter.rs#L153-L164), which also knows the one answer that is not a temperature:

[`src/counter.rs`, lines 153 to 164](https://github.com/vonglurt/radbeeper/blob/main/src/counter.rs#L153-L164)

```rust
pub fn decode_temp(r: &[u8]) -> TempAnswer {
    if r.len() != 4 || r[3] != 0xAA || r[1] > 9 {
        return TempAnswer::Garbled;
    }
    if r[0] == 85 && r[1] == 0 && r[2] == 0 {
        return TempAnswer::NotYet;
    }
    // Tenths as a whole number and ONE division, so 1A 01 is the double
    // nearest 26.1 and not a sum that lands one bit beside it.
    let v = (r[0] as f64 * 10.0 + r[1] as f64) / 10.0;
    TempAnswer::Degrees(if r[2] != 0 { -v } else { v })
}
```

The four results are the [`TempAnswer`](https://github.com/vonglurt/radbeeper/blob/main/src/counter.rs#L92-L102). `NotYet` is asked again in two seconds, five times at most, and after that at the ordinary spacing. `Silent` or `Garbled` three times running ([`TEMP_MISSES`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L1650)) and the counter is not asked again until it is next plugged in.

From the channel on, a `Tube::Temp` takes a short road. The service publishes it and writes it, and `continue`s before anything that handles a count:

| Step | Function |
|---|---|
| To every client, and kept as the newest for whoever attaches next | [`Server::publish_temp`](https://github.com/vonglurt/radbeeper/blob/main/src/broker.rs#L359-L365), the `t` line |
| To `temp-<serial>-YYYY-MM.tsv` | [`write_temp`](https://github.com/vonglurt/radbeeper/blob/main/src/log.rs#L291-L301) |
| Read back for the ten days of drum | [`read_temps`](https://github.com/vonglurt/radbeeper/blob/main/src/log.rs#L310-L345) |
| Laid down a line a day | [`temp_drum`](https://github.com/vonglurt/radbeeper/blob/main/src/analysis.rs#L915-L979) |
| Drawn in the terminal | [`draw_thermometer`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L1247-L1280), [`draw_temp_drum`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L1343-L1400) |
| Drawn in the window | [`Chart::thermometers`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L3268-L3315), [`Chart::temp_paper`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L3343-L3365) |
| Given to another program | [`now_cmd`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L4098-L4202), the `temp_c` and `temp_age` columns |

The writer is short enough to show whole. It opens, appends and closes each time, and writes the header when the file is new:

[`src/log.rs`, lines 291 to 301](https://github.com/vonglurt/radbeeper/blob/main/src/log.rs#L291-L301)

```rust
pub fn write_temp(directory: &Path, serial: &str, when: f64, celsius: f64)
    -> std::io::Result<()>
{
    let path = temp_path(when, directory, serial);
    let fresh = fs::metadata(&path).map(|m| m.len() == 0).unwrap_or(true);
    let mut f = fs::OpenOptions::new().create(true).append(true).open(&path)?;
    if fresh {
        writeln!(f, "#time\ttemp_c")?;
    }
    writeln!(f, "{}\t{:.1}", clock::stamp(when), celsius)
}
```

The `t` line was added without changing [`PROTOCOL`](https://github.com/vonglurt/radbeeper/blob/main/src/broker.rs#L34): a client built before it skips a word it does not know.

## 4. The record on disk

**Everything is a tab-separated text file in one directory, named by what it is, whose it is and its month. A file is appended to by the process that holds the port and by nobody else. There is no rotation code, because a row is written to the file for its own month.**

The directory is `--logs`, or what [`state_dir`](https://github.com/vonglurt/radbeeper/blob/main/src/log.rs#L1040-L1061) finds writable: `/var/lib/radbeeper`, then `/var/log/radbeeper`, then the user's own.

| File | What a row is | Written by |
|---|---|---|
| `cpm-<serial>-YYYY-MM.tsv` | one counter's interval: counts, seconds, each average and its peak, `src`, `site` | [`row`](https://github.com/vonglurt/radbeeper/blob/main/src/log.rs#L230-L255) through [`Logger::add`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L4317-L4334) and [`Writer::write`](https://github.com/vonglurt/radbeeper/blob/main/src/log.rs#L1263-L1281); rows from the flash through [`merge`](https://github.com/vonglurt/radbeeper/blob/main/src/log.rs#L637-L685) |
| `cpm-merged-YYYY-MM.tsv` | the room's interval, with the counts of each tube beside it | [`merged_row`](https://github.com/vonglurt/radbeeper/blob/main/src/log.rs#L166-L213) through [`MergedLogger::add`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L4456-L4499) |
| `temp-<serial>-YYYY-MM.tsv` | a time and degrees Celsius | [`write_temp`](https://github.com/vonglurt/radbeeper/blob/main/src/log.rs#L291-L301) |
| `spectrum-YYYY-MM.tsv` | 512 seconds of the room as a power for each period | [`write_spectrum`](https://github.com/vonglurt/radbeeper/blob/main/src/log.rs#L405-L430) |
| `drum-day.tsv`, `drum-month.tsv` | a finished day's or month's line of spectrum | [`keep`](https://github.com/vonglurt/radbeeper/blob/main/src/longdrum.rs#L385-L421), on a thread, at start and when a day ends |
| `random-<serial>-YYYY-MM.tsv` and `.hex` | an emission and the counts it was drawn from | [`write_record`](https://github.com/vonglurt/radbeeper/blob/main/src/entropy.rs#L439-L462), [`write_hex`](https://github.com/vonglurt/radbeeper/blob/main/src/entropy.rs#L470-L483) |
| `random-<serial>-YYYY-MM.bin` | the emission's raw seconds as a frame, chained to the one before | [`write_frame`](https://github.com/vonglurt/radbeeper/blob/main/src/entropy.rs#L765-L773); read back by [`Series`](https://github.com/vonglurt/radbeeper/blob/main/src/frames.rs#L61-L65) |
| `status` | one line: what the service is doing | [`write_status`](https://github.com/vonglurt/radbeeper/blob/main/src/log.rs#L1073-L1079) |
| `starts` | one line a counter a start: what the backfill and the clock did | [`note_start`](https://github.com/vonglurt/radbeeper/blob/main/src/log.rs#L1092-L1099) |
| `sock` | the socket | [`Server::start`](https://github.com/vonglurt/radbeeper/blob/main/src/broker.rs#L200-L228) |
| `sites.tsv` | where a counter has been | the Python program's `site` command; read here by [`read_sites`](https://github.com/vonglurt/radbeeper/blob/main/src/log.rs#L1394-L1414) |

**Month rotation by construction.** The name is made from the row's own time, so a month ending is not an event:

[`src/log.rs`, lines 263 to 269](https://github.com/vonglurt/radbeeper/blob/main/src/log.rs#L263-L269)

```rust
pub fn path(when: f64, directory: &Path, serial: Option<&str>) -> PathBuf {
    directory.join(format!(
        "cpm-{}-{}.tsv",
        serial.unwrap_or("unknown"),
        clock::format(when, "%Y-%m")
    ))
}
```

`Writer::write` asks for that path on every row and reopens when it differs from the file it holds. It reopens also when the inode under the path has changed, which is what a backfill's rename does to an appender.

**One row per slot.** A row's identity is the slot its time falls in, [`slot_of`](https://github.com/vonglurt/radbeeper/blob/main/src/log.rs#L622-L624): the time divided by the interval, floored. The live writer refuses a second row for a slot, and reads the last slot off the disk when it opens so a restart cannot write one either:

[`src/log.rs`, lines 1271 to 1280](https://github.com/vonglurt/radbeeper/blob/main/src/log.rs#L1271-L1280)

```rust
        let slot = slot_of(when, self.every);
        if self.last_slot.map(|last| slot <= last).unwrap_or(false) {
            return Ok(false);
        }
        if let Some(f) = self.file.as_mut() {
            writeln!(f, "{}", text)?;
            f.flush()?;
        }
        self.last_slot = Some(slot);
        Ok(true)
```

**Backfill.** The counter records to its own flash whether or not anything is listening. At each start, before the readers run, [`backfill_at_start`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L4015-L4045) measures the counter's clock against this machine's ([`Counter::clock_ahead`](https://github.com/vonglurt/radbeeper/blob/main/src/counter.rs#L216-L241)), reads the newest end of the flash ([`read_history_tail`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L5374-L5400), which uses [`find_history_end`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L5322-L5366) to bisect for the write pointer in a ring that has wrapped), and hands the bytes to [`backfill`](https://github.com/vonglurt/radbeeper/blob/main/src/history.rs#L329-L411). That decodes them ([`samples`](https://github.com/vonglurt/radbeeper/blob/main/src/history.rs#L241-L249)), drops anything from the future or from the slot being logged now, cuts them into rows of the live logger's shape, and folds them in with [`merge`](https://github.com/vonglurt/radbeeper/blob/main/src/log.rs#L637-L685). `merge` adds a row only to a slot that has none, so a live row is never replaced by one from the flash, and it writes a new file and renames it over the old one so an interrupted backfill leaves the old log whole. Then the counter's clock is set if it was out by more than a second.

The count log's characters are fixed: [The log](the-log.md) describes the format and section 8 how it is held to the Python program's.

## 5. The spectra

**Radioactive decay arrives by chance, and the spectrum of chance is flat. Every spectrum here is the same sum with different windows, held against what chance would put in a bin, so that 1.0 means nothing is arriving on a schedule. There are four of them, for four lengths of time.**

| Spectrum | Made from | Type or function | Reaches |
|---|---|---|---|
| The running average | every whole second, averaged for as long as the program runs | [`Spectrum`](https://github.com/vonglurt/radbeeper/blob/main/src/analysis.rs#L988-L995), three at once in [`Ladder`](https://github.com/vonglurt/radbeeper/blob/main/src/analysis.rs#L1121-L1123) or at the lengths in [`LADDER`](https://github.com/vonglurt/radbeeper/blob/main/src/drum.rs#L24) | periods up to 512 s, or up to nine hours |
| The live drum | the last 512 seconds, a row every 8 | [`Trail`](https://github.com/vonglurt/radbeeper/blob/main/src/analysis.rs#L684-L696), rows of [`TrailRow`](https://github.com/vonglurt/radbeeper/blob/main/src/analysis.rs#L658-L666) | twelve minutes of rows |
| The day paper | a day of 30-second log rows | [`row`](https://github.com/vonglurt/radbeeper/blob/main/src/longdrum.rs#L307-L329) for the `Day` arm of [`Paper`](https://github.com/vonglurt/radbeeper/blob/main/src/longdrum.rs#L57-L62) | two minutes to eight hours, a line a day |
| The month paper | a month of the log in five-minute sums | the same, for its `Month` arm | twenty minutes to ten days, a line a month |

`Spectrum` is Welch's method: the mean removed, a Hann taper, a radix-2 transform, half-overlapped windows averaged. [`Spectrum::chance_max`](https://github.com/vonglurt/radbeeper/blob/main/src/analysis.rs#L1092-L1094) is how tall the tallest bin gets by luck for the number of averages behind it, and it is the line everything else is coloured against. [`Waterfall`](https://github.com/vonglurt/radbeeper/blob/main/src/analysis.rs#L453-L471) is the first version of the drum, a whole transform a row; it is still in the file and its comment is where the levelling is explained, but nothing outside its own tests constructs one now.

`Trail` is what both front ends and the spectrum log use. A row is joined from several windows that end at the same second: every bin of the shortest, and from each longer window only the bins longer than the one before could hold. Those few bins are worked out by direct sums and not by a transform, and a row is worked out only when it is asked for ([`Trail::rows`](https://github.com/vonglurt/radbeeper/blob/main/src/analysis.rs#L778-L794)), so a window handed eight hours of replay works out the rows on show and not three thousand on the way.

**`src/drum.rs` is the shared paper.** It holds no state. It says where a period stands across the paper, how hot a power is and how wide the pen is, in plain numbers, so the terminal and the window are two ways of putting ink where it says. The axis is cut into stretches at the windows' lengths ([`stretches`](https://github.com/vonglurt/radbeeper/blob/main/src/drum.rs#L71-L94)) because the long periods have few bins and would otherwise take most of the width:

[`src/drum.rs`, lines 98 to 103](https://github.com/vonglurt/radbeeper/blob/main/src/drum.rs#L98-L103)

```rust
pub fn place(axis: &[Stretch], period: f64) -> Option<f32> {
    let s = axis.iter().find(|s| period <= s.long && period >= s.short)?;
    let span = (s.long / s.short).ln();
    let t = if span > 0.0 { ((s.long / period).ln() / span) as f32 } else { 0.0 };
    Some(s.left + t.clamp(0.0, 1.0) * (s.right - s.left))
}
```

[`shade_of`](https://github.com/vonglurt/radbeeper/blob/main/src/drum.rs#L118-L121) turns a power into one of 64 shades by its logarithm against the luck line, [`heat`](https://github.com/vonglurt/radbeeper/blob/main/src/drum.rs#L139-L160) gives a shade its colour as three numbers and no colour type of anybody's, [`pen`](https://github.com/vonglurt/radbeeper/blob/main/src/drum.rs#L164-L166) its width, [`front_row`](https://github.com/vonglurt/radbeeper/blob/main/src/drum.rs#L206-L227) joins the hours-long averages into the row at the front, and [`strength`](https://github.com/vonglurt/radbeeper/blob/main/src/drum.rs#L248-L251) says how strongly a whole row is inked.

**`src/longdrum.rs` makes the long papers from the count log**, so they exist for every day there is a log for, including the days before the code was written. [The drum spectrogram](the-drum-spectrogram.md) and [The monthly record](the-monthly-record.md) are the reports. The steps:

| Step | Function | What it does |
|---|---|---|
| Read | [`Counts::load`](https://github.com/vonglurt/radbeeper/blob/main/src/longdrum.rs#L135-L161) | every counter's rows as (middle of the interval, counts, seconds) |
| Bin | [`Counts::binned`](https://github.com/vonglurt/radbeeper/blob/main/src/longdrum.rs#L183-L216) | counts a second in bins of 30 s or 300 s; `None` where nobody counted |
| Transform | [`spectrum_of`](https://github.com/vonglurt/radbeeper/blob/main/src/longdrum.rs#L238-L295) | mean off, taper on, each power against the scatter, folded into at most 192 bands |
| One line | [`row`](https://github.com/vonglurt/radbeeper/blob/main/src/longdrum.rs#L307-L329) | a [`LongRow`](https://github.com/vonglurt/radbeeper/blob/main/src/longdrum.rs#L110-L124); `None` if under a quarter of the span was counted |
| Keep | [`keep`](https://github.com/vonglurt/radbeeper/blob/main/src/longdrum.rs#L385-L421) | appends each finished line to `drum-day.tsv` or `drum-month.tsv`, once |
| Show | [`rows`](https://github.com/vonglurt/radbeeper/blob/main/src/longdrum.rs#L448-L479) | the newest lines, kept ones from the file and the rest from the counts; empty spans closed up |
| Share the room | [`share`](https://github.com/vonglurt/radbeeper/blob/main/src/longdrum.rs#L492-L504) | how many lines the live drum, the day paper and the month paper each get |

The slowest period a line is asked about is one that comes round three times in it ([`SLOWEST`](https://github.com/vonglurt/radbeeper/blob/main/src/longdrum.rs#L51)); once or twice is a drift and not a rhythm.

**The temperature has a drum too.** [`temp_drum`](https://github.com/vonglurt/radbeeper/blob/main/src/analysis.rs#L915-L979) lays ten days down a line a day on one scale and one reference, with a track worth the middle one of the days' own ranges, so an ordinary day stays inside its track.

## 6. The two front ends over one library

**The terminal monitor and the window draw the same charts from the same arithmetic. What is shared is the arithmetic; each has its own state loop, its own layout and its own way of filling in the hours before it was opened.**

| | Shared, in the library | Terminal monitor, `src/main.rs` | Window, `gui/src/main.rs` |
|---|---|---|---|
| The stream | [`Client`](https://github.com/vonglurt/radbeeper/blob/main/src/broker.rs#L449-L461), [`Event`](https://github.com/vonglurt/radbeeper/blob/main/src/broker.rs#L113-L141) | [`Feed::poll`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L2250-L2265), [`Tick`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L2023-L2043) | [`feed`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L4379-L4906), on its own thread |
| Averages | [`Windows`](https://github.com/vonglurt/radbeeper/blob/main/src/analysis.rs#L12-L23) | [`mean_cpm`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L1990-L1992) | [`mean_of`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L4349-L4351) |
| The counts strip | [`tiers_with`](https://github.com/vonglurt/radbeeper/blob/main/src/analysis.rs#L1270-L1338), [`tiers_arrivals`](https://github.com/vonglurt/radbeeper/blob/main/src/analysis.rs#L1476-L1559) | bars of block characters in `watch` | [`Chart::cascade`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L3686-L3866) |
| The live drum | [`Trail`](https://github.com/vonglurt/radbeeper/blob/main/src/analysis.rs#L684-L696), `src/drum.rs` | [`draw_drum`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L584-L624) on a [`Paper`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L319-L323) | [`Chart::trail`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L3916-L4029), [`Chart::spectrum`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L4087-L4230) |
| The long papers | [`rows`](https://github.com/vonglurt/radbeeper/blob/main/src/longdrum.rs#L448-L479) | [`draw_long`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L845-L878), [`long_axis`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L710-L734) | [`Chart::long_paper`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L3135-L3247), [`long_ends`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L296-L309) |
| The temperature | [`temp_drum`](https://github.com/vonglurt/radbeeper/blob/main/src/analysis.rs#L915-L979), [`read_temps`](https://github.com/vonglurt/radbeeper/blob/main/src/log.rs#L310-L345) | [`draw_temp_drum`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L1343-L1400) | [`Chart::temp_paper`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L3343-L3365) |
| Sharing the height | [`share`](https://github.com/vonglurt/radbeeper/blob/main/src/longdrum.rs#L492-L504) | [`drum_rooms`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L1024-L1062) | [`regions`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L165-L175), [`papers`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L411-L416) |
| Keys | | `t`, `d`, `m`, `q` in `watch` | the same in [`App::subscription`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L1651-L1698), and clicks in [`Chart::pressed`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L3037-L3057) |

**The terminal monitor** is one function, [`watch`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L2387-L3429), of 1,043 lines. It owns the screen through [`Screen`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L126-L128) (raw mode, restored on drop), wakes ten times a second for the keys, and redraws the whole screen into one string on every sample. Lines are drawn on a [`Paper`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L319-L323), a sheet of Braille dots described in section 9. The colour depth is read from the terminal's own environment ([`depth_of`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L212-L222)) and the same pen is written as 24-bit, 256-colour or eight-colour escapes ([`ink`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L264-L270)).

**The window** is an iced application. Its feed thread holds all the state that changes with a sample; the interface thread holds only the latest [`Snapshot`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L1428-L1485), the needle's position ([`Meters`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L1377-L1392), sent twelve times a second on its own message) and what is open. The charts are one canvas program, [`Chart`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L2833-L2879). Its [`Kept`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L2894-L2909) caches the live drum, the temperature drum and the long papers as geometry and clears each cache only when the numbers it was drawn from change, because the software renderer in a virtual machine notices five thousand lines redrawn twelve times a second to move a needle.

**Filling from the log.** The server's replay reaches back only as far as the server has been up. Both front ends fill the stretch before it from the count log, and each does it its own way:

| | Terminal monitor | Window |
|---|---|---|
| Reads | [`log_rows`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L1084-L1106): one counter's file for this month and last | [`Counts::load`](https://github.com/vonglurt/radbeeper/blob/main/src/longdrum.rs#L135-L161): every counter's file in the directory |
| Makes seconds with | [`prefill_seconds`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L1130-L1154): rows laid end to end back from the first sample | [`Counts::binned`](https://github.com/vonglurt/radbeeper/blob/main/src/longdrum.rs#L183-L216) into 30 s bins, then [`seconds_from_log`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L472-L487) |
| Closes a gap at the join of | 600 s ([`PREFILL_JOIN`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L1077)) | 4 bins, 120 s ([`FILL_JOIN`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L452)) |
| Stops at a hole of | 90 s ([`PREFILL_HOLE`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L1078)) | one empty bin |
| Reaches back | 8 hours, or the longest span if longer ([`PREFILL_REACH`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L1076)) | 45,056 s ([`FILL_REACH`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L451)) |
| With several counters | fills each counter's averages; not the strip | fills nothing |
| Bars finer than a log row | emptied by [`blank_fine`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L1160-L1172) | emptied by [`blank_fine`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L493-L505) |

Both deal a row's counts over its seconds as evenly as whole numbers go, so an average of a row or longer holds what was counted; neither gives those seconds to the spectrum, the drum or the entropy pool. The terminal's version, where it is called:

[`src/main.rs`, lines 2710 to 2714](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L2710-L2714)

```rust
                    let per: Vec<Vec<u32>> = id
                        .counters
                        .iter()
                        .map(|c| prefill_seconds(&log_rows(dir, &c.serial_no, when, reach), when, reach))
                        .collect();
```

## 7. The pages

**Four generators write static pages from the log directory and the documents. Two of them exist twice, once in Rust and once in Python, and are held to each other byte for byte.**

| Page | Rust | Python | Pinned |
|---|---|---|---|
| The report (`index.html` or `monitor.html`) and `random.html` | [`export`](https://github.com/vonglurt/radbeeper/blob/main/src/export.rs#L2421-L2540), [`render_html`](https://github.com/vonglurt/radbeeper/blob/main/src/export.rs#L997-L1536), [`render_random_html`](https://github.com/vonglurt/radbeeper/blob/main/src/export.rs#L1933-L2399) | `cmd_export` in `radbeeper` | yes, by `TestTheTwoExportsWriteTheSamePages` |
| The frame browser | [`write_pages`](https://github.com/vonglurt/radbeeper/blob/main/src/browser.rs#L62-L110), `export --frames-page` | none | no |
| The drums page, `drums.html` | [`write_page`](https://github.com/vonglurt/radbeeper/blob/main/src/longpage.rs#L796-L818), `export --drums-page` | none | no |
| The site: landing page and every document in `docs/` | [`build`](https://github.com/vonglurt/radbeeper/blob/main/src/site.rs#L1082-L1193), `radbeeper pages` | [`main`](https://github.com/vonglurt/radbeeper/blob/main/tools/landing.py#L599-L670) | yes, by `TestTheTwoSiteGeneratorsWriteTheSamePages` |

`src/export.rs` has no plotting library; the charts are SVG written as strings ([`svg_plot`](https://github.com/vonglurt/radbeeper/blob/main/src/export.rs#L647-L744)). Matching the Python to the byte took the same float arithmetic in the same order, `libm` calls where Python calls them, and CPython's compensated `sum`; the file's head lists them.

`src/longpage.rs` draws the day paper, the month paper and the temperature drum as inline SVG with no script ([`long_svg`](https://github.com/vonglurt/radbeeper/blob/main/src/longpage.rs#L388-L478), [`temp_svg`](https://github.com/vonglurt/radbeeper/blob/main/src/longpage.rs#L515-L601)), from what [`gather`](https://github.com/vonglurt/radbeeper/blob/main/src/longpage.rs#L96-L121) reads. It has no Python half because the long drums have none, which is why it is a flag on `export` and not part of the pinned page.

`src/site.rs` is a Markdown renderer written by hand over the subset the documents use ([`render`](https://github.com/vonglurt/radbeeper/blob/main/src/site.rs#L382-L579)), with no regular expressions because the crate has no regex dependency. `tools/landing.py` is the same renderer in standard-library Python ([`render`](https://github.com/vonglurt/radbeeper/blob/main/tools/landing.py#L117-L252)). [`run`](https://github.com/vonglurt/radbeeper/blob/main/src/serve.rs#L176-L220) is a small static file server for looking at the result, `radbeeper preview`.

**The robot.** `.github/workflows/pages.yml` rebuilds the published site when logs, documents or the generators are pushed. It uses the Python for the report and the site, and builds the Rust for the one page only the Rust can write:

[`.github/workflows/pages.yml`, lines 90 to 104](https://github.com/vonglurt/radbeeper/blob/main/.github/workflows/pages.yml#L90-L104)

```yaml
      - name: Build the counter's report from logs/
        run: python3 radbeeper export --logs logs -o monitor.html

      # AFTER THE PYTHON'S REPORT AND NEVER OVER IT: `-o` points at the runner's
      # temporary directory, so the only file this step leaves in the checkout
      # is drums.html. It is written even for a fork with no logs yet, saying
      # there is no record, because the landing page links to it either way.
      - name: Build the day and month drums and the temperature from logs/
        run: |
          cargo run --release --locked -- export --logs logs \
            -o "$RUNNER_TEMP/monitor.html" \
            --drums-page --drums-output drums.html

      - name: Build the landing page and the lab reports from the documents
        run: python3 tools/landing.py
```

It then commits `index.html`, `monitor.html`, `drums.html` and the documents' pages back to `main`.

## 8. How it is kept honest

**Four kinds of check: unit tests beside the code, a fake counter on a pseudo-terminal that the real binary is run against, a comparison of the Rust's output with the Python's character for character, and made-up logs in the real format for the things that take a month to happen.**

| Check | Where | What it proves |
|---|---|---|
| Unit tests beside the code | a `mod tests` at the foot of each `src/*.rs`; 261 `#[test]` functions by a count of the attribute, and 34 more in `gui/src/main.rs` | arithmetic, layout arithmetic, the wire format's parsing, the Braille paper's dots |
| The fake counter | [`FakeGMC`](https://github.com/vonglurt/radbeeper/blob/main/tests/fake_gmc.py#L87-L260) | speaks the GMC protocol on a pty: heartbeat, flash, clock, thermometer |
| The port, end to end | `tests/test_serial.py` | identification, the lock, a killed session's leftover stream, the monitor's screen |
| The thermometer | `tests/test_temp.py`, on [`TempCase`](https://github.com/vonglurt/radbeeper/blob/main/tests/test_temp.py#L53-L124) | the service publishes exactly the counts the fake sent, in order, while being asked every second |
| Rust against Python | `tests/test_differential.py` with `examples/format_oracle.rs` | the log row, `%g`, the stamp, the flash decoder, the emission digest, the report pages, the site |
| The frame format | `tests/frames_fixture.rs`, `tests/test_frames_js.py`, `tests/test_browser_js.py` | the binary frames read the same in Rust and in the page's JavaScript |
| Made-up logs | [`counts`](https://github.com/vonglurt/radbeeper/blob/main/src/sample.rs#L76-L107), [`temps`](https://github.com/vonglurt/radbeeper/blob/main/src/sample.rs#L115-L149), `examples/sample_logs.rs` | a month of counts with a twenty-minute rhythm laid over decay, so a day line can be tested without waiting a day |

The fake counter writes down every count it sends, and can be told to answer the temperature after any delay, to answer 85.0 first, to send a second of counts ahead of every answer, or to send its stream without the flag bit. The thermometer's test is one assertion over that record:

[`tests/test_temp.py`, lines 109 to 114](https://github.com/vonglurt/radbeeper/blob/main/tests/test_temp.py#L109-L114)

```python
    def assert_every_second_is_the_counters(self, counts):
        self.assertGreater(len(counts), 10, "the service read almost nothing")
        sent = list(self.dev.sent)
        self.assertEqual(
            counts, sent[:len(counts)],
            "what was published is not what the counter sent, second for second")
```

`tests/test_temp.py` runs it with the answer 0.31, 0.55, 0.85 and 1.3 seconds late, the last two later than the wait and later than the next second.

The differential test builds `examples/format_oracle.rs`, which prints what the library would write for a directive on standard input, and compares each line with what the Python produces for the same input:

[`tests/test_differential.py`, lines 78 to 83](https://github.com/vonglurt/radbeeper/blob/main/tests/test_differential.py#L78-L83)

```python
    def compare(self, cases):
        """cases: [(directive line, the string Python produces)]"""
        got = self.rust([d for d, _ in cases])
        self.assertEqual(len(got), len(cases))
        for (directive, want), mine in zip(cases, got):
            self.assertEqual(mine, want, "on %r" % directive)
```

`examples/site_oracle.rs` is the same idea for the Markdown renderer, and `examples/long_drum.rs` prints the long papers as plain text for looking at a real record over ssh.

The continuous integration is `.github/workflows/rust.yml`: a build with warnings as errors, `cargo test`, two groups of Clippy lints, `cargo package`, the Python suite, and `cargo audit` over both lock files; a second job builds with the oldest toolchain the manifest promises. The review below says what it does not cover.

## 9. Techniques worth naming

**Exact-length reads with a deadline, on a protocol with no framing.** Every reply from the counter is a fixed number of raw bytes. The only way to stay in step is to ask for exactly that many and stop at a deadline. A conversation flushes the input first; the stream never does.

[`src/counter.rs`, lines 167 to 172](https://github.com/vonglurt/radbeeper/blob/main/src/counter.rs#L167-L172)

```rust
    fn ask(&self, cmd: &[u8], want: usize) -> Vec<u8> {
        self.port.flush_input();
        let _ = self.port.write_all(cmd);
        self.port
            .read_exact_or_timeout(want, Duration::from_millis(1000))
    }
```

Where a reply runs past what was asked for (both GMC-320s answer a 2,048-byte flash read with 2,049), a flush is not enough, because the spare byte is still in flight. [`Serial::drain`](https://github.com/vonglurt/radbeeper/blob/main/src/serial.rs#L180-L193) reads until the line has been quiet for 20 ms. The comment in [`Counter::read_history`](https://github.com/vonglurt/radbeeper/blob/main/src/counter.rs#L274-L333) is the account of the corrupt flash that turned out to be this.

**The flag-bit discriminator.** What a pair of bytes is, is decided by what it is. The excerpt is in section 3. The stream's own top bit separates a second of counts from the head of an answer, and a stream that does not carry the bit is never asked.

**Replay on attach.** A client starts complete: the greeting, the ring, the rows, the temperatures, then `live`. The excerpt is in section 2. The replay is written blocking with a five-second deadline and live traffic is non-blocking, so a client that is reading gets all of it and one that is not costs the counter nothing.

**One row per slot.** A row's identity is `floor(time / interval)`. The live writer, the restart and the backfill's merge all refuse a second row for a slot, which is what lets a flash download be folded into a log that was being written live. The excerpt is in section 4.

**Dealing a log row across bins.** A row is thirty of the counter's seconds, which is a little over thirty of ours, so rows dropped whole into thirty-second bins by their middles leave an empty bin every quarter of an hour. [`Counts::binned`](https://github.com/vonglurt/radbeeper/blob/main/src/longdrum.rs#L183-L216) gives each bin the share of the row that lies in it:

[`src/longdrum.rs`, lines 196 to 212](https://github.com/vonglurt/radbeeper/blob/main/src/longdrum.rs#L196-L212)

```rust
        for &(mid, counts, seconds) in &self.rows[from..] {
            let (a, b) = (mid - seconds / 2.0, mid + seconds / 2.0);
            if a >= end {
                break;
            }
            if b <= start {
                continue;
            }
            let first = (((a - start) / bin).floor().max(0.0)) as usize;
            let last = ((((b - start) / bin).ceil()) as usize).min(n);
            for (k, cell) in sums.iter_mut().enumerate().take(last).skip(first) {
                let (lo, hi) = (start + k as f64 * bin, start + (k + 1) as f64 * bin);
                let share = (b.min(hi) - a.max(lo)).max(0.0);
                cell.0 += counts * share / seconds;
                cell.1 += share;
            }
        }
```

A bin is counted only when at least half of it was covered.

**Folding bands by the mean.** A day has 720 periods between two minutes and itself and the paper has 192 places. The loudest of the periods that share a place is high by construction, and a row of those is a wall. Each band is the mean power of its periods, at the geometric mean of their periods, and a band no period fell in is left out:

[`src/longdrum.rs`, lines 289 to 293](https://github.com/vonglurt/radbeeper/blob/main/src/longdrum.rs#L289-L293)

```rust
    let bands = sums
        .iter()
        .filter(|c| c.2 > 0)
        .map(|c| ((c.1 / c.2 as f64).exp() as f32, (c.0 / c.2 as f64) as f32))
        .collect();
```

**Braille paper with hidden-line removal.** A Braille character is two dots across and four down, so a terminal cell is eight places for ink. [`Paper`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L319-L323) is a grid of optional [`Dot`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L305-L309)s; [`Paper::cell`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L407-L428) packs eight of them into one character and gives it the colour of the dot with the highest rank. The drum is drawn back to front, and each row first rubs out everything from its own line down to its baseline, so a row in front hides what is behind it. That is in [`Paper::trace`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L376-L403):

[`src/main.rs`, lines 393 to 400](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L393-L400)

```rust
            if let Some(base) = hide_to {
                for yy in top..=base.max(foot) {
                    self.clear(x, yy);
                }
            }
            for yy in top..=foot {
                self.set(x, yy, dot);
            }
```

**A PNG written by hand.** The window's `--shot` writes a picture of itself with no image dependency. A PNG's pixels are a zlib stream, and zlib allows blocks that are stored as they stand, so [`png_of`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L4265-L4311) is the pixels with a header and three checksums ([`crc32`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L4314-L4324), [`adler32`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L4327-L4340)):

[`gui/src/main.rs`, lines 4279 to 4290](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L4279-L4290)

```rust
    let mut z = vec![0x78, 0x01];
    let mut blocks = raw.chunks(65535).peekable();
    if blocks.peek().is_none() {
        z.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    }
    while let Some(block) = blocks.next() {
        z.push(blocks.peek().is_none() as u8);
        z.extend_from_slice(&(block.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        z.extend_from_slice(block);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());
```

**Byte-for-byte differential testing.** The log format and two sets of pages each have two implementations, and the test is that they write the same characters, not equivalent ones. The excerpt is in section 8. It is what made a port one piece at a time safe, and finding 7 of the review says what it now costs.

---

# Part two: a software architecture review

**The process design is right and the record's rules are right. The weaknesses are in how the code is divided: three copies of the loop that consumes the stream, two front ends that have begun to disagree, a reference implementation whose reason for existing has changed, and several failures that are swallowed.**

Each finding below gives the evidence in the code, a recommendation and a rough size: small is an afternoon, medium is a few days, large is a week or more.

## What is sound

**One reader, many consumers.** The lock is taken before the line settings are touched, the server never reads from a client, and the window cannot open a port because its crate has no code for one. Live writes to clients are non-blocking, so no client can stop the logger.

**The record's invariants.** One row per slot, a file per counter per month, a merge that never replaces a live row and renames a whole file into place, columns matched by name and not by position. These are few, they are stated where they are enforced, and the tests exercise them.

**Counts and everything else are kept apart.** A temperature goes to its own file and its own line. The merged room is a second file beside the per-counter files, with the per-tube counts in it so it can be taken apart again. The pinned count log was not touched to add either.

**The arithmetic is in the library and has no opinion about drawing.** `src/drum.rs` returns fractions and three floats, `src/analysis.rs` returns numbers, and both front ends and the page use them.

**The comments record why.** Nearly every non-obvious line has the incident that put it there, with a date. A reviewer can check a claim against the code, which is how most of what follows was found.

## The findings

### 1. The stream is consumed in three hand-written loops

**This is a real problem, and it is the root of several others.**

The service, the terminal monitor and the window each contain the code that turns samples into a room's state. The whole-second sum for the spectrum, `sec_bin` and `sec_sum`, is written out three times: in [`service`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L3445-L3954), in [`watch`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L2387-L3429) and in [`feed`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L4379-L4906). So is the handling of a counter joining, a counter going quiet, and the rule that the replay is not poured into the entropy pool. [`mean_cpm`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L1990-L1992) and [`mean_of`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L4349-L4351) are the same three lines. `STRIP_KEEP` is defined in both files with the same value.

The copies have already drifted. The join tolerances of the two prefills in section 6 are 600 seconds and 120: after a service has been stopped for five minutes, the terminal fills its averages from the log and the window does not. The long papers' luck line, which decides where the pen turns from green to hot, is 5.0 in the window ([`LONG_LUCK`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L283)) and on the page ([`LUCK`](https://github.com/vonglurt/radbeeper/blob/main/src/longpage.rs#L69)), and in the terminal it is [`long_luck`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L787-L789), which is `chance_max_of(192, 1.0)` and comes to about 7.7 (1 + √(2 × ln 191) + 2 × ln 191 / 3). The same band at six times chance is hot in the window and green in the terminal.

*Recommendation.* Put one type in the library, fed `broker::Event`s, that owns the windows for each counter and the room, the whole-second sum, the ladder, the trail, the arrival strip and the prefill, and gives each front end a read-only view. The service can use the same type for the parts it shares. Each front end then keeps only layout and ink. *Medium.* The existing unit tests of both prefills become tests of one.

### 2. `src/main.rs` and `gui/src/main.rs` are too large, and the seams are visible

**A real problem for navigation and review, not for correctness.** `src/main.rs` is 7,586 lines in one module; `watch` is 1,043 lines, `service` 510, `main` 439. In the window, `App::view` is 952 lines and `feed` 528.

The file already falls into parts with little between them:

| Lines of `src/main.rs` | What | Where it could live |
|---|---|---|
| 54 to 1,620 | the terminal's drawing: digits, `Screen`, colour depth, `Paper`, the drums, the thermometers, the table | a `tui` module of the binary |
| 1,620 to 2,345 | `Tube`, `Bank`, `Prober`, `Feed`, `rejoin`: owning ports | the library, with `counter.rs` and `serial.rs` |
| 2,387 to 3,429 | `watch` | the binary, much shorter after finding 1 |
| 3,445 to 4,055 | `service`, `while_serving`, `backfill_at_start` | the library, as a `collect` module |
| 4,286 to 4,600 | `Logger`, `MergedLogger` | the library, beside `log::Writer` |
| 4,058 to 4,270 and 4,769 to 5,720 | the one-shot commands | a `commands` module of the binary |
| 6,250 to 6,546 | `hotplug` and finding a terminal | a module of the binary |

`counter.rs` and `serial.rs` being outside the library is deliberate for the window, but it also means nothing outside the binary can test the reader thread: `Bank::spawn`, where the temperature incident happened, has no unit test and is reached only through the Python tests against the fake counter. Moving them into the library behind the existing API does not give the window a port; it still has to choose to call it.

Three smaller things in the same vein. The comment at the head of `src/main.rs` describes a program that no longer exists:

[`src/main.rs`, lines 6 to 9](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L6-L9)

```rust
// Everything that writes the log format -- service, backfill, export, site,
// random, hotplug -- is in the one-file Python program in the same repository
// and stays there while that format is still moving. Asking this binary for
// one of them says so rather than pretending.
```

The service, the backfill, the export, the site generator and hotplug are all in the Rust now. `probe`'s lines for a counter are printed by four separate blocks, two in [`probe_cmd`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L4204-L4267) and two in [`main`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L5793-L6231). And [`Waterfall`](https://github.com/vonglurt/radbeeper/blob/main/src/analysis.rs#L453-L471) is constructed only by its own tests.

*Recommendation.* Split along the table, drawing first because it has the fewest ties. Correct the head comment. *Medium*, mechanical, and safe because the tests move with the code.

### 3. The window has private copies of helpers the library now has

**Real, small, and the easiest of the duplications to remove.** `src/longpage.rs` needed the window's axis and pen arithmetic and could not import it from a binary crate, so it copied it and says so in its comments. The window still has its own:

| Helper | Window | Library copy |
|---|---|---|
| The ends of a long paper's axis | [`long_ends`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L296-L309) | [`long_ends`](https://github.com/vonglurt/radbeeper/blob/main/src/longpage.rs#L128-L140) |
| Where a period stands on it | [`long_place`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L314-L322) | [`long_place`](https://github.com/vonglurt/radbeeper/blob/main/src/longpage.rs#L144-L152) |
| A period in letters | [`period_name`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L326-L333) | [`period_name`](https://github.com/vonglurt/radbeeper/blob/main/src/longpage.rs#L155-L162) |
| The periods named along the head | [`long_ticks`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L337-L351) | [`long_ticks`](https://github.com/vonglurt/radbeeper/blob/main/src/longpage.rs#L166-L180) |
| The temperature pen's shade | [`temp_shade`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L624-L629) | [`temp_shade`](https://github.com/vonglurt/radbeeper/blob/main/src/longpage.rs#L186-L191) |

The bodies are the same today; `long_ends` differs only in taking `&[Option<LongRow>]`. The terminal has a third version of the axis in [`long_axis`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L710-L734).

*Recommendation.* Move the five to where they belong (`longdrum` for the axis, `drum` for the shade), delete the window's and the page's copies, and have the terminal use the same ends. Put the luck line there too, once. *Small.*

### 4. The long drums pool counters of different sensitivity

**A real problem when two counters are not alike, and the bench's are not.**

[`Counts::load`](https://github.com/vonglurt/radbeeper/blob/main/src/longdrum.rs#L135-L161) reads every counter's rows into one list, and [`Counts::binned`](https://github.com/vonglurt/radbeeper/blob/main/src/longdrum.rs#L183-L216) divides the summed counts by the summed seconds. The comment calls the result "the room's rate for one tube however many were listening". That holds only if every tube counts the room at the same rate. If one reads 20 counts a minute and the other 40, the pooled figure is 30 while both listen and 20 or 40 when one is away, so a counter joining or leaving is a step in the series the transform is given. The scatter that powers are held against is taken from neighbouring differences and hardly sees a single step, so the step's power is not normalised away; it lands in the long-period bands.

The effect on a line's content is already recorded: the commit of 16 November that relaxed the sample month's test (ed91831) notes that through the week a second tube counts beside the first, the first tube's twenty-minute rhythm is half as deep in the pooled rate and its power a quarter.

The live spectrum has the same shape of problem. The service, the monitor and the window all feed the whole second *summed* across counters to the ladder and the trail, so one of two counters going quiet halves the series. The averages were moved to tube-seconds for exactly this reason ([`Windows::mean`](https://github.com/vonglurt/radbeeper/blob/main/src/analysis.rs#L120-L123)); the spectrum's input was not.

*Recommendation.* Make a line per counter and pool the *spectra*, or divide each counter's rate by its own long mean before pooling so that every tube reads 1.0 on an ordinary day. For the live drum, feed the mean of the counters answering, not the sum. *Medium*, and the kept lines in `drum-day.tsv` would need writing again, which `keep` can do because the count log is the record.

### 5. Failures to write the record are swallowed, or end the service with a success code

**Real, and cheap to fix.** What happens when the log directory cannot be written depends on which file, and none of it is loud.

The count log is the one failure that stops the service, and it stops it by leaving the loop:

[`src/main.rs`, lines 3926 to 3937](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L3926-L3937)

```rust
        match loggers[who].add(when, counts, &averages) {
            Ok(Some(line)) => {
                if let Some(s) = srv.as_mut() {
                    s.publish_row(who, &line);
                }
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!("radbeeper: could not write the log -- {}", e);
                break;
            }
        }
```

After the loop the function writes `stopped` to the status file and returns 0. A supervisor sees a clean exit. The same code is returned for "no counter is plugged in". If `--logs` names a directory that cannot be written, the service runs for about one interval, says so on stderr and exits 0; [`state_dir`](https://github.com/vonglurt/radbeeper/blob/main/src/log.rs#L1040-L1061) probes for a writable directory only when `--logs` is not given.

Other writes fail silently:

| Write | What happens on failure |
|---|---|
| The emission's audit record | ignored: `let _ = entropy::write_record(...)`, and the hex is still published to every client |
| The status file | ignored in [`write_status`](https://github.com/vonglurt/radbeeper/blob/main/src/log.rs#L1073-L1079) |
| The last short row at shutdown | ignored in [`Logger::finish`](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L4339-L4345) |
| A backfill's merge | ignored: [`backfill`](https://github.com/vonglurt/radbeeper/blob/main/src/history.rs#L329-L411) counts only the files whose merge returned `Ok`, so an unwritable log reads as "0 added" |
| The socket | reported once and carried on without, by design |
| The temperature, the spectrum row, a frame | one line on stderr, and carried on |

The emission is the sharpest of these, because the rule in `src/broker.rs` is that a published line is always the one in the file:

[`src/main.rs`, lines 3914 to 3919](https://github.com/vonglurt/radbeeper/blob/main/src/main.rs#L3914-L3919)

```rust
            let _ = entropy::write_record(&dir, &record, serial, suspect);
            if let Some(f) = frame.as_ref() {
                if let Err(e) = entropy::write_frame(&dir, f, serial) {
                    eprintln!("radbeeper: could not write the frame -- {}", e);
                }
            }
```

*Recommendation.* Exit non-zero when the count log cannot be written. Do not publish an emission whose record was not written. Have `backfill` return the merge's error. Check that `--logs` is writable at start and say so then. *Small.*

### 6. The continuous integration is red, and it does not build the window

**Real.** The `rust` workflow has failed on the pushes of 16 November at its last step, and the failure is in the window's dependency tree, not in anything this repository wrote. On the run for commit cf8eb39 `cargo audit` in `gui/` reported `zvariant` and `zvariant_derive` as unmaintained, `rustix` 1.1.5 as unsound, and two yanked crates, none of them among the three advisories the step already ignores:

[`.github/workflows/rust.yml`, lines 105 to 112](https://github.com/vonglurt/radbeeper/blob/main/.github/workflows/rust.yml#L105-L112)

```yaml
      - name: No known advisories against anything this builds
        run: |
          cargo install cargo-audit --locked
          cargo audit --deny warnings
          cd gui && cargo audit --deny warnings \
            --ignore RUSTSEC-2024-0436 \
            --ignore RUSTSEC-2026-0192 \
            --ignore RUSTSEC-2026-0253
```

Two consequences. A workflow that is red on every push stops carrying information: every earlier step passed on that run, and a real failure in one of them would look the same from the commit list. And the step is the only thing in the workflow that touches `gui/` at all. Nothing compiles the window in CI and nothing runs its 34 tests; the workflow's `paths` filter does not include `gui/**`, so a push that changes only the window does not start it.

*Recommendation.* Move the audit of `gui/` to a job of its own that may fail without failing the build (or runs on a schedule), update `rustix`, and add the new advisories to the ignored list with the reason beside each as the existing three have. Add a job that runs `cargo build` and `cargo test` in `gui/`, and add `gui/**` to the paths. *Small.*

### 7. The Python program: what it still owns, and what the pin costs

**The pin was the right tool for the port. The port is over, and the stated reason for keeping the second generators no longer holds.**

The Python file says at its head that it is deprecated and the Rust is the implementation. It still owns two commands the Rust does not have: `site` ([`cmd_site`](https://github.com/vonglurt/radbeeper/blob/main/radbeeper#L3139-L3194)), which writes `sites.tsv`, and `recompute` ([`cmd_recompute`](https://github.com/vonglurt/radbeeper/blob/main/radbeeper#L3099-L3135)), which fills long-window columns in an old log. The Rust refuses `site` by name. It does not know `recompute` at all, and because an unknown command falls through to the code that opens a counter, `radbeeper recompute` beside a running service answers "port busy" and not "unknown command". The Python has no socket, no temperature and no long drums.

The pin's costs are concrete:

| Cost | Evidence |
|---|---|
| A second copy of the report generator | `src/export.rs`, 2,811 lines, written to reproduce CPython's float behaviour |
| A second copy of the Markdown renderer | `src/site.rs`, 1,348 lines, against `tools/landing.py`, 660 |
| The test that holds them | `tests/test_differential.py`, 1,510 lines |
| Features that could not go where they belong | the temperature is a separate file because the count row's characters are pinned; the drums and the frame browser are separate pages because the pinned page cannot grow |
| Every change to a pinned page is made twice | the front page's edit of 16 November 2026 was 28 lines added and 14 removed in `src/site.rs`, and the same in `tools/landing.py` |

The reason given in `src/site.rs` for keeping `tools/landing.py` is that the workflow can then rebuild the site "with no toolchain at all". The workflow now runs `cargo run --release` for the drums page (the excerpt in section 7), so it needs the toolchain anyway.

*Recommendation.* Decide the end state and write it down. One that keeps what the pin bought: freeze the log format against a small set of golden files made once from the Python, have the workflow build every page with the Rust binary it already compiles, port `site` and `recompute` (both small), and retire the Python export and `tools/landing.py`. The report page is then free to carry the drums and the temperature. *Medium to large*, mostly deletion, and it should follow finding 6 so the change is made under a green build.

### 8. The socket protocol has a version and no capabilities

**A real gap, small today.** The rule in [`PROTOCOL`](https://github.com/vonglurt/radbeeper/blob/main/src/broker.rs#L34), that a line may be added without a bump and an unknown line is skipped, is sound and has worked: the `t` line was added that way. Three things it does not cover.

A client cannot tell what a server will never send. A monitor attached to a service started with `--no-temp`, or to another monitor that holds the port, sees no `t` line and cannot say whether the temperature is not asked or has not come yet. The greeting carries the spans and not the log interval, so the window assumes it: [`every: log::DEFAULT_LOG_EVERY`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L4866) is what places the log-row ticks on the counts, whatever `--log-every` the service was given.

A version mismatch is indistinguishable from nobody serving. [`Client::connect`](https://github.com/vonglurt/radbeeper/blob/main/src/broker.rs#L472-L504) returns `None` for a greeting whose number is not exactly its own:

[`src/broker.rs`, lines 481 to 484](https://github.com/vonglurt/radbeeper/blob/main/src/broker.rs#L481-L484)

```rust
        let f: Vec<&str> = line.trim_end().split('\t').collect();
        if f.len() < 4 || f[0] != "hello" || f[1].parse::<u32>().ok()? != PROTOCOL {
            return None;
        }
```

`Feed::open` then tries the port, finds it locked, and prints "port busy". The message does mention a build from before the stream existed, but the program knew the real reason and did not say it.

The replay ring is shared. [`RING`](https://github.com/vonglurt/radbeeper/blob/main/src/broker.rs#L46) is 30,000 samples in all, and its comment says a client's 30,000-second window is full if the service has been up that long. With two counters the ring holds 15,000 seconds of each.

*Recommendation.* Add one optional greeting line of `key=value` words (the log interval, whether the temperature is asked, the server's version) that old clients skip. Return a distinct error from `connect` for a wrong version. Size the ring by counter. *Small.*

### 9. Only the service asks the temperature

**Real for anyone who runs `watch` without a service.** A `watch` that holds the port logs counts, backfills, writes emissions and exports pages, but its bank's `temp_every` is `None`, and `watch` has no call to `log::write_temp`. On such a machine the thermometer is empty and no temperature file is written. The reason given in the code is that a monitor holds a port only while a window is open. That argues against relying on it for a continuous record; it does not argue against recording while it is open, which is what it does for the counts.

*Recommendation.* Let a monitor that owns the port and is logging ask at the same interval and write the same file. The reader thread already does the asking; `watch` needs the setting and the write. *Small.*

### 10. The fill from the log works for one counter

**Real, and a consequence of finding 1.** The window fills nothing when more than one counter is attached:

[`gui/src/main.rs`, lines 4546 to 4550](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L4546-L4550)

```rust
                                    if tubes == 1 {
                                        let log = longdrum::Counts::load(&dir, when - FILL_REACH, when);
                                        let bin = longdrum::DAY_BIN;
                                        let bins = log.binned(when - FILL_REACH, when, bin);
                                        let seconds = seconds_from_log(&bins, bin as usize, FILL_JOIN);
```

The terminal fills each counter's averages but not the strip. And the window's fill, when it runs, reads every counter's file in the directory through `Counts::load`, including the files of a counter that is not attached now, and pools them as finding 4 describes.

*Recommendation.* One fill in the shared type of finding 1, by counter, from that counter's own file. *Small once finding 1 is done.*

### 11. The stream has no way back into step

**This is an inference from the code; it has not been seen to happen.** `Counter::next_raw` reads two bytes and takes them for a second. If a single byte is ever lost on the line, every pair after it is the low byte of one second and the high byte of the next, and nothing in the reader looks for that. Before 16 November there was nothing to look with. Now there is: on a stream that has always carried `STREAM_FLAG`, a pair without it when no temperature answer is owed is, by the reader's own reasoning, not a second of counts. Today that case sets `marked = false`, stops asking the temperature, and sends the pair on as a count. A misaligned pair made of a low byte of 1 and a high byte of `0x80` would be logged as 384 counts in a second.

*Recommendation.* In that case, do not count the pair; drop one byte, and require the flag on the next few pairs before counting again. Add a fake-counter test that swallows one byte. *Small.* Whether a real adapter ever drops a byte is not known, and the cost of handling it is low.

### 12. The thread model is simple, and one invariant rests on a comment

**Mostly sound.** In the service: the main thread owns the server, the windows, the pools and the loggers; one reader thread per counter and the prober's threads send on channels; a flash download for a counter plugged in later runs on its own thread and hands the counter back on a channel; `keep` runs on a thread that only reads the count log and appends to its own files. The only shared state is each `Arc<Counter>`, an `AtomicBool` for the stop signal and a `OnceLock` for one setting. In the window, the feed thread owns the state and the interface gets immutable snapshots and `Arc`s.

Two things to know. The rule that no conversation happens on a port while its reader runs is what `Bank::start` exists for, and its comment records that breaking it put 16,383 counts a second into the log twice. It is kept by the order of calls. `Bank::live` hands out the `Arc<Counter>` to any caller at any time, and every method of `Counter` takes `&self`. And the release profile sets `panic = "abort"`, so a panic on any thread, including the `keep` thread or a prober, ends the service. The comments in `service` record one panic that did, an index out of bounds seven minutes after a second counter joined. Whether the init script restarts it is in the Copal repository and was not checked.

*Recommendation.* Make the reader own its `Counter` while it runs, and give the bank a handle that can only stop it; a conversation then needs the counter back, and the type system keeps the rule. *Medium*, and it fits the move in finding 2.

### 13. What the tests do not reach

**The window's drawing and its clicks are untested.** The window's 34 tests cover the arithmetic under the drawing: where regions fall, what a pen's shade is, the PNG writer, the needle's ballistics, `seconds_from_log`. None constructs a `Chart`. The test named for clicks exercises [`next_open`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L669-L678), the rule for which drum opens next; [`Chart::pressed`](https://github.com/vonglurt/radbeeper/blob/main/gui/src/main.rs#L3037-L3057), which maps a point to a message, and `Chart::draw` are not called by any test. The pictures were checked by eye, with `--shot`.

**A timezone dependence was found on 16 November, by CI and not by a local run.** The sample month's test asked every whole day for six times chance at twenty minutes. Cut at Pacific midnight every day cleared six; cut at UTC's one day read 4.95. Every machine the tests had been run on was on the bench's clock. The fix was to ask for three, and the commit records the suite passing under six zones.

The cause is structural and worth stating once. [`stamp`](https://github.com/vonglurt/radbeeper/blob/main/src/clock.rs#L73-L75) writes local wall-clock time with no offset, and [`parse_stamp`](https://github.com/vonglurt/radbeeper/blob/main/src/clock.rs#L131-L133) reads it back in the reader's zone. A day is cut at local midnight ([`day_start`](https://github.com/vonglurt/radbeeper/blob/main/src/clock.rs#L67-L70)). So the same log gives different day lines on machines in different zones, the published pages are built by a workflow that sets no timezone, on a runner that need not be in the zone the log was written in, and the hour that repeats when the clocks go back is ambiguous in the per-counter log. The merged log carries a `unix` column for this reason; the per-counter log cannot, because its characters are pinned.

*Recommendation.* Run the Rust tests in CI under two zones, one of them not UTC. For the window, a test that builds a `Chart` from a fixed `Snapshot`, calls `pressed` at the centre of each tile and thermometer, and checks the message, needs no renderer. A golden picture through the software renderer and `png_of` is possible but is *medium*; the click test is *small*. The offset in the per-counter log belongs with finding 7.

### 14. The 85.0 sentinel and the flag bit rest on two units

**Not a problem as built; a known limit.** `decode_temp` refuses exactly `55 00 00` and nothing near it, and a counter is asked again a few times and then at the ordinary spacing, so a firmware that rests at some other value would log that value. The flag-bit assumption is guarded at run time and fails safe: a stream without the bit is not asked. What is not guarded is a firmware that sets the bit on its seconds but answers in more than four bytes, or whose answer is preceded by something else. The fake counter encodes what the two units on the bench do, so the tests prove the reader against the belief and not against other hardware. [The thermometer](the-thermometer.md) says this in its own section 8.

*Recommendation.* Nothing to build. When a third firmware is on the bench, run the forty-second mid-stream test from that report against it before trusting its temperature file, and record the result there.

## What to do first

| Order | What | Finding | Size |
|---|---|---|---|
| 1 | Make CI green: audit of `gui/` in its own job; build and test the window in CI | 6 | small |
| 2 | Exit non-zero when the count log cannot be written; do not publish an emission that was not recorded; surface the backfill's merge error | 5 | small |
| 3 | Move the five long-paper helpers and the luck line into the library; one value for both front ends and the page | 3, 1 | small |
| 4 | Per-counter ring; a capabilities line in the greeting; a distinct error for a wrong protocol number | 8 | small |
| 5 | Resynchronise the stream on a pair without the flag; let a logging monitor ask the temperature | 11, 9 | small |
| 6 | One room-state type in the library, with one fill from the log, by counter | 1, 10 | medium |
| 7 | Stop pooling counters before the transform, on the long papers and the live drum | 4 | medium |
| 8 | Split `src/main.rs`; move the port and the collectors into the library; the reader owns its counter | 2, 12 | medium |
| 9 | Decide the Python's end state; build every page with the Rust; retire the second generators | 7 | medium to large |
| 10 | A click test for the window; the Rust tests under a second timezone | 13 | small |

The first five are independent of each other and of everything after them. Six should come before eight, because it removes most of what makes `watch` and `feed` long.

## What was not verified

- **No test suite was run and no binary was built.** This was written from reading the code at commit cf8eb39. The one thing run was this document itself, through both site generators on a scratch copy; they wrote the same page.
- **The CI result** is from the log of the run on GitHub for that commit, read on 16 November; earlier runs were not read beyond their status.
- **The init script and the hotplug rule** are in the Copal repository. Whether a service that exits is restarted was not checked.
- **Finding 11** is reasoning about a lost byte. No log was searched for evidence that it has happened.
- **Finding 4's size of effect** on real logs was not measured; the evidence is the arithmetic of `binned` and the commit message cited.
- **The test counts** in section 8 are counts of the `#[test]` attribute and of `def test` lines, not of tests seen to pass.

---

## See also

- [The stream](the-stream.md): the socket, the replay and the window's layout, at length.
- [The thermometer](the-thermometer.md): the temperature question, the incident and the proof.
- [The log](the-log.md): the count log's format.
- [The drum spectrogram](the-drum-spectrogram.md) and [The monthly record](the-monthly-record.md): the spectra.
- [The chain of custody](the-chain-of-custody.md): the frames and their links.
- [Reference](reference.md): every command and option.

[← back to the overview](../README.md)
