// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Paul Richeson
// The GQ GMC protocol, and finding a counter that speaks it.
//
// Commands are ASCII <NAME>> and replies are a fixed number of raw bytes with
// no framing, so the only way to stay in step is to ask for exactly what you
// expect and time out rather than block.
use crate::serial::{OpenError, Serial};
use std::fs;
use std::time::Duration;

pub const BAUD_RATES: [u32; 2] = [115200, 57600];
pub const COUNT_MASK: u16 = 0x3FFF;
pub const DEFAULT_CPM_PER_USVH: f64 = 151.5;
pub const SPIR_CHUNK: usize = 2048;

/// How far the counter's clock is ahead of this machine's, and how far that
/// figure can be trusted. Negative `ahead` is behind.
#[derive(Clone, Copy, Debug)]
pub struct ClockOffset {
    pub ahead: f64,
    pub within: f64,
    /// What the counter's clock read, as seconds since the epoch in local time.
    pub theirs: f64,
}

pub struct Counter {
    port: Serial,
    pub version: String,
    pub serial_no: String,
    pub path: String,
    pub baud: u32,
}

/// No counter, with a reason a person can act on. The three ways this fails
/// have three different fixes, so they are three different messages.
pub struct NotFound {
    pub reason: String,
    pub detail: String,
    pub busy: bool,
}

/// The device nodes a counter could be behind, sorted.
///
/// `hotplug` watches this list rather than the port itself: opening a serial
/// port every few seconds to ask what is on it would fight the monitor for the
/// device the moment one was running, and rattle every other serial cable on
/// the machine besides. A node APPEARING is the event worth acting on.
pub fn candidate_ports() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir("/dev") {
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            // `cu.usbserial-*` is the same adapter on a Mac: the calling-out
            // node, which opens without waiting for a carrier the counter
            // never raises. Linux has no node of that name, so the one list
            // serves both.
            .filter(|n| {
                n.starts_with("ttyUSB") || n.starts_with("ttyACM")
                    || n.starts_with("cu.usbserial")
            })
            .collect();
        names.sort();
        out.extend(names.into_iter().map(|n| format!("/dev/{}", n)));
    }
    out
}

/// Whether the running kernel could bind a USB serial adapter at all.
///
/// Alpine's linux-virt ships none: no ch341, no usbserial. A counter plugged
/// into a VM running it can never appear, and dmesg is silent because nothing
/// ever claims the device.
fn usb_serial_driver_present() -> bool {
    // Only Linux keeps its drivers where this looks. Anywhere else the
    // question has no answer here, and "no driver" would be a guess.
    if !cfg!(target_os = "linux") {
        return true;
    }
    if let Ok(mut d) = fs::read_dir("/sys/bus/usb-serial/drivers") {
        if d.next().is_some() {
            return true;
        }
    }
    false
}

/// What came back from a temperature question asked in the middle of the
/// stream.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TempAnswer {
    /// Degrees Celsius, to the tenth the counter gives.
    Degrees(f64),
    /// The counter answered, with the reading its thermometer holds before
    /// it has taken one. Ask again in a moment. See `decode_temp`.
    NotYet,
    /// Nothing came back at all: this firmware does not know the command.
    Silent,
    /// Bytes came back and they were not an answer.
    Garbled,
}

/// A temperature question's whole result: the answer, and every second of
/// counts that arrived on the wire in front of it, in order.
#[derive(Clone, Debug, PartialEq)]
pub struct StreamTemp {
    pub answer: TempAnswer,
    pub samples: Vec<u16>,
}

/// The mark a second of counts carries in the stream: its top bit.
///
/// Both counters on the bench set it on every second they send (`80 00` is
/// a second with no counts). It is what lets an answer to a question be
/// told from a count: see `Counter::temp_in_stream`.
pub const STREAM_FLAG: u16 = 0x8000;

/// Whether this firmware answers <GETTEMP>>.
///
/// GQ's protocol document (GQ-RFC1201) gives it to "GMC-320 Re.3.01 or
/// later" and to nothing else, and both units this was written against are
/// GMC-320s: "GMC-320Re 4.26" and "GMC-320+V4Re 4.83". A model the document
/// does not name is not asked. A wrong guess would cost nothing but the
/// question -- see `temp_in_stream`, which gives up on a counter that stays
/// silent -- but a question the maker never promised an answer to is not one
/// to put to an instrument every half minute.
pub fn temp_supported(version: &str) -> bool {
    if model_of(version) != "GMC-320" {
        return false;
    }
    let Some(at) = version.rfind("Re") else { return false };
    let digits: String = version[at + 2..]
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    digits.parse::<f64>().map(|v| v >= 3.01).unwrap_or(false)
}

/// Four bytes of <GETTEMP>> as degrees, or what else they are.
///
/// WHOLE DEGREES, TENTHS, A SIGN, AND 0xAA. 1A 01 00 AA is 26.1 C, which is
/// what the counter's own screen said beside it (2026-11-16, Re 4.26).
///
/// 85.0 IS NOT A TEMPERATURE. It is what both counters answered the first
/// time each was asked on 2026-11-15, and what the Re 4.26 answered first
/// after being plugged in the next morning, half a minute before it said
/// 26.1 and went on saying it. 85 C is the value a one-wire thermometer of
/// the usual kind holds until it has made its first conversion; whether that
/// is the part inside is not known, and it does not need to be: a handheld
/// counter at 85 C has other problems, and the reading that follows is real.
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

impl Counter {
    fn ask(&self, cmd: &[u8], want: usize) -> Vec<u8> {
        self.port.flush_input();
        let _ = self.port.write_all(cmd);
        self.port
            .read_exact_or_timeout(want, Duration::from_millis(1000))
    }

    pub fn model(&self) -> String {
        model_of(&self.version)
    }

    pub fn cpm(&self) -> Option<u16> {
        let r = self.ask(b"<GETCPM>>", 2);
        (r.len() == 2).then(|| u16::from_be_bytes([r[0], r[1]]))
    }

    pub fn voltage(&self) -> Option<f64> {
        let r = self.ask(b"<GETVOLT>>", 1);
        (r.len() == 1).then(|| r[0] as f64 / 10.0)
    }

    /// One <GETDATETIME>> as (the counter's time, when it was asked, when it
    /// answered), all seconds since the epoch.
    fn clock_reading(&self) -> Option<(f64, f64, f64)> {
        let sent = radbeeper::clock::now();
        let r = self.ask(b"<GETDATETIME>>", 7);
        let got = radbeeper::clock::now();
        if r.len() != 7 {
            return None;
        }
        let theirs = radbeeper::clock::from_parts(
            2000 + r[0] as i32, r[1] as i32, r[2] as i32,
            r[3] as i32, r[4] as i32, r[5] as i32,
        )?;
        Some((theirs, sent, got))
    }

    /// The counter's clock against this machine's, timed to its tick.
    ///
    /// ONE READING IS ONLY GOOD TO A SECOND. The counter answers in whole
    /// seconds, so "15:31:54" is anywhere in that second and the difference
    /// from this machine's clock is uncertain by all of it -- 0.85 s out on
    /// the unit this was written against. So it asks again, back to back,
    /// until the second changes. The tick happened after the counter took
    /// the last old answer and before it sent the first new one, which pins
    /// it between two round trips: a few hundredths of a second at 115200.
    ///
    /// Falls back to the single reading, and says so in `within`, if the
    /// second never turns over -- a stopped clock is worth reporting too.
    pub fn clock_ahead(&self) -> Option<ClockOffset> {
        let (first, sent, got) = self.clock_reading()?;
        let fallback = ClockOffset {
            ahead: first - (sent + got) / 2.0 + 0.5,
            within: 0.5 + (got - sent) / 2.0,
            theirs: first,
        };
        let mut before = sent;
        let give_up = got + 2.5;
        while radbeeper::clock::now() < give_up {
            let (theirs, s, g) = match self.clock_reading() {
                Some(r) => r,
                None => return Some(fallback),
            };
            if theirs != first {
                let tick = (before + g) / 2.0;
                return Some(ClockOffset {
                    ahead: theirs - tick,
                    within: (g - before) / 2.0,
                    theirs,
                });
            }
            before = s;
        }
        Some(fallback)
    }

    /// Set the counter's clock to `when`, seconds since the epoch, read as
    /// local time. True if the counter acknowledged it.
    pub fn set_clock(&self, when: f64) -> bool {
        let parts = radbeeper::clock::format(when, "%y %m %d %H %M %S");
        let bytes: Vec<u8> = parts
            .split(' ')
            .filter_map(|p| p.parse::<u8>().ok())
            .collect();
        if bytes.len() != 6 {
            return false;
        }
        let mut cmd = b"<SETDATETIME".to_vec();
        cmd.extend_from_slice(&bytes);
        cmd.extend_from_slice(b">>");
        self.ask(&cmd, 1) == [0xAA]
    }

    /// How much history flash the model carries.
    pub fn flash_size(&self) -> usize {
        match self.model().as_str() {
            "GMC-320" | "GMC-500" | "GMC-600" => 0x100000,
            _ => 0x10000,
        }
    }

    /// `length` bytes of the history flash from `address`.
    ///
    /// The reply has no framing, so each chunk asks for exactly what it
    /// expects and stops short rather than falling out of step with the
    /// device -- a short read here is the end of what the counter will give,
    /// not something to retry into.
    pub fn read_history(&self, address: usize, length: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(length);
        while out.len() < length {
            let n = SPIR_CHUNK.min(length - out.len());
            let a = address + out.len();
            let mut cmd = b"<SPIR".to_vec();
            cmd.push(((a >> 16) & 0xFF) as u8);
            cmd.push(((a >> 8) & 0xFF) as u8);
            cmd.push((a & 0xFF) as u8);
            cmd.push(((n >> 8) & 0xFF) as u8);
            cmd.push((n & 0xFF) as u8);
            cmd.extend_from_slice(b">>");
            // A SHORT REPLY IS ASKED AGAIN BEFORE IT IS BELIEVED. The reply
            // has no framing, so a chunk that lost bytes on the link and the
            // real end of the flash look the same -- and taking the first for
            // the second cut a monitor's backfill off after its oldest 7 KiB,
            // so the newest hours, the ones it was reading for, were never
            // read. The port is drained between tries so a late remainder of
            // the last reply cannot be taken for the start of the next.
            let mut chunk = Vec::new();
            for attempt in 0..3 {
                if attempt > 0 {
                    std::thread::sleep(Duration::from_millis(300));
                }
                self.port.flush_input();
                let _ = self.port.write_all(&cmd);
                chunk = self
                    .port
                    .read_exact_or_timeout(n, Duration::from_millis(5000));
                if chunk.len() == n {
                    break;
                }
            }
            // AND THEN WAIT FOR THE COUNTER TO STOP TALKING. Both GMC-320s
            // on the bench -- Re 4.26 and +V4 Re 4.83 -- answer a 2048-byte
            // <SPIR>> with 2049: the extra byte is the next byte of flash,
            // whatever GQ's document says. The flush before the next command
            // cannot catch it -- at 115200 baud it is still in flight when
            // the flush runs, and lands as the first byte of the next reply.
            // Read back to back, every chunk after the first then began
            // with the previous chunk's spare byte and lost its own last
            // one: a duplicated byte every 2 KiB, which is the "corrupt or
            // half-erased flash" the decoder has been coping with on this
            // unit since the beginning. And in the tail search a single
            // spare data byte at the head of an all-0xFF probe made a
            // blank 512 KiB block look written, so a freshly erased flash
            // backfilled nothing at all (2026-09-30). Twenty milliseconds
            // is two hundred byte-times, and a few USB polling intervals.
            self.port.drain(Duration::from_millis(20));
            if chunk.is_empty() {
                break;
            }
            let short = chunk.len() < n;
            out.extend_from_slice(&chunk);
            if short {
                break;
            }
        }
        out
    }

    pub fn heartbeat(&self, on: bool) {
        self.port.flush_input();
        let _ = self
            .port
            .write_all(if on { b"<HEARTBEAT1>>" } else { b"<HEARTBEAT0>>" });
        if !on {
            // THE DEVICE MAY ALREADY HAVE QUEUED A SAMPLE. Turning the
            // stream off does not unsend the two bytes already on their way,
            // and the next thing to open this port asks <GETVER>> and could
            // read them as its answer. The Python has done this since the
            // beginning; this is parity with it. Costs a fifth of a second,
            // once, at the end of a session.
            std::thread::sleep(Duration::from_millis(200));
            self.port.flush_input();
        }
    }

    /// One counts-per-second sample, or None if the counter went quiet.
    ///
    /// The top two bits of the 16-bit value are status flags on current
    /// firmware, not count, which is why the mask is here and not optional.
    pub fn next_sample(&self, timeout: Duration) -> Option<u16> {
        self.next_raw(timeout).map(|raw| raw & COUNT_MASK)
    }

    /// One second of the stream as the counter sent it, flag bits and all.
    pub fn next_raw(&self, timeout: Duration) -> Option<u16> {
        let r = self.port.read_exact_or_timeout(2, timeout);
        (r.len() == 2).then(|| u16::from_be_bytes([r[0], r[1]]))
    }

    /// Ask the temperature WITHOUT STOPPING THE STREAM, and without losing a
    /// second of it or inventing one.
    ///
    /// THE COUNTER ANSWERS MID-STREAM, AND GOES ON COUNTING. Measured on the
    /// Re 4.26 on 2026-11-16: with <HEARTBEAT1>> on, nine questions in forty
    /// seconds each came back whole and every one of the forty seconds
    /// arrived, a second apart. So there is no handover here, no
    /// <HEARTBEAT0>>, and above all NO FLUSH: `ask` empties the input first,
    /// which is right for a conversation and here would throw away whatever
    /// second was in flight.
    ///
    /// HOW LONG AN ANSWER TAKES IS THE COUNTER'S BUSINESS, AND AN ANSWER IS
    /// TOLD FROM A COUNT BY WHAT IT IS, NEVER BY WHEN IT CAME. The Re 4.26
    /// answered in 55 ms and this waited 300 for four bytes. The +V4 Re 4.83
    /// takes from 170 to just over 300: on the afternoon it was first asked
    /// in service its answers arrived as the wait ran out, were left in the
    /// buffer, and were read as two seconds of counts -- 19 07 is 6,407 of
    /// them -- and three rows of the log said six and a half thousand counts
    /// in half a minute (2026-11-16, 13:21 to 13:23). A longer wait alone
    /// would only move the edge. What tells the two apart is the stream's
    /// own mark: a second of counts has its top bit set (`STREAM_FLAG`), on
    /// both firmwares, and the first byte of an answer is whole degrees,
    /// which never has. So the bytes that arrive are read a pair at a time:
    /// a pair with the mark is a second of counts and is handed back as one;
    /// the first pair without it is the head of the answer, and the next
    /// pair is its tail. An answer that comes after the wait is caught the
    /// same way by the reader: see `late_answer`.
    ///
    /// ONLY A STREAM THAT CARRIES THE MARK MAY BE ASKED. The caller checks
    /// that every second so far had it before it asks at all.
    pub fn temp_in_stream(&self, wait: Duration) -> StreamTemp {
        let mut samples = Vec::new();
        if self.port.write_all(b"<GETTEMP>>").is_err() {
            return StreamTemp { answer: TempAnswer::Silent, samples };
        }
        let deadline = std::time::Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return StreamTemp { answer: TempAnswer::Silent, samples };
            }
            let head = self.port.read_exact_or_timeout(2, left);
            match head.len() {
                0 => return StreamTemp { answer: TempAnswer::Silent, samples },
                2 => {}
                _ => {
                    self.port.drain(Duration::from_millis(20));
                    return StreamTemp { answer: TempAnswer::Garbled, samples };
                }
            }
            let raw = u16::from_be_bytes([head[0], head[1]]);
            if raw & STREAM_FLAG != 0 {
                samples.push(raw & COUNT_MASK);
                continue;
            }
            return StreamTemp { answer: self.late_answer(raw), samples };
        }
    }

    /// The rest of an answer whose first two bytes have already been read,
    /// as `head`: the two that follow, and what the four say.
    ///
    /// For the reader, when a pair without the stream's mark turns up where
    /// a second of counts was expected and a question is still unanswered:
    /// it is that question's answer, late, and must not be counted.
    pub fn late_answer(&self, head: u16) -> TempAnswer {
        let tail = self.port.read_exact_or_timeout(2, Duration::from_millis(400));
        if tail.len() != 2 {
            self.port.drain(Duration::from_millis(20));
            return TempAnswer::Garbled;
        }
        let [a, b] = head.to_be_bytes();
        let answer = decode_temp(&[a, b, tail[0], tail[1]]);
        if answer == TempAnswer::Garbled {
            self.port.drain(Duration::from_millis(20));
        }
        answer
    }
}

fn identify(path: &str, baud: Option<u32>) -> Result<Option<Counter>, OpenError> {
    let rates: Vec<u32> = match baud {
        Some(b) => vec![b],
        None => BAUD_RATES.to_vec(),
    };
    for rate in rates {
        let port = match Serial::open(path, rate, Duration::from_millis(1000)) {
            Ok(p) => p,
            Err(OpenError::Busy) => return Err(OpenError::Busy),
            Err(_) => continue,
        };
        // A SESSION THAT WAS KILLED LEFT THE STREAM ON. `heartbeat(false)`
        // only runs on a clean exit; a SIGKILL, a pulled cable or a crash
        // leaves the counter sending two bytes a second into a port nobody
        // is reading, and flush-then-ask cannot beat that -- a sample lands
        // between the flush and the answer and every reply after it is two
        // bytes out of step. `probe` printed the counter's clock as
        // 20128-00-26 that way, straight after a recording killed `watch`.
        // So say stop first, whatever state it was left in.
        port.flush_input();
        let _ = port.write_all(b"<HEARTBEAT0>>");
        std::thread::sleep(Duration::from_millis(200));
        port.flush_input();
        if port.write_all(b"<GETVER>>").is_err() {
            continue;
        }
        // FOURTEEN BYTES IS THE OLD MODELS' ANSWER, NOT EVERY MODEL'S. A
        // GMC-320 replies "GMC-320Re 4.26", which is exactly 14, and this read
        // exactly 14 for years and was right every time. A GMC-320+ V4 replies
        // "GMC-320+V4Re 4.83", which is 17: the version came out truncated to
        // "GMC-320+V4Re 4" -- a firmware that does not exist -- and three
        // bytes were left sitting in the input buffer for whatever asked next.
        //
        // So take the 14 that are certainly there, then keep taking whatever
        // else arrives until it stops. The short second deadline is what keeps
        // this from costing a second per port on every model that answers in
        // 14: nothing more is coming, and 120 ms is long enough to be sure of
        // that at 115200 baud.
        let mut raw = port.read_exact_or_timeout(14, Duration::from_millis(1000));
        loop {
            let more = port.read_exact_or_timeout(16, Duration::from_millis(120));
            if more.is_empty() {
                break;
            }
            raw.extend_from_slice(&more);
            if raw.len() > 64 {
                break;
            }
        }
        let text = String::from_utf8_lossy(&raw).trim().to_string();
        if text.is_empty() || !text.to_uppercase().contains("GMC") {
            continue;
        }
        port.flush_input();
        let _ = port.write_all(b"<GETSERIAL>>");
        let s = port.read_exact_or_timeout(7, Duration::from_millis(1000));
        let serial_no = if s.len() == 7 {
            s.iter().map(|b| format!("{:02X}", b)).collect()
        } else {
            "unknown".to_string()
        };
        return Ok(Some(Counter {
            port,
            version: text,
            serial_no,
            path: path.to_string(),
            baud: rate,
        }));
    }
    Ok(None)
}

/// The model out of a firmware string.
///
/// A FREE FUNCTION, because `probe` can now learn what the counter is from the
/// service holding it rather than from the port, and all it gets that way is
/// the version string. Both paths must name the same model or the two probes
/// disagree about one counter.
pub fn model_of(version: &str) -> String {
    for m in ["GMC-320", "GMC-300", "GMC-500", "GMC-600"] {
        if version.contains(m) {
            return m.to_string();
        }
    }
    version.split_whitespace().next().unwrap_or("unknown").to_string()
}

/// Every counter on the machine, not just the first.
///
/// WHY THIS IS SEPARATE FROM `find`. One counter is still the ordinary case
/// and `find` is still what `probe`, `clock` and `log pull` want: those ask a
/// counter a question and there is no sense in which two of them answer it.
/// Collecting is different -- two tubes watching the same room are two
/// independent measurements of one number, and the whole point of reading both
/// is that averaging them is more precise than either. So the collector finds
/// all of them and everything else finds one.
///
/// Returns what it found, and -- only when that is nothing -- why.
/// One port, identified, or None for anything that is not a counter we can
/// have -- not a GMC, not answering, or locked by somebody else.
///
/// THE LOCK IS THE FILTER. A port this process already holds fails at the
/// flock exactly as another process's would, so a rescan can simply try every
/// candidate and let the ones it already has fall out. That costs one open
/// and one close per port per scan and needs no bookkeeping to go stale.
pub fn open_at(path: &str, baud: Option<u32>) -> Option<Counter> {
    identify(path, baud).ok().flatten()
}

pub fn find_all(devices: &[String], baud: Option<u32>) -> (Vec<Counter>, Option<NotFound>) {
    let ports: Vec<String> = if devices.is_empty() {
        candidate_ports()
    } else {
        devices.to_vec()
    };
    let mut found = Vec::new();
    let mut busy = Vec::new();
    for p in &ports {
        match identify(p, baud) {
            Ok(Some(c)) => found.push(c),
            Ok(None) => {}
            Err(OpenError::Busy) => busy.push(p.clone()),
            Err(_) => {}
        }
    }
    if !found.is_empty() {
        return (found, None);
    }
    // No port at all is `find`'s to describe: it knows a kernel with no
    // driver from a desk with nothing plugged in. This used to fall through
    // to the diagnosis below with an empty list and say "a serial device is
    // present but is not a GMC counter. Tried ." of a Mac with nothing on it.
    if ports.is_empty() {
        return match find(None, baud) {
            Ok(c) => (vec![c], None),
            Err(e) => (Vec::new(), Some(e)),
        };
    }
    // Nothing: say why, in the same words `find` would have used, so one
    // failure does not have two vocabularies.
    let why = match find_in(&ports, baud, busy) {
        Ok(c) => return (vec![c], None),
        Err(e) => e,
    };
    (Vec::new(), Some(why))
}

pub fn find(device: Option<&str>, baud: Option<u32>) -> Result<Counter, NotFound> {
    let ports: Vec<String> = match device {
        Some(d) => vec![d.to_string()],
        None => candidate_ports(),
    };
    if ports.is_empty() {
        if !usb_serial_driver_present() {
            return Err(NotFound {
                reason: "no USB-serial driver in this kernel".into(),
                detail: "Alpine's linux-virt has no ch341/usbserial at all, so a\n\
                         counter plugged in here can never appear as /dev/ttyUSB0.\n\
                         linux-lts and linux-rpi carry the drivers."
                    .into(),
                busy: false,
            });
        }
        return Err(NotFound {
            reason: "no serial device is present".into(),
            detail: "Nothing matching /dev/ttyUSB* or /dev/ttyACM*.".into(),
            busy: false,
        });
    }
    find_in(&ports, baud, Vec::new())
}

/// The diagnosis, given the ports to try and any already known to be busy.
fn find_in(ports: &[String], baud: Option<u32>, mut busy: Vec<String>) -> Result<Counter, NotFound> {
    if busy.is_empty() {
        for p in ports {
            match identify(p, baud) {
                Ok(Some(c)) => return Ok(c),
                Ok(None) => {}
                Err(OpenError::Busy) => busy.push(p.clone()),
                Err(_) => {}
            }
        }
    }
    if !busy.is_empty() {
        return Err(NotFound {
            reason: "the port is already open by another radbeeper".into(),
            detail: format!(
                "{} is locked by another process that is not sharing it.\n\
                 Nothing is wrong with the counter -- something else is reading\n\
                 it. A `service` from this version hands the stream to anyone\n\
                 who asks and never produces this message, so the holder is a\n\
                 one-shot that wants the port to itself -- a `random`, a\n\
                 `backfill`, a `log pull` -- or a build from before the stream\n\
                 existed. Wait for it with --wait, or:\n\
                 doas rc-service radbeeper stop",
                busy.join(", ")
            ),
            busy: true,
        });
    }
    Err(NotFound {
        reason: "a serial device is present but is not a GMC counter".into(),
        detail: format!("Tried {}. None answered <GETVER>> with a GMC version.", ports.join(", ")),
        busy: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_bytes_are_degrees_tenths_a_sign_and_an_end() {
        assert_eq!(decode_temp(&[0x1A, 0x01, 0x00, 0xAA]), TempAnswer::Degrees(26.1));
        assert_eq!(decode_temp(&[0x03, 0x05, 0x01, 0xAA]), TempAnswer::Degrees(-3.5));
        assert_eq!(decode_temp(&[0x00, 0x00, 0x00, 0xAA]), TempAnswer::Degrees(0.0));
    }

    #[test]
    fn eighty_five_is_the_thermometer_not_having_read_yet() {
        assert_eq!(decode_temp(&[0x55, 0x00, 0x00, 0xAA]), TempAnswer::NotYet);
        // 85.1 would be a reading, absurd as it is: only the exact resting
        // value is refused.
        assert_eq!(decode_temp(&[0x55, 0x01, 0x00, 0xAA]), TempAnswer::Degrees(85.1));
    }

    #[test]
    fn two_seconds_of_counts_are_not_a_temperature() {
        // What two heartbeat samples look like, back to back.
        assert_eq!(decode_temp(&[0x80, 0x00, 0x80, 0x03]), TempAnswer::Garbled);
        assert_eq!(decode_temp(&[0x1A, 0x0B, 0x00, 0xAA]), TempAnswer::Garbled);
        assert_eq!(decode_temp(&[0x1A, 0x01, 0x00]), TempAnswer::Garbled);
    }

    #[test]
    fn only_the_firmware_the_document_names_is_asked() {
        assert!(temp_supported("GMC-320Re 4.26"));
        assert!(temp_supported("GMC-320+V4Re 4.83"));
        assert!(temp_supported("GMC-320Re 3.01"));
        assert!(!temp_supported("GMC-320Re 2.10"));
        assert!(!temp_supported("GMC-300Re 4.26"));
        assert!(!temp_supported("GMC-500+Re 2.24"));
        assert!(!temp_supported("GMC-320"));
    }
}
