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

fn candidate_ports() -> Vec<String> {
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

    pub fn datetime(&self) -> Option<String> {
        let r = self.ask(b"<GETDATETIME>>", 7);
        (r.len() == 7).then(|| {
            format!(
                "20{:02}-{:02}-{:02} {:02}:{:02}:{:02}",
                r[0], r[1], r[2], r[3], r[4], r[5]
            )
        })
    }

    pub fn heartbeat(&self, on: bool) {
        let _ = self
            .port
            .write_all(if on { b"<HEARTBEAT1>>" } else { b"<HEARTBEAT0>>" });
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
