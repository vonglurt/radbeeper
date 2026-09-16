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
            .filter(|n| n.starts_with("ttyUSB") || n.starts_with("ttyACM"))
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
    if let Ok(mut d) = fs::read_dir("/sys/bus/usb-serial/drivers") {
        if d.next().is_some() {
            return true;
        }
    }
    false
}

impl Counter {
    fn ask(&self, cmd: &[u8], want: usize) -> Vec<u8> {
        self.port.flush_input();
        let _ = self.port.write_all(cmd);
        self.port
            .read_exact_or_timeout(want, Duration::from_millis(1000))
    }

    pub fn model(&self) -> String {
        for m in ["GMC-320", "GMC-300", "GMC-500", "GMC-600"] {
            if self.version.contains(m) {
                return m.to_string();
            }
        }
        self.version
            .split_whitespace()
            .next()
            .unwrap_or("unknown")
            .to_string()
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
            self.port.flush_input();
            let _ = self.port.write_all(&cmd);
            let chunk = self
                .port
                .read_exact_or_timeout(n, Duration::from_millis(5000));
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
        let r = self.port.read_exact_or_timeout(2, timeout);
        (r.len() == 2).then(|| u16::from_be_bytes([r[0], r[1]]) & COUNT_MASK)
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
        let raw = port.read_exact_or_timeout(14, Duration::from_millis(1000));
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
    let mut busy = Vec::new();
    for p in &ports {
        match identify(p, baud) {
            Ok(Some(c)) => return Ok(c),
            Ok(None) => {}
            Err(OpenError::Busy) => busy.push(p.clone()),
            Err(_) => {}
        }
    }
    if !busy.is_empty() {
        return Err(NotFound {
            reason: "the port is already open by another radbeeper".into(),
            detail: format!(
                "{} is locked by another process. Nothing is wrong with the\n\
                 counter -- something else is reading it. Usually that is the\n\
                 logger service:  doas rc-service radbeeper stop  hands it over.",
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
